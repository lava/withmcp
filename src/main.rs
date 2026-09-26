mod adapters;
mod cli;
mod config;
mod expand;
mod native;
mod resolve;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use crate::adapters::Harness;
use crate::cli::{Command, Options};
use crate::config::{Config, Server};
use crate::native::{Locations, Scan};
use crate::resolve::Resolution;

const TEMPLATE: &str = include_str!("../examples/config.toml");

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
        Command::Help => print!("{}", cli::USAGE),
        Command::Version => println!("withmcp {}", env!("CARGO_PKG_VERSION")),
        Command::Edit => return edit(&config_path(&opts)?),
        Command::Pick => bail!("the picker is not implemented yet"),
        Command::Which(argv) => print_plan(&plan(&opts, argv)?),
        Command::Launch(argv) => {
            if opts.interactive {
                bail!("the picker is not implemented yet");
            }
            return launch(plan(&opts, Some(argv))?);
        }
    }
    Ok(ExitCode::SUCCESS)
}

struct Plan {
    config_path: PathBuf,
    home: Option<PathBuf>,
    cwd: PathBuf,
    profile_from_env: bool,
    resolution: Resolution,
    target: Option<Target>,
}

struct Target {
    harness: Harness,
    argv: Vec<OsString>,
    scan: Scan,
    /// Enabled servers the harness already defines, with the defining file.
    collisions: Vec<(String, PathBuf)>,
    /// Enabled servers to add, before `${VAR}` expansion.
    servers: BTreeMap<String, Server>,
}

fn config_path(opts: &Options) -> Result<PathBuf> {
    match &opts.config {
        Some(path) => Ok(path.clone()),
        None => config::default_path(),
    }
}

fn plan(opts: &Options, argv: Option<Vec<OsString>>) -> Result<Plan> {
    let config_path = config_path(opts)?;
    let config = Config::load(&config_path)?;
    let home = config::home_dir().ok();
    let cwd = std::env::current_dir()
        .and_then(|d| d.canonicalize())
        .context("cannot determine the current directory")?;
    let env_profile = std::env::var("WITHMCP_PROFILE").ok().filter(|p| !p.is_empty());
    let profile_from_env = opts.profile.is_none() && env_profile.is_some();
    let profile = opts.profile.clone().or(env_profile);
    let resolution = resolve::resolve(
        &config,
        profile.as_deref(),
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
                match scan.servers.get(name) {
                    Some(path) => collisions.push((name.to_string(), path.clone())),
                    None => {
                        servers.insert(name.to_string(), config.servers[name].clone());
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
        config_path,
        home,
        cwd,
        profile_from_env,
        resolution,
        target,
    })
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

fn print_plan(plan: &Plan) {
    let home = plan.home.as_deref();
    let via_env = if plan.profile_from_env { " (from $WITHMCP_PROFILE)" } else { "" };
    println!("config:  {}", display(&plan.config_path, home));
    println!("profile: {}{via_env}", plan.resolution.profile);
    println!("cwd:     {}", display(&plan.cwd, home));
    println!();
    let width = plan.resolution.decisions.keys().map(String::len).max().unwrap_or(0);
    for (name, decision) in &plan.resolution.decisions {
        let state = if decision.enabled { "on " } else { "off" };
        println!("  {state}  {name:width$}  {}", decision.source);
    }
    let Some(target) = &plan.target else {
        return;
    };
    println!();
    println!("{} servers checked in:", target.harness.name());
    if target.scan.checked.is_empty() {
        println!("  (no config files found)");
    }
    for path in &target.scan.checked {
        println!("  {}", display(path, home));
    }
    for warning in &target.scan.warnings {
        println!("  warning: {warning}");
    }
    for (name, path) in &target.collisions {
        println!("  skipping `{name}`: already defined in {}", display(path, home));
    }
    // Built from unexpanded servers so `${VAR}` values are not printed.
    println!();
    println!("command (before ${{VAR}} expansion):");
    match target.harness.prepare(&target.servers, &adapters::runtime_dir()) {
        Ok(prepared) => {
            let words: Vec<_> = std::iter::once(&target.argv[0])
                .chain(&prepared.args)
                .chain(&target.argv[1..])
                .map(|w| shell_quote(&w.to_string_lossy()))
                .collect();
            println!("  {}", words.join(" "));
        }
        Err(err) => println!("  error: {err:#}"),
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
    if let Err(err) = Config::load(path) {
        eprintln!("withmcp: the config is invalid: {err:#}");
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}
