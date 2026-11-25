//! Per-session capability usage: what was available vs. actually called.

use super::mcp::normalize;
use super::{CapKind, Capability, Catalog, Scope};
use crate::thread::Thread;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UseState {
    Unavailable,
    Available,
    Used { count: u32, last: Option<i64> },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsage {
    pub entries: Vec<(CapKind, String, UseState)>,
}

/// Workflow id: stem of `scriptPath` (matches the workflows provider ids), else
/// `meta.name` scanned out of an inline `script`, else the bare `"workflow"`.
fn workflow_id(input: &Value) -> String {
    let s = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    };
    if let Some(stem) = s("scriptPath")
        .and_then(|p| std::path::Path::new(p).file_stem())
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
    {
        return stem.to_string();
    }
    s("script")
        .and_then(meta_name)
        .unwrap_or_else(|| "workflow".to_string())
}

/// Tolerant scan for `name: '...'` (single or double quotes) after `meta`.
fn meta_name(script: &str) -> Option<String> {
    let mut rest = &script[script.find("meta")? + 4..];
    while let Some(i) = rest.find("name") {
        let before_ok = rest[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        let after = rest[i + 4..].trim_start();
        rest = &rest[i + 4..];
        if !before_ok {
            continue;
        }
        let Some(v) = after.strip_prefix(':').map(str::trim_start) else {
            continue;
        };
        let Some(q) = v.chars().next().filter(|c| matches!(c, '\'' | '"' | '`')) else {
            continue;
        };
        let body = &v[1..];
        let end = body.find(q)?;
        let n = body[..end].trim();
        return (!n.is_empty()).then(|| n.to_string());
    }
    None
}

/// Map one tool call to the capability it exercises: `(kind, normalized id)`.
fn target(name: &str, input: &Value) -> Option<(CapKind, String)> {
    let s = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    };
    match name {
        "Skill" => Some((CapKind::Skill, s("skill")?.to_string())),
        "Agent" | "Task" => Some((
            CapKind::Agent,
            s("subagent_type").unwrap_or("general-purpose").to_string(),
        )),
        "Workflow" => Some((CapKind::Workflow, workflow_id(input))),
        _ => {
            let server = name.strip_prefix("mcp__")?.split("__").next()?;
            (!server.is_empty()).then(|| (CapKind::Mcp, server.to_string()))
        }
    }
}

/// MCP ids are compared in tool-prefix form; everything else verbatim.
fn key(kind: CapKind, id: &str) -> String {
    if kind == CapKind::Mcp {
        normalize(id)
    } else {
        id.to_string()
    }
}

fn inventory_names(threads: &[&Thread], kind: CapKind) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for t in threads {
        let inv = t.inventory();
        let l = match kind {
            CapKind::Skill => &inv.skills,
            CapKind::Mcp => &inv.mcp,
            CapKind::Agent => &inv.agents,
            _ => continue,
        };
        for n in l {
            if !v.contains(n) {
                v.push(n.clone());
            }
        }
    }
    v
}

fn used_workflows_off_disk(
    counts: &HashMap<(CapKind, String), (u32, Option<i64>)>,
    catalog: &Catalog,
) -> Vec<String> {
    let mut v: Vec<String> = counts
        .keys()
        .filter(|(k, id)| *k == CapKind::Workflow && !catalog.of(*k).any(|c| &c.id == id))
        .map(|(_, id)| id.clone())
        .collect();
    v.sort();
    v
}

fn used_counts(threads: &[&Thread]) -> HashMap<(CapKind, String), (u32, Option<i64>)> {
    let mut counts: HashMap<(CapKind, String), (u32, Option<i64>)> = HashMap::new();
    let mut seen = HashSet::new();
    for t in threads {
        for (id, name, input, ts) in t.tool_uses() {
            if !seen.insert(id.to_string()) {
                continue;
            }
            let Some((k, n)) = target(name, input) else {
                continue;
            };
            let e = counts.entry((k, key(k, &n))).or_insert((0, None));
            e.0 += 1;
            e.1 = e.1.max(ts);
        }
    }
    counts
}

