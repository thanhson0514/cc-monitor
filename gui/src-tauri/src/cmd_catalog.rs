//! Tauri commands for the capability catalog (Library page).

use cc_monitor::catalog::usage::{session_usage, UseState};
use cc_monitor::catalog::{CapKind, Capability, Catalog, Scope, ScanCtx};
use cc_monitor::platform;
use cc_monitor::thread::Thread;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::State;

use crate::Shared;

const CACHE_TTL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityDto {
    pub kind: CapKind,
    pub id: String,
    pub name: String,
    pub description: String,
    pub scope: Scope,
    pub path: Option<String>,
    pub enabled_globally: bool,
    pub meta: Map<String, Value>,
}

impl From<&Capability> for CapabilityDto {
    fn from(c: &Capability) -> Self {
        CapabilityDto {
            kind: c.kind,
            id: c.id.clone(),
            name: c.name.clone(),
            description: c.description.clone(),
            scope: c.scope.clone(),
            path: c.path.as_ref().map(|p| p.display().to_string()),
            enabled_globally: c.enabled_globally,
            meta: c.meta.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDto {
    pub kind: CapKind,
    pub id: String,
    /// "unavailable" | "available" | "used"
    pub state: &'static str,
    pub count: u32,
    pub last: Option<i64>,
}

pub fn usage_dto(kind: CapKind, id: &str, st: &UseState) -> UsageDto {
    let (state, count, last) = match st {
        UseState::Unavailable => ("unavailable", 0, None),
        UseState::Available => ("available", 0, None),
        UseState::Used { count, last } => ("used", *count, *last),
    };
    UsageDto { kind, id: id.to_string(), state, count, last }
}

fn scan_ctx(cwd: Option<&str>) -> ScanCtx {
    let claude_home = platform::claude_home();
    let claude_json = claude_home
        .parent()
        .map(|p| p.join(".claude.json"))
        .unwrap_or_else(|| PathBuf::from(".claude.json"));
    ScanCtx { claude_home, claude_json, cwd: cwd.filter(|c| !c.is_empty()).map(PathBuf::from) }
}

type CatalogCache = Mutex<HashMap<Option<String>, (Instant, Catalog)>>;
static CACHE: std::sync::OnceLock<CatalogCache> = std::sync::OnceLock::new();

/// Catalog for `cwd`, rescanned at most every 5 s.
fn catalog_for(cwd: Option<&str>) -> Catalog {
    let key = cwd.filter(|c| !c.is_empty()).map(str::to_string);
    let cache = CACHE.get_or_init(Default::default);
    if let Some((at, cat)) = cache.lock().unwrap().get(&key) {
        if at.elapsed() < CACHE_TTL {
            return cat.clone();
        }
    }
    let cat = Catalog::scan(&scan_ctx(cwd));
    cache.lock().unwrap().insert(key, (Instant::now(), cat.clone()));
    cat
}

#[tauri::command]
pub fn get_catalog(cwd: Option<String>) -> Vec<CapabilityDto> {
    catalog_for(cwd.as_deref()).items.iter().map(CapabilityDto::from).collect()
}

/// Working directory of a session (the Library scans project-level items there).
#[tauri::command]
pub fn get_session_cwd(state: State<Shared>, sid: String) -> Option<String> {
    let store = state.store.lock().unwrap();
    store.sessions.get(&sid)?.main.cwd().map(str::to_string)
}

pub fn usage_dtos(threads: &[&Thread], catalog: &Catalog) -> Vec<UsageDto> {
    session_usage(threads, catalog)
        .entries
        .iter()
        .map(|(k, id, st)| usage_dto(*k, id, st))
        .collect()
}

#[tauri::command]
pub fn get_session_usage(state: State<Shared>, sid: String) -> Vec<UsageDto> {
    let cwd = {
        let store = state.store.lock().unwrap();
        match store.sessions.get(&sid) {
            Some(s) => s.main.cwd().map(str::to_string),
            None => return Vec::new(),
        }
    };
    // Scan outside the store lock (disk I/O).
    let catalog = catalog_for(cwd.as_deref());
    let store = state.store.lock().unwrap();
    let Some(sess) = store.sessions.get(&sid) else { return Vec::new() };
    let mut threads: Vec<&Thread> = vec![&sess.main];
    threads.extend(sess.agents.values().map(|a| &a.thread));
    usage_dtos(&threads, &catalog)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cap(kind: CapKind, scope: Scope) -> Capability {
        Capability {
            kind,
            id: "x".into(),
            name: "x".into(),
            description: "d".into(),
            scope,
            path: Some(PathBuf::from("/a/b")),
            enabled_globally: true,
            meta: Map::new(),
        }
    }

    #[test]
    fn catalog_dto_serializes_scope_and_kind() {
        let j = |c: Capability| serde_json::to_value(CapabilityDto::from(&c)).unwrap();
        let v = j(cap(CapKind::Mcp, Scope::Plugin("p@m".into())));
        assert_eq!(v["kind"], json!("mcp"));
        assert_eq!(v["scope"], json!({"plugin": "p@m"}));
        assert_eq!(v["enabledGlobally"], json!(true));
        assert_eq!(v["path"], json!("/a/b"));
        let v = j(cap(CapKind::Skill, Scope::BuiltIn));
        assert_eq!(v["kind"], json!("skill"));
        assert_eq!(v["scope"], json!("builtIn"));
    }

    #[test]
    fn usage_dto_states() {
        let u = |s| serde_json::to_value(usage_dto(CapKind::Skill, "s", &s)).unwrap();
        assert_eq!(
            u(UseState::Used { count: 3, last: Some(9) }),
            json!({"kind":"skill","id":"s","state":"used","count":3,"last":9})
        );
        assert_eq!(u(UseState::Available)["state"], json!("available"));
        assert_eq!(u(UseState::Unavailable)["count"], json!(0));
    }
}
