//! Per-harness translation of the enabled servers into command-line arguments.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::config::Server;
use crate::native::{self, Locations, Scan};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
    Pi,
}

/// Arguments to insert right after the harness program, plus files they
/// reference that must be written before launching.
#[derive(Debug, Default)]
pub struct Prepared {
    pub args: Vec<OsString>,
    pub files: Vec<(PathBuf, Vec<u8>)>,
    pub warnings: Vec<String>,
}

impl Harness {
    pub fn detect(program: &OsStr) -> Result<Self> {
        let name = Path::new(program).file_stem().and_then(OsStr::to_str);
        match name {
            Some("claude") => Ok(Harness::Claude),
            Some("codex") => Ok(Harness::Codex),
            Some("pi") => Ok(Harness::Pi),
            _ => bail!(
                "unsupported harness `{}` (supported: claude, codex, pi)",
                program.to_string_lossy()
            ),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Pi => "pi",
        }
    }

    pub fn scan(self, loc: &Locations) -> Scan {
        match self {
            Harness::Claude => native::scan_claude(loc),
            Harness::Codex => native::scan_codex(loc),
            Harness::Pi => native::scan_pi(loc),
        }
    }

    pub fn prepare(
        self,
        servers: &BTreeMap<String, Server>,
        loc: &Locations,
        runtime_dir: &Path,
    ) -> Result<Prepared> {
        if servers.is_empty() {
            return Ok(Prepared::default());
        }
        Ok(match self {
            Harness::Claude => config_file(runtime_dir, "claude", &claude_config(servers))?,
            Harness::Codex => Prepared {
                args: codex_args(servers),
                ..Prepared::default()
            },
            Harness::Pi => {
                let mut prepared = config_file(runtime_dir, "pi", &pi_config(servers, loc)?)?;
                if !native::pi_has_mcp_adapter(loc) {
                    prepared.warnings.push(
                        "pi-mcp-adapter does not seem to be installed, and pi needs it for MCP \
                         servers (`pi install npm:pi-mcp-adapter`)"
                            .into(),
                    );
                }
                prepared
            }
        })
    }
}

/// A generated config file named after its contents, passed with
/// `--mcp-config`.
fn config_file(runtime_dir: &Path, stem: &str, config: &serde_json::Value) -> Result<Prepared> {
    let bytes = serde_json::to_vec_pretty(config)?;
    let mut hasher = std::hash::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let path = runtime_dir.join(format!("{stem}-{:016x}.json", hasher.finish()));
    Ok(Prepared {
        args: vec!["--mcp-config".into(), path.clone().into()],
        files: vec![(path, bytes)],
        ..Prepared::default()
    })
}