/// Per capability of the catalog (plus announced-but-not-on-disk ones):
/// `Used` when called by the main thread or any subagent/workflow thread,
/// else `Available` when the session announced it (or announced nothing of
/// that kind and it is on disk), else `Unavailable`.
pub fn session_usage(threads: &[&Thread], catalog: &Catalog) -> SessionUsage {
    let counts = used_counts(threads);
    let mut entries = Vec::new();
    let mut emit = |kind: CapKind, id: &str, on_disk: bool, inv: &[String]| {
        let announced = inv.iter().any(|n| key(kind, n) == key(kind, id));
        let state = match counts.get(&(kind, key(kind, id))) {
            Some(&(count, last)) => UseState::Used { count, last },
            None if announced || (on_disk && inv.is_empty()) => UseState::Available,
            None => UseState::Unavailable,
        };
        entries.push((kind, id.to_string(), state));
    };
    for kind in [
        CapKind::Skill,
        CapKind::Agent,
        CapKind::Mcp,
        CapKind::Workflow,
    ] {
        let inv = inventory_names(threads, kind);
        for c in catalog.of(kind) {
            emit(kind, &c.id, true, &inv);
        }
        for n in inv
            .iter()
            .filter(|n| !catalog.of(kind).any(|c| key(kind, &c.id) == key(kind, n)))
        {
            emit(kind, n, false, &inv);
        }
        if kind == CapKind::Workflow {
            for id in used_workflows_off_disk(&counts, catalog) {
                emit(kind, &id, false, &[]);
            }
        }
    }
    SessionUsage { entries }
}

