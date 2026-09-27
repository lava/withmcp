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
  withmcp [options] clientsecret <server>
  withmcp [options] export [<harness>]
  withmcp [options] edit

Options:
  -p, --profile <name>    profile to use (default: `default`)
  +<server>, --enable <server>
                          enable a server for this run; <profile>/<server>
                          pulls one in from another profile
  -<server>, --disable <server>
                          disable a server for this run
      --config <file>     use this profile file instead of a named profile;
                          the default prefix is derived from its file name
  -h, --help              show this help
  -V, --version           show the version

`list` prints the servers for the current directory, marking disabled ones
as (not enabled); `which` also shows why, and what a launch of <harness> would do.
Wherever a server name is accepted, a group name can be used instead.

`enable` and `disable` change the selected profile file: `--scope global`
(the default) sets the server's `enabled` flag, `--scope project` changes the
path rule for the current directory.

`clientsecret` stores the OAuth client secret of a server with `oauth`
settings in Claude Code, which only accepts secrets when a server is added.
It adds a placeholder entry with local scope in
~/.local/share/withmcp/claude-secrets; keep that entry.

`export` writes top-level enabled servers to the harness's user-wide MCP config
and path rules to project configs. Configured disabled servers are removed;
unrelated native servers are kept. Without <harness>, it exports to every
locally installed harness.

Profiles live in ~/.config/withmcp/profiles/<name>.toml; `edit` opens the
selected one.

Everything after <harness> is passed to the harness unchanged. Use `--` to
launch a harness whose name clashes with a subcommand.

Supported harnesses: claude, codex, pi

Environment variables:
  WITHMCP_PROFILE         profile to use when `--profile` is not given
  WITHMCP_CONFIG_DIR      config directory to use instead of ~/.config/withmcp
  VISUAL, EDITOR          editor for `edit` (default: `vi`)
  NO_COLOR                disable colored output
