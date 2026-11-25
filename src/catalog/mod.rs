//! Capability catalog: what Claude Code *can* use on this machine (skills,
//! plugins, MCP servers, agents, workflows), read from disk.
//!
//! Extending: add one provider file implementing [`Provider`] and one line in
//! [`providers()`]. Nothing else needs to change.

pub mod frontmatter;
pub mod plugins;
pub mod skills;

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
        let items = providers().iter().flat_map(|p| p.scan(ctx)).collect();
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
    ]
}

#[cfg(test)]
pub(crate) fn write(p: &std::path::Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}
