use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::cli::Override;
use crate::config::{Profile, Target, is_bare_key};

pub const DEFAULT_PROFILE: &str = "default";

/// Where the final on/off decision for a server came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Unset,
    Flag,
    Group(String),
    Path { path: String, group: Option<String> },
    Cli { group: Option<String> },
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let via = |f: &mut fmt::Formatter<'_>, group: &Option<String>| match group {
            Some(group) => write!(f, " via group `{group}`"),
            None => Ok(()),
        };
        match self {
            Source::Unset => write!(f, "off by default"),
            Source::Flag => write!(f, "`enabled` flag"),
            Source::Group(group) => write!(f, "group `{group}`"),
            Source::Path { path, group } => {
                write!(f, "path `{path}`")?;
                via(f, group)
            }
            Source::Cli { group } => {
                write!(f, "command line")?;
                via(f, group)
            }
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
/// 1. A server is on if its own `enabled` flag or that of a group containing
///    it is set.
/// 2. Path rules matching `cwd`, least specific first; within a rule, groups
///    before servers.
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
            let decision = match (entry.enabled, profile.enabling_group(server)) {
                (true, _) => Decision {
                    enabled: true,
                    source: Source::Flag,
                },
                (false, Some(group)) => Decision {
                    enabled: true,
                    source: Source::Group(group.to_string()),
                },
                (false, None) => Decision {
                    enabled: false,
                    source: Source::Unset,
                },
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
        let source = |group: Option<&str>| Source::Path {
            path: raw.clone(),
            group: group.map(String::from),
        };
        let is_group = |name: &&String| profile.groups.contains_key(name.as_str());
        for groups_first in [true, false] {
            for name in rule.enable.iter().filter(|n| is_group(n) == groups_first) {
                apply(&mut decisions, profile, name, true, &source)?;
            }
            for name in rule.disable.iter().filter(|n| is_group(n) == groups_first) {
                apply(&mut decisions, profile, name, false, &source)?;
            }
        }
    }

    for o in overrides {
        let (name, enabled) = match o {
            Override::Enable(name) => (name, true),
            Override::Disable(name) => (name, false),
        };
        let source = |group: Option<&str>| Source::Cli {
            group: group.map(String::from),
        };
        apply(&mut decisions, profile, name, enabled, &source)?;
    }

    Ok(Resolution {
        prefix: prefix(name, profile)?,
        decisions,
    })
}

/// Sets server `name`, or every server of group `name`.
fn apply(
    decisions: &mut BTreeMap<String, Decision>,
    profile: &Profile,
    name: &str,
    enabled: bool,
    source: &dyn Fn(Option<&str>) -> Source,
) -> Result<()> {
    let (servers, group) = match profile.target(name) {
        Some(Target::Server) => (vec![name.to_string()], None),
        Some(Target::Group(servers)) => (servers.to_vec(), Some(name)),
        None => bail!("unknown server or group `{name}`"),
    };
    for server in servers {
        decisions.insert(
            server,
            Decision {
                enabled,
                source: source(group),
            },
        );
    }
    Ok(())
}

/// The prefix of server names of profile `name` passed to the harness.
pub fn prefix(name: &str, profile: &Profile) -> Result<String> {
    match &profile.prefix {
        Some(prefix) => Ok(prefix.clone()),
        None if is_bare_key(name) => Ok(format!("{name}_")),
        None => {
            bail!("cannot derive a server name prefix from profile name `{name}`; set `prefix`")
        }
    }
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
            enabled = true
            [servers.linear]
            url = "https://example.com/linear"
            [servers.playwright]
            command = "npx"
            [servers.chrome]
            command = "npx"
            [servers.pinned]
            command = "npx"
            enabled = false

            [groups.devtools]
            servers = ["playwright", "chrome"]
            [groups.always]
            servers = ["linear", "pinned"]
            enabled = true

            [paths."~/code/acme"]
            disable = ["linear"]
            [paths."~/code/acme/docs"]
            enable = ["linear", "devtools"]
            disable = ["chrome"]
            "#,
        )
        .unwrap()
    }

    fn run(cwd: &str, overrides: &[Override]) -> Result<Resolution> {
        let cwd = cwd.replace('~', HOME);
        resolve(
            "work",
            &profile(),
            Path::new(&cwd),
            Some(Path::new(HOME)),
            overrides,
        )
    }

    fn state(r: &Resolution, server: &str) -> (bool, String) {
        let d = &r.decisions[server];
        (d.enabled, d.source.to_string())
    }

    #[test]
    fn flags_and_groups() {
        let r = run("/elsewhere", &[]).unwrap();
        assert_eq!(state(&r, "github"), (true, "`enabled` flag".into()));
        assert_eq!(state(&r, "linear"), (true, "group `always`".into()));
        assert_eq!(
            state(&r, "pinned"),
            (true, "group `always`".into()),
            "groups win over `enabled = false`"
        );
        assert_eq!(state(&r, "playwright"), (false, "off by default".into()));
        assert_eq!(
            r.enabled().collect::<Vec<_>>(),
            ["github", "linear", "pinned"]
        );
    }

    #[test]
    fn deeper_paths_win_and_servers_beat_groups_in_a_rule() {
        let r = run("~/code/acme", &[]).unwrap();
        assert_eq!(state(&r, "linear"), (false, "path `~/code/acme`".into()));
        let r = run("~/code/acme/docs/src", &[]).unwrap();
        assert_eq!(
            state(&r, "linear"),
            (true, "path `~/code/acme/docs`".into())
        );
        assert_eq!(
            state(&r, "playwright"),
            (true, "path `~/code/acme/docs` via group `devtools`".into())
        );
        assert_eq!(
            state(&r, "chrome"),
            (false, "path `~/code/acme/docs`".into())
        );
    }

    #[test]
    fn paths_match_whole_components() {
        let r = run("~/code/acme-other", &[]).unwrap();
        assert!(state(&r, "linear").0);
    }

    #[test]
    fn cli_overrides_apply_in_order() {
        let overrides = [
            Override::Disable("github".into()),
            Override::Enable("devtools".into()),
            Override::Disable("chrome".into()),
        ];
        let r = run("/elsewhere", &overrides).unwrap();
        assert_eq!(state(&r, "github"), (false, "command line".into()));
        assert_eq!(
            state(&r, "playwright"),
            (true, "command line via group `devtools`".into())
        );
        assert_eq!(state(&r, "chrome"), (false, "command line".into()));
        let err = run("/", &[Override::Enable("nope".into())]).unwrap_err();
        assert!(err.to_string().contains("unknown server or group `nope`"));
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
        assert!(
            resolve(
                "my.work",
                &profile,
                Path::new("/"),
                Some(Path::new(HOME)),
                &[]
            )
            .is_err()
        );
    }
}
