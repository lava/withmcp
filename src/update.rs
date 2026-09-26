//! Format-preserving changes to profile files for `enable` and `disable`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, Value};

use crate::cli::Scope;
use crate::config::{Profile, Target};
use crate::resolve::normalize;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Changed(String),
    Unchanged(String),
}

/// Applies `enable`/`disable` of server or group `name` to `doc`, which must
/// hold the same content as `profile`.
pub fn toggle(
    doc: &mut DocumentMut,
    profile: &Profile,
    name: &str,
    enable: bool,
    scope: Scope,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<Outcome> {
    let Some(target) = profile.target(name) else {
        bail!("unknown server or group `{name}`");
    };
    let verb = if enable { "enabled" } else { "disabled" };
    let what = match target {
        Target::Server => format!("`{name}`"),
        Target::Group(_) => format!("group `{name}`"),
    };
    Ok(match scope {
        Scope::Global => {
            let changed = match target {
                Target::Server => set_server_flag(doc, profile, name, enable)?,
                Target::Group(_) => set_group_flag(doc, name, enable)?,
            };
            if changed {
                Outcome::Changed(format!("{verb} {what} globally"))
            } else if !enable && profile.servers.contains_key(name) && profile.base_enabled(name) {
                Outcome::Unchanged(format!("{what} has no `enabled` flag to clear"))
            } else {
                Outcome::Unchanged(format!("{what} is already {verb} globally"))
            }
        }
        Scope::Project => {
            let key = cwd_key(profile, cwd, home)?;
            let (add, remove) = if enable {
                ("enable", "disable")
            } else {
                ("disable", "enable")
            };
            // Add first so a rule emptied by the removal is not dropped.
            let added = add_to_rule(doc, &key, add, name)?;
            let removed = remove_from_rule(doc, &key, remove, name)?;
            if added || removed {
                Outcome::Changed(format!("{verb} {what} for path `{key}`"))
            } else {
                Outcome::Unchanged(format!("{what} is already {verb} for path `{key}`"))
            }
        }
    })
}

/// Sets or clears the `enabled` flag of `server`. The flag is not written
/// when an enabled group already turns the server on.
fn set_server_flag(
    doc: &mut DocumentMut,
    profile: &Profile,
    server: &str,
    enabled: bool,
) -> Result<bool> {
    let current = profile.servers[server].enabled;
    if current == enabled || (enabled && profile.base_enabled(server)) {
        return Ok(false);
    }
    let table = table_mut(doc, "servers", server)?;
    if enabled {
        table.insert("enabled", toml_edit::value(true));
    } else {
        table.remove("enabled");
    }
    Ok(true)
}

/// Sets the `enabled` flag of group `group`, dropping it when false.
fn set_group_flag(doc: &mut DocumentMut, group: &str, enabled: bool) -> Result<bool> {
    let table = table_mut(doc, "groups", group)?;
    let current = table
        .get("enabled")
        .and_then(Item::as_bool)
        .unwrap_or(false);
    if current == enabled {
        return Ok(false);
    }
    if enabled {
        table.insert("enabled", toml_edit::value(true));
    } else {
        table.remove("enabled");
    }
    Ok(true)
}

fn table_mut<'a>(
    doc: &'a mut DocumentMut,
    kind: &str,
    name: &str,
) -> Result<&'a mut dyn TableLike> {
    doc.get_mut(kind)
        .and_then(|t| t.get_mut(name))
        .and_then(Item::as_table_like_mut)
        .with_context(|| format!("cannot find the table `{kind}.{name}`"))
}

/// The key of the path rule for `cwd`: an existing key naming the same
/// directory, else `cwd` written relative to `~` where possible.
fn cwd_key(profile: &Profile, cwd: &Path, home: Option<&Path>) -> Result<String> {
    for key in profile.paths.keys() {
        if normalize(key, home)? == cwd {
            return Ok(key.clone());
        }
    }
    let key = match home.and_then(|h| cwd.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => cwd.display().to_string(),
    };
    Ok(key)
}

fn add_to_rule(doc: &mut DocumentMut, key: &str, list: &str, server: &str) -> Result<bool> {
    let paths = doc
        .entry("paths")
        .or_insert_with(|| {
            let mut table = Table::new();
            table.set_implicit(true);
            Item::Table(table)
        })
        .as_table_like_mut()
        .context("`paths` is not a table")?;
    let rule = paths
        .entry(key)
        .or_insert(Item::Table(Table::new()))
        .as_table_like_mut()
        .with_context(|| format!("path `{key}` is not a table"))?;
    let array = rule
        .entry(list)
        .or_insert(toml_edit::value(Array::new()))
        .as_array_mut()
        .with_context(|| format!("`{list}` of path `{key}` is not an array"))?;
    if array.iter().any(|v| v.as_str() == Some(server)) {
        return Ok(false);
    }
    array.push(server);
    Ok(true)
}

/// Removes `server` from `list` of the rule for `key`, dropping the list and
/// the rule once they are empty.
fn remove_from_rule(doc: &mut DocumentMut, key: &str, list: &str, server: &str) -> Result<bool> {
    let Some(paths) = doc.get_mut("paths").and_then(Item::as_table_like_mut) else {
        return Ok(false);
    };
    let Some(rule) = paths.get_mut(key).and_then(Item::as_table_like_mut) else {
        return Ok(false);
    };
    let Some(array) = rule.get_mut(list).and_then(Item::as_array_mut) else {
        return Ok(false);
    };
    let before = array.len();
    array.retain(|v: &Value| v.as_str() != Some(server));
    if array.len() == before {
        return Ok(false);
    }
    if array.is_empty() {
        rule.remove(list);
    }
    if is_empty(rule) {
        paths.remove(key);
    }
    Ok(true)
}

