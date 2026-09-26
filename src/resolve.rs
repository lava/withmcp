use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::cli::Override;
use crate::config::{Profile, is_bare_key};

pub const DEFAULT_PROFILE: &str = "default";

/// Where the final on/off decision for a server came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Profile,
    Path(String),
    Cli,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Profile => write!(f, "profile"),
            Source::Path(path) => write!(f, "path `{path}`"),
            Source::Cli => write!(f, "command line"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub enabled: bool,
    pub source: Source,
}

#[derive(Debug)]
pub struct Resolution {
    pub prefix: String,
    /// Keyed by the server name in the profile, without the prefix.
    pub decisions: BTreeMap<String, Decision>,
}

impl Resolution {
    pub fn enabled(&self) -> impl Iterator<Item = &str> {
        self.decisions
            .iter()
            .filter(|(_, d)| d.enabled)
            .map(|(name, _)| name.as_str())
    }

    /// The name the harness sees for server `name`.
    pub fn exposed_name(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }
}

/// Decides which servers of profile `name` are enabled. Later steps win:
/// 1. Each server's `enabled` flag.
/// 2. Path rules matching `cwd`, least specific first.
/// 3. Command-line overrides, in order.
pub fn resolve(
    name: &str,
    profile: &Profile,
    cwd: &Path,
    home: Option<&Path>,
    overrides: &[Override],
) -> Result<Resolution> {
    let mut decisions: BTreeMap<String, Decision> = profile
        .servers
        .iter()
        .map(|(server, entry)| {
            let decision = Decision {
                enabled: entry.enabled,
                source: Source::Profile,
            };
            (server.clone(), decision)
        })
        .collect();

    let mut matches = Vec::new();
    for (raw, rule) in &profile.paths {
        let path = normalize(raw, home)?;
        if cwd.starts_with(&path) {
            matches.push((path.components().count(), raw, rule));
        }
    }
    matches.sort_by_key(|&(depth, ..)| depth);
    for (_, raw, rule) in matches {
        let source = Source::Path(raw.clone());
        for server in &rule.enable {
            set(&mut decisions, server, true, &source)?;
        }
        for server in &rule.disable {
            set(&mut decisions, server, false, &source)?;
        }
    }

    for o in overrides {
        let (server, enabled) = match o {
            Override::Enable(server) => (server, true),
            Override::Disable(server) => (server, false),
        };
        set(&mut decisions, server, enabled, &Source::Cli)?;
    }

    let prefix = match &profile.prefix {
        Some(prefix) => prefix.clone(),
        None if is_bare_key(name) => format!("{name}_"),
        None => bail!("cannot derive a server name prefix from profile name `{name}`; set `prefix`"),
    };
    Ok(Resolution { prefix, decisions })
}

fn set(
    decisions: &mut BTreeMap<String, Decision>,
    server: &str,
    enabled: bool,
    source: &Source,
) -> Result<()> {
    let Some(decision) = decisions.get_mut(server) else {
        bail!("unknown server `{server}`");
    };
    *decision = Decision {
        enabled,
        source: source.clone(),
    };
    Ok(())
}

pub fn normalize(raw: &str, home: Option<&Path>) -> Result<PathBuf> {
    let path = match raw.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let Some(home) = home else {
                bail!("cannot expand `{raw}`: $HOME is not set");
            };
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(raw),
    };
    if !path.is_absolute() {
        bail!("path `{raw}` must be absolute or start with `~/`");
    }
    Ok(path.canonicalize().unwrap_or(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/nonexistent/home";

    fn profile() -> Profile {
        Profile::parse(
            r#"
            [servers.github]
            url = "https://example.com/gh"
            [servers.linear]
            url = "https://example.com/linear"
            enabled = false
            [servers.playwright]
            command = "npx"
            enabled = false

            [paths."~/code/tenzir"]
            enable = ["linear"]
            [paths."~/code/tenzir/docs"]
            enable = ["playwright"]
            disable = ["linear"]
            "#,
        )
        .unwrap()
    }

    fn run(cwd: &str, overrides: &[Override]) -> Result<Resolution> {
        let cwd = cwd.replace('~', HOME);
        resolve("work", &profile(), Path::new(&cwd), Some(Path::new(HOME)), overrides)
    }

    fn state(r: &Resolution, server: &str) -> (bool, String) {
        let d = &r.decisions[server];
        (d.enabled, d.source.to_string())
    }

    #[test]
    fn enabled_flags() {
        let r = run("/elsewhere", &[]).unwrap();
        assert_eq!(state(&r, "github"), (true, "profile".into()));
        assert_eq!(state(&r, "linear"), (false, "profile".into()));
        assert_eq!(r.enabled().collect::<Vec<_>>(), ["github"]);
    }

    #[test]
    fn deeper_paths_win() {
        let r = run("~/code/tenzir", &[]).unwrap();
        assert_eq!(state(&r, "linear"), (true, "path `~/code/tenzir`".into()));
        let r = run("~/code/tenzir/docs/src", &[]).unwrap();
        assert_eq!(state(&r, "linear"), (false, "path `~/code/tenzir/docs`".into()));
        assert_eq!(state(&r, "playwright"), (true, "path `~/code/tenzir/docs`".into()));
    }

    #[test]
    fn paths_match_whole_components() {
        let r = run("~/code/tenzir-other", &[]).unwrap();
        assert!(!state(&r, "linear").0);
    }

    #[test]
    fn cli_overrides_win() {
        let overrides = [
            Override::Disable("github".into()),
            Override::Enable("playwright".into()),
        ];
        let r = run("/elsewhere", &overrides).unwrap();
        assert_eq!(state(&r, "github"), (false, "command line".into()));
        assert_eq!(state(&r, "playwright"), (true, "command line".into()));
        let err = run("/", &[Override::Enable("nope".into())]).unwrap_err();
        assert!(err.to_string().contains("unknown server `nope`"));
    }

    #[test]
    fn prefix() {
        let r = run("/", &[]).unwrap();
        assert_eq!(r.exposed_name("linear"), "work_linear");
        let mut profile = profile();
        profile.prefix = Some(String::new());
        let r = resolve("work", &profile, Path::new("/"), Some(Path::new(HOME)), &[]).unwrap();
        assert_eq!(r.exposed_name("linear"), "linear");
        profile.prefix = None;
        assert!(resolve("my.work", &profile, Path::new("/"), Some(Path::new(HOME)), &[]).is_err());
    }
}
