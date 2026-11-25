//! MCP server provider: `~/.claude.json`, per-project config, `.mcp.json`, plugins.

use super::{plugins, CapKind, Capability, Provider, ScanCtx, Scope};
use serde_json::{Map, Value};
use std::path::Path;

pub struct McpProvider;

/// Claude Code turns every char of a server name outside `[A-Za-z0-9_-]` into
/// `_` (hyphens are kept) when it builds tool names (`mcp__<server>__<tool>`); compare in this form.
pub fn normalize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// Describe a server config without copying secrets: transport plus the bare
/// command or the URL minus its query string / fragment. Args and env are dropped.
fn describe(cfg: &Value) -> Map<String, Value> {
    let mut m = Map::new();
    let s = |k: &str| cfg.get(k).and_then(Value::as_str);
    let transport = s("type").unwrap_or(if s("url").is_some() { "http" } else { "stdio" });
    m.insert("transport".into(), transport.into());
    if let Some(c) = s("command") {
        m.insert("command".into(), c.into());
    }
    if let Some(u) = s("url") {
        let end = u.find(['?', '#']).unwrap_or(u.len());
        let u = &u[..end];
        // drop `user:pass@` userinfo from the authority
        let u = match u.split_once("://") {
            Some((scheme, rest)) => {
                let slash = rest.find('/').unwrap_or(rest.len());
                let host = rest[..slash].rsplit('@').next().unwrap_or("");
                format!("{scheme}://{host}{}", &rest[slash..])
            }
            None => u.to_string(),
        };
        m.insert("url".into(), u.into());
    }
    m
}

