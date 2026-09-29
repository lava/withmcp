//! Synchronize a resolved profile into a harness's user-wide MCP config.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::adapters::{self, Harness};
use crate::config::Server;
use crate::native::Locations;

pub struct Report {
    pub path: PathBuf,
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
    pub disabled: Vec<String>,
    pub left_alone: Vec<String>,
    pub changed: bool,
}

#[derive(Clone)]
pub enum ProjectChange {
    Enable(Server),
    Disable,
    Remove,
}

impl Report {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            added: Vec::new(),
            updated: Vec::new(),
            removed: Vec::new(),
            disabled: Vec::new(),
            left_alone: Vec::new(),
            changed: false,
        }
    }
}

pub fn export(
    harness: Harness,
    loc: &Locations,
    servers: &BTreeMap<String, Option<Server>>,
) -> Result<Report> {
    let path = match harness {
        Harness::Claude => loc
            .claude_config_dir
            .as_ref()
            .unwrap_or(&loc.home)
            .join(".claude.json"),
        Harness::Codex => loc
            .codex_home
            .clone()
            .unwrap_or_else(|| loc.home.join(".codex"))
            .join("config.toml"),
        Harness::Pi => loc.pi_agent_dir().join("mcp.json"),
    };
    let changes = servers
        .iter()
        .map(|(name, server)| {
            (
                name.clone(),
                match server {
                    Some(server) => ProjectChange::Enable(server.clone()),
                    None => ProjectChange::Remove,
                },
            )
        })
        .collect();
    match harness {
        Harness::Claude | Harness::Pi => export_json(harness, &path, &changes),
        Harness::Codex => export_codex(&path, &changes, false),
    }
}

pub fn export_project(
    harness: Harness,
    loc: &Locations,
    dir: &Path,
    changes: &BTreeMap<String, ProjectChange>,
) -> Result<Vec<Report>> {
    let path = match harness {
        Harness::Claude => dir.join(".mcp.json"),
        Harness::Codex => dir.join(".codex/config.toml"),
        Harness::Pi => dir.join(".pi/mcp.json"),
    };
    let mut reports = vec![match harness {
        Harness::Claude | Harness::Pi => export_json(harness, &path, changes)?,
        Harness::Codex => export_codex(&path, changes, true)?,
    }];
    if harness == Harness::Claude {
        reports.push(export_claude_disables(loc, dir, changes)?);
    }
    Ok(reports)
}