fn is_empty(table: &dyn TableLike) -> bool {
    table.iter().next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/nonexistent/home";

    const PROFILE: &str = r#"# My work profile.
[servers.linear] # the work account
url = "https://mcp.linear.app/mcp"
enabled = true

[servers.playwright]
command = "npx"

[servers.chrome]
command = "npx"

[groups.devtools]
servers = ["playwright", "chrome"]

[paths."~/code/web"]
enable = ["playwright"]
"#;

    /// Runs `toggle` in `cwd` (with `~` for the home directory) and returns
    /// the new document text and the outcome.
    fn run(text: &str, name: &str, enable: bool, scope: Scope, cwd: &str) -> (String, Outcome) {
        let profile = Profile::parse(text).unwrap();
        let mut doc: DocumentMut = text.parse().unwrap();
        let cwd = cwd.replace('~', HOME);
        let outcome = toggle(
            &mut doc,
            &profile,
            name,
            enable,
            scope,
            Path::new(&cwd),
            Some(Path::new(HOME)),
        )
        .unwrap();
        let text = doc.to_string();
        Profile::parse(&text).unwrap();
        (text, outcome)
    }

    fn changed(s: &str) -> Outcome {
        Outcome::Changed(s.into())
    }

    #[test]
    fn server_flag_keeps_formatting() {
        let (text, outcome) = run(PROFILE, "linear", false, Scope::Global, "/");
        assert_eq!(outcome, changed("disabled `linear` globally"));
        assert!(text.starts_with("# My work profile.\n[servers.linear] # the work account\n"));
        assert!(!text.contains("enabled"), "false is the default");

        let (text, outcome) = run(PROFILE, "playwright", true, Scope::Global, "/");
        assert_eq!(outcome, changed("enabled `playwright` globally"));
        assert!(text.contains("[servers.playwright]\ncommand = \"npx\"\nenabled = true\n"));

        let (text, outcome) = run(PROFILE, "linear", true, Scope::Global, "/");
        assert!(matches!(outcome, Outcome::Unchanged(_)));
        assert_eq!(text, PROFILE);
    }

    #[test]
    fn server_flag_not_written_when_a_group_enables_it() {
        let (text, _) = run(PROFILE, "devtools", true, Scope::Global, "/");
        let (after, outcome) = run(&text, "chrome", true, Scope::Global, "/");
        assert!(matches!(outcome, Outcome::Unchanged(_)));
        assert_eq!(after, text);
        // A group cannot be overridden by the server's own flag.
        let (after, outcome) = run(&text, "chrome", false, Scope::Global, "/");
        assert_eq!(
            outcome,
            Outcome::Unchanged("`chrome` has no `enabled` flag to clear".into())
        );
        assert_eq!(after, text);
    }

    #[test]
    fn group_flag() {
        let (text, outcome) = run(PROFILE, "devtools", true, Scope::Global, "/");
        assert_eq!(outcome, changed("enabled group `devtools` globally"));
        assert!(text.contains("servers = [\"playwright\", \"chrome\"]\nenabled = true\n"));
        let (text, outcome) = run(&text, "devtools", false, Scope::Global, "/");
        assert_eq!(outcome, changed("disabled group `devtools` globally"));
        assert_eq!(text, PROFILE);
        let (_, outcome) = run(PROFILE, "devtools", false, Scope::Global, "/");
        assert!(matches!(outcome, Outcome::Unchanged(_)));
    }

    #[test]
    fn project_scope_reuses_existing_rule() {
        let (text, outcome) = run(PROFILE, "playwright", false, Scope::Project, "~/code/web");
        assert_eq!(
            outcome,
            changed("disabled `playwright` for path `~/code/web`")
        );
        assert!(text.ends_with("[paths.\"~/code/web\"]\ndisable = [\"playwright\"]\n"));

        let (text, outcome) = run(PROFILE, "devtools", true, Scope::Project, "~/code/web");
        assert_eq!(
            outcome,
            changed("enabled group `devtools` for path `~/code/web`")
        );
        assert!(text.ends_with("enable = [\"playwright\", \"devtools\"]\n"));

        let (_, outcome) = run(PROFILE, "playwright", true, Scope::Project, "~/code/web");
        assert!(matches!(outcome, Outcome::Unchanged(_)));
    }

    #[test]
    fn project_scope_creates_rule() {
        let (text, _) = run(PROFILE, "linear", false, Scope::Project, "~/other");
        assert!(text.ends_with("[paths.\"~/other\"]\ndisable = [\"linear\"]\n"));

        let (text, _) = run(
            "[servers.a]\ncommand = \"x\"\n",
            "a",
            false,
            Scope::Project,
            "/srv",
        );
        assert_eq!(
            text,
            "[servers.a]\ncommand = \"x\"\n\n[paths.\"/srv\"]\ndisable = [\"a\"]\n"
        );
    }

    #[test]
    fn project_scope_drops_empty_rule() {
        let text = "[servers.a]\ncommand = \"x\"\n\n[paths.\"/srv\"]\ndisable = [\"a\"]\n";
        let (text, _) = run(text, "a", true, Scope::Project, "/srv");
        assert_eq!(
            text,
            "[servers.a]\ncommand = \"x\"\n\n[paths.\"/srv\"]\nenable = [\"a\"]\n"
        );
    }

    #[test]
    fn unknown_name() {
        let profile = Profile::parse(PROFILE).unwrap();
        let mut doc: DocumentMut = PROFILE.parse().unwrap();
        let err = toggle(
            &mut doc,
            &profile,
            "nope",
            true,
            Scope::Global,
            Path::new("/"),
            None,
        );
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("unknown server or group")
        );
    }
}