fn servers(v: Option<&Value>) -> Vec<(String, Value)> {
    v.and_then(Value::as_object)
        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

fn names(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn push(
    out: &mut Vec<Capability>,
    id: String,
    scope: Scope,
    path: &Path,
    enabled: bool,
    cfg: &Value,
) {
    // later (more specific) sources replace earlier ones of the same name
    out.retain(|c| c.id != id);
    out.push(Capability {
        kind: CapKind::Mcp,
        name: id.clone(),
        id,
        description: String::new(),
        scope,
        path: Some(path.to_path_buf()),
        enabled_globally: enabled,
        meta: describe(cfg),
    });
}

impl Provider for McpProvider {
    fn kind(&self) -> CapKind {
        CapKind::Mcp
    }

    fn scan(&self, ctx: &ScanCtx) -> Vec<Capability> {
        let mut out = Vec::new();
        let cj = read_json(&ctx.claude_json);
        if let Some(cj) = &cj {
            for (n, c) in servers(cj.get("mcpServers")) {
                push(&mut out, n, Scope::User, &ctx.claude_json, true, &c);
            }
        }
        for p in plugins::installed(ctx).into_iter().filter(|p| p.enabled) {
            let file = p.install_path.join(".mcp.json");
            let Some(v) = read_json(&file) else { continue };
            for (n, c) in servers(Some(v.get("mcpServers").unwrap_or(&v))) {
                let id = format!("plugin:{}:{n}", p.name);
                push(&mut out, id, Scope::Plugin(p.id.clone()), &file, true, &c);
            }
        }
        if let Some(cwd) = &ctx.cwd {
            let proj = cj
                .as_ref()
                .and_then(|j| j.get("projects"))
                .and_then(|p| p.get(cwd.to_string_lossy().as_ref()));
            let disabled = names(proj.and_then(|p| p.get("disabledMcpjsonServers")));
            let file = cwd.join(".mcp.json");
            if let Some(v) = read_json(&file) {
                for (n, c) in servers(v.get("mcpServers")) {
                    let on = !disabled.contains(&n);
                    push(&mut out, n, Scope::Project, &file, on, &c);
                }
            }
            for (n, c) in servers(proj.and_then(|p| p.get("mcpServers"))) {
                push(&mut out, n, Scope::Project, &ctx.claude_json, true, &c);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::write;
    use super::*;

    fn ctx(root: &std::path::Path) -> ScanCtx {
        let home = root.join("home/.claude");
        let cwd = root.join("proj");
        let inst = root.join("cache/gl");
        write(
            &root.join("home/.claude.json"),
            &format!(
                r#"{{"mcpServers":{{"codegraph":{{"command":"codegraph","args":["serve","--token","SECRET"],"env":{{"K":"SECRET"}}}},
                  "web":{{"type":"http","url":"https://x.test/mcp?key=SECRET"}}}},
                  "projects":{{"{p}":{{"mcpServers":{{"local":{{"command":"l"}},"web":{{"type":"sse","url":"https://p.test"}}}},
                  "disabledMcpjsonServers":["off"],"enabledMcpjsonServers":["on"]}}}}}}"#,
                p = cwd.display()
            ),
        );
        write(
            &cwd.join(".mcp.json"),
            r#"{"mcpServers":{"on":{"command":"a"},"off":{"command":"b"}}}"#,
        );
        write(
            &inst.join(".mcp.json"),
            r#"{"gitlab":{"type":"http","url":"https://gl"}}"#,
        );
        write(
            &home.join("plugins/installed_plugins.json"),
            &format!(
                r#"{{"plugins":{{"gitlab@mk":[{{"scope":"user","installPath":"{}"}}]}}}}"#,
                inst.display()
            ),
        );
        write(
            &home.join("settings.json"),
            r#"{"enabledPlugins":{"gitlab@mk":true}}"#,
        );
        ScanCtx {
            claude_home: home,
            claude_json: root.join("home/.claude.json"),
            cwd: Some(cwd),
            registry: Default::default(),
        }
    }

    #[test]
    fn mcp_merges_user_project_and_mcp_json_with_disabled_list() {
        let t = tempfile::tempdir().unwrap();
        let caps = McpProvider.scan(&ctx(t.path()));
        let get = |id: &str| {
            caps.iter()
                .find(|c| c.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
        };
        assert_eq!(get("codegraph").scope, Scope::User);
        assert_eq!(get("codegraph").meta["transport"], "stdio");
        assert_eq!(get("local").scope, Scope::Project);
        // project entry overrides the user one of the same name
        assert_eq!(get("web").scope, Scope::Project);
        assert_eq!(get("web").meta["transport"], "sse");
        assert_eq!(caps.iter().filter(|c| c.id == "web").count(), 1);
        assert!(get("on").enabled_globally);
        assert!(!get("off").enabled_globally);
        assert_eq!(get("on").scope, Scope::Project);
        assert_eq!(
            get("plugin:gitlab:gitlab").scope,
            Scope::Plugin("gitlab@mk".into())
        );
        // secrets (args, env, url query) are not copied
        let all = serde_json::to_string(&caps).unwrap();
        assert!(!all.contains("SECRET"), "{all}");
    }

    #[test]
    fn url_userinfo_is_stripped() {
        let d = describe(&serde_json::json!({"url":"https://user:pass@host.test:8080/p?k=1"}));
        assert_eq!(d["url"], "https://host.test:8080/p");
        let d = describe(&serde_json::json!({"url":"http://h.test"}));
        assert_eq!(d["url"], "http://h.test");
    }

    #[test]
    fn mcp_name_with_space_matches_normalized_tool_prefix() {
        assert_eq!(normalize("claude.ai Claude Docs"), "claude_ai_Claude_Docs");
        assert_eq!(normalize("plugin:gitlab:gitlab"), "plugin_gitlab_gitlab");
        assert_eq!(normalize("codegraph"), "codegraph");
        assert_eq!(normalize("docs-langchain"), "docs-langchain");
        assert_eq!(
            normalize("plugin:chrome-devtools-mcp:chrome-devtools"),
            "plugin_chrome-devtools-mcp_chrome-devtools"
        );
    }

    #[test]
    fn missing_or_malformed_config_is_empty() {
        let t = tempfile::tempdir().unwrap();
        let c = ScanCtx {
            claude_home: t.path().join("h"),
            claude_json: t.path().join("nope.json"),
            cwd: Some(t.path().join("x")),
            registry: Default::default(),
        };
        assert!(McpProvider.scan(&c).is_empty());
        write(&c.claude_json, "{not json");
        assert!(McpProvider.scan(&c).is_empty());
    }
}