fn read(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

fn export_json(
    harness: Harness,
    path: &Path,
    servers: &BTreeMap<String, ProjectChange>,
) -> Result<Report> {
    let original = read(path)?;
    let mut root: Value = match &original {
        Some(text) => {
            serde_json::from_str(text).with_context(|| format!("parsing {}", path.display()))?
        }
        None => json!({}),
    };
    let object = root
        .as_object_mut()
        .with_context(|| format!("{} is not a JSON object", path.display()))?;
    let key = if harness == Harness::Pi
        && object.contains_key("mcp-servers")
        && !object.contains_key("mcpServers")
    {
        "mcp-servers"
    } else {
        "mcpServers"
    };
    let existing = object.get(key).and_then(Value::as_object);
    if object.contains_key(key) && existing.is_none() {
        bail!("{}: `{key}` is not an object", path.display());
    }
    let mut report = Report::new(path.to_path_buf());
    report.left_alone = existing
        .into_iter()
        .flat_map(|map| map.keys())
        .filter(|name| !servers.contains_key(*name))
        .cloned()
        .collect();
    let mut desired = serde_json::Map::new();
    for (name, server) in servers {
        let value = match server {
            ProjectChange::Enable(server) => Some(match harness {
                Harness::Claude => adapters::claude_server_value(server),
                Harness::Pi => adapters::pi_server_value(server),
                Harness::Codex => unreachable!(),
            }),
            ProjectChange::Disable if harness == Harness::Pi => Some(json!({ "disabled": true })),
            _ => None,
        };
        if let Some(value) = value {
            desired.insert(name.clone(), value);
        }
    }
    for name in servers.keys() {
        let old = existing.and_then(|map| map.get(name));
        match (old, desired.get(name)) {
            (None, Some(_)) if matches!(servers[name], ProjectChange::Disable) => {
                report.disabled.push(name.clone())
            }
            (Some(old), Some(new))
                if old != new && matches!(servers[name], ProjectChange::Disable) =>
            {
                report.disabled.push(name.clone())
            }
            (None, Some(_)) => report.added.push(name.clone()),
            (Some(_), None) => report.removed.push(name.clone()),
            (Some(old), Some(new)) if old != new => report.updated.push(name.clone()),
            _ => {}
        }
    }
    report.changed = !report.added.is_empty()
        || !report.updated.is_empty()
        || !report.removed.is_empty()
        || !report.disabled.is_empty();
    if report.changed {
        let map = object
            .entry(key)
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap();
        for name in servers.keys() {
            if let Some(value) = desired.remove(name) {
                map.insert(name.clone(), value);
            } else {
                map.remove(name);
            }
        }
        let text = format!("{}\n", serde_json::to_string_pretty(&root)?);
        write_atomically(path, &text)?;
    }
    Ok(report)
}

fn export_codex(
    path: &Path,
    servers: &BTreeMap<String, ProjectChange>,
    project: bool,
) -> Result<Report> {
    let original = read(path)?;
    let mut doc: toml_edit::DocumentMut = original
        .as_deref()
        .unwrap_or("")
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    if !doc.as_table().contains_key("mcp_servers") {
        doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let table = doc["mcp_servers"]
        .as_table_mut()
        .with_context(|| format!("{}: `mcp_servers` is not a table", path.display()))?;
    let mut report = Report::new(path.to_path_buf());
    report.left_alone = table
        .iter()
        .map(|(name, _)| name.to_string())
        .filter(|name| !servers.contains_key(name))
        .collect();
    for (name, server) in servers {
        match server {
            ProjectChange::Enable(server) => {
                let mut desired = codex_item(name, server)?;
                if project {
                    desired["enabled"] = toml_edit::value(true);
                }
                match table.get(name) {
                    None => report.added.push(name.clone()),
                    Some(old) if old.to_string() != desired.to_string() => {
                        report.updated.push(name.clone())
                    }
                    _ => {}
                }
                table.insert(name, desired);
            }
            ProjectChange::Disable => {
                let mut desired = toml_edit::Item::Table(toml_edit::Table::new());
                desired["enabled"] = toml_edit::value(false);
                if table
                    .get(name)
                    .is_none_or(|old| old.to_string() != desired.to_string())
                {
                    report.disabled.push(name.clone());
                }
                table.insert(name, desired);
            }
            ProjectChange::Remove => {
                if table.remove(name).is_some() {
                    report.removed.push(name.clone());
                }
            }
        }
    }
    report.changed = !report.added.is_empty()
        || !report.updated.is_empty()
        || !report.removed.is_empty()
        || !report.disabled.is_empty();
    if report.changed {
        write_atomically(path, &doc.to_string())?;
    }
    Ok(report)
}

fn export_claude_disables(
    loc: &Locations,
    dir: &Path,
    changes: &BTreeMap<String, ProjectChange>,
) -> Result<Report> {
    let path = loc
        .claude_config_dir
        .as_ref()
        .unwrap_or(&loc.home)
        .join(".claude.json");
    let original = read(&path)?;
    let mut root: Value = match &original {
        Some(text) => {
            serde_json::from_str(text).with_context(|| format!("parsing {}", path.display()))?
        }
        None => json!({}),
    };
    let dir = dir.to_str().context("project path is not valid UTF-8")?;
    let object = root
        .as_object()
        .with_context(|| format!("{} is not a JSON object", path.display()))?;
    let projects = object
        .get("projects")
        .map(|v| {
            v.as_object()
                .with_context(|| format!("{}: `projects` is not an object", path.display()))
        })
        .transpose()?;
    let project = projects
        .and_then(|p| p.get(dir))
        .map(|v| {
            v.as_object()
                .with_context(|| format!("{}: project `{dir}` is not an object", path.display()))
        })
        .transpose()?;
    let previous = project.and_then(|p| p.get("disabledMcpServers"));
    let mut disabled = match previous {
        Some(value) => value
            .as_array()
            .with_context(|| {
                format!(
                    "{}: `disabledMcpServers` for `{dir}` is not an array",
                    path.display()
                )
            })?
            .iter()
            .map(|v| {
                v.as_str().map(str::to_owned).with_context(|| {
                    format!(
                        "{}: `disabledMcpServers` for `{dir}` must contain strings",
                        path.display()
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?,
        None => Vec::new(),
    };
    let mut report = Report::new(path.clone());
    report.left_alone = disabled
        .iter()
        .filter(|name| !changes.contains_key(*name))
        .cloned()
        .collect();
    for (name, change) in changes {
        match change {
            ProjectChange::Disable if !disabled.contains(name) => {
                disabled.push(name.clone());
                report.disabled.push(name.clone());
            }
            ProjectChange::Disable => {}
            _ if disabled.contains(name) => {
                disabled.retain(|entry| entry != name);
                report.removed.push(name.clone());
            }
            _ => {}
        }
    }
    report.changed = !report.disabled.is_empty() || !report.removed.is_empty();
    if report.changed {
        let projects = root
            .as_object_mut()
            .unwrap()
            .entry("projects")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap();
        let project = projects
            .entry(dir)
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap();
        if disabled.is_empty() {
            project.remove("disabledMcpServers");
        } else {
            project.insert("disabledMcpServers".into(), json!(disabled));
        }
        write_atomically(
            &path,
            &format!("{}\n", serde_json::to_string_pretty(&root)?),
        )?;
    }
    Ok(report)
}

fn codex_item(name: &str, server: &Server) -> Result<toml_edit::Item> {
    let mut root = toml::Table::new();
    root.insert(
        "mcp_servers".into(),
        toml::Value::Table(toml::Table::from_iter([(
            name.into(),
            toml::Value::Table(codex_fields(server)),
        )])),
    );
    let generated: toml_edit::DocumentMut = toml::to_string(&root)?.parse()?;
    Ok(generated["mcp_servers"][name].clone())
}

pub(crate) fn codex_fields(server: &Server) -> toml::Table {
    let mut fields = toml::Table::new();
    match server {
        Server::Stdio {
            command,
            args,
            env,
            env_passthrough,
        } => {
            fields.insert("command".into(), command.clone().into());
            if !args.is_empty() {
                fields.insert(
                    "args".into(),
                    toml::Value::Array(args.iter().cloned().map(Into::into).collect()),
                );
            }
            if !env.is_empty() {
                fields.insert(
                    "env".into(),
                    toml::Value::Table(
                        env.iter()
                            .map(|(k, v)| (k.clone(), v.clone().into()))
                            .collect(),
                    ),
                );
            }
            if !env_passthrough.is_empty() {
                fields.insert(
                    "env_vars".into(),
                    toml::Value::Array(env_passthrough.iter().cloned().map(Into::into).collect()),
                );
            }
        }
        Server::Http {
            url,
            headers,
            oauth,
        } => {
            fields.insert("url".into(), url.clone().into());
            if !headers.is_empty() {
                fields.insert(
                    "http_headers".into(),
                    toml::Value::Table(
                        headers
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone().into()))
                            .collect(),
                    ),
                );
            }
            if let Some(oauth) = oauth {
                fields.insert(
                    "oauth".into(),
                    toml::Value::Table(toml::Table::from_iter([(
                        "client_id".into(),
                        oauth.client_id.clone().into(),
                    )])),
                );
            }
        }
    }
    fields
}

fn write_atomically(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("config path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut created = false;
    let result = (|| -> Result<()> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        created = true;
        file.write_all(text.as_bytes())?;
        if let Ok(metadata) = std::fs::metadata(path) {
            file.set_permissions(metadata.permissions())?;
        }
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() && created {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn locations(root: &Path) -> Locations {
        Locations {
            home: root.to_path_buf(),
            cwd: root.to_path_buf(),
            claude_config_dir: None,
            codex_home: None,
            pi_agent_dir: None,
        }
    }

    fn servers() -> BTreeMap<String, Option<Server>> {
        BTreeMap::from([
            (
                "work_on".into(),
                Some(Server::Stdio {
                    command: "npx".into(),
                    args: vec!["server".into()],
                    env: BTreeMap::from([("TOKEN".into(), "value".into())]),
                    env_passthrough: BTreeSet::from(["HERDR_ENV".to_string()]),
                }),
            ),
            ("work_off".into(), None),
        ])
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn json_exports_preserve_unrelated_entries_and_settings() {
        for harness in [Harness::Claude, Harness::Pi] {
            let dir = tempfile::tempdir().unwrap();
            let loc = locations(dir.path());
            let path = match harness {
                Harness::Claude => dir.path().join(".claude.json"),
                Harness::Pi => loc.pi_agent_dir().join("mcp.json"),
                Harness::Codex => unreachable!(),
            };
            let key = if harness == Harness::Pi {
                "mcp-servers"
            } else {
                "mcpServers"
            };
            write(
                &path,
                &json!({
                    "setting": { "keep": true },
                    key: {
                        "foreign": { "command": "foreign" },
                        "work_off": { "command": "old" },
                        "work_on": { "command": "old" }
                    }
                })
                .to_string(),
            );
            let report = export(harness, &loc, &servers()).unwrap();
            assert!(report.added.is_empty());
            assert_eq!(report.updated, ["work_on"]);
            assert_eq!(report.removed, ["work_off"]);
            assert_eq!(report.left_alone, ["foreign"]);
            let after = std::fs::read_to_string(&path).unwrap();
            let value: Value = serde_json::from_str(&after).unwrap();
            assert_eq!(value["setting"]["keep"], true);
            assert_eq!(value[key]["foreign"]["command"], "foreign");
            assert!(value[key].get("work_off").is_none());
            assert_eq!(value[key]["work_on"]["command"], "npx");
            assert!(value[key]["work_on"].get("env_passthrough").is_none());
            assert!(value[key]["work_on"].get("env_vars").is_none());
            assert!(!export(harness, &loc, &servers()).unwrap().changed);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
        }
    }

    #[test]
    fn codex_export_preserves_other_tables_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let loc = locations(dir.path());
        let path = dir.path().join(".codex/config.toml");
        write(
            &path,
            "# keep this comment\nmodel = \"gpt\"\n\n[mcp_servers.foreign]\ncommand = \"foreign\"\n\n[mcp_servers.work_off]\ncommand = \"old\"\n\n[mcp_servers.work_on]\ncommand = \"old\"\n",
        );
        let report = export(Harness::Codex, &loc, &servers()).unwrap();
        assert_eq!(report.updated, ["work_on"]);
        assert_eq!(report.removed, ["work_off"]);
        assert_eq!(report.left_alone, ["foreign"]);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with("# keep this comment\nmodel = \"gpt\""));
        let parsed: toml::Value = toml::from_str(&after).unwrap();
        assert_eq!(
            parsed["mcp_servers"]["foreign"]["command"].as_str(),
            Some("foreign")
        );
        assert!(parsed["mcp_servers"].get("work_off").is_none());
        assert_eq!(
            parsed["mcp_servers"]["work_on"]["env"]["TOKEN"].as_str(),
            Some("value")
        );
        assert_eq!(
            parsed["mcp_servers"]["work_on"]["env_vars"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>(),
            ["HERDR_ENV"]
        );
        assert!(!export(Harness::Codex, &loc, &servers()).unwrap().changed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
    }

    #[test]
    fn creates_config_and_rejects_invalid_existing_content() {
        for harness in [Harness::Claude, Harness::Codex, Harness::Pi] {
            let dir = tempfile::tempdir().unwrap();
            let loc = locations(dir.path());
            let report = export(harness, &loc, &servers()).unwrap();
            assert_eq!(report.added, ["work_on"]);
            let content = std::fs::read_to_string(&report.path).unwrap();
            write(&report.path, "invalid [");
            assert!(export(harness, &loc, &servers()).is_err());
            assert_eq!(std::fs::read_to_string(&report.path).unwrap(), "invalid [");
            assert_ne!(content, "invalid [");
        }
    }

    #[test]
    fn project_exports_enable_and_disable_without_touching_foreign_servers() {
        for harness in [Harness::Claude, Harness::Codex, Harness::Pi] {
            let tmp = tempfile::tempdir().unwrap();
            let loc = locations(tmp.path());
            let dir = tmp.path().join("project");
            std::fs::create_dir_all(&dir).unwrap();
            match harness {
                Harness::Claude => write(
                    &dir.join(".mcp.json"),
                    r#"{"mcpServers":{"foreign":{"command":"keep"}}}"#,
                ),
                Harness::Codex => write(
                    &dir.join(".codex/config.toml"),
                    "[mcp_servers.foreign]\ncommand = \"keep\"\n",
                ),
                Harness::Pi => write(
                    &dir.join(".pi/mcp.json"),
                    r#"{"mcpServers":{"foreign":{"command":"keep"}}}"#,
                ),
            }
            let changes = BTreeMap::from([
                (
                    "work_on".into(),
                    ProjectChange::Enable(servers()["work_on"].clone().unwrap()),
                ),
                ("work_off".into(), ProjectChange::Disable),
            ]);
            let reports = export_project(harness, &loc, &dir, &changes).unwrap();
            assert!(reports.iter().any(|r| r.changed));
            assert_eq!(reports[0].left_alone, ["foreign"]);
            match harness {
                Harness::Claude => {
                    let project: Value = serde_json::from_str(
                        &std::fs::read_to_string(dir.join(".mcp.json")).unwrap(),
                    )
                    .unwrap();
                    assert_eq!(project["mcpServers"]["work_on"]["command"], "npx");
                    assert_eq!(project["mcpServers"]["foreign"]["command"], "keep");
                    assert!(project["mcpServers"].get("work_off").is_none());
                    let user: Value = serde_json::from_str(
                        &std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap(),
                    )
                    .unwrap();
                    assert_eq!(
                        user["projects"][dir.to_str().unwrap()]["disabledMcpServers"],
                        json!(["work_off"])
                    );
                }
                Harness::Codex => {
                    let project: toml::Value = toml::from_str(
                        &std::fs::read_to_string(dir.join(".codex/config.toml")).unwrap(),
                    )
                    .unwrap();
                    assert_eq!(
                        project["mcp_servers"]["work_on"]["enabled"].as_bool(),
                        Some(true)
                    );
                    assert_eq!(
                        project["mcp_servers"]["foreign"]["command"].as_str(),
                        Some("keep")
                    );
                    assert_eq!(
                        project["mcp_servers"]["work_off"]["enabled"].as_bool(),
                        Some(false)
                    );
                }
                Harness::Pi => {
                    let project: Value = serde_json::from_str(
                        &std::fs::read_to_string(dir.join(".pi/mcp.json")).unwrap(),
                    )
                    .unwrap();
                    assert_eq!(project["mcpServers"]["work_on"]["command"], "npx");
                    assert_eq!(project["mcpServers"]["foreign"]["command"], "keep");
                    assert_eq!(project["mcpServers"]["work_off"]["disabled"], true);
                }
            }
            assert!(
                export_project(harness, &loc, &dir, &changes)
                    .unwrap()
                    .iter()
                    .all(|r| !r.changed)
            );
        }
    }

    #[test]
    fn scanned_exports_match_their_servers() {
        for harness in [Harness::Claude, Harness::Codex, Harness::Pi] {
            let tmp = tempfile::tempdir().unwrap();
            let mut loc = locations(tmp.path());
            let server = servers()["work_on"].clone().unwrap();
            export(harness, &loc, &servers()).unwrap();
            let scan = harness.scan(&loc);
            let defined = &scan.servers["work_on"].value;
            assert!(harness.defines(defined, &server));
            let mut changed = server.clone();
            if let Server::Stdio { command, .. } = &mut changed {
                *command = "other".into();
            }
            assert!(!harness.defines(defined, &changed));

            let dir = tmp.path().join("project");
            std::fs::create_dir_all(&dir).unwrap();
            let changes = BTreeMap::from([(
                "project_on".to_string(),
                ProjectChange::Enable(server.clone()),
            )]);
            export_project(harness, &loc, &dir, &changes).unwrap();
            loc.cwd = dir;
            let scan = harness.scan(&loc);
            assert!(harness.defines(&scan.servers["project_on"].value, &server));
        }
    }

    #[test]
    fn project_export_removes_stale_configured_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = locations(tmp.path());
        let dir = tmp.path().join("project");
        std::fs::create_dir_all(&dir).unwrap();
        let old = BTreeMap::from([("work_on".into(), ProjectChange::Disable)]);
        export_project(Harness::Claude, &loc, &dir, &old).unwrap();
        let changes = BTreeMap::from([("work_on".into(), ProjectChange::Remove)]);
        let reports = export_project(Harness::Claude, &loc, &dir, &changes).unwrap();
        assert_eq!(reports[1].removed, ["work_on"]);
        let user: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert!(
            user["projects"][dir.to_str().unwrap()]
                .get("disabledMcpServers")
                .is_none()
        );
    }
}
