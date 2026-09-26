mod adapters;
mod cli;
mod config;
mod expand;
mod native;
mod resolve;
mod update;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use crate::adapters::Harness;
use crate::cli::{Command, Options, Override, Scope};
use crate::config::{Profile, Server};
use crate::native::{Locations, Scan};
use crate::resolve::Resolution;

/// Appends a line to a `String`.
macro_rules! outln {
    ($out:expr) => {
        $out.push('\n')
    };
    ($out:expr, $($arg:tt)*) => {{
        $out.push_str(&format!($($arg)*));
        $out.push('\n');
    }};
}

const TEMPLATE: &str = include_str!("../examples/profile.toml");

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            diagnose(Level::Error, &format!("{err:#}"));
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode> {
    let (opts, command) = cli::parse(std::env::args_os().skip(1))?;
    match command {
        Command::Help => emit(cli::USAGE)?,
        Command::Version => emit(&format!("withmcp {}\n", env!("CARGO_PKG_VERSION")))?,
        Command::Edit => return edit(&Selection::new(&opts)?.path),
        Command::Pick => bail!("the picker is not implemented yet"),
        Command::Which(argv) => emit(&render_plan(&plan(&opts, argv)?))?,
        Command::List => emit(&render_list(&plan(&opts, None)?, use_color()))?,
        Command::Toggle {
            enable,
            servers,
            scope,
        } => toggle(&opts, enable, &servers, scope)?,
        Command::ClientSecret(server) => client_secret(&opts, &server)?,
        Command::Launch(argv) => {
            if opts.interactive {
                bail!("the picker is not implemented yet");
            }
            return launch(plan(&opts, Some(argv))?);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The profile chosen by `--config`, `-p`, `$WITHMCP_PROFILE` or the default.
struct Selection {
    name: String,
    path: PathBuf,
    origin: Origin,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Default,
    Flag,
    Env,
    ConfigFlag,
}

impl Selection {
    fn new(opts: &Options) -> Result<Self> {
        if let Some(path) = &opts.config {
            if opts.profile.is_some() {
                bail!("`--config` and `--profile` cannot be used together");
            }
            let name = path
                .file_stem()
                .with_context(|| format!("`{}` is not a file name", path.display()))?;
            return Ok(Self {
                name: name.to_string_lossy().into_owned(),
                path: path.clone(),
                origin: Origin::ConfigFlag,
            });
        }
        let env = std::env::var("WITHMCP_PROFILE").ok().filter(|p| !p.is_empty());
        let (name, origin) = match (&opts.profile, env) {
            (Some(name), _) => (name.clone(), Origin::Flag),
            (None, Some(name)) => (name, Origin::Env),
            (None, None) => (resolve::DEFAULT_PROFILE.into(), Origin::Default),
        };
        let path = config::profile_path(&config::config_dir()?, &name)?;
        Ok(Self { name, path, origin })
    }
}

struct Plan {
    selection: Selection,
    profile: Profile,
    profile_exists: bool,
    home: Option<PathBuf>,
    cwd: PathBuf,
    resolution: Resolution,
    borrowed: Vec<Borrowed>,
    target: Option<Target>,
}

/// A server pulled in from another profile with `+<profile>/<server>`. It
/// keeps that profile's prefix, so it shares logins with launches of it.
struct Borrowed {
    label: String,
    exposed: String,
    server: Server,
}

struct Target {
    harness: Harness,
    argv: Vec<OsString>,
    locations: Locations,
    scan: Scan,
    /// Enabled servers the harness already defines, by exposed name, with
    /// the defining file.
    collisions: Vec<(String, PathBuf)>,
    /// Enabled servers to add by exposed name, before `${VAR}` and `$(command)` expansion.
    servers: BTreeMap<String, Server>,
}

fn plan(opts: &Options, argv: Option<Vec<OsString>>) -> Result<Plan> {
    let selection = Selection::new(opts)?;
    let home = config::home_dir().ok();
    let loaded = Profile::load(&selection.path)?;
    let profile_exists = loaded.is_some();
    let profile = match loaded {
        Some(profile) => profile,
        None if selection.origin == Origin::Default => Profile::default(),
        None if selection.origin == Origin::ConfigFlag => bail!(
            "{} does not exist (create it with `withmcp --config <file> edit`)",
            display(&selection.path, home.as_deref()),
        ),
        None => bail!(
            "profile `{name}` not found: {} does not exist (create it with `withmcp -p {name} edit`)",
            display(&selection.path, home.as_deref()),
            name = selection.name,
        ),
    };
    let cwd = current_dir()?;
    let mut overrides = Vec::new();
    let mut requests = Vec::new();
    for o in &opts.overrides {
        match o {
            Override::Enable(spec) => match spec.split_once('/') {
                Some((other, server))
                    if other == selection.name && selection.origin != Origin::ConfigFlag =>
                {
                    overrides.push(Override::Enable(server.into()));
                }
                Some((other, server)) => requests.push((spec, other, server)),
                None => overrides.push(o.clone()),
            },
            Override::Disable(spec) if spec.contains('/') => {
                bail!("`-{spec}`: servers of other profiles can only be enabled")
            }
            Override::Disable(_) => overrides.push(o.clone()),
        }
    }
    let resolution = resolve::resolve(
        &selection.name,
        &profile,
        &cwd,
        home.as_deref(),
        &overrides,
    )?;
    let borrowed = borrow(&requests, &resolution, &selection, home.as_deref())?;
    let target = match argv {
        None => None,
        Some(argv) => {
            let harness = Harness::detect(&argv[0])?;
            let home = home.clone().context("$HOME is not set")?;
            let locations = Locations::from_env(home, cwd.clone());
            let scan = harness.scan(&locations);
            let mut collisions = Vec::new();
            let mut servers = BTreeMap::new();
            let enabled = resolution.enabled().map(|name| {
                let exposed = resolution.exposed_name(name);
                (exposed, &profile.servers[name].server)
            });
            let enabled = enabled.chain(borrowed.iter().map(|b| (b.exposed.clone(), &b.server)));
            for (exposed, server) in enabled {
                match scan.servers.get(&exposed) {
                    Some(path) => collisions.push((exposed, path.clone())),
                    None => {
                        servers.insert(exposed, server.clone());
                    }
                }
            }
            Some(Target {
                harness,
                argv,
                locations,
                scan,
                collisions,
                servers,
            })
        }
    };
    Ok(Plan {
        selection,
        profile,
        profile_exists,
        home,
        cwd,
        resolution,
        borrowed,
        target,
    })
}

/// Loads the servers requested as `+<profile>/<server>`.
fn borrow(
    requests: &[(&String, &str, &str)],
    resolution: &Resolution,
    selection: &Selection,
    home: Option<&Path>,
) -> Result<Vec<Borrowed>> {
    let config_dir = config::config_dir()?;
    let mut profiles = BTreeMap::new();
    let mut borrowed: Vec<Borrowed> = Vec::new();
    for &(spec, other, server) in requests {
        if !profiles.contains_key(other) {
            let path = config::profile_path(&config_dir, other).with_context(|| format!("`+{spec}`"))?;
            let Some(profile) = Profile::load(&path)? else {
                bail!(
                    "`+{spec}`: profile `{other}` not found: {} does not exist",
                    display(&path, home)
                );
            };
            profiles.insert(other.to_string(), profile);
        }
        let profile = &profiles[other];
        let members = match profile.target(server) {
            Some(config::Target::Server) => vec![server.to_string()],
            Some(config::Target::Group(members)) => members.to_vec(),
            None => bail!("`+{spec}`: profile `{other}` has no server or group `{server}`"),
        };
        let prefix = resolve::prefix(other, profile)?;
        for member in members {
            let exposed = format!("{prefix}{member}");
            if let Some(name) = resolution
                .decisions
                .keys()
                .find(|name| resolution.exposed_name(name) == exposed)
            {
                bail!(
                    "`+{spec}` would pass `{exposed}`, like server `{name}` of profile `{}`",
                    selection.name
                );
            }
            if borrowed.iter().any(|b| b.exposed == exposed) {
                continue;
            }
            borrowed.push(Borrowed {
                label: format!("{other}/{member}"),
                exposed,
                server: profile.servers[&member].server.clone(),
            });
        }
    }
    Ok(borrowed)
}

/// Reads the selected profile file, which must exist.
fn read_profile(selection: &Selection, home: Option<&Path>) -> Result<(String, Profile)> {
    let shown = display(&selection.path, home);
    let text = match std::fs::read_to_string(&selection.path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("{shown} does not exist (create it with `withmcp edit`)")
        }
        Err(err) => return Err(err).with_context(|| format!("reading {shown}")),
    };
    let profile = Profile::parse(&text).with_context(|| format!("in {shown}"))?;
    Ok((text, profile))
}

/// Claude Code only accepts an OAuth client secret when a server is added,
/// so add a local-scope entry for the server in a directory of our own. The
/// harness then finds the stored secret when withmcp passes the server.
fn client_secret(opts: &Options, server: &str) -> Result<()> {
    let selection = Selection::new(opts)?;
    let home = config::home_dir().ok();
    let (_, profile) = read_profile(&selection, home.as_deref())?;
    let entry = profile
        .servers
        .get(server)
        .with_context(|| format!("unknown server `{server}`"))?;
    let expanded = expand::expand_server(&entry.server, &expand::Sources::system()).with_context(|| format!("server `{server}`"))?;
    let Server::Http {
        url,
        oauth: Some(oauth),
        ..
    } = expanded
    else {
        bail!("server `{server}` has no `oauth` settings, so it needs no client secret");
    };
    let name = format!("{}{server}", resolve::prefix(&selection.name, &profile)?);
    let dir = config::data_dir()?.join("claude-secrets");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    // Claude Code keys local-scope servers by the canonical project path.
    let dir = dir.canonicalize()?;
    // Replace an entry from an earlier run so the secret can be changed.
    let _ = std::process::Command::new("claude")
        .args(["mcp", "remove", "--scope", "local", &name])
        .current_dir(&dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let mut command = std::process::Command::new("claude");
    command.current_dir(&dir).args([
        "mcp",
        "add",
        "--scope",
        "local",
        "--transport",
        "http",
        "--client-id",
        &oauth.client_id,
        "--client-secret",
    ]);
    if let Some(port) = oauth.callback_port {
        command.args(["--callback-port", &port.to_string()]);
    }
    command.arg(&name).arg(&url);
    let status = command.status().context("cannot run `claude`")?;
    if !status.success() {
        bail!("`claude mcp add` failed with {status}");
    }
    emit(&format!(
        "stored the client secret for `{name}`; keep its placeholder entry in {}\n",
        display(&dir, home.as_deref())
    ))
}

fn toggle(opts: &Options, enable: bool, names: &[String], scope: Scope) -> Result<()> {
    let selection = Selection::new(opts)?;
    let home = config::home_dir().ok();
    let shown = display(&selection.path, home.as_deref());
    let (text, mut profile) = read_profile(&selection, home.as_deref())?;
    let mut doc: toml_edit::DocumentMut = text.parse().with_context(|| format!("in {shown}"))?;
    let cwd = current_dir()?;
    let mut report = String::new();
    for name in names {
        match update::toggle(&mut doc, &profile, name, enable, scope, &cwd, home.as_deref())? {
            update::Outcome::Changed(change) => outln!(report, "{change}"),
            update::Outcome::Unchanged(note) => outln!(report, "{note}"),
        }
        // Later names may depend on this change, e.g. a group's flag.
        profile = Profile::parse(&doc.to_string()).context("bug: the updated profile is invalid")?;
    }
    let updated_text = doc.to_string();
    let updated = profile;
    if updated_text != text {
        write_atomically(&selection.path, &updated_text).with_context(|| format!("writing {shown}"))?;
        outln!(report, "updated {shown}");
    }
    emit(&report)?;
    let resolution = resolve::resolve(&selection.name, &updated, &cwd, home.as_deref(), &[])?;
    let servers = names.iter().flat_map(|name| match updated.target(name) {
        Some(config::Target::Group(servers)) => servers.to_vec(),
        _ => vec![name.clone()],
    });
    for server in servers {
        let decision = &resolution.decisions[&server];
        if decision.enabled != enable {
            let state = if decision.enabled { "enabled" } else { "disabled" };
            let hint = match (&decision.source, scope) {
                (resolve::Source::Path { .. }, Scope::Global) => "; use `--scope project` to override it here",
                (resolve::Source::Group(_), _) => "; disable the group or use `--scope project`",
                _ => "",
            };
            diagnose(
                Level::Warning,
                &format!("`{server}` is still {state} here by {}{hint}", decision.source),
            );
        }
    }
    Ok(())
}

fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn current_dir() -> Result<PathBuf> {
    std::env::current_dir()
        .and_then(|d| d.canonicalize())
        .context("cannot determine the current directory")
}

fn launch(plan: Plan) -> Result<ExitCode> {
    let target = plan.target.context("no harness to launch")?;
    for warning in &target.scan.warnings {
        diagnose(Level::Warning, warning);
    }
    for (name, path) in &target.collisions {
        diagnose(
            Level::Warning,
            &format!(
                "not adding `{name}`: {} already defines it in {}",
                target.harness.name(),
                display(path, plan.home.as_deref())
            ),
        );
    }
    let sources = expand::Sources::system();
    let servers = target
        .servers
        .iter()
        .map(|(name, server)| {
            let server = expand::expand_server(server, &sources)
                .with_context(|| format!("server `{name}`"))?;
            Ok((name.clone(), server))
        })
        .collect::<Result<_>>()?;
    let prepared = target
        .harness
        .prepare(&servers, &target.locations, &adapters::runtime_dir())?;
    for warning in &prepared.warnings {
        diagnose(Level::Warning, warning);
    }
    prepared.write_files()?;
    let mut command = std::process::Command::new(&target.argv[0]);
    command.args(&prepared.args).args(&target.argv[1..]);
    exec(command)
}

#[cfg(unix)]
fn exec(mut command: std::process::Command) -> Result<ExitCode> {
    use std::os::unix::process::CommandExt;
    let err = command.exec();
    Err(err).with_context(|| format!("cannot run `{}`", command.get_program().to_string_lossy()))
}

#[cfg(not(unix))]
fn exec(mut command: std::process::Command) -> Result<ExitCode> {
    let status = command
        .status()
        .with_context(|| format!("cannot run `{}`", command.get_program().to_string_lossy()))?;
    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}

/// One line per server by its name in the profile (`<profile>/<name>` for
/// borrowed ones), disabled ones marked `(not enabled)` and, with `color`,
/// dimmed.
fn render_list(plan: &Plan, color: bool) -> String {
    let local = plan.resolution.decisions.iter().map(|(name, decision)| {
        let server = &plan.profile.servers[name].server;
        (decision.enabled, name.clone(), server)
    });
    let borrowed = plan.borrowed.iter().map(|b| (true, b.label.clone(), &b.server));
    let rows: Vec<_> = local.chain(borrowed).collect();
    let width = rows.iter().map(|(_, name, _)| name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for (enabled, name, server) in rows {
        let summary = match server {
            Server::Stdio { command, args, .. } => std::iter::once(command)
                .chain(args)
                .map(|w| shell_quote(w))
                .collect::<Vec<_>>()
                .join(" "),
            Server::Http { url, .. } => url.clone(),
        };
        let suffix = if enabled { "" } else { "  (not enabled)" };
        let line = format!("{name:width$}  {summary}{suffix}");
        if color && !enabled {
            outln!(out, "\x1b[2m{line}\x1b[0m");
        } else {
            outln!(out, "{line}");
        }
    }
    out
}

fn use_color() -> bool {
    color_on(&std::io::stdout())
}

fn color_on(stream: &impl std::io::IsTerminal) -> bool {
    stream.is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
}

enum Level {
    Error,
    Warning,
}

fn diagnose(level: Level, message: &str) {
    eprintln!("{}", render_diagnostic(level, message, color_on(&std::io::stderr())));
}

/// Formats a message for stderr, dropping the backticks around `code`
/// spans. With `color`, the label is red or yellow and the spans are blue.
fn render_diagnostic(level: Level, message: &str, color: bool) -> String {
    let (label, code) = match level {
        Level::Error => ("error:", "31"),
        Level::Warning => ("warning:", "33"),
    };
    let mut out = if color {
        format!("\x1b[1;{code}m{label}\x1b[0m ")
    } else {
        format!("{label} ")
    };
    let mut rest = message;
    while let Some((before, after)) = rest.split_once('`')
        && let Some((span, tail)) = after.split_once('`')
    {
        out.push_str(before);
        if color {
            out.push_str(&format!("\x1b[94m{span}\x1b[0m"));
        } else {
            out.push_str(span);
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn render_plan(plan: &Plan) -> String {
    let mut out = String::new();
    let home = plan.home.as_deref();
    let selection = &plan.selection;
    let via = match selection.origin {
        Origin::Env => " (from $WITHMCP_PROFILE)",
        Origin::ConfigFlag => " (from --config)",
        Origin::Default | Origin::Flag => "",
    };
    let missing = if plan.profile_exists { "" } else { " (does not exist)" };
    outln!(out, "profile: {}{via}", selection.name);
    outln!(out, "file:    {}{missing}", display(&selection.path, home));
    outln!(out, "prefix:  {:?}", plan.resolution.prefix);
    outln!(out, "cwd:     {}", display(&plan.cwd, home));
    outln!(out);
    let rows: Vec<_> = plan
        .resolution
        .decisions
        .iter()
        .map(|(name, decision)| {
            let state = if decision.enabled { "on " } else { "off" };
            (name.clone(), plan.resolution.exposed_name(name), state, decision.source.to_string())
        })
        .chain(
            plan.borrowed
                .iter()
                .map(|b| (b.label.clone(), b.exposed.clone(), "on ", "command line".to_string())),
        )
        .collect();
    let width = rows.iter().map(|(name, ..)| name.len()).max().unwrap_or(0);
    let exposed_width = rows.iter().map(|(_, e, ..)| e.len()).max().unwrap_or(0);
    for (name, exposed, state, source) in &rows {
        outln!(out, "  {state}  {name:width$}  as {exposed:exposed_width$}  {source}");
    }
    let Some(target) = &plan.target else {
        return out;
    };
    outln!(out);
    outln!(out, "{} servers checked in:", target.harness.name());
    if target.scan.checked.is_empty() {
        outln!(out, "  (no config files found)");
    }
    for path in &target.scan.checked {
        outln!(out, "  {}", display(path, home));
    }
    for warning in &target.scan.warnings {
        outln!(out, "  warning: {warning}");
    }
    for (name, path) in &target.collisions {
        outln!(out, "  skipping `{name}`: already defined in {}", display(path, home));
    }
    // Built from unexpanded servers so secrets are not printed and no commands run.
    outln!(out);
    outln!(out, "command (before ${{VAR}} and $(command) expansion):");
    match target
        .harness
        .prepare(&target.servers, &target.locations, &adapters::runtime_dir())
    {
        Ok(prepared) => {
            let words: Vec<_> = std::iter::once(&target.argv[0])
                .chain(&prepared.args)
                .chain(&target.argv[1..])
                .map(|w| shell_quote(&w.to_string_lossy()))
                .collect();
            outln!(out, "  {}", words.join(" "));
            for warning in &prepared.warnings {
                outln!(out, "  warning: {warning}");
            }
        }
        Err(err) => outln!(out, "  error: {err:#}"),
    }
    out
}

/// Writes to stdout; a closed pipe (e.g. `withmcp list | head`) is not an
/// error.
fn emit(text: &str) -> Result<()> {
    use std::io::Write;
    match std::io::stdout().lock().write_all(text.as_bytes()) {
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        result => result.context("writing to stdout"),
    }
}

fn display(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn shell_quote(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./=:@+,".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

fn edit(path: &Path) -> Result<ExitCode> {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(path, TEMPLATE).with_context(|| format!("writing {}", path.display()))?;
    }
    let editor = ["VISUAL", "EDITOR"]
        .into_iter()
        .find_map(|var| std::env::var(var).ok().filter(|e| !e.trim().is_empty()))
        .unwrap_or_else(|| "vi".into());
    let mut words = editor.split_whitespace();
    let program = words.next().context("empty editor command")?;
    let status = std::process::Command::new(program)
        .args(words)
        .arg(path)
        .status()
        .with_context(|| format!("cannot run editor `{program}`"))?;
    if !status.success() {
        bail!("editor exited with {status}");
    }
    if let Err(err) = Profile::load(path) {
        diagnose(Level::Error, &format!("the profile is invalid: {err:#}"));
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics() {
        let message = "server `github`: environment variable `GITHUB_MCP_PAT` is not set";
        assert_eq!(
            render_diagnostic(Level::Error, message, false),
            "error: server github: environment variable GITHUB_MCP_PAT is not set"
        );
        assert_eq!(
            render_diagnostic(Level::Error, message, true),
            "\x1b[1;31merror:\x1b[0m server \x1b[94mgithub\x1b[0m: environment variable \
             \x1b[94mGITHUB_MCP_PAT\x1b[0m is not set"
        );
        assert_eq!(
            render_diagnostic(Level::Warning, "stray ` here", true),
            "\x1b[1;33mwarning:\x1b[0m stray ` here"
        );
    }
}
