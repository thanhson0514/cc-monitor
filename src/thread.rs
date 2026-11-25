//! One conversation thread (main session, subagent or workflow agent) built from events.

use crate::bgtask::{BgKind, BgState, BgTask, Registry};
use crate::event::{Block, Event, InventoryDelta, Kind, RateInfo, Usage};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Ok,
    Err,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ThreadLink {
    Agent(String),
    Workflow(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Prompt {
        ts: Option<i64>,
        text: String,
    },
    Thinking {
        ts: Option<i64>,
        text: String,
        /// Thinking duration reported by Claude Code (the text itself is often not stored).
        ms: Option<u64>,
    },
    Text {
        ts: Option<i64>,
        text: String,
    },
    Tool(ToolItem),
    Compact {
        ts: Option<i64>,
        pre: u64,
        post: u64,
    },
    System {
        ts: Option<i64>,
        text: String,
    },
    /// Start of one assistant API response (model + token usage of that request).
    Turn {
        ts: Option<i64>,
        model: Option<String>,
        usage: Option<Usage>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolItem {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub input: Value,
    pub state: ToolState,
    pub started: Option<i64>,
    pub ended: Option<i64>,
    pub result: String,
    pub persisted: Option<PathBuf>,
    pub link: Option<ThreadLink>,
    /// Background task this tool launched, or targets (`TaskOutput`, `TaskStop`, ...).
    pub bg: Option<BgTask>,
    /// `Yield` only: background tasks pending when it started.
    pub waits: Vec<BgTask>,
    /// `Yield` only: task id of the first notice after it started.
    pub woke_by: Option<String>,
}

#[derive(Debug, Clone)]
struct ResultInfo {
    is_error: bool,
    ts: Option<i64>,
    content: String,
    link: Option<ThreadLink>,
}

#[derive(Debug, Default)]
pub struct Thread {
    events: Vec<Event>,
    by_uuid: HashMap<String, usize>,
    leaf: Option<usize>,
    results: HashMap<String, ResultInfo>,
    pending: HashSet<String>,
    tool_names: HashMap<String, String>,
    workflow_names: HashMap<String, String>,
    link_to_tool: HashMap<ThreadLink, String>,
    tool_inputs: HashMap<String, (Value, Option<i64>)>,
    bg: Registry,
    /// Task notices in arrival order: (task id, ts).
    notices: Vec<(String, Option<i64>)>,
    items: Vec<Item>,
    dirty: bool,
    last_usage: Option<Usage>,
    model: Option<String>,
    effort: Option<String>,
    title: Option<String>,
    lines: Option<(u64, u64)>,
    rate: Option<RateInfo>,
    window: Option<u64>,
    compactions: u32,
    git_branch: Option<String>,
    cwd: Option<String>,
    last_tool: Option<String>,
    first_ts: Option<i64>,
    user_files: Vec<UserFile>,
    inventory: InventoryDelta,
    inventory_seen: HashSet<String>,
}

fn merge_list(into: &mut Vec<String>, add: &[String], remove: &[String]) {
    for a in add {
        if !into.contains(a) {
            into.push(a.clone());
        }
    }
    into.retain(|x| !remove.contains(x));
}

fn merge_inventory(into: &mut InventoryDelta, d: &InventoryDelta) {
    let none = InventoryDelta::default();
    let r = d.removed.as_deref().unwrap_or(&none);
    merge_list(&mut into.skills, &d.skills, &r.skills);
    merge_list(&mut into.mcp, &d.mcp, &r.mcp);
    merge_list(&mut into.agents, &d.agents, &r.agents);
    merge_list(&mut into.deferred_tools, &d.deferred_tools, &r.deferred_tools);
}

/// A file the user supplied: pasted image, `@` mention of a file or directory.
#[derive(Debug, Clone, PartialEq)]
pub struct UserFile {
    pub ts: Option<i64>,
    pub path: String,
    pub kind: UserFileKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserFileKind {
    Image,
    File,
    Directory,
}

/// Paths in `[Image: source: /path]` markers Claude Code adds to prompts with pasted images.
pub fn image_sources(prompt: &str) -> Vec<String> {
    let key = "[Image: source: ";
    let mut out = Vec::new();
    let mut rest = prompt;
    while let Some(i) = rest.find(key) {
        rest = &rest[i + key.len()..];
        if let Some(end) = rest.find(']') {
            out.push(rest[..end].trim().to_string());
            rest = &rest[end..];
        }
    }
    out
}

impl Thread {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn push(&mut self, ev: Event) {
        if let Some(ts) = ev.ts {
            self.first_ts.get_or_insert(ts);
        }
        if let Some(b) = &ev.git_branch {
            self.git_branch = Some(b.clone());
        }
        if self.cwd.is_none() {
            self.cwd = ev.cwd.clone();
        }
        if let (Kind::User, Some(p)) = (&ev.kind, &ev.prompt) {
            for path in image_sources(p) {
                self.push_user_file(ev.ts, path, UserFileKind::Image);
            }
        }
        match &ev.kind {
            Kind::Attachment { path, is_dir } => {
                let kind = if *is_dir {
                    UserFileKind::Directory
                } else {
                    UserFileKind::File
                };
                self.push_user_file(ev.ts, path.clone(), kind);
            }
            Kind::Title(t) => {
                self.title = Some(t.clone());
                return;
            }
            Kind::CostState { added, removed } => {
                self.lines = Some((*added, *removed));
                return;
            }
            Kind::System { subtype, .. } if subtype == "compact_boundary" => self.compactions += 1,
            Kind::Inventory(d) => {
                // Replayed lines (same uuid) must not re-add what a later line removed.
                if ev.uuid.as_ref().is_none_or(|u| self.inventory_seen.insert(u.clone())) {
                    merge_inventory(&mut self.inventory, d);
                }
            }
            Kind::TaskNotice(n) => {
                self.bg.notice(n, ev.ts);
                self.notices.push((n.task_id.clone(), ev.ts));
            }
            Kind::Meta => {
                if let Some(u) = ev.usage.filter(|u| u.context() > 0) {
                    self.last_usage = Some(u);
                }
                if let Some(m) = &ev.model {
                    self.model = Some(m.clone());
                }
                if let Some(e) = &ev.effort {
                    self.effort = Some(e.clone());
                }
                if ev.rate.is_some() {
                    self.rate = ev.rate;
                }
                if ev.window.is_some() {
                    self.window = ev.window;
                }
                return;
            }
            Kind::Assistant => {
                if let Some(u) = ev.usage {
                    if u.context() > 0 {
                        self.last_usage = Some(u);
                    }
                }
                if let Some(m) = &ev.model {
                    if m != "<synthetic>" {
                        self.model = Some(m.clone());
                    }
                }
                if let Some(e) = &ev.effort {
                    self.effort = Some(e.clone());
                }
            }
            _ => {}
        }
        for b in &ev.blocks {
            match b {
                Block::ToolUse { id, name, input } => {
                    self.pending.insert(id.clone());
                    self.tool_names.insert(id.clone(), name.clone());
                    self.tool_inputs.insert(id.clone(), (input.clone(), ev.ts));
                    if name == "Workflow" {
                        if let Some(n) = workflow_meta_name(input) {
                            self.workflow_names.insert(id.clone(), n);
                        }
                    }
                    self.last_tool = Some(name.clone());
                }
                Block::ToolResult {
                    tool_use_id,
                    is_error,
                    content,
                } => {
                    self.pending.remove(tool_use_id);
                    let link = ev.link.as_ref().and_then(|l| {
                        l.agent_id
                            .clone()
                            .map(ThreadLink::Agent)
                            .or_else(|| l.run_id.clone().map(ThreadLink::Workflow))
                    });
                    if let (Some(l), Some(info)) = (&link, &ev.link) {
                        self.link_to_tool.insert(l.clone(), tool_use_id.clone());
                        if let (ThreadLink::Workflow(_), Some(n)) = (l, &info.workflow_name) {
                            self.workflow_names.insert(tool_use_id.clone(), n.clone());
                        }
                    }
                    self.launch_bg(tool_use_id, ev.bg_task_id.as_deref(), ev.link.as_ref());
                    self.results.insert(
                        tool_use_id.clone(),
                        ResultInfo {
                            is_error: *is_error,
                            ts: ev.ts,
                            content: content.clone(),
                            link,
                        },
                    );
                }
                Block::Thinking(_) | Block::Text(_) => {}
            }
        }
        if let Some(u) = &ev.uuid {
            let idx = self.events.len();
            self.by_uuid.insert(u.clone(), idx);
            self.leaf = Some(idx);
        }
        self.events.push(ev);
        self.dirty = true;
    }

    /// Items on the live branch (leaf → root via parentUuid, crossing compact boundaries).
    pub fn items(&mut self) -> &[Item] {
        if self.dirty {
            self.rebuild();
            self.dirty = false;
        }
        &self.items
    }

    fn rebuild(&mut self) {
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut cur = self.leaf;
        while let Some(i) = cur {
            if !seen.insert(i) {
                break;
            }
            chain.push(i);
            let ev = &self.events[i];
            let next = ev.parent.as_ref().or(ev.logical_parent.as_ref());
            cur = next.and_then(|p| self.by_uuid.get(p).copied());
        }
        chain.reverse();
        let mut items = Vec::with_capacity(chain.len());
        let mut last_msg: Option<&str> = None;
        let mut turn_idx: Option<usize> = None;
        for i in chain {
            let ev = &self.events[i];
            if ev.kind == Kind::Assistant {
                if ev.msg_id.is_none() || ev.msg_id.as_deref() != last_msg {
                    last_msg = ev.msg_id.as_deref();
                    turn_idx = Some(items.len());
                    items.push(Item::Turn {
                        ts: ev.ts,
                        model: ev.model.clone(),
                        usage: ev.usage,
                    });
                } else if let (Some(t), Some(u)) = (turn_idx, ev.usage) {
                    if let Some(Item::Turn { usage, .. }) = items.get_mut(t) {
                        *usage = Some(u);
                    }
                }
            } else if ev.kind == Kind::User {
                last_msg = None;
            }
            if let Some(c) = &ev.compact {
                items.push(Item::Compact {
                    ts: ev.ts,
                    pre: c.pre,
                    post: c.post,
                });
                continue;
            }
            if ev.is_compact_summary {
                continue;
            }
            match &ev.kind {
                Kind::System { subtype, text } => {
                    if subtype == "local_command" || subtype == "informational" {
                        items.push(Item::System {
                            ts: ev.ts,
                            text: text.clone(),
                        });
                    }
                    continue;
                }
                Kind::TaskNotice(n) => {
                    let known = self.bg.get(&n.task_id).is_some()
                        || n.tool_use_id
                            .as_deref()
                            .is_some_and(|u| self.bg.by_tool_use(u).is_some());
                    if !known {
                        let text = if n.summary.is_empty() {
                            format!("task {} {}", n.task_id, n.status)
                        } else {
                            n.summary.clone()
                        };
                        items.push(Item::System { ts: ev.ts, text });
                    }
                    continue;
                }
                Kind::User => {
                    if let Some(p) = &ev.prompt {
                        if ev.is_meta || p.starts_with('<') {
                            items.push(Item::System {
                                ts: ev.ts,
                                text: p.clone(),
                            });
                        } else {
                            items.push(Item::Prompt {
                                ts: ev.ts,
                                text: p.clone(),
                            });
                        }
                    }
                }
                _ => {}
            }
            for b in &ev.blocks {
                match b {
                    Block::Text(t) if !t.trim().is_empty() => items.push(Item::Text {
                        ts: ev.ts,
                        text: t.clone(),
                    }),
                    Block::Thinking(t) => {
                        // One reasoning step can be stored as several thinking blocks.
                        if let Some(Item::Thinking { text, ms, .. }) = items.last_mut() {
                            if ev.thinking_ms.is_some() {
                                *ms = ev.thinking_ms;
                            }
                            if !t.is_empty() {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(t);
                            }
                        } else {
                            items.push(Item::Thinking {
                                ts: ev.ts,
                                text: t.clone(),
                                ms: ev.thinking_ms,
                            });
                        }
                    }
                    Block::ToolUse { id, name, input } => {
                        let r = self.results.get(id);
                        let summary = match self.workflow_names.get(id) {
                            Some(n) if name == "Workflow" => format!("workflow {n}"),
                            _ => tool_summary(name, input),
                        };
                        items.push(Item::Tool(ToolItem {
                            id: id.clone(),
                            name: name.clone(),
                            summary,
                            input: input.clone(),
                            state: match r {
                                None => ToolState::Running,
                                Some(r) if r.is_error => ToolState::Err,
                                Some(_) => ToolState::Ok,
                            },
                            started: ev.ts,
                            ended: r.and_then(|r| r.ts),
                            result: r.map(|r| r.content.clone()).unwrap_or_default(),
                            persisted: r.and_then(|r| persisted_path(&r.content)),
                            link: r.and_then(|r| r.link.clone()),
                            bg: self.bg_for_tool(id, name, input),
                            waits: match (name.as_str(), ev.ts) {
                                ("Yield", Some(ts)) => {
                                    self.bg.pending_at(ts).into_iter().cloned().collect()
                                }
                                _ => Vec::new(),
                            },
                            woke_by: match (name.as_str(), ev.ts) {
                                ("Yield", Some(ts)) => self
                                    .notices
                                    .iter()
                                    .filter(|(_, t)| t.is_some_and(|t| t >= ts))
                                    .min_by_key(|(id, _)| {
                                        !self.bg.pending_at(ts).iter().any(|w| w.id == *id)
                                    })
                                    .map(|(id, _)| id.clone()),
                                _ => None,
                            },
                        }))
                    }
                    _ => {}
                }
            }
        }
        self.items = items;
    }

    /// Register a background task started by the tool call `tool_use_id`, if it started one.
    fn launch_bg(
        &mut self,
        tool_use_id: &str,
        bg_task_id: Option<&str>,
        link: Option<&crate::event::ToolLinkInfo>,
    ) {
        let (Some(name), Some((input, started))) = (
            self.tool_names.get(tool_use_id),
            self.tool_inputs.get(tool_use_id),
        ) else {
            return;
        };
        let s = |k: &str| {
            input
                .get(k)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
        };
        let (id, kind, label) = match name.as_str() {
            "Bash" => {
                let Some(id) = bg_task_id else { return };
                let label = s("description")
                    .or_else(|| s("command").and_then(|c| c.lines().next()))
                    .unwrap_or(id);
                (id.to_string(), BgKind::Shell, label.to_string())
            }
            "Monitor" => (
                bg_task_id.unwrap_or(tool_use_id).to_string(),
                BgKind::Monitor,
                s("description").unwrap_or("Monitor").to_string(),
            ),
            "Agent" => {
                let Some(l) = link.filter(|l| l.status.as_deref() == Some("async_launched")) else {
                    return;
                };
                let Some(a) = &l.agent_id else { return };
                let label = s("description").or(s("subagent_type")).unwrap_or(a);
                (a.clone(), BgKind::Agent, label.to_string())
            }
            "Workflow" => {
                let Some(run) = link.and_then(|l| l.run_id.as_ref()) else {
                    return;
                };
                let label = link
                    .and_then(|l| l.workflow_name.clone())
                    .or_else(|| self.workflow_names.get(tool_use_id).cloned())
                    .unwrap_or_else(|| run.clone());
                let id = link
                    .and_then(|l| l.task_id.clone())
                    .unwrap_or_else(|| run.clone());
                (id, BgKind::Workflow, label)
            }
            _ => return,
        };
        let started = *started;
        self.bg.launch(BgTask {
            id,
            kind,
            label,
            tool_use_id: tool_use_id.to_string(),
            output_file: None,
            state: BgState::Running,
            started,
            ended: None,
            summary: None,
        });
    }

    fn bg_for_tool(&self, id: &str, name: &str, input: &Value) -> Option<BgTask> {
        if let Some(t) = self.bg.by_tool_use(id) {
            return Some(t.clone());
        }
        if !matches!(name, "TaskOutput" | "TaskStop" | "BashOutput" | "KillShell") {
            return None;
        }
        ["task_id", "shell_id", "bash_id"]
            .iter()
            .filter_map(|k| input.get(*k).and_then(Value::as_str))
            .find_map(|t| self.bg.get(t).cloned())
    }

    /// The session closed: background tasks without a notice have an unknown outcome.
    pub fn finalize_bg_unknown(&mut self) {
        if self.bg.finalize_unknown() {
            self.dirty = true;
        }
    }

    fn push_user_file(&mut self, ts: Option<i64>, path: String, kind: UserFileKind) {
        if !self.user_files.iter().any(|f| f.path == path) {
            self.user_files.push(UserFile { ts, path, kind });
        }
    }

    /// Everything the session announced as available, accumulated over all lines.
    pub fn inventory(&self) -> &InventoryDelta {
        &self.inventory
    }

    /// Every distinct tool call seen: `(tool_use id, name, input, ts)`. Keyed by
    /// tool_use id, so a line pushed twice is counted once.
    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &Value, Option<i64>)> {
        self.tool_inputs.iter().filter_map(|(id, (input, ts))| {
            let name = self.tool_names.get(id)?;
            Some((id.as_str(), name.as_str(), input, *ts))
        })
    }

    pub fn user_files(&self) -> &[UserFile] {
        &self.user_files
    }

    pub fn last_usage(&self) -> Option<&Usage> {
        self.last_usage.as_ref()
    }
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
    pub fn effort(&self) -> Option<&str> {
        self.effort.as_deref()
    }
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
    /// Plan usage limits reported inside the transcript (Codex).
    pub fn rate(&self) -> Option<RateInfo> {
        self.rate
    }
    /// Context window size reported inside the transcript (Codex).
    pub fn window(&self) -> Option<u64> {
        self.window
    }
    pub fn lines(&self) -> Option<(u64, u64)> {
        self.lines
    }
    pub fn compactions(&self) -> u32 {
        self.compactions
    }
    pub fn git_branch(&self) -> Option<&str> {
        self.git_branch.as_deref()
    }
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }
    pub fn pending_tools(&self) -> usize {
        self.pending.len()
    }
    pub fn last_tool_name(&self) -> Option<&str> {
        self.last_tool.as_deref()
    }
    pub fn first_ts(&self) -> Option<i64> {
        self.first_ts
    }
    pub fn tool_state(&self, tool_id: &str) -> Option<ToolState> {
        if self.pending.contains(tool_id) {
            return Some(ToolState::Running);
        }
        self.results.get(tool_id).map(|r| {
            if r.is_error {
                ToolState::Err
            } else {
                ToolState::Ok
            }
        })
    }
    pub fn has_tool(&self, tool_id: &str) -> bool {
        self.tool_names.contains_key(tool_id)
    }
    /// Tool id in this thread that launched the given link.
    pub fn tool_for_link(&self, link: &ThreadLink) -> Option<&str> {
        self.link_to_tool.get(link).map(String::as_str)
    }
    pub fn workflow_name_for_run(&self, run_id: &str) -> Option<&str> {
        let tool = self
            .link_to_tool
            .get(&ThreadLink::Workflow(run_id.to_string()))?;
        self.workflow_names.get(tool).map(String::as_str)
    }
}

fn workflow_meta_name(input: &Value) -> Option<String> {
    let script = input.get("script")?.as_str()?;
    let i = script.find("name:")?;
    let rest = script[i + 5..].trim_start();
    let q = rest.chars().next()?;
    if q != '\'' && q != '"' && q != '`' {
        return None;
    }
    let end = rest[1..].find(q)?;
    Some(rest[1..1 + end].to_string())
}

pub fn persisted_path(content: &str) -> Option<PathBuf> {
    if !content.starts_with("<persisted-output>") {
        return None;
    }
    let key = "Full output saved to: ";
    let i = content.find(key)?;
    let rest = &content[i + key.len()..];
    let end = rest.find('\n').unwrap_or(rest.len());
    Some(PathBuf::from(rest[..end].trim()))
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        let mut t: String = flat.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    } else {
        flat
    }
}

pub fn tool_summary(name: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str);
    let text = match name {
        "Bash" => s("command").map(str::to_string),
        "Read" | "Edit" | "Write" | "NotebookEdit" => {
            s("file_path").or(s("notebook_path")).map(str::to_string)
        }
        "Grep" | "Glob" => s("pattern").map(|p| match s("path") {
            Some(path) => format!("{p}  in {path}"),
            None => p.to_string(),
        }),
        "Agent" | "Task" => {
            s("description").map(|d| format!("{}: {d}", s("subagent_type").unwrap_or("agent")))
        }
        "Workflow" => Some(
            workflow_meta_name(input).map_or("workflow".to_string(), |n| format!("workflow {n}")),
        ),
        "Skill" => s("skill").map(str::to_string),
        "WebFetch" => s("url").map(str::to_string),
        "WebSearch" => s("query").map(str::to_string),
        "ToolSearch" => s("query").map(str::to_string),
        _ => None,
    };
    let text = text.unwrap_or_else(|| {
        if input.is_null() {
            String::new()
        } else {
            input.to_string()
        }
    });
    one_line(&text, 120)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::parse_line;

    /// Items without per-response `Turn` markers.
    fn content(t: &mut Thread) -> Vec<Item> {
        t.items()
            .iter()
            .filter(|i| !matches!(i, Item::Turn { .. }))
            .cloned()
            .collect()
    }

    fn thread(lines: &[&str]) -> Thread {
        let mut t = Thread::new();
        for l in lines {
            t.push(parse_line(l).expect(l));
        }
        t
    }

    #[test]
    fn active_branch_hides_abandoned() {
        let mut t = thread(&[
            r#"{"type":"user","uuid":"A","message":{"content":"a"}}"#,
            r#"{"type":"user","uuid":"B","parentUuid":"A","message":{"content":"b"}}"#,
            r#"{"type":"user","uuid":"C","parentUuid":"A","message":{"content":"c"}}"#,
        ]);
        let texts: Vec<_> = t
            .items()
            .iter()
            .map(|i| match i {
                Item::Prompt { text, .. } => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(texts, vec!["a", "c"]);
    }

    #[test]
    fn chain_passes_through_attachments() {
        let mut t = thread(&[
            r#"{"type":"user","uuid":"A","message":{"content":"a"}}"#,
            r#"{"type":"attachment","uuid":"X","parentUuid":"A","attachment":{"type":"file"}}"#,
            r#"{"type":"user","uuid":"B","parentUuid":"X","message":{"content":"b"}}"#,
        ]);
        assert_eq!(t.items().len(), 2);
    }

    #[test]
    fn merges_consecutive_thinking() {
        let mut t = thread(&[
            r#"{"type":"assistant","uuid":"A","message":{"id":"m1","content":[{"type":"thinking","thinking":""}]}}"#,
            r#"{"type":"assistant","uuid":"B","parentUuid":"A","message":{"id":"m1","usage":{"input_tokens":2,"output_tokens":9},"content":[{"type":"thinking","thinking":"x"}]}}"#,
        ]);
        let items = t.items().to_vec();
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], Item::Turn { usage: Some(u), .. } if u.output == 9));
        assert_eq!(
            items[1],
            Item::Thinking {
                ts: None,
                text: "x".into(),
                ms: None
            }
        );
    }

    #[test]
    fn collects_user_files() {
        let t = thread(&[
            r#"{"type":"user","uuid":"A","message":{"content":"xem [Image #1] [Image: source: /tmp/x/images/1.png] và [Image: source: /tmp/x/images/2.png]"}}"#,
            r#"{"type":"attachment","uuid":"B","parentUuid":"A","attachment":{"type":"directory","path":"/repo/JOBRunner","content":""}}"#,
        ]);
        let f: Vec<_> = t
            .user_files()
            .iter()
            .map(|f| (f.path.as_str(), f.kind))
            .collect();
        assert_eq!(
            f,
            vec![
                ("/tmp/x/images/1.png", UserFileKind::Image),
                ("/tmp/x/images/2.png", UserFileKind::Image),
                ("/repo/JOBRunner", UserFileKind::Directory)
            ]
        );
    }

    #[test]
    fn crosses_compact_boundary() {
        let mut t = thread(&[
            r#"{"type":"user","uuid":"A","message":{"content":"before"}}"#,
            r#"{"type":"system","subtype":"compact_boundary","uuid":"CB","parentUuid":null,"logicalParentUuid":"A","compactMetadata":{"preTokens":900,"postTokens":20}}"#,
            r#"{"type":"user","uuid":"S","parentUuid":"CB","isCompactSummary":true,"message":{"content":"summary"}}"#,
            r#"{"type":"user","uuid":"D","parentUuid":"S","message":{"content":"after"}}"#,
        ]);
        assert_eq!(t.compactions(), 1);
        let items = t.items().to_vec();
        assert_eq!(items.len(), 3);
        assert!(matches!(&items[0], Item::Prompt { text, .. } if text == "before"));
        assert!(matches!(
            items[1],
            Item::Compact {
                pre: 900,
                post: 20,
                ..
            }
        ));
        assert!(matches!(&items[2], Item::Prompt { text, .. } if text == "after"));
    }

    #[test]
    fn pairs_tool_result_with_duration_and_error() {
        let mut t = thread(&[
            r#"{"type":"assistant","uuid":"A","timestamp":"2026-11-25T00:00:00.000Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"false"}}]}}"#,
        ]);
        assert_eq!(t.pending_tools(), 1);
        assert!(matches!(
            &content(&mut t)[0],
            Item::Tool(ToolItem {
                state: ToolState::Running,
                ..
            })
        ));
        t.push(parse_line(r#"{"type":"user","uuid":"B","parentUuid":"A","timestamp":"2026-11-25T00:00:01.500Z","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"boom"}]}}"#).unwrap());
        assert_eq!(t.pending_tools(), 0);
        match &content(&mut t)[0] {
            Item::Tool(ti) => {
                assert_eq!(ti.state, ToolState::Err);
                assert_eq!(ti.ended.unwrap() - ti.started.unwrap(), 1500);
                assert_eq!(ti.result, "boom");
                assert_eq!(ti.summary, "false");
            }
            i => panic!("{i:?}"),
        }
    }

    #[test]
    fn extracts_persisted_path() {
        let c = "<persisted-output>\nOutput too large (35.5KB). Full output saved to: /x/tool-results/b.txt\n\nPreview";
        assert_eq!(
            persisted_path(c),
            Some(PathBuf::from("/x/tool-results/b.txt"))
        );
        assert_eq!(persisted_path("plain"), None);
    }

    #[test]
    fn links_agent_and_workflow() {
        let mut t = thread(&[
            r#"{"type":"assistant","uuid":"A","message":{"content":[{"type":"tool_use","id":"t1","name":"Agent","input":{"description":"Audit","subagent_type":"Explore"}},{"type":"tool_use","id":"t2","name":"Workflow","input":{"script":"export const meta = {\n  name: 'review-x',\n}"}}]}}"#,
            r#"{"type":"user","uuid":"B","parentUuid":"A","toolUseResult":{"agentId":"ag1","status":"async_launched"},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
            r#"{"type":"user","uuid":"C","parentUuid":"B","toolUseResult":{"runId":"wf_1","workflowName":"review-x"},"message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"}]}}"#,
        ]);
        assert_eq!(
            t.tool_for_link(&ThreadLink::Agent("ag1".into())),
            Some("t1")
        );
        assert_eq!(t.workflow_name_for_run("wf_1"), Some("review-x"));
        let items = content(&mut t);
        assert!(
            matches!(&items[0], Item::Tool(ToolItem { link: Some(ThreadLink::Agent(a)), summary, .. }) if a == "ag1" && summary == "Explore: Audit")
        );
        assert!(
            matches!(&items[1], Item::Tool(ToolItem { link: Some(ThreadLink::Workflow(w)), summary, .. }) if w == "wf_1" && summary == "workflow review-x")
        );
    }

    const NOTICE_A: &str = "<task-notification>\\n<task-id>bgA</task-id>\\n<tool-use-id>t1</tool-use-id>\\n<status>completed</status>\\n<summary>Background command completed (exit code 0)</summary>\\n</task-notification>";

    fn bg_lines() -> Vec<String> {
        vec![
            r#"{"type":"assistant","uuid":"A","timestamp":"2026-11-25T00:00:00.050Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"sleep 9\nx","description":"Sleeper","run_in_background":true}},{"type":"tool_use","id":"t2","name":"Agent","input":{"description":"Audit","subagent_type":"Explore","run_in_background":true}}]}}"#.into(),
            r#"{"type":"user","uuid":"B","parentUuid":"A","timestamp":"2026-11-25T00:00:00.100Z","toolUseResult":{"backgroundTaskId":"bgA"},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"running"}]}}"#.into(),
            r#"{"type":"user","uuid":"C","parentUuid":"B","timestamp":"2026-11-25T00:00:00.110Z","toolUseResult":{"agentId":"ag1","status":"async_launched"},"message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"}]}}"#.into(),
            r#"{"type":"assistant","uuid":"D","parentUuid":"C","timestamp":"2026-11-25T00:00:00.120Z","message":{"content":[{"type":"tool_use","id":"t3","name":"Yield","input":{}}]}}"#.into(),
        ]
    }

    fn notice_line(uuid: &str, parent: &str, ts: &str, body: &str) -> String {
        format!(
            r#"{{"type":"attachment","uuid":"{uuid}","parentUuid":"{parent}","timestamp":"{ts}","attachment":{{"type":"queued_command","prompt":"{body}"}}}}"#
        )
    }

    fn tool<'a>(items: &'a [Item], id: &str) -> &'a ToolItem {
        items
            .iter()
            .find_map(|i| match i {
                Item::Tool(t) if t.id == id => Some(t),
                _ => None,
            })
            .unwrap()
    }

    fn thread_from(lines: &[String]) -> Thread {
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        thread(&refs)
    }

    #[test]
    fn yield_waits_on_agents_and_shells() {
        let mut lines = bg_lines();
        lines.push(notice_line("E", "D", "2026-11-25T00:00:00.130Z", NOTICE_A));
        let mut t = thread_from(&lines);
        let items = content(&mut t);
        let y = tool(&items, "t3");
        let ids: Vec<_> = y.waits.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, vec!["bgA", "ag1"]);
        assert_eq!(y.woke_by.as_deref(), Some("bgA"));
        let b = tool(&items, "t1").bg.as_ref().unwrap();
        assert_eq!(b.state, BgState::Completed);
        assert_eq!(b.label, "Sleeper");
        assert!(b.summary.as_deref().unwrap().contains("completed"));
        let a = tool(&items, "t2").bg.as_ref().unwrap();
        assert_eq!((a.kind, a.state), (BgKind::Agent, BgState::Running));
        assert_eq!(a.label, "Audit");
    }

    #[test]
    fn bg_task_without_notice_is_unknown_after_session_end() {
        let mut t = thread_from(&bg_lines());
        assert_eq!(
            tool(&content(&mut t), "t1").bg.as_ref().unwrap().state,
            BgState::Running
        );
        t.finalize_bg_unknown();
        let items = content(&mut t);
        assert_eq!(
            tool(&items, "t1").bg.as_ref().unwrap().state,
            BgState::Unknown
        );
    }

    #[test]
    fn task_output_links_to_target_task() {
        let mut lines = bg_lines();
        lines.push(r#"{"type":"assistant","uuid":"F","parentUuid":"D","message":{"content":[{"type":"tool_use","id":"t4","name":"TaskOutput","input":{"task_id":"bgA"}}]}}"#.into());
        let mut t = thread_from(&lines);
        let items = content(&mut t);
        assert_eq!(tool(&items, "t4").bg.as_ref().unwrap().id, "bgA");
    }

    #[test]
    fn unmatched_notice_stays_visible_matched_does_not_duplicate() {
        let mut lines = bg_lines();
        lines.push(notice_line("E", "D", "2026-11-25T00:00:00.130Z", NOTICE_A));
        let other = NOTICE_A.replace("bgA", "zzz").replace("t1", "nope");
        lines.push(notice_line("F", "E", "2026-11-25T00:00:00.140Z", &other));
        let mut t = thread_from(&lines);
        let sys: Vec<_> = content(&mut t)
            .into_iter()
            .filter_map(|i| match i {
                Item::System { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(sys.len(), 1);
        assert!(sys[0].contains("completed"));
    }

    #[test]
    fn real_shape_agent_and_workflow_notices_complete_tasks() {
        // Real transcripts: agent notice task-id == agentId; workflow notice task-id == taskId
        // (not runId) and both carry the launching tool_use id.
        let agent_n = "<task-notification>\\n<task-id>ag1</task-id>\\n<tool-use-id>t2</tool-use-id>\\n<status>completed</status>\\n<summary>Agent done</summary>\\n</task-notification>";
        let wf_n = "<task-notification>\\n<task-id>wnb6</task-id>\\n<tool-use-id>t5</tool-use-id>\\n<status>completed</status>\\n<summary>Workflow done</summary>\\n</task-notification>";
        let mut lines = bg_lines();
        lines.push(r#"{"type":"assistant","uuid":"W","parentUuid":"D","timestamp":"2026-11-25T00:00:00.121Z","message":{"content":[{"type":"tool_use","id":"t5","name":"Workflow","input":{}}]}}"#.into());
        lines.push(r#"{"type":"user","uuid":"X","parentUuid":"W","timestamp":"2026-11-25T00:00:00.122Z","toolUseResult":{"status":"async_launched","taskId":"wnb6","runId":"wf_1","workflowName":"rev"},"message":{"content":[{"type":"tool_result","tool_use_id":"t5","content":"ok"}]}}"#.into());
        lines.push(notice_line("E", "X", "2026-11-25T00:00:00.130Z", agent_n));
        lines.push(notice_line("F", "E", "2026-11-25T00:00:00.140Z", wf_n));
        let mut t = thread_from(&lines);
        let items = content(&mut t);
        let a = tool(&items, "t2").bg.as_ref().unwrap();
        assert_eq!(a.state, BgState::Completed);
        let w = tool(&items, "t5").bg.as_ref().unwrap();
        assert_eq!((w.id.as_str(), w.state), ("wnb6", BgState::Completed));
        assert!(items.iter().all(|i| !matches!(i, Item::System { .. })));
    }

    #[test]
    fn unknown_task_not_waited_on_by_later_yield() {
        let mut lines = bg_lines();
        let mut t = thread_from(&lines);
        t.finalize_bg_unknown();
        lines.clear();
        t.push(parse_line(r#"{"type":"assistant","uuid":"Y2","parentUuid":"D","timestamp":"2026-11-25T00:00:01.000Z","message":{"content":[{"type":"tool_use","id":"t9","name":"Yield","input":{}}]}}"#).unwrap());
        let items = content(&mut t);
        assert!(tool(&items, "t9").waits.is_empty());
    }

    #[test]
    fn tool_summary_per_tool() {
        use serde_json::json;
        assert_eq!(
            tool_summary("Read", &json!({"file_path":"/a/b.rs"})),
            "/a/b.rs"
        );
        assert_eq!(tool_summary("Grep", &json!({"pattern":"foo"})), "foo");
        assert_eq!(
            tool_summary("Bash", &json!({"command":"ls\n  -la"})),
            "ls -la"
        );
        assert!(
            tool_summary("Zzz", &json!({"k":"x".repeat(500)}))
                .chars()
                .count()
                <= 120
        );
    }

    #[test]
    fn inventory_delta_accumulates_and_removes() {
        let att = |u: &str, p: &str, body: &str| {
            format!(r#"{{"type":"attachment","uuid":"{u}","parentUuid":"{p}","attachment":{body}}}"#)
        };
        let l1 = att("I1", "A", r#"{"type":"mcp_instructions_delta","addedNames":["cde","gh"]}"#);
        let l2 = att("I2", "I1", r#"{"type":"mcp_instructions_delta","addedNames":["x"],"removedNames":["cde"]}"#);
        let l3 = att("I3", "I2", r#"{"type":"agent_listing_delta","addedTypes":["Explore"]}"#);
        let l4 = att("I4", "I3", r#"{"type":"skill_listing","content":"- s1: d\n- s2: d"}"#);
        let a = r#"{"type":"user","uuid":"A","message":{"content":"a"}}"#.to_string();
        let b = r#"{"type":"user","uuid":"B","parentUuid":"I4","message":{"content":"b"}}"#.to_string();
        let mut t = thread(&[&a, &l1, &l2, &l1, &l3, &l4, &b]); // l1 replayed after l2 removed "cde"
        let inv = t.inventory();
        assert_eq!(inv.mcp, ["gh", "x"]);
        assert_eq!(inv.agents, ["Explore"]);
        assert_eq!(inv.skills, ["s1", "s2"]);
        assert_eq!(t.items().len(), 2); // attachments stay in the chain, not shown
    }
}
