//! Best-effort discovery of the MCP servers a harness already defines itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

pub struct Locations {
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub claude_config_dir: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
}

impl Locations {
    pub fn from_env(home: PathBuf, cwd: PathBuf) -> Self {
        let dir = |var| std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from);
        Self {
            home,
            cwd,
            claude_config_dir: dir("CLAUDE_CONFIG_DIR"),
            codex_home: dir("CODEX_HOME"),
        }
    }
}

#[derive(Debug, Default)]
pub struct Scan {
    /// Files that were read.
    pub checked: Vec<PathBuf>,
    /// Server name to the first file defining it.
    pub servers: BTreeMap<String, PathBuf>,
    pub warnings: Vec<String>,
}

impl Scan {
    fn add<'a>(&mut self, path: &Path, names: impl IntoIterator<Item = &'a String>) {
        for name in names {
            self.servers
                .entry(name.clone())
                .or_insert_with(|| path.to_path_buf());
        }
    }

    fn read(&mut self, path: &Path) -> Option<String> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                self.checked.push(path.to_path_buf());
                Some(text)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                self.warnings.push(format!("cannot read {}: {err}", path.display()));
                None
            }
        }
    }

    fn read_json(&mut self, path: &Path) -> Option<Value> {
        let text = self.read(path)?;
        serde_json::from_str(&text)
            .inspect_err(|err| self.warnings.push(format!("cannot parse {}: {err}", path.display())))
            .ok()
    }

    fn read_toml(&mut self, path: &Path) -> Option<toml::Table> {
        let text = self.read(path)?;
        toml::from_str(&text)
            .inspect_err(|err| self.warnings.push(format!("cannot parse {}: {err}", path.display())))
            .ok()
    }
}

/// Claude Code: user and local scope in `~/.claude.json`, project scope in
/// the nearest `.mcp.json`.
pub fn scan_claude(loc: &Locations) -> Scan {
    let mut scan = Scan::default();
    let user = match &loc.claude_config_dir {
        Some(dir) => dir.join(".claude.json"),
        None => loc.home.join(".claude.json"),
    };
    if let Some(json) = scan.read_json(&user) {
        scan.add(&user, keys(&json["mcpServers"]));
        if let Some(projects) = json["projects"].as_object() {
            for dir in loc.cwd.ancestors() {
                if let Some(project) = dir.to_str().and_then(|d| projects.get(d)) {
                    scan.add(&user, keys(&project["mcpServers"]));
                }
            }
        }
    }
    if let Some(path) = nearest(&loc.cwd, ".mcp.json")
        && let Some(json) = scan.read_json(&path)
    {
        scan.add(&path, keys(&json["mcpServers"]));
    }
    scan
}

/// Codex: `mcp_servers` in `$CODEX_HOME/config.toml` and in the nearest
/// project `.codex/config.toml`.
pub fn scan_codex(loc: &Locations) -> Scan {
    let mut scan = Scan::default();
    let user = match &loc.codex_home {
        Some(dir) => dir.join("config.toml"),
        None => loc.home.join(".codex").join("config.toml"),
    };
    let project = nearest(&loc.cwd, ".codex/config.toml").filter(|p| *p != user);
    for path in std::iter::once(user).chain(project) {
        if let Some(table) = scan.read_toml(&path)
            && let Some(servers) = table.get("mcp_servers").and_then(|s| s.as_table())
        {
            scan.add(&path, servers.keys());
        }
    }
    scan
}

fn keys(value: &Value) -> impl Iterator<Item = &String> {
    value.as_object().into_iter().flat_map(|o| o.keys())
}

fn nearest(cwd: &Path, relative: &str) -> Option<PathBuf> {
    cwd.ancestors()
        .map(|dir| dir.join(relative))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn names(scan: &Scan) -> Vec<&str> {
        scan.servers.keys().map(String::as_str).collect()
    }

    #[test]
    fn claude_scopes() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let project = home.join("code/p");
        let cwd = project.join("src");
        std::fs::create_dir_all(&cwd).unwrap();
        let claude_json = serde_json::json!({
            "mcpServers": { "user": {} },
            "projects": {
                project.to_str().unwrap(): { "mcpServers": { "local": {} } },
                "/unrelated": { "mcpServers": { "other": {} } },
            },
        });
        write(&home.join(".claude.json"), &claude_json.to_string());
        write(&project.join(".mcp.json"), r#"{"mcpServers": {"shared": {}}}"#);
        let loc = Locations {
            home: home.clone(),
            cwd,
            claude_config_dir: None,
            codex_home: None,
        };
        let scan = scan_claude(&loc);
        assert_eq!(names(&scan), ["local", "shared", "user"]);
        assert_eq!(scan.servers["shared"], project.join(".mcp.json"));
        assert!(scan.warnings.is_empty());
    }

    #[test]
    fn codex_scopes_and_parse_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cwd = home.join("code/p");
        write(&home.join(".codex/config.toml"), "[mcp_servers.user]\ncommand = \"x\"");
        write(&cwd.join(".codex/config.toml"), "[mcp_servers.project]\ncommand = \"x\"");
        let loc = Locations {
            home: home.clone(),
            cwd: cwd.clone(),
            claude_config_dir: None,
            codex_home: None,
        };
        assert_eq!(names(&scan_codex(&loc)), ["project", "user"]);

        write(&cwd.join(".codex/config.toml"), "not toml [");
        let scan = scan_codex(&loc);
        assert_eq!(names(&scan), ["user"]);
        assert_eq!(scan.warnings.len(), 1);
    }
}
