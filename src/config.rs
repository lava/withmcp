use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// A profile file, `<config dir>/profiles/<name>.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Prepended to server names before they are passed to the harness;
    /// defaults to `<profile>_`.
    pub prefix: Option<String>,
    #[serde(default)]
    pub servers: BTreeMap<String, Entry>,
    #[serde(default)]
    pub paths: BTreeMap<String, Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawServer")]
pub struct Entry {
    pub enabled: bool,
    pub server: Server,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Server {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
        oauth: Option<OAuth>,
    },
}

/// A pre-registered OAuth client, for servers without dynamic client
/// registration. The harness still runs the login.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuth {
    pub client_id: String,
    /// Fixed port for the OAuth callback; only supported by Claude Code.
    pub callback_port: Option<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServer {
    #[serde(default = "default_true")]
    enabled: bool,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    oauth: Option<OAuth>,
}

fn default_true() -> bool {
    true
}

impl TryFrom<RawServer> for Entry {
    type Error = String;

    fn try_from(raw: RawServer) -> Result<Self, String> {
        let server = match (raw.command, raw.url) {
            (Some(command), None) => {
                if !raw.headers.is_empty() || raw.oauth.is_some() {
                    return Err("`headers` and `oauth` are only valid for `url` servers".into());
                }
                Server::Stdio {
                    command,
                    args: raw.args,
                    env: raw.env,
                }
            }
            (None, Some(url)) => {
                if !raw.args.is_empty() || !raw.env.is_empty() {
                    return Err("`args` and `env` are only valid for `command` servers".into());
                }
                Server::Http {
                    url,
                    headers: raw.headers,
                    oauth: raw.oauth,
                }
            }
            (Some(_), Some(_)) => {
                return Err("a server needs either `command` or `url`, not both".into());
            }
            (None, None) => return Err("a server needs either `command` or `url`".into()),
        };
        Ok(Entry {
            enabled: raw.enabled,
            server,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(default)]
    pub enable: Vec<String>,
    #[serde(default)]
    pub disable: Vec<String>,
}

impl Profile {
    /// Loads the profile at `path`, or `None` if the file does not exist.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        Self::parse(&text)
            .map(Some)
            .with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let profile: Self = toml::from_str(text)?;
        profile.validate()?;
        Ok(profile)
    }

    fn validate(&self) -> Result<()> {
        if let Some(prefix) = &self.prefix
            && !prefix.is_empty()
            && !is_bare_key(prefix)
        {
            bail!("invalid prefix `{prefix}`: use only letters, digits, `-` and `_`");
        }
        for (name, entry) in &self.servers {
            // Codex addresses servers as `mcp_servers.<name>` in `-c` overrides.
            if !is_bare_key(name) {
                bail!("invalid server name `{name}`: use only letters, digits, `-` and `_`");
            }
            let keys = match &entry.server {
                Server::Stdio { env, .. } => env.keys(),
                Server::Http { headers, .. } => headers.keys(),
            };
            for key in keys {
                if !is_bare_key(key) {
                    bail!("server `{name}`: invalid key `{key}`: use only letters, digits, `-` and `_`");
                }
            }
        }
        for (path, rule) in &self.paths {
            let what = format!("path `{path}`");
            if !(path == "~" || path.starts_with("~/") || Path::new(path).is_absolute()) {
                bail!("{what}: paths must be absolute or start with `~/`");
            }
            for server in rule.enable.iter().chain(&rule.disable) {
                if !self.servers.contains_key(server) {
                    bail!("{what}: unknown server `{server}`");
                }
            }
            if let Some(server) = rule.enable.iter().find(|s| rule.disable.contains(s)) {
                bail!("{what}: server `{server}` is both enabled and disabled");
            }
        }
        Ok(())
    }
}

pub fn is_bare_key(s: &str) -> bool {
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

/// `$WITHMCP_CONFIG_DIR`, else `$XDG_CONFIG_HOME/withmcp`, else
/// `~/.config/withmcp`.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("WITHMCP_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Ok(dir.into());
    }
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home_dir()?.join(".config"),
    };
    Ok(base.join("withmcp"))
}