";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub profile: Option<String>,
    pub config: Option<PathBuf>,
    pub overrides: Vec<Override>,
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
    ClientSecret(String),
    Export(Option<String>),
    /// `enable` (true) or `disable` (false).
    Toggle {
        enable: bool,
        servers: Vec<String>,
        scope: Scope,
    },
    Edit,
    Help,
    /// The launch usage and the configured servers, shown without arguments.
    Overview,
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
    ClientSecret,
    Export,
    Edit,
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
        let takes_names = matches!(
            sub,
            Some(
                Subcommand::Enable
                    | Subcommand::Disable
                    | Subcommand::ClientSecret
                    | Subcommand::Export
            )
        );
        if arg == "--" {
            if takes_names {
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
            "-p" | "--profile" => opts.profile = Some(value(s)?),
            "--enable" => opts.overrides.push(Override::Enable(value(s)?)),
            "--disable" => opts.overrides.push(Override::Disable(value(s)?)),
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
                } else if let Some(v) = s.strip_prefix('-')
                    && !v.is_empty()
                    && !v.starts_with('-')
                {
                    const LONG: [&str; 7] = [
                        "help", "version", "profile", "enable", "disable", "config", "scope",
                    ];
                    let name = v.split('=').next().unwrap_or(v);
                    if LONG.contains(&name) {
                        bail!("unknown option `{s}`; did you mean `-{s}`?");
                    }
                    // Every other single-dash word disables a server.
                    opts.overrides.push(Override::Disable(v.into()));
                } else if s.starts_with('-') {
                    bail!("unknown option `{s}` (see `withmcp --help`)");
                } else if sub.is_none()
                    && let Some(found) = subcommand(s)
                {
                    sub = Some(found);
                } else if takes_names {
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
        None if harness.is_empty() && !saw_any => Command::Overview,
        None if harness.is_empty() => bail!("missing harness (see `withmcp --help`)"),
        None => Command::Launch(harness),
        Some(Subcommand::Which) => Command::Which((!harness.is_empty()).then_some(harness)),
        Some(Subcommand::ClientSecret) => {
            let [name] = <[String; 1]>::try_from(names)
                .map_err(|_| anyhow::anyhow!("`clientsecret` takes exactly one server name"))?;
            Command::ClientSecret(name)
        }
        Some(Subcommand::Export) => {
            if names.len() > 1 {
                bail!("`export` takes at most one harness name");
            }
            Command::Export(names.into_iter().next())
        }
        Some(sub @ (Subcommand::Enable | Subcommand::Disable)) => {
            if names.is_empty() {
                bail!("missing server name (see `withmcp --help`)");
            }
            if !opts.overrides.is_empty() {
                bail!("`+<server>` and `-<server>` cannot be combined with `enable` or `disable`");
            }
            Command::Toggle {
                enable: sub == Subcommand::Enable,
                servers: names,
                scope: scope.unwrap_or(Scope::Global),
            }
        }
        Some(sub @ (Subcommand::List | Subcommand::Edit)) => {
            if let Some(extra) = harness.first() {
                bail!("unexpected argument `{}`", extra.to_string_lossy());
            }
            if sub == Subcommand::List {
                Command::List
            } else {
                Command::Edit
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
        "clientsecret" => Some(Subcommand::ClientSecret),
        "export" => Some(Subcommand::Export),
        "edit" => Some(Subcommand::Edit),
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
        assert_eq!(
            cmd,
            Command::Launch(argv(&["claude", "--resume", "-p", "+x"]))
        );
    }

    #[test]
    fn options_before_harness() {
        let (opts, cmd) = run(&[
            "-p",
            "work",
            "+gh",
            "-slack",
            "--enable=pw",
            "+home/x",
            "--disable",
            "p",
            "codex",
            "exec",
        ])
        .unwrap();
        assert_eq!(opts.profile.as_deref(), Some("work"));
        assert_eq!(
            opts.overrides,
            [
                Override::Enable("gh".into()),
                Override::Disable("slack".into()),
                Override::Enable("pw".into()),
                Override::Enable("home/x".into()),
                Override::Disable("p".into()),
            ]
        );
        assert_eq!(cmd, Command::Launch(argv(&["codex", "exec"])));
        let (opts, _) = run(&["--config", "/x/work.toml", "list"]).unwrap();
        assert_eq!(opts.config, Some(PathBuf::from("/x/work.toml")));
    }

    #[test]
    fn double_dash_forces_harness() {
        assert_eq!(
            run(&["--", "edit"]).unwrap().1,
            Command::Launch(argv(&["edit"]))
        );
        assert_eq!(run(&["edit"]).unwrap().1, Command::Edit);
        assert_eq!(run(&["-p", "work", "list"]).unwrap().1, Command::List);
        assert_eq!(
            run(&["--", "list"]).unwrap().1,
            Command::Launch(argv(&["list"]))
        );
    }

    #[test]
    fn which_with_and_without_harness() {
        assert_eq!(run(&["which"]).unwrap().1, Command::Which(None));
        let (opts, cmd) = run(&["which", "-p", "work", "--", "claude"]).unwrap();
        assert_eq!(opts.profile.as_deref(), Some("work"));
        assert_eq!(cmd, Command::Which(Some(argv(&["claude"]))));
        assert_eq!(
            run(&["which", "which"]).unwrap().1,
            Command::Which(Some(argv(&["which"])))
        );
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
        assert_eq!(
            run(&["--", "enable"]).unwrap().1,
            Command::Launch(argv(&["enable"]))
        );
        assert!(run(&["enable"]).is_err());
        assert!(run(&["enable", "--scope", "planet", "a"]).is_err());
        assert!(run(&["+x", "enable", "a"]).is_err());
        assert!(run(&["--scope", "global", "list"]).is_err());
    }

    #[test]
    fn client_secret() {
        let (opts, cmd) = run(&["-p", "work", "clientsecret", "slack"]).unwrap();
        assert_eq!(opts.profile.as_deref(), Some("work"));
        assert_eq!(cmd, Command::ClientSecret("slack".into()));
        assert!(run(&["clientsecret"]).is_err());
        assert!(run(&["clientsecret", "a", "b"]).is_err());
    }

    #[test]
    fn export_harness() {
        let (opts, cmd) = run(&["-p", "work", "export", "codex"]).unwrap();
        assert_eq!(opts.profile.as_deref(), Some("work"));
        assert_eq!(cmd, Command::Export(Some("codex".into())));
        assert_eq!(run(&["export"]).unwrap().1, Command::Export(None));
        assert!(run(&["export", "claude", "pi"]).is_err());
        assert_eq!(
            run(&["--", "export"]).unwrap().1,
            Command::Launch(argv(&["export"]))
        );
    }

    #[test]
    fn errors() {
        assert!(run(&["--x", "claude"]).is_err());
        assert!(run(&["-", "claude"]).is_err());
        let err = run(&["-profile", "work", "claude"]).unwrap_err();
        assert!(err.to_string().contains("did you mean `--profile`"));
        assert!(run(&["-p"]).is_err());
        assert!(run(&["-p", "work"]).is_err());
        assert!(run(&["edit", "extra"]).is_err());
        assert!(run(&["list", "claude"]).is_err());
        assert_eq!(run(&[]).unwrap().1, Command::Overview);
    }
}
