//! Plugin provider: `installed_plugins.json` + `enabledPlugins` from user,
//! project and local project settings (later overrides earlier).

use super::{CapKind, Capability, Provider, ScanCtx, Scope};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct PluginProvider;

#[derive(Debug, Clone)]
pub(crate) struct Installed {
    pub id: String,
    /// Part of the id before `@`.
    pub name: String,
    pub install_path: PathBuf,
    pub version: Option<String>,
    pub scope: Scope,
    pub enabled: bool,
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

fn enabled_plugins(ctx: &ScanCtx) -> HashMap<String, bool> {
    let mut files = vec![ctx.claude_home.join("settings.json")];
    if let Some(cwd) = &ctx.cwd {
        files.push(cwd.join(".claude/settings.json"));
        files.push(cwd.join(".claude/settings.local.json"));
    }
    let mut out = HashMap::new();
    for f in files {
        let Some(v) = read_json(&f) else { continue };
        if let Some(o) = v.get("enabledPlugins").and_then(|x| x.as_object()) {
            for (k, b) in o {
                if let Some(b) = b.as_bool() {
                    out.insert(k.clone(), b);
                }
            }
        }
    }
    out
}

/// All installed plugins (sorted by id), with the merged enabled flag.
pub(crate) fn installed(ctx: &ScanCtx) -> Vec<Installed> {
    let Some(v) = read_json(&ctx.claude_home.join("plugins/installed_plugins.json")) else {
        return vec![];
    };
    let Some(plugins) = v.get("plugins").and_then(|p| p.as_object()) else {
        return vec![];
    };
    let enabled = enabled_plugins(ctx);
    let mut out: Vec<Installed> = plugins
        .iter()
        .filter_map(|(id, entries)| {
            let e = entries.as_array()?.first()?;
            let install_path = PathBuf::from(e.get("installPath")?.as_str()?);
            let scope = match e.get("scope").and_then(|s| s.as_str()) {
                Some("project") | Some("local") => Scope::Project,
                Some("managed") => Scope::Managed,
                _ => Scope::User,
            };
            Some(Installed {
                id: id.clone(),
                name: id.split('@').next().unwrap_or(id).to_string(),
                install_path,
                version: e.get("version").and_then(|s| s.as_str()).map(str::to_string),
                scope,
                enabled: enabled.get(id).copied().unwrap_or(false),
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn count_entries(dir: &Path, want_dir: bool) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    let p = e.path();
                    if want_dir {
                        p.join("SKILL.md").is_file()
                    } else {
                        p.extension().is_some_and(|x| x == "md")
                    }
                })
                .count()
        })
        .unwrap_or(0)
}

/// Names of MCP servers declared in `<plugin>/.mcp.json` (either
/// `{"mcpServers": {..}}` or a bare map of servers).
pub(crate) fn mcp_names(install: &Path) -> Vec<String> {
    let Some(v) = read_json(&install.join(".mcp.json")) else { return vec![] };
    let m = v.get("mcpServers").unwrap_or(&v);
    m.as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

fn manifest(install: &Path) -> Option<Value> {
    read_json(&install.join(".claude-plugin/plugin.json")).or_else(|| read_json(&install.join("plugin.json")))
}

impl Provider for PluginProvider {
    fn kind(&self) -> CapKind {
        CapKind::Plugin
    }

    fn scan(&self, ctx: &ScanCtx) -> Vec<Capability> {
        installed(ctx)
            .into_iter()
            .map(|p| {
                let mf = manifest(&p.install_path);
                let description = mf
                    .as_ref()
                    .and_then(|m| m.get("description"))
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut meta = Map::new();
                if let Some(v) = &p.version {
                    meta.insert("version".into(), json!(v));
                }
                meta.insert(
                    "components".into(),
                    json!({
                        "skills": count_entries(&p.install_path.join("skills"), true),
                        "agents": count_entries(&p.install_path.join("agents"), false),
                        "commands": count_entries(&p.install_path.join("commands"), false),
                        "mcp": mcp_names(&p.install_path),
                    }),
                );
                Capability {
                    kind: CapKind::Plugin,
                    id: p.id,
                    name: p.name,
                    description,
                    scope: p.scope,
                    path: Some(p.install_path),
                    enabled_globally: p.enabled,
                    meta,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::write;
    use super::*;

    pub(crate) fn fixture(root: &Path) -> ScanCtx {
        let home = root.join("home/.claude");
        let inst = root.join("cache/foo");
        write(&inst.join("skills/s1/SKILL.md"), "---\nname: s1\n---\n");
        write(&inst.join("agents/a.md"), "---\nname: a\n---\n");
        write(&inst.join("commands/c.md"), "x");
        write(&inst.join(".mcp.json"), r#"{"mcpServers":{"srv":{}}}"#);
        write(
            &home.join("plugins/installed_plugins.json"),
            &format!(
                r#"{{"version":2,"plugins":{{"foo@mk":[{{"scope":"user","installPath":"{}","version":"1.0"}}]}}}}"#,
                inst.display()
            ),
        );
        ScanCtx {
            claude_home: home,
            claude_json: root.join("home/.claude.json"),
            cwd: Some(root.join("proj")),
        }
    }

    #[test]
    fn plugin_enabled_flag_project_overrides_user() {
        let t = tempfile::tempdir().unwrap();
        let ctx = fixture(t.path());
        write(&ctx.claude_home.join("settings.json"), r#"{"enabledPlugins":{"foo@mk":true}}"#);
        let cap = &PluginProvider.scan(&ctx)[0];
        assert!(cap.enabled_globally);
        assert_eq!(cap.id, "foo@mk");
        assert_eq!(cap.name, "foo");
        assert_eq!(cap.meta["version"], "1.0");
        assert_eq!(
            cap.meta["components"],
            json!({"skills":1,"agents":1,"commands":1,"mcp":["srv"]})
        );

        let proj = ctx.cwd.clone().unwrap();
        write(&proj.join(".claude/settings.json"), r#"{"enabledPlugins":{"foo@mk":false}}"#);
        assert!(!PluginProvider.scan(&ctx)[0].enabled_globally);

        write(&proj.join(".claude/settings.local.json"), r#"{"enabledPlugins":{"foo@mk":true}}"#);
        assert!(PluginProvider.scan(&ctx)[0].enabled_globally);
    }

    #[test]
    fn missing_or_malformed_files_yield_nothing() {
        let t = tempfile::tempdir().unwrap();
        let ctx = fixture(t.path());
        write(&ctx.claude_home.join("plugins/installed_plugins.json"), "{not json");
        assert!(PluginProvider.scan(&ctx).is_empty());
    }
}