pub fn profile_path(config_dir: &Path, profile: &str) -> Result<PathBuf> {
    // Also keeps names like `../x` from escaping the profiles directory.
    if !is_bare_key(profile) {
        bail!("invalid profile name `{profile}`: use only letters, digits, `-` and `_`");
    }
    Ok(config_dir.join("profiles").join(format!("{profile}.toml")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../examples/profile.toml");

    fn error(text: &str) -> String {
        format!("{:#}", Profile::parse(text).unwrap_err())
    }

    #[test]
    fn example_profile_is_empty() {
        let profile = Profile::parse(EXAMPLE).unwrap();
        assert!(profile.prefix.is_none() && profile.servers.is_empty() && profile.paths.is_empty());
    }

    #[test]
    fn uncommented_example_profile_is_valid() {
        // `## ` marks prose, `# ` marks commented-out config.
        let uncommented: String = EXAMPLE
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(rest) if !line.starts_with("##") => format!("{rest}\n"),
                _ => format!("{line}\n"),
            })
            .collect();
        let profile = Profile::parse(&uncommented).unwrap();
        assert_eq!(profile.prefix.as_deref(), Some(""));
        assert_eq!(profile.servers.len(), 3);
        assert!(!profile.servers["playwright"].enabled);
        assert_eq!(profile.paths.len(), 2);
    }

    #[test]
    fn server_kinds() {
        let profile = Profile::parse(
            r#"
            [servers.a]
            command = "npx"
            args = ["x"]
            env = { K = "v" }
            [servers.b]
            url = "https://example.com/mcp"
            enabled = false
            oauth = { client_id = "id", callback_port = 3118 }
            "#,
        )
        .unwrap();
        assert!(profile.servers["a"].enabled);
        assert!(matches!(profile.servers["a"].server, Server::Stdio { .. }));
        assert!(!profile.servers["b"].enabled);
        let Server::Http { oauth, .. } = &profile.servers["b"].server else {
            panic!("not an http server");
        };
        assert_eq!(
            oauth,
            &Some(OAuth {
                client_id: "id".into(),
                callback_port: Some(3118),
            })
        );
    }

    #[test]
    fn rejects_invalid_servers() {
        assert!(error("[servers.a]\ncommand = \"x\"\nurl = \"y\"").contains("not both"));
        assert!(error("[servers.a]\nargs = []").contains("either `command` or `url`"));
        assert!(error("[servers.a]\nurl = \"y\"\nenv = { K = \"v\" }").contains("only valid"));
        assert!(error("[servers.a]\ncommand = \"x\"\noauth = { client_id = \"i\" }").contains("only valid"));
        assert!(error("[servers.a]\nurl = \"y\"\noauth = { client_secret = \"s\" }").contains("unknown field"));
        assert!(error("[servers.\"a.b\"]\ncommand = \"x\"").contains("invalid server name"));
        assert!(error("prefix = \"a.\"").contains("invalid prefix"));
        assert!(error("extends = \"x\"").contains("unknown field"));
    }

    #[test]
    fn rejects_invalid_paths() {
        let servers = "[servers.a]\ncommand = \"x\"\n";
        assert!(error(&format!("{servers}[paths.\"/p\"]\nenable = [\"b\"]")).contains("unknown server `b`"));
        assert!(
            error(&format!("{servers}[paths.\"/p\"]\nenable = [\"a\"]\ndisable = [\"a\"]"))
                .contains("both enabled and disabled")
        );
        assert!(error(&format!("{servers}[paths.\"code\"]\nenable = [\"a\"]")).contains("must be absolute"));
    }

    #[test]
    fn profile_paths() {
        let dir = Path::new("/cfg");
        assert_eq!(profile_path(dir, "work").unwrap(), Path::new("/cfg/profiles/work.toml"));
        assert!(profile_path(dir, "../work").is_err());
        assert!(profile_path(dir, "").is_err());
    }
}