impl Prepared {
    pub fn write_files(&self) -> Result<()> {
        for (path, bytes) in &self.files {
            if std::fs::read(path).is_ok_and(|existing| existing == *bytes) {
                continue;
            }
            let dir = path.parent().context("generated file has no parent directory")?;
            create_private_dir(dir)?;
            let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
            write_private(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
            std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
        }
        Ok(())
    }
}

fn claude_config(servers: &BTreeMap<String, Server>) -> serde_json::Value {
    let servers: serde_json::Map<_, _> = servers
        .iter()
        .map(|(name, server)| {
            let value = match server {
                Server::Stdio { command, args, env } => {
                    json!({ "type": "stdio", "command": command, "args": args, "env": env })
                }
                Server::Http { url, headers, oauth } => {
                    let mut value = json!({ "type": "http", "url": url, "headers": headers });
                    if let Some(oauth) = oauth {
                        let mut config = json!({ "clientId": oauth.client_id });
                        if let Some(port) = oauth.callback_port {
                            config["callbackPort"] = port.into();
                        }
                        value["oauth"] = config;
                    }
                    value
                }
            };
            (name.clone(), value)
        })
        .collect();
    json!({ "mcpServers": servers })
}

/// pi-mcp-adapter's `--mcp-config` replaces the Pi agent dir's `mcp.json`
/// instead of adding to it, so the servers are merged into a copy of that
/// file. Names it already defines were filtered out as collisions.
fn pi_config(servers: &BTreeMap<String, Server>, loc: &Locations) -> Result<serde_json::Value> {
    let base = loc.pi_agent_dir().join("mcp.json");
    let mut config = match std::fs::read_to_string(&base) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("cannot merge the servers into {}", base.display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(err) => return Err(err).with_context(|| format!("reading {}", base.display())),
    };
    let Some(object) = config.as_object_mut() else {
        bail!("cannot merge the servers into {}: not a JSON object", base.display());
    };
    let key = if object.contains_key("mcp-servers") && !object.contains_key("mcpServers") {
        "mcp-servers"
    } else {
        "mcpServers"
    };
    let Some(existing) = object.entry(key).or_insert_with(|| json!({})).as_object_mut() else {
        bail!("cannot merge the servers into {}: `{key}` is not an object", base.display());
    };
    for (name, server) in servers {
        let value = match server {
            Server::Stdio { command, args, env } => {
                json!({ "command": command, "args": args, "env": env })
            }
            Server::Http { url, headers, oauth } => {
                let mut value = json!({ "url": url, "headers": headers });
                if let Some(oauth) = oauth {
                    let mut config = json!({ "clientId": oauth.client_id });
                    if let Some(port) = oauth.callback_port {
                        config["redirectUri"] = format!("http://localhost:{port}/callback").into();
                    }
                    value["auth"] = "oauth".into();
                    value["oauth"] = config;
                }
                value
            }
        };
        existing.insert(name.clone(), value);
    }
    Ok(config)
}

/// Server names and env/header keys are validated as TOML bare keys, so they
/// can be spliced into the dotted key path unquoted.
fn codex_args(servers: &BTreeMap<String, Server>) -> Vec<OsString> {
    let mut args = Vec::new();
    let mut push = |key: String, value: toml::Value| {
        args.push("-c".into());
        args.push(format!("mcp_servers.{key}={value}").into());
    };
    let string = |s: &String| toml::Value::String(s.clone());
    for (name, server) in servers {
        match server {
            Server::Stdio { command, args, env } => {
                push(format!("{name}.command"), string(command));
                if !args.is_empty() {
                    push(format!("{name}.args"), toml::Value::Array(args.iter().map(string).collect()));
                }
                for (key, value) in env {
                    push(format!("{name}.env.{key}"), string(value));
                }
            }
            Server::Http { url, headers, oauth } => {
                push(format!("{name}.url"), string(url));
                for (key, value) in headers {
                    push(format!("{name}.http_headers.{key}"), string(value));
                }
                // Codex has no fixed callback port, so `callback_port` is dropped.
                if let Some(oauth) = oauth {
                    push(format!("{name}.oauth.client_id"), string(&oauth.client_id));
                }
            }
        }
    }
    args
}

/// `$XDG_RUNTIME_DIR/withmcp`, else a per-user directory in the temp dir.
pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("withmcp"),
        None => {
            let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
            std::env::temp_dir().join(format!("withmcp-{user}"))
        }
    }
}

