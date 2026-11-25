//! Managed-session control: per-session overlay + ccmon-owned state.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub mod state;

/// Per-session launch overlay. Ids: skills = catalog skill id (`name` or
/// `plugin:name`), plugins = `name@marketplace`, MCP = catalog server id,
/// agents = agent name/type.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Overlay {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
    pub disabled_skills: BTreeSet<String>,
    pub disabled_plugins: BTreeSet<String>,
    pub disabled_mcp: BTreeSet<String>,
    pub disabled_agents: BTreeSet<String>,
    pub disabled_workflows: BTreeSet<String>,
    /// Master switch set by the user; never derived from the registry.
    pub workflows_off: bool,
    pub main_agent: Option<String>, // --agent
}

/// ccmon-wide switches; files and history of disabled items are untouched.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Registry {
    pub disabled_agents: BTreeSet<String>,
    pub disabled_workflows: BTreeSet<String>,
    pub notes: BTreeMap<String, String>,
}
