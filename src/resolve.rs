use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::cli::Override;
use crate::config::Config;

pub const DEFAULT_PROFILE: &str = "default";

/// Where the final on/off decision for a server came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Unset,
    Profile(String),
    Path { profile: String, path: String },
    Cli,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Unset => write!(f, "not enabled"),
            Source::Profile(profile) => write!(f, "profile `{profile}`"),
            Source::Path { profile, path } => write!(f, "path `{path}` in profile `{profile}`"),
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
    pub profile: String,
    pub decisions: BTreeMap<String, Decision>,
}

impl Resolution {
    pub fn enabled(&self) -> impl Iterator<Item = &str> {
        self.decisions
            .iter()
            .filter(|(_, d)| d.enabled)
            .map(|(name, _)| name.as_str())
    }
}

/// Decides which servers are enabled. Later steps win:
/// 1. `enable`/`disable` of each profile in the `extends` chain, root first.
/// 2. Path rules of the whole chain matching `cwd`, least specific first; at
///    equal depth, rules of the child profile come last.
/// 3. Command-line overrides, in order.
///
/// `profile` is `None` when the user selected none; a missing `default`
/// profile then counts as empty rather than as an error.
pub fn resolve(
    config: &Config,
    profile: Option<&str>,
    cwd: &Path,
    home: Option<&Path>,
    overrides: &[Override],
) -> Result<Resolution> {
    let name = profile.unwrap_or(DEFAULT_PROFILE);
    let chain = if profile.is_none() && !config.profiles.contains_key(name) {
        Vec::new()
    } else {
        config.chain(name)?
    };
    let mut decisions: BTreeMap<String, Decision> = config
        .servers
        .keys()
        .map(|server| {
            let decision = Decision {
                enabled: false,
                source: Source::Unset,
            };
            (server.clone(), decision)
        })
        .collect();

    for (profile, rules) in &chain {
        let source = Source::Profile(profile.to_string());
        apply(&mut decisions, &rules.enable, &rules.disable, &source)?;
    }

    let mut matches = Vec::new();
    for (index, (profile, rules)) in chain.iter().enumerate() {
        for (raw, rule) in &rules.paths {
            let path = normalize(raw, home)?;
            if cwd.starts_with(&path) {
                matches.push((path.components().count(), index, profile, raw, rule));
            }
        }
    }
    matches.sort_by_key(|&(depth, index, ..)| (depth, index));
    for (_, _, profile, raw, rule) in matches {
        let source = Source::Path {
            profile: profile.to_string(),
            path: raw.clone(),
        };
        apply(&mut decisions, &rule.enable, &rule.disable, &source)?;
    }

    for o in overrides {
        let (server, enabled) = match o {
            Override::Enable(server) => (server, true),
            Override::Disable(server) => (server, false),
        };
        set(&mut decisions, server, enabled, &Source::Cli)?;
    }

    Ok(Resolution {
        profile: name.to_string(),
        decisions,
    })
}

fn apply(
    decisions: &mut BTreeMap<String, Decision>,
    enable: &[String],
    disable: &[String],
    source: &Source,
) -> Result<()> {
    for server in enable {
        set(decisions, server, true, source)?;
    }
    for server in disable {
        set(decisions, server, false, source)?;
    }
    Ok(())
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

fn normalize(raw: &str, home: Option<&Path>) -> Result<PathBuf> {
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

    fn config() -> Config {
        Config::parse(
            r#"
            [servers.github]
            url = "https://example.com/gh"
            [servers.linear]
            url = "https://example.com/linear"
            [servers.playwright]
            command = "npx"

            [profiles.default]
            enable = ["github"]
            [profiles.default.paths."~/code/tenzir"]
            enable = ["linear"]
            [profiles.default.paths."~/code/tenzir/docs"]
            enable = ["playwright"]
            disable = ["linear"]

            [profiles.work]
            extends = "default"
            enable = ["linear"]
            disable = ["github"]
            [profiles.work.paths."~/code/tenzir"]
            enable = ["github"]
            "#,
        )
        .unwrap()
    }

    fn run(profile: Option<&str>, cwd: &str, overrides: &[Override]) -> Resolution {
        let cwd = cwd.replace('~', HOME);
        resolve(&config(), profile, Path::new(&cwd), Some(Path::new(HOME)), overrides).unwrap()
    }

    fn state(r: &Resolution, server: &str) -> (bool, String) {
        let d = &r.decisions[server];
        (d.enabled, d.source.to_string())
    }

    #[test]
    fn missing_default_profile_is_empty() {
        let config = Config::parse("[servers.a]\ncommand = \"x\"").unwrap();
        let r = resolve(&config, None, Path::new("/"), None, &[]).unwrap();
        assert_eq!(r.enabled().count(), 0);
        assert!(resolve(&config, Some("default"), Path::new("/"), None, &[]).is_err());
    }

    #[test]
    fn profile_chain() {
        let r = run(Some("work"), "/elsewhere", &[]);
        assert_eq!(state(&r, "github"), (false, "profile `work`".into()));
        assert_eq!(state(&r, "linear"), (true, "profile `work`".into()));
        assert_eq!(state(&r, "playwright"), (false, "not enabled".into()));
    }

    #[test]
    fn paths_override_profiles_and_deeper_paths_win() {
        let r = run(None, "~/code/tenzir/docs/src", &[]);
        assert_eq!(state(&r, "github"), (true, "profile `default`".into()));
        assert_eq!(
            state(&r, "linear"),
            (false, "path `~/code/tenzir/docs` in profile `default`".into())
        );
        assert!(state(&r, "playwright").0);
    }

    #[test]
    fn inherited_paths_apply_after_child_base_and_child_wins_ties() {
        let r = run(Some("work"), "~/code/tenzir", &[]);
        assert_eq!(
            state(&r, "github"),
            (true, "path `~/code/tenzir` in profile `work`".into())
        );
        assert_eq!(
            state(&r, "linear"),
            (true, "path `~/code/tenzir` in profile `default`".into())
        );
    }

    #[test]
    fn paths_match_whole_components() {
        let r = run(None, "~/code/tenzir-other", &[]);
        assert!(!state(&r, "linear").0);
    }

    #[test]
    fn cli_overrides_win() {
        let overrides = [
            Override::Disable("github".into()),
            Override::Enable("playwright".into()),
        ];
        let r = run(None, "/elsewhere", &overrides);
        assert_eq!(state(&r, "github"), (false, "command line".into()));
        assert_eq!(state(&r, "playwright"), (true, "command line".into()));
        let err = resolve(
            &config(),
            None,
            Path::new("/"),
            Some(Path::new(HOME)),
            &[Override::Enable("nope".into())],
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown server `nope`"));
    }
}