// Generated files can contain expanded environment values, so keep them
// private to the user.
#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn servers() -> BTreeMap<String, Server> {
        crate::config::Profile::parse(
            r#"
            [servers.gh]
            url = "https://example.com/mcp"
            headers = { X-Api-Key = "k" }
            [servers.slack]
            url = "https://mcp.slack.com/mcp"
            oauth = { client_id = "cid", callback_port = 3118 }
            [servers.pw]
            command = "npx"
            args = ["@playwright/mcp@latest", "--say \"hi\""]
            env = { DEBUG = "1" }
            "#,
        )
        .unwrap()
        .servers
        .into_iter()
        .map(|(name, entry)| (name, entry.server))
        .collect()
    }

    fn locations(home: &Path) -> Locations {
        Locations {
            home: home.to_path_buf(),
            cwd: home.to_path_buf(),
            claude_config_dir: None,
            codex_home: None,
            pi_agent_dir: None,
        }
    }

    #[test]
    fn detect() {
        assert_eq!(Harness::detect(OsStr::new("/usr/bin/claude")).unwrap(), Harness::Claude);
        assert_eq!(Harness::detect(OsStr::new("codex")).unwrap(), Harness::Codex);
        assert_eq!(Harness::detect(OsStr::new("pi")).unwrap(), Harness::Pi);
        assert!(Harness::detect(OsStr::new("edit")).is_err());
    }

    #[test]
    fn claude_writes_config_file() {
        let prepared = Harness::Claude.prepare(&servers(), &locations(Path::new("/nonexistent")), Path::new("/run")).unwrap();
        let (path, bytes) = &prepared.files[0];
        assert_eq!(prepared.args, [OsString::from("--mcp-config"), path.clone().into()]);
        let json: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(
            json,
            json!({ "mcpServers": {
                "gh": { "type": "http", "url": "https://example.com/mcp", "headers": { "X-Api-Key": "k" } },
                "pw": {
                    "type": "stdio",
                    "command": "npx",
                    "args": ["@playwright/mcp@latest", "--say \"hi\""],
                    "env": { "DEBUG": "1" },
                },
                "slack": {
                    "type": "http",
                    "url": "https://mcp.slack.com/mcp",
                    "headers": {},
                    "oauth": { "clientId": "cid", "callbackPort": 3118 },
                },
            }})
        );
    }

    #[test]
    fn codex_uses_config_overrides() {
        let prepared = Harness::Codex.prepare(&servers(), &locations(Path::new("/nonexistent")), Path::new("/run")).unwrap();
        let args: Vec<_> = prepared.args.iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "-c",
                r#"mcp_servers.gh.url="https://example.com/mcp""#,
                "-c",
                r#"mcp_servers.gh.http_headers.X-Api-Key="k""#,
                "-c",
                r#"mcp_servers.pw.command="npx""#,
                "-c",
                r#"mcp_servers.pw.args=["@playwright/mcp@latest", '--say "hi"']"#,
                "-c",
                r#"mcp_servers.pw.env.DEBUG="1""#,
                "-c",
                r#"mcp_servers.slack.url="https://mcp.slack.com/mcp""#,
                "-c",
                r#"mcp_servers.slack.oauth.client_id="cid""#,
            ]
        );
        assert!(prepared.files.is_empty());
    }

    #[test]
    fn no_servers_no_args() {
        let prepared = Harness::Claude.prepare(&BTreeMap::new(), &locations(Path::new("/nonexistent")), Path::new("/run")).unwrap();
        assert!(prepared.args.is_empty() && prepared.files.is_empty());
    }

    #[test]
    fn write_files_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("rt");
        let prepared = Harness::Claude.prepare(&servers(), &locations(tmp.path()), &dir).unwrap();
        prepared.write_files().unwrap();
        prepared.write_files().unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn pi_merges_into_agent_config() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = locations(tmp.path());
        let agent = loc.pi_agent_dir();
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(
            agent.join("mcp.json"),
            r#"{"settings": {"directTools": true}, "mcpServers": {"own": {"command": "x"}}}"#,
        )
        .unwrap();
        let prepared = Harness::Pi.prepare(&servers(), &loc, Path::new("/run")).unwrap();
        let (path, bytes) = &prepared.files[0];
        assert_eq!(prepared.args, [OsString::from("--mcp-config"), path.clone().into()]);
        assert!(path.file_name().unwrap().to_str().unwrap().starts_with("pi-"));
        assert_eq!(prepared.warnings.len(), 1);
        let json: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(
            json,
            json!({
                "settings": { "directTools": true },
                "mcpServers": {
                    "own": { "command": "x" },
                    "gh": { "url": "https://example.com/mcp", "headers": { "X-Api-Key": "k" } },
                    "pw": {
                        "command": "npx",
                        "args": ["@playwright/mcp@latest", "--say \"hi\""],
                        "env": { "DEBUG": "1" },
                    },
                    "slack": {
                        "url": "https://mcp.slack.com/mcp",
                        "headers": {},
                        "auth": "oauth",
                        "oauth": { "clientId": "cid", "redirectUri": "http://localhost:3118/callback" },
                    },
                },
            })
        );

        std::fs::write(agent.join("settings.json"), r#"{"packages": ["npm:pi-mcp-adapter"]}"#).unwrap();
        std::fs::write(agent.join("mcp.json"), r#"{"mcp-servers": {}}"#).unwrap();
        let prepared = Harness::Pi.prepare(&servers(), &loc, Path::new("/run")).unwrap();
        assert!(prepared.warnings.is_empty());
        let json: serde_json::Value = serde_json::from_slice(&prepared.files[0].1).unwrap();
        assert_eq!(json["mcp-servers"].as_object().unwrap().len(), 3);

        std::fs::write(agent.join("mcp.json"), "not json").unwrap();
        assert!(Harness::Pi.prepare(&servers(), &loc, Path::new("/run")).is_err());
    }
}