/// Capabilities the session announced that are not on disk: built-in agents
/// (`Scope::BuiltIn`), claude.ai MCP servers and anything else (`Scope::Managed`).
/// Workflows that were run (e.g. from an inline `script`) but have no file on
/// disk are added too, as `Scope::Managed`.
pub fn inventory_only(threads: &[&Thread], catalog: &Catalog) -> Vec<Capability> {
    let mut out = Vec::new();
    for (kind, scope) in [
        (CapKind::Agent, Scope::BuiltIn),
        (CapKind::Mcp, Scope::Managed),
        (CapKind::Skill, Scope::BuiltIn),
    ] {
        for n in inventory_names(threads, kind) {
            if catalog.of(kind).any(|c| key(kind, &c.id) == key(kind, &n)) {
                continue;
            }
            out.push(Capability {
                kind,
                id: n.clone(),
                name: n,
                description: String::new(),
                scope: scope.clone(),
                path: None,
                enabled_globally: true,
                meta: Map::new(),
            });
        }
    }
    for id in used_workflows_off_disk(&used_counts(threads), catalog) {
        out.push(Capability {
            kind: CapKind::Workflow,
            id: id.clone(),
            name: id,
            description: String::new(),
            scope: Scope::Managed,
            path: None,
            enabled_globally: true,
            meta: Map::new(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{CapKind::*, Scope};
    use super::*;
    use crate::event::parse_line;
    use serde_json::{json, Map};

    fn cap(kind: CapKind, id: &str) -> Capability {
        Capability {
            kind,
            id: id.into(),
            name: id.into(),
            description: String::new(),
            scope: Scope::User,
            path: None,
            enabled_globally: true,
            meta: Map::new(),
        }
    }

    fn tool_line(uuid: &str, ts: &str, name: &str, input: serde_json::Value) -> String {
        json!({"type":"assistant","uuid":uuid,"timestamp":ts,"message":{"id":format!("m{uuid}"),"role":"assistant",
          "content":[{"type":"tool_use","id":format!("t{uuid}"),"name":name,"input":input}]}})
        .to_string()
    }

    fn thread(lines: &[String]) -> Thread {
        let mut t = Thread::new();
        for l in lines {
            t.push(parse_line(l).expect(l));
        }
        t
    }

    fn state<'a>(u: &'a SessionUsage, k: CapKind, id: &str) -> &'a UseState {
        &u.entries
            .iter()
            .find(|e| e.0 == k && e.1 == id)
            .unwrap_or_else(|| panic!("no {id}"))
            .2
    }

    #[test]
    fn usage_counts_skill_agent_and_mcp_across_subagents() {
        let catalog = Catalog {
            items: vec![
                cap(Skill, "brainstorm"),
                cap(Skill, "idle"),
                cap(Agent, "general-purpose"),
                cap(Agent, "reviewer"),
                cap(Mcp, "claude.ai Claude Docs"),
                cap(Mcp, "plugin:gitlab:gitlab"),
                cap(Mcp, "unused"),
                cap(Workflow, "ship"),
            ],
        };
        let inv = r#"{"type":"attachment","uuid":"i","attachment":{"type":"mcp_instructions_delta","addedNames":["claude.ai Claude Docs","unused","builtin srv"]}}"#;
        let main = thread(&[
            inv.to_string(),
            tool_line(
                "1",
                "2026-11-25T00:00:01.000Z",
                "Skill",
                json!({"skill":"brainstorm"}),
            ),
            tool_line(
                "2",
                "2026-11-25T00:00:03.000Z",
                "Skill",
                json!({"skill":"brainstorm"}),
            ),
            tool_line(
                "3",
                "2026-11-25T00:00:02.000Z",
                "Agent",
                json!({"description":"x"}),
            ), // default type
            tool_line(
                "4",
                "2026-11-25T00:00:04.000Z",
                "Workflow",
                json!({"scriptPath":"/p/.claude/workflows/ship.js"}),
            ),
            tool_line(
                "5",
                "2026-11-25T00:00:05.000Z",
                "mcp__claude_ai_Claude_Docs__batch",
                json!({}),
            ),
        ]);
        let sub = thread(&[
            tool_line(
                "6",
                "2026-11-25T00:00:09.000Z",
                "Agent",
                json!({"subagent_type":"reviewer"}),
            ),
            tool_line(
                "7",
                "2026-11-25T00:00:08.000Z",
                "mcp__plugin_gitlab_gitlab__authenticate",
                json!({}),
            ),
            tool_line(
                "8",
                "2026-11-25T00:00:08.000Z",
                "Skill",
                json!({"skill":"brainstorm"}),
            ),
        ]);
        let u = session_usage(&[&main, &sub], &catalog);
        let ts = |s: &str| {
            chrono::DateTime::parse_from_rfc3339(s)
                .unwrap()
                .timestamp_millis()
        };
        assert_eq!(
            state(&u, Skill, "brainstorm"),
            &UseState::Used {
                count: 3,
                last: Some(ts("2026-11-25T00:00:08.000Z").max(ts("2026-11-25T00:00:03.000Z")))
            }
        );
        assert!(matches!(
            state(&u, Agent, "general-purpose"),
            UseState::Used { count: 1, .. }
        ));
        assert!(matches!(
            state(&u, Agent, "reviewer"),
            UseState::Used { count: 1, .. }
        ));
        assert!(matches!(
            state(&u, Workflow, "ship"),
            UseState::Used { count: 1, .. }
        ));
        assert!(matches!(
            state(&u, Mcp, "claude.ai Claude Docs"),
            UseState::Used { count: 1, .. }
        ));
        assert!(matches!(
            state(&u, Mcp, "plugin:gitlab:gitlab"),
            UseState::Used { count: 1, .. }
        ));
        // in inventory, never called
        assert_eq!(state(&u, Mcp, "unused"), &UseState::Available);
        // on disk but the session never announced skills/agents... mcp inventory exists, so:
        // skill inventory is empty -> falls back to "on disk = available"
        assert_eq!(state(&u, Skill, "idle"), &UseState::Available);
        assert_eq!(
            state(&u, Agent, "reviewer").clone(),
            UseState::Used {
                count: 1,
                last: Some(ts("2026-11-25T00:00:09.000Z"))
            }
        );
        // an mcp inventory exists and does not list this one
        let cat2 = Catalog {
            items: vec![cap(Mcp, "elsewhere")],
        };
        let u2 = session_usage(&[&main], &cat2);
        assert_eq!(state(&u2, Mcp, "elsewhere"), &UseState::Unavailable);
        // announced but not on disk -> extra capability with Managed scope
        let extra = inventory_only(&[&main], &catalog);
        let b = extra.iter().find(|c| c.id == "builtin srv").unwrap();
        assert_eq!((b.kind, &b.scope), (Mcp, &Scope::Managed));
        assert!(!extra.iter().any(|c| c.id == "unused"));
    }

    #[test]
    fn same_event_pushed_twice_counts_once() {
        let l = tool_line(
            "1",
            "2026-11-25T00:00:01.000Z",
            "Skill",
            json!({"skill":"a"}),
        );
        let t = thread(&[l.clone(), l]);
        let u = session_usage(
            &[&t],
            &Catalog {
                items: vec![cap(Skill, "a")],
            },
        );
        assert!(matches!(
            state(&u, Skill, "a"),
            UseState::Used { count: 1, .. }
        ));
    }

    #[test]
    fn usage_serializes_camel_case() {
        let u = SessionUsage {
            entries: vec![(
                Mcp,
                "x".into(),
                UseState::Used {
                    count: 2,
                    last: None,
                },
            )],
        };
        assert_eq!(
            serde_json::to_value(&u).unwrap(),
            json!({"entries":[["mcp","x",{"used":{"count":2,"last":null}}]]})
        );
    }

    #[test]
    fn workflow_usage_from_real_input_shapes() {
        let inline = "export const meta = { name: 'review-changes', description: 'x' }\nrun()";
        let t = thread(&[
            tool_line(
                "1",
                "2026-11-25T00:00:01.000Z",
                "Workflow",
                json!({"script": inline}),
            ),
            tool_line(
                "2",
                "2026-11-25T00:00:02.000Z",
                "Workflow",
                json!({"scriptPath":"/p/.claude/workflows/ship.js"}),
            ),
            tool_line(
                "3",
                "2026-11-25T00:00:03.000Z",
                "Workflow",
                json!({"description":"x","script":"export const meta = {\n  description: 'd',\n  name: \"dq\",\n}"}),
            ),
            tool_line(
                "4",
                "2026-11-25T00:00:04.000Z",
                "Workflow",
                json!({"args":{}}),
            ),
            tool_line(
                "5",
                "2026-11-25T00:00:05.000Z",
                "Workflow",
                json!({"script":"const meta = { rename: 'no' }"}),
            ),
        ]);
        let cat = Catalog {
            items: vec![cap(Workflow, "ship")],
        };
        let u = session_usage(&[&t], &cat);
        assert!(matches!(
            state(&u, Workflow, "ship"),
            UseState::Used { count: 1, .. }
        ));
        assert!(matches!(
            state(&u, Workflow, "review-changes"),
            UseState::Used { count: 1, .. }
        ));
        assert!(matches!(
            state(&u, Workflow, "dq"),
            UseState::Used { count: 1, .. }
        ));
        assert!(matches!(
            state(&u, Workflow, "workflow"),
            UseState::Used { count: 2, .. }
        ));
        let extra = inventory_only(&[&t], &cat);
        let r = extra.iter().find(|c| c.id == "review-changes").unwrap();
        assert_eq!((r.kind, &r.scope), (Workflow, &Scope::Managed));
        assert!(!extra.iter().any(|c| c.id == "ship"));
    }

    #[test]
    fn plugin_mcp_catalog_id_matches_real_tool_prefix() {
        let cat = Catalog {
            items: vec![cap(Mcp, "plugin:gitlab:gitlab")],
        };
        let t = thread(&[tool_line(
            "1",
            "2026-11-25T00:00:01.000Z",
            "mcp__plugin_gitlab_gitlab__authenticate",
            json!({}),
        )]);
        assert!(matches!(
            state(&session_usage(&[&t], &cat), Mcp, "plugin:gitlab:gitlab"),
            UseState::Used { count: 1, .. }
        ));
        assert_eq!(normalize("plugin:gitlab:gitlab"), "plugin_gitlab_gitlab");
    }

    #[test]
    fn hyphenated_mcp_ids_match_real_tool_prefix() {
        let cat = Catalog {
            items: vec![
                cap(Mcp, "docs-langchain"),
                cap(Mcp, "plugin:chrome-devtools-mcp:chrome-devtools"),
            ],
        };
        let t = thread(&[
            tool_line(
                "1",
                "2026-11-25T00:00:01.000Z",
                "mcp__docs-langchain__search_docs_by_lang_chain",
                json!({}),
            ),
            tool_line(
                "2",
                "2026-11-25T00:00:02.000Z",
                "mcp__plugin_chrome-devtools-mcp_chrome-devtools__click",
                json!({}),
            ),
        ]);
        let u = session_usage(&[&t], &cat);
        assert!(matches!(
            state(&u, Mcp, "docs-langchain"),
            UseState::Used { count: 1, .. }
        ));
        assert!(matches!(
            state(&u, Mcp, "plugin:chrome-devtools-mcp:chrome-devtools"),
            UseState::Used { count: 1, .. }
        ));
    }
}
