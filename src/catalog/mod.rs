//! Capability catalog: what Claude Code *can* use on this machine (skills,
//! plugins, MCP servers, agents, workflows), read from disk.
//!
//! Extending: add one provider file implementing [`Provider`] and one line in
//! [`providers()`]. Nothing else needs to change.

pub mod agents;
pub mod frontmatter;
pub mod history;
pub mod mcp;
pub mod plugins;
pub mod skills;
pub mod usage;
pub mod workflows;

use serde::Serialize;
use serde_json::{Map, Value};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CapKind {
    Skill,
    Plugin,
    Mcp,
    Agent,
    Workflow,
}

/// Where a capability comes from. Serializes (serde default, externally tagged,
/// camelCase) as a bare string for unit variants (`"user"`, `"project"`,
/// `"builtIn"`, `"managed"`) and as `{"plugin": "<plugin id>"}` for plugins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
    User,
    Project,
    Plugin(String),
    BuiltIn,
    Managed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub kind: CapKind,
    /// Stable id: skill `name` or `plugin:name`; MCP server name; agent name; plugin `p@m`.
    pub id: String,
    pub name: String,
    pub description: String,
    pub scope: Scope,
    pub path: Option<PathBuf>,
    pub enabled_globally: bool,
    /// Kind-specific fields (tools, model, version, transport, components...).
    pub meta: Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct ScanCtx {
    pub claude_home: PathBuf,
    pub claude_json: PathBuf,
    pub cwd: Option<PathBuf>,
    /// ccmon registry (disabled agents/workflows). Passed in so providers stay
    /// free of IO beyond `ScanCtx`; [`Catalog::scan`] applies it after scanning.
    pub registry: crate::control::Registry,
}

pub trait Provider {
    fn kind(&self) -> CapKind;
    fn scan(&self, ctx: &ScanCtx) -> Vec<Capability>;
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub items: Vec<Capability>,
}

impl Catalog {
    pub fn scan(ctx: &ScanCtx) -> Catalog {
        let mut items: Vec<Capability> = providers().iter().flat_map(|p| p.scan(ctx)).collect();
        // Post-scan: disabling keeps files and history; it only flips this flag.
        for c in &mut items {
            match c.kind {
                CapKind::Agent => {
                    c.enabled_globally = !ctx.registry.disabled_agents.contains(&c.id)
                }
                CapKind::Workflow => {
                    c.enabled_globally = !ctx.registry.disabled_workflows.contains(&c.id)
                }
                _ => {}
            }
        }
        Catalog { items }
    }

    pub fn of(&self, k: CapKind) -> impl Iterator<Item = &Capability> {
        self.items.iter().filter(move |c| c.kind == k)
    }
}

/// The single registration list. Add new capability kinds here.
pub fn providers() -> Vec<Box<dyn Provider>> {
    vec![
        Box::new(plugins::PluginProvider),
        Box::new(skills::SkillProvider),
        Box::new(mcp::McpProvider),
        Box::new(agents::AgentProvider),
        Box::new(workflows::WorkflowProvider),
    ]
}

#[cfg(test)]
pub(crate) fn write(p: &std::path::Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[test]
    fn registry_drives_enabled_globally_for_agents_and_workflows() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home/.claude");
        write(&home.join("agents/a.md"), "---\nname: a\n---\n");
        write(&home.join("agents/b.md"), "---\nname: b\n---\n");
        write(&home.join("workflows/w.js"), "// w");
        let mut reg = crate::control::Registry::default();
        reg.disabled_agents.insert("a".into());
        reg.disabled_workflows.insert("w".into());
        let ctx = ScanCtx {
            claude_home: home,
            claude_json: t.path().join("c.json"),
            cwd: None,
            registry: reg,
        };
        let cat = Catalog::scan(&ctx);
        let en = |k, id: &str| cat.of(k).find(|c| c.id == id).unwrap().enabled_globally;
        assert!(!en(CapKind::Agent, "a"));
        assert!(en(CapKind::Agent, "b"));
        assert!(!en(CapKind::Workflow, "w"));
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    #[test]
    fn scope_json_shape() {
        let j = |s: Scope| serde_json::to_value(s).unwrap();
        assert_eq!(j(Scope::User), serde_json::json!("user"));
        assert_eq!(j(Scope::Project), serde_json::json!("project"));
        assert_eq!(j(Scope::BuiltIn), serde_json::json!("builtIn"));
        assert_eq!(j(Scope::Managed), serde_json::json!("managed"));
        assert_eq!(
            j(Scope::Plugin("x".into())),
            serde_json::json!({"plugin": "x"})
        );
    }
}
