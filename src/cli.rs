use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

pub const USAGE: &str = "\
withmcp - launch a coding-agent harness with extra MCP servers

Usage:
  withmcp [options] [--] <harness> [args...]
  withmcp [options] list
  withmcp [options] which [[--] <harness> [args...]]
  withmcp [options] enable [--scope global|project] <server>...
  withmcp [options] disable [--scope global|project] <server>...
  withmcp [options] edit
  withmcp [options] pick

Options:
  -p, --profile <name>    profile to use (default: $WITHMCP_PROFILE, else `default`)
  -e, --enable <server>   enable a server for this run (shorthand: +<server>)
  -d, --disable <server>  disable a server for this run
  -i, --interactive       pick servers before launching
      --config <file>     use this profile file instead of a named profile;
                          the default prefix is derived from its file name
  -h, --help              show this help
  -V, --version           show the version

`list` prints the servers enabled in the current directory; `which` also
shows why, and what a launch of <harness> would do.

`enable` and `disable` change the selected profile file: `--scope global`
(the default) sets the server's `enabled` flag, `--scope project` changes the
path rule for the current directory.

Profiles live in ~/.config/withmcp/profiles/<name>.toml (or under
$WITHMCP_CONFIG_DIR); `edit` opens the selected one.

Everything after <harness> is passed to the harness unchanged. Use `--` to
launch a harness whose name clashes with a subcommand.

