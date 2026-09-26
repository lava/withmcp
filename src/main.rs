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
use crate::cli::{Command, Options, Scope};
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
            eprintln!("withmcp: {err:#}");
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
        Command::List => emit(&render_list(&plan(&opts, None)?))?,
        Command::Toggle {
            enable,
            servers,
            scope,
        } => toggle(&opts, enable, &servers, scope)?,
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
    target: Option<Target>,
}

struct Target {
    harness: Harness,
    argv: Vec<OsString>,
    scan: Scan,
    /// Enabled servers the harness already defines, by exposed name, with
    /// the defining file.
    collisions: Vec<(String, PathBuf)>,
    /// Enabled servers to add by exposed name, before `${VAR}` expansion.
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
    let resolution = resolve::resolve(
        &selection.name,
        &profile,
        &cwd,
        home.as_deref(),
        &opts.overrides,
    )?;
    let target = match argv {
        None => None,
        Some(argv) => {
            let harness = Harness::detect(&argv[0])?;
            let home = home.clone().context("$HOME is not set")?;
            let scan = harness.scan(&Locations::from_env(home, cwd.clone()));
            let mut collisions = Vec::new();
            let mut servers = BTreeMap::new();
            for name in resolution.enabled() {
                let exposed = resolution.exposed_name(name);
                match scan.servers.get(&exposed) {
                    Some(path) => collisions.push((exposed, path.clone())),
                    None => {
                        servers.insert(exposed, profile.servers[name].server.clone());
                    }
                }
            }
            Some(Target {
                harness,
                argv,
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
        target,
    })
}

fn toggle(opts: &Options, enable: bool, servers: &[String], scope: Scope) -> Result<()> {
    let selection = Selection::new(opts)?;
    let home = config::home_dir().ok();
    let shown = display(&selection.path, home.as_deref());
    let text = match std::fs::read_to_string(&selection.path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("{shown} does not exist (create it with `withmcp edit`)")
        }
        Err(err) => return Err(err).with_context(|| format!("reading {shown}")),
    };
    let profile = Profile::parse(&text).with_context(|| format!("in {shown}"))?;
    let mut doc: toml_edit::DocumentMut = text.parse().with_context(|| format!("in {shown}"))?;
    let cwd = current_dir()?;
    let mut report = String::new();
    for server in servers {
        match update::toggle(&mut doc, &profile, server, enable, scope, &cwd, home.as_deref())? {
            update::Outcome::Changed(change) => outln!(report, "{change}"),
            update::Outcome::Unchanged(note) => outln!(report, "{note}"),
        }
    }
    let updated_text = doc.to_string();
    let updated = Profile::parse(&updated_text).context("bug: the updated profile is invalid")?;
    if updated_text != text {
        write_atomically(&selection.path, &updated_text).with_context(|| format!("writing {shown}"))?;
        outln!(report, "updated {shown}");
    }
    emit(&report)?;
    let resolution = resolve::resolve(&selection.name, &updated, &cwd, home.as_deref(), &[])?;
    for server in servers {
        let decision = &resolution.decisions[server];
        if decision.enabled != enable {
            let state = if decision.enabled { "enabled" } else { "disabled" };
            let hint = match (&decision.source, scope) {
                (resolve::Source::Path(_), Scope::Global) => "; use `--scope project` to override it here",
                _ => "",
            };
            eprintln!(
                "withmcp: warning: `{server}` is still {state} here by {}{hint}",
                decision.source
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
        eprintln!("withmcp: warning: {warning}");
    }
    for (name, path) in &target.collisions {
        eprintln!(
            "withmcp: warning: not adding `{name}`: {} already defines it in {}",
            target.harness.name(),
            display(path, plan.home.as_deref())
        );
    }
    let lookup = |name: &str| std::env::var(name).ok();
    let servers = target
        .servers
        .iter()
        .map(|(name, server)| {
            let server = expand::expand_server(server, &lookup)
                .with_context(|| format!("server `{name}`"))?;
            Ok((name.clone(), server))
        })
        .collect::<Result<_>>()?;
    let prepared = target.harness.prepare(&servers, &adapters::runtime_dir())?;
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

fn render_list(plan: &Plan) -> String {
    let mut out = String::new();
    let rows: Vec<_> = plan
        .resolution
        .enabled()
        .map(|name| {
            let summary = match &plan.profile.servers[name].server {
                Server::Stdio { command, args, .. } => std::iter::once(command)
                    .chain(args)
                    .map(|w| shell_quote(w))
                    .collect::<Vec<_>>()
                    .join(" "),
                Server::Http { url, .. } => url.clone(),
            };
            (plan.resolution.exposed_name(name), summary)
        })
        .collect();
    let width = rows.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    for (name, summary) in rows {
        outln!(out, "{name:width$}  {summary}");
    }
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
        .map(|(name, decision)| (name, plan.resolution.exposed_name(name), decision))
        .collect();
    let width = rows.iter().map(|(name, ..)| name.len()).max().unwrap_or(0);
    let exposed_width = rows.iter().map(|(_, e, _)| e.len()).max().unwrap_or(0);
    for (name, exposed, decision) in &rows {
        let state = if decision.enabled { "on " } else { "off" };
        outln!(out, 
            "  {state}  {name:width$}  as {exposed:exposed_width$}  {}",
            decision.source
        );
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
    // Built from unexpanded servers so `${VAR}` values are not printed.
    outln!(out);
    outln!(out, "command (before ${{VAR}} expansion):");
    match target.harness.prepare(&target.servers, &adapters::runtime_dir()) {
        Ok(prepared) => {
            let words: Vec<_> = std::iter::once(&target.argv[0])
                .chain(&prepared.args)
                .chain(&target.argv[1..])
                .map(|w| shell_quote(&w.to_string_lossy()))
                .collect();
            outln!(out, "  {}", words.join(" "));
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
        eprintln!("withmcp: the profile is invalid: {err:#}");
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}
