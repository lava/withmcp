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
}

/// Arguments to insert right after the harness program, plus files they
/// reference that must be written before launching.
#[derive(Debug, Default)]
pub struct Prepared {
    pub args: Vec<OsString>,
    pub files: Vec<(PathBuf, Vec<u8>)>,
}

impl Harness {
    pub fn detect(program: &OsStr) -> Result<Self> {
        let name = Path::new(program).file_stem().and_then(OsStr::to_str);
        match name {
            Some("claude") => Ok(Harness::Claude),
            Some("codex") => Ok(Harness::Codex),
            _ => bail!(
                "unsupported harness `{}` (supported: claude, codex)",
                program.to_string_lossy()
            ),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
        }
    }

    pub fn scan(self, loc: &Locations) -> Scan {
        match self {
            Harness::Claude => native::scan_claude(loc),
            Harness::Codex => native::scan_codex(loc),
        }
    }

    pub fn prepare(self, servers: &BTreeMap<String, Server>, runtime_dir: &Path) -> Result<Prepared> {
        if servers.is_empty() {
            return Ok(Prepared::default());
        }
        Ok(match self {
            Harness::Claude => {
                let bytes = serde_json::to_vec_pretty(&claude_config(servers))?;
                let mut hasher = std::hash::DefaultHasher::new();
                bytes.hash(&mut hasher);
                let path = runtime_dir.join(format!("claude-{:016x}.json", hasher.finish()));
                Prepared {
                    args: vec!["--mcp-config".into(), path.clone().into()],
                    files: vec![(path, bytes)],
                }
            }
            Harness::Codex => Prepared {
                args: codex_args(servers),
                files: Vec::new(),
            },
        })
    }
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
                Server::Http { url, headers } => {
                    json!({ "type": "http", "url": url, "headers": headers })
                }
            };
            (name.clone(), value)
        })
        .collect();
    json!({ "mcpServers": servers })
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
            Server::Http { url, headers } => {
                push(format!("{name}.url"), string(url));
                for (key, value) in headers {
                    push(format!("{name}.http_headers.{key}"), string(value));
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
        crate::config::Config::parse(
            r#"
            [servers.gh]
            url = "https://example.com/mcp"
            headers = { X-Api-Key = "k" }
            [servers.pw]
            command = "npx"
            args = ["@playwright/mcp@latest", "--say \"hi\""]
            env = { DEBUG = "1" }
            "#,
        )
        .unwrap()
        .servers
    }

    #[test]
    fn detect() {
        assert_eq!(Harness::detect(OsStr::new("/usr/bin/claude")).unwrap(), Harness::Claude);
        assert_eq!(Harness::detect(OsStr::new("codex")).unwrap(), Harness::Codex);
        assert!(Harness::detect(OsStr::new("edit")).is_err());
    }

    #[test]
    fn claude_writes_config_file() {
        let prepared = Harness::Claude.prepare(&servers(), Path::new("/run")).unwrap();
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
            }})
        );
    }

    #[test]
    fn codex_uses_config_overrides() {
        let prepared = Harness::Codex.prepare(&servers(), Path::new("/run")).unwrap();
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
            ]
        );
        assert!(prepared.files.is_empty());
    }

    #[test]
    fn no_servers_no_args() {
        let prepared = Harness::Claude.prepare(&BTreeMap::new(), Path::new("/run")).unwrap();
        assert!(prepared.args.is_empty() && prepared.files.is_empty());
    }

    #[test]
    fn write_files_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("rt");
        let prepared = Harness::Claude.prepare(&servers(), &dir).unwrap();
        prepared.write_files().unwrap();
        prepared.write_files().unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }
}
