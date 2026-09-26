//! Format-preserving changes to profile files for `enable` and `disable`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, Value};

use crate::cli::Scope;
use crate::config::Profile;
use crate::resolve::normalize;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Changed(String),
    Unchanged(String),
}

/// Applies `enable`/`disable` of `server` to `doc`, which must hold the same
/// content as `profile`.
pub fn toggle(
    doc: &mut DocumentMut,
    profile: &Profile,
    server: &str,
    enable: bool,
    scope: Scope,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<Outcome> {
    if !profile.servers.contains_key(server) {
        bail!("unknown server `{server}`");
    }
    let verb = if enable { "enabled" } else { "disabled" };
    Ok(match scope {
        Scope::Global => {
            if set_flag(doc, server, enable)? {
                Outcome::Changed(format!("{verb} `{server}` globally"))
            } else {
                Outcome::Unchanged(format!("`{server}` is already {verb} globally"))
            }
        }
        Scope::Project => {
            let key = cwd_key(profile, cwd, home)?;
            let (add, remove) = if enable { ("enable", "disable") } else { ("disable", "enable") };
            // Add first so a rule emptied by the removal is not dropped.
            let added = add_to_rule(doc, &key, add, server)?;
            let removed = remove_from_rule(doc, &key, remove, server)?;
            if added || removed {
                Outcome::Changed(format!("{verb} `{server}` for path `{key}`"))
            } else {
                Outcome::Unchanged(format!("`{server}` is already {verb} for path `{key}`"))
            }
        }
    })
}

/// Sets the `enabled` flag of `server`, dropping the key when it would be the
/// default. Returns whether anything changed.
fn set_flag(doc: &mut DocumentMut, server: &str, enabled: bool) -> Result<bool> {
    let table = doc
        .get_mut("servers")
        .and_then(|s| s.get_mut(server))
        .and_then(Item::as_table_like_mut)
        .with_context(|| format!("cannot find the table of server `{server}`"))?;
    let current = table.get("enabled").and_then(Item::as_bool).unwrap_or(true);
    if current == enabled {
        return Ok(false);
    }
    if enabled {
        table.remove("enabled");
    } else {
        table.insert("enabled", toml_edit::value(false));
    }
    Ok(true)
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

[servers.playwright]
command = "npx"
enabled = false

[paths."~/code/web"]
enable = ["playwright"]
"#;

    /// Runs `toggle` in `cwd` (with `~` for the home directory) and returns
    /// the new document text and the outcome.
    fn run(text: &str, server: &str, enable: bool, scope: Scope, cwd: &str) -> (String, Outcome) {
        let profile = Profile::parse(text).unwrap();
        let mut doc: DocumentMut = text.parse().unwrap();
        let cwd = cwd.replace('~', HOME);
        let outcome = toggle(&mut doc, &profile, server, enable, scope, Path::new(&cwd), Some(Path::new(HOME)))
            .unwrap();
        let text = doc.to_string();
        Profile::parse(&text).unwrap();
        (text, outcome)
    }

    fn changed(s: &str) -> Outcome {
        Outcome::Changed(s.into())
    }

    #[test]
    fn global_flag_keeps_formatting() {
        let (text, outcome) = run(PROFILE, "linear", false, Scope::Global, "/");
        assert_eq!(outcome, changed("disabled `linear` globally"));
        assert!(text.starts_with("# My work profile.\n[servers.linear] # the work account\n"));
        assert!(text.contains("url = \"https://mcp.linear.app/mcp\"\nenabled = false\n"));

        let (text, outcome) = run(PROFILE, "playwright", true, Scope::Global, "/");
        assert_eq!(outcome, changed("enabled `playwright` globally"));
        assert!(!text.contains("enabled = false"));

        let (text, outcome) = run(PROFILE, "linear", true, Scope::Global, "/");
        assert!(matches!(outcome, Outcome::Unchanged(_)));
        assert_eq!(text, PROFILE);
    }

    #[test]
    fn project_scope_reuses_existing_rule() {
        let (text, outcome) = run(PROFILE, "playwright", false, Scope::Project, "~/code/web");
        assert_eq!(outcome, changed("disabled `playwright` for path `~/code/web`"));
        assert!(text.ends_with("[paths.\"~/code/web\"]\ndisable = [\"playwright\"]\n"));

        let (text, outcome) = run(PROFILE, "linear", true, Scope::Project, "~/code/web");
        assert_eq!(outcome, changed("enabled `linear` for path `~/code/web`"));
        assert!(text.ends_with("enable = [\"playwright\", \"linear\"]\n"));

        let (_, outcome) = run(PROFILE, "playwright", true, Scope::Project, "~/code/web");
        assert!(matches!(outcome, Outcome::Unchanged(_)));
    }

    #[test]
    fn project_scope_creates_rule() {
        let (text, _) = run(PROFILE, "linear", false, Scope::Project, "~/other");
        assert!(text.ends_with("[paths.\"~/other\"]\ndisable = [\"linear\"]\n"));

        let (text, _) = run("[servers.a]\ncommand = \"x\"\n", "a", false, Scope::Project, "/srv");
        assert_eq!(text, "[servers.a]\ncommand = \"x\"\n\n[paths.\"/srv\"]\ndisable = [\"a\"]\n");
    }

    #[test]
    fn project_scope_drops_empty_rule() {
        let text = "[servers.a]\ncommand = \"x\"\n\n[paths.\"/srv\"]\ndisable = [\"a\"]\n";
        let (text, _) = run(text, "a", true, Scope::Project, "/srv");
        assert_eq!(text, "[servers.a]\ncommand = \"x\"\n\n[paths.\"/srv\"]\nenable = [\"a\"]\n");
    }

    #[test]
    fn unknown_server() {
        let profile = Profile::parse(PROFILE).unwrap();
        let mut doc: DocumentMut = PROFILE.parse().unwrap();
        let err = toggle(&mut doc, &profile, "nope", true, Scope::Global, Path::new("/"), None);
        assert!(err.unwrap_err().to_string().contains("unknown server"));
    }
}
