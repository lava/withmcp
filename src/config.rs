use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub servers: BTreeMap<String, Server>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawServer")]
pub enum Server {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServer {
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

impl TryFrom<RawServer> for Server {
    type Error = String;

    fn try_from(raw: RawServer) -> Result<Self, String> {
        match (raw.command, raw.url) {
            (Some(command), None) => {
                if !raw.headers.is_empty() {
                    return Err("`headers` is only valid for `url` servers".into());
                }
                Ok(Server::Stdio {
                    command,
                    args: raw.args,
                    env: raw.env,
                })
            }
            (None, Some(url)) => {
                if !raw.args.is_empty() || !raw.env.is_empty() {
                    return Err("`args` and `env` are only valid for `command` servers".into());
                }
                Ok(Server::Http {
                    url,
                    headers: raw.headers,
                })
            }
            (Some(_), Some(_)) => Err("a server needs either `command` or `url`, not both".into()),
            (None, None) => Err("a server needs either `command` or `url`".into()),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub extends: Option<String>,
    #[serde(default)]
    pub enable: Vec<String>,
    #[serde(default)]
    pub disable: Vec<String>,
    #[serde(default)]
    pub paths: BTreeMap<String, Rule>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(default)]
    pub enable: Vec<String>,
    #[serde(default)]
    pub disable: Vec<String>,
}

impl Config {
    /// Loads the config at `path`; a missing file yields an empty config.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        for (name, server) in &self.servers {
            // Codex addresses servers as `mcp_servers.<name>` in `-c` overrides.
            if !is_bare_key(name) {
                bail!("invalid server name `{name}`: use only letters, digits, `-` and `_`");
            }
            let keys = match server {
                Server::Stdio { env, .. } => env.keys(),
                Server::Http { headers, .. } => headers.keys(),
            };
            for key in keys {
                if !is_bare_key(key) {
                    bail!("server `{name}`: invalid key `{key}`: use only letters, digits, `-` and `_`");
                }
            }
        }
        for (name, profile) in &self.profiles {
            if let Some(parent) = &profile.extends
                && !self.profiles.contains_key(parent)
            {
                bail!("profile `{name}` extends unknown profile `{parent}`");
            }
            self.check_rule(&format!("profile `{name}`"), &profile.enable, &profile.disable)?;
            for (path, rule) in &profile.paths {
                let what = format!("profile `{name}`, path `{path}`");
                if !(path == "~" || path.starts_with("~/") || Path::new(path).is_absolute()) {
                    bail!("{what}: paths must be absolute or start with `~/`");
                }
                self.check_rule(&what, &rule.enable, &rule.disable)?;
            }
            self.chain(name)?;
        }
        Ok(())
    }

    fn check_rule(&self, what: &str, enable: &[String], disable: &[String]) -> Result<()> {
        for server in enable.iter().chain(disable) {
            if !self.servers.contains_key(server) {
                bail!("{what}: unknown server `{server}`");
            }
        }
        if let Some(server) = enable.iter().find(|s| disable.contains(s)) {
            bail!("{what}: server `{server}` is both enabled and disabled");
        }
        Ok(())
    }

    /// Returns the `extends` chain of profile `name`, root profile first.
    pub fn chain<'a>(&'a self, name: &'a str) -> Result<Vec<(&'a str, &'a Profile)>> {
        let mut chain: Vec<(&str, &Profile)> = Vec::new();
        let mut current = Some(name);
        while let Some(name) = current {
            if chain.iter().any(|(n, _)| *n == name) {
                bail!("cycle in `extends` involving profile `{name}`");
            }
            let profile = self
                .profiles
                .get(name)
                .with_context(|| format!("unknown profile `{name}`"))?;
            chain.push((name, profile));
            current = profile.extends.as_deref();
        }
        chain.reverse();
        Ok(chain)
    }
}

fn is_bare_key(s: &str) -> bool {
    !s.is_empty()
        && s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .context("$HOME is not set")
}

/// `$WITHMCP_CONFIG`, else `$XDG_CONFIG_HOME/withmcp/config.toml`, else
/// `~/.config/withmcp/config.toml`.
pub fn default_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("WITHMCP_CONFIG").filter(|p| !p.is_empty()) {
        return Ok(path.into());
    }
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home_dir()?.join(".config"),
    };
    Ok(base.join("withmcp").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(text: &str) -> String {
        format!("{:#}", Config::parse(text).unwrap_err())
    }

    const EXAMPLE: &str = include_str!("../examples/config.toml");

    #[test]
    fn example_config_is_empty() {
        let config = Config::parse(EXAMPLE).unwrap();
        assert!(config.servers.is_empty() && config.profiles.is_empty());
    }

    #[test]
    fn uncommented_example_config_is_valid() {
        // `## ` marks prose, `# ` marks commented-out config.
        let uncommented: String = EXAMPLE
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(rest) if !line.starts_with("##") => format!("{rest}\n"),
                _ => format!("{line}\n"),
            })
            .collect();
        let config = Config::parse(&uncommented).unwrap();
        assert_eq!(config.servers.len(), 3);
        assert_eq!(config.profiles["work"].extends.as_deref(), Some("default"));
    }

    #[test]
    fn server_kinds() {
        let config = Config::parse(
            r#"
            [servers.a]
            command = "npx"
            args = ["x"]
            env = { K = "v" }
            [servers.b]
            url = "https://example.com/mcp"
            "#,
        )
        .unwrap();
        assert!(matches!(config.servers["a"], Server::Stdio { .. }));
        assert!(matches!(config.servers["b"], Server::Http { .. }));
    }

    #[test]
    fn rejects_invalid_servers() {
        assert!(error("[servers.a]\ncommand = \"x\"\nurl = \"y\"").contains("not both"));
        assert!(error("[servers.a]\nargs = []").contains("either `command` or `url`"));
        assert!(error("[servers.a]\nurl = \"y\"\nenv = { K = \"v\" }").contains("only valid"));
        assert!(error("[servers.\"a.b\"]\ncommand = \"x\"").contains("invalid server name"));
    }

    #[test]
    fn rejects_invalid_profiles() {
        let servers = "[servers.a]\ncommand = \"x\"\n";
        assert!(error(&format!("{servers}[profiles.p]\nenable = [\"b\"]")).contains("unknown server `b`"));
        assert!(
            error(&format!("{servers}[profiles.p]\nenable = [\"a\"]\ndisable = [\"a\"]"))
                .contains("both enabled and disabled")
        );
        assert!(error(&format!("{servers}[profiles.p]\nextends = \"q\"")).contains("unknown profile"));
        assert!(
            error(&format!(
                "{servers}[profiles.p]\nextends = \"q\"\n[profiles.q]\nextends = \"p\""
            ))
            .contains("cycle")
        );
        assert!(
            error(&format!("{servers}[profiles.p.paths.\"code\"]\nenable = [\"a\"]"))
                .contains("must be absolute")
        );
    }

    #[test]
    fn chain_is_root_first() {
        let config = Config::parse(
            "[profiles.a]\n[profiles.b]\nextends = \"a\"\n[profiles.c]\nextends = \"b\"",
        )
        .unwrap();
        let names: Vec<_> = config.chain("c").unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }
}