Supported harnesses: claude, codex
";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub profile: Option<String>,
    pub config: Option<PathBuf>,
    pub overrides: Vec<Override>,
    pub interactive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Override {
    Enable(String),
    Disable(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// The harness program followed by its arguments.
    Launch(Vec<OsString>),
    Which(Option<Vec<OsString>>),
    List,
    /// `enable` (true) or `disable` (false).
    Toggle {
        enable: bool,
        servers: Vec<String>,
        scope: Scope,
    },
    Edit,
    Pick,
    Help,
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Project,
}

impl std::str::FromStr for Scope {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "global" => Ok(Scope::Global),
            "project" => Ok(Scope::Project),
            _ => bail!("invalid scope `{s}` (expected `global` or `project`)"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Subcommand {
    Which,
    List,
    Enable,
    Disable,
    Edit,
    Pick,
}

/// Parses the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<(Options, Command)> {
    let mut args = args.into_iter();
    let mut opts = Options::default();
    let mut sub = None;
    let mut saw_any = false;
    let mut harness = Vec::new();
    let mut names = Vec::new();
    let mut scope = None;
    while let Some(arg) = args.next() {
        saw_any = true;
        let toggling = matches!(sub, Some(Subcommand::Enable | Subcommand::Disable));
        if arg == "--" {
            if toggling {
                for name in args.by_ref() {
                    names.push(utf8(name)?);
                }
            }
            harness.extend(args);
            break;
        }
        let Some(s) = arg.to_str() else {
            harness.push(arg);
            harness.extend(args);
            break;
        };
        let mut value = |name: &str| -> Result<String> {
            args.next()
                .with_context(|| format!("`{name}` needs a value"))?
                .into_string()
                .map_err(|_| anyhow::anyhow!("the value of `{name}` is not valid UTF-8"))
        };
        match s {
            "-h" | "--help" => return Ok((opts, Command::Help)),
            "-V" | "--version" => return Ok((opts, Command::Version)),
            "-i" | "--interactive" => opts.interactive = true,
            "-p" | "--profile" => opts.profile = Some(value(s)?),
            "-e" | "--enable" => opts.overrides.push(Override::Enable(value(s)?)),
            "-d" | "--disable" => opts.overrides.push(Override::Disable(value(s)?)),
            "--config" => opts.config = Some(value(s)?.into()),
            "--scope" => scope = Some(value(s)?.parse()?),
            _ => {
                if let Some(v) = s.strip_prefix("--profile=") {
                    opts.profile = Some(v.into());
                } else if let Some(v) = s.strip_prefix("--enable=") {
                    opts.overrides.push(Override::Enable(v.into()));
                } else if let Some(v) = s.strip_prefix("--disable=") {
                    opts.overrides.push(Override::Disable(v.into()));
                } else if let Some(v) = s.strip_prefix("--config=") {
                    opts.config = Some(v.into());
                } else if let Some(v) = s.strip_prefix("--scope=") {
                    scope = Some(v.parse()?);
                } else if let Some(v) = s.strip_prefix('+')
                    && !v.is_empty()
                {
                    opts.overrides.push(Override::Enable(v.into()));
                } else if s.starts_with('-') {
                    bail!("unknown option `{s}` (see `withmcp --help`)");
                } else if sub.is_none()
                    && let Some(found) = subcommand(s)
                {
                    sub = Some(found);
                } else if toggling {
                    names.push(s.to_string());
                } else {
                    harness.push(arg);
                    harness.extend(args);
                    break;
                }
            }
        }
    }
    if scope.is_some() && !matches!(sub, Some(Subcommand::Enable | Subcommand::Disable)) {
        bail!("`--scope` only applies to `enable` and `disable`");
    }
    let command = match sub {
        None if harness.is_empty() && !saw_any => Command::Help,
        None if harness.is_empty() => bail!("missing harness (see `withmcp --help`)"),
        None => Command::Launch(harness),
        Some(Subcommand::Which) => Command::Which((!harness.is_empty()).then_some(harness)),
        Some(sub @ (Subcommand::Enable | Subcommand::Disable)) => {
            if names.is_empty() {
                bail!("missing server name (see `withmcp --help`)");
            }
            if !opts.overrides.is_empty() {
                bail!("`-e`, `-d` and `+<server>` cannot be combined with `enable` or `disable`");
            }
            Command::Toggle {
                enable: sub == Subcommand::Enable,
                servers: names,
                scope: scope.unwrap_or(Scope::Global),
            }
        }
        Some(sub @ (Subcommand::List | Subcommand::Edit | Subcommand::Pick)) => {
            if let Some(extra) = harness.first() {
                bail!("unexpected argument `{}`", extra.to_string_lossy());
            }
            match sub {
                Subcommand::List => Command::List,
                Subcommand::Edit => Command::Edit,
                _ => Command::Pick,
            }
        }
    };
    Ok((opts, command))
}

fn subcommand(s: &str) -> Option<Subcommand> {
    match s {
        "which" => Some(Subcommand::Which),
        "list" => Some(Subcommand::List),
        "enable" => Some(Subcommand::Enable),
        "disable" => Some(Subcommand::Disable),
        "edit" => Some(Subcommand::Edit),
        "pick" => Some(Subcommand::Pick),
        _ => None,
    }
}

fn utf8(arg: OsString) -> Result<String> {
    arg.into_string()
        .map_err(|arg| anyhow::anyhow!("`{}` is not valid UTF-8", arg.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<(Options, Command)> {
        parse(args.iter().map(OsString::from))
    }

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn launch_passes_harness_args_through() {
        let (opts, cmd) = run(&["claude", "--resume", "-p", "+x"]).unwrap();
        assert_eq!(opts, Options::default());
        assert_eq!(cmd, Command::Launch(argv(&["claude", "--resume", "-p", "+x"])));
    }

    #[test]
    fn options_before_harness() {
        let (opts, cmd) = run(&["-p", "work", "+gh", "-d", "slack", "--enable=pw", "codex", "exec"]).unwrap();
        assert_eq!(opts.profile.as_deref(), Some("work"));
        assert_eq!(
            opts.overrides,
            [
                Override::Enable("gh".into()),
                Override::Disable("slack".into()),
                Override::Enable("pw".into()),
            ]
        );
        assert_eq!(cmd, Command::Launch(argv(&["codex", "exec"])));
        let (opts, _) = run(&["--config", "/x/work.toml", "list"]).unwrap();
        assert_eq!(opts.config, Some(PathBuf::from("/x/work.toml")));
    }

    #[test]
    fn double_dash_forces_harness() {
        assert_eq!(run(&["--", "edit"]).unwrap().1, Command::Launch(argv(&["edit"])));
        assert_eq!(run(&["edit"]).unwrap().1, Command::Edit);
        assert_eq!(run(&["-p", "work", "list"]).unwrap().1, Command::List);
        assert_eq!(run(&["--", "list"]).unwrap().1, Command::Launch(argv(&["list"])));
    }

    #[test]
    fn which_with_and_without_harness() {
        assert_eq!(run(&["which"]).unwrap().1, Command::Which(None));
        let (opts, cmd) = run(&["which", "-p", "work", "--", "claude"]).unwrap();
        assert_eq!(opts.profile.as_deref(), Some("work"));
        assert_eq!(cmd, Command::Which(Some(argv(&["claude"]))));
        assert_eq!(run(&["which", "which"]).unwrap().1, Command::Which(Some(argv(&["which"]))));
    }

    #[test]
    fn toggle() {
        let (opts, cmd) = run(&["-p", "work", "enable", "a", "--scope", "project", "b"]).unwrap();
        assert_eq!(opts.profile.as_deref(), Some("work"));
        assert_eq!(
            cmd,
            Command::Toggle {
                enable: true,
                servers: vec!["a".into(), "b".into()],
                scope: Scope::Project,
            }
        );
        assert_eq!(
            run(&["disable", "--", "-weird"]).unwrap().1,
            Command::Toggle {
                enable: false,
                servers: vec!["-weird".into()],
                scope: Scope::Global,
            }
        );
        assert_eq!(run(&["--", "enable"]).unwrap().1, Command::Launch(argv(&["enable"])));
        assert!(run(&["enable"]).is_err());
        assert!(run(&["enable", "--scope", "planet", "a"]).is_err());
        assert!(run(&["+x", "enable", "a"]).is_err());
        assert!(run(&["--scope", "global", "list"]).is_err());
    }

    #[test]
    fn errors() {
        assert!(run(&["-x", "claude"]).is_err());
        assert!(run(&["-p"]).is_err());
        assert!(run(&["-p", "work"]).is_err());
        assert!(run(&["edit", "extra"]).is_err());
        assert!(run(&["list", "claude"]).is_err());
        assert_eq!(run(&[]).unwrap().1, Command::Help);
    }
}
