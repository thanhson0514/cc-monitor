//! UI-side model: sessions → thread tree, fed by `discovery::Msg`.

use crate::discovery::{AgentMeta, Key, LiveInfo, Msg, Tool};
use crate::thread::{Thread, ThreadLink, ToolState};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// A session counts as active if its main file was written within this window.
pub const ACTIVE_MS: i64 = 600_000;
/// Main thread is "running" if written within this window or has pending tools.
const MAIN_BUSY_MS: i64 = 20_000;
/// An agent with no pending tool and no writes for this long is considered done.
const AGENT_IDLE_DONE_MS: i64 = 30_000;
const END_TOOLS: &[&str] = &["SubagentHandback", "StructuredOutput"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Done,
    Error,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeId {
    Main(String),
    Agent(String, String),
    Workflow(String, String),
}

impl NodeId {
    pub fn session(&self) -> &str {
        match self {
            NodeId::Main(s) | NodeId::Agent(s, _) | NodeId::Workflow(s, _) => s,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    Main,
    Agent { agent_type: String },
    Workflow,
    WorkflowAgent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TreeRow {
    pub depth: u16,
    pub id: NodeId,
    pub label: String,
    pub status: Status,
    pub kind: NodeKind,
    /// For workflow groups: (running, total) agents.
    pub progress: Option<(usize, usize)>,
    /// For sessions: the running `claude` process, if the session is open.
    pub live: Option<LiveInfo>,
    /// For sessions: last write time (ms).
    pub mtime: Option<i64>,
    /// For sessions: which coding agent.
    pub tool: Option<Tool>,
    /// For sessions: open now (Claude: live process; Codex: written recently).
    pub open: bool,
}

pub struct AgentState {
    pub thread: Thread,
    pub meta: AgentMeta,
    pub run_id: Option<String>,
    pub mtime: i64,
}

pub struct Session {
    pub project: String,
    pub main: Thread,
    pub mtime: i64,
    pub agents: BTreeMap<String, AgentState>,
    pub live: Option<LiveInfo>,
    pub tool: Tool,
    /// Main transcript file.
    pub path: PathBuf,
}

#[derive(Default)]
pub struct Store {
    pub sessions: HashMap<String, Session>,
    pub ready: bool,
    /// Last live-session map from the worker (also applied to sessions created later).
    live: HashMap<String, LiveInfo>,
}

impl Store {
    /// Apply one worker message. Returns true if the UI should redraw.
    pub fn apply(&mut self, m: Msg) -> bool {
        match m {
            Msg::Session {
                sid,
                project,
                tool,
                path,
            } => {
                self.sessions.entry(sid).or_insert(Session {
                    tool,
                    path,
                    project,
                    main: Thread::new(),
                    mtime: 0,
                    agents: BTreeMap::new(),
                    live: None,
                });
                for (sid, s) in self.sessions.iter_mut() {
                    if s.live.is_none() {
                        s.live = self.live.get(sid).cloned();
                    }
                }
            }
            Msg::Agent {
                sid,
                agent_id,
                meta,
                run_id,
            } => {
                if let Some(s) = self.sessions.get_mut(&sid) {
                    s.agents.entry(agent_id).or_insert(AgentState {
                        thread: Thread::new(),
                        meta,
                        run_id,
                        mtime: 0,
                    });
                }
            }
            Msg::Events {
                key,
                reset,
                events,
                mtime_ms,
            } => {
                let Some((thread, mtime)) = self.thread_and_mtime(&key) else {
                    return false;
                };
                if reset {
                    thread.clear();
                }
                for e in events {
                    thread.push(e);
                }
                *mtime = mtime_ms;
            }
            Msg::Gone { sid } => {
                self.sessions.remove(&sid);
            }
            Msg::Live(map) => {
                for (sid, s) in self.sessions.iter_mut() {
                    s.live = map.get(sid).cloned();
                }
                self.live = map;
            }
            Msg::Ready => self.ready = true,
        }
        true
    }

    fn thread_and_mtime(&mut self, key: &Key) -> Option<(&mut Thread, &mut i64)> {
        match key {
            Key::Main(sid) => self
                .sessions
                .get_mut(sid)
                .map(|s| (&mut s.main, &mut s.mtime)),
            Key::Agent(sid, a) => {
                let ag = self.sessions.get_mut(sid)?.agents.get_mut(a)?;
                Some((&mut ag.thread, &mut ag.mtime))
            }
        }
    }

    pub fn thread_mut(&mut self, id: &NodeId) -> Option<&mut Thread> {
        match id {
            NodeId::Main(s) => self.sessions.get_mut(s).map(|s| &mut s.main),
            NodeId::Agent(s, a) => self
                .sessions
                .get_mut(s)?
                .agents
                .get_mut(a)
                .map(|a| &mut a.thread),
            NodeId::Workflow(..) => None,
        }
    }

    pub fn thread(&self, id: &NodeId) -> Option<&Thread> {
        match id {
            NodeId::Main(s) => self.sessions.get(s).map(|s| &s.main),
            NodeId::Agent(s, a) => self.sessions.get(s)?.agents.get(a).map(|a| &a.thread),
            NodeId::Workflow(..) => None,
        }
    }

    /// Node to jump to for a link found in a timeline tool item.
    pub fn node_for_link(&self, sid: &str, link: &ThreadLink) -> Option<NodeId> {
        let s = self.sessions.get(sid)?;
        match link {
            // Codex `spawn_agent` results name the child by its agent path.
            ThreadLink::Agent(a) if a.starts_with("path:") => s
                .agents
                .iter()
                .find(|(_, x)| x.meta.agent_path.as_deref() == Some(&a[5..]))
                .map(|(id, _)| NodeId::Agent(sid.into(), id.clone())),
            ThreadLink::Agent(a) => s
                .agents
                .contains_key(a)
                .then(|| NodeId::Agent(sid.into(), a.clone())),
            ThreadLink::Workflow(w) => s
                .agents
                .values()
                .any(|a| a.run_id.as_deref() == Some(w))
                .then(|| NodeId::Workflow(sid.into(), w.clone())),
        }
    }

    pub fn session_label(&self, sid: &str) -> String {
        let Some(s) = self.sessions.get(sid) else {
            return sid.to_string();
        };
        let base = s
            .main
            .cwd()
            .and_then(crate::platform::basename)
            .map(str::to_string)
            .unwrap_or_else(|| s.project.trim_start_matches('-').to_string());
        match s.main.title() {
            Some(t) => format!("{base} · {t}"),
            None => base,
        }
    }

    pub fn session_status(&self, sid: &str, now: i64) -> Status {
        let Some(s) = self.sessions.get(sid) else {
            return Status::Idle;
        };
        if let Some(st) = s.live.as_ref().and_then(|l| l.status.as_deref()) {
            return if st == "busy" {
                Status::Running
            } else {
                Status::Idle
            };
        }
        let running =
            s.main.pending_tools() > 0 && now - s.mtime < ACTIVE_MS || now - s.mtime < MAIN_BUSY_MS;
        if running {
            Status::Running
        } else {
            Status::Idle
        }
    }

    fn agent_status(&self, sid: &str, a: &AgentState, now: i64) -> Status {
        let s = &self.sessions[sid];
        if let Some(tid) = &a.meta.tool_use_id {
            let parent = std::iter::once(&s.main)
                .chain(s.agents.values().map(|x| &x.thread))
                .find(|t| t.has_tool(tid));
            if parent.and_then(|p| p.tool_state(tid)) == Some(ToolState::Err) {
                return Status::Error;
            }
        }
        let t = &a.thread;
        let idle = now - a.mtime;
        if t.pending_tools() == 0 {
            if t.last_tool_name().is_some_and(|n| END_TOOLS.contains(&n))
                || idle > AGENT_IDLE_DONE_MS
            {
                return Status::Done;
            }
        }
        if idle < ACTIVE_MS {
            Status::Running
        } else {
            Status::Idle
        }
    }

    /// Flattened tree: sessions (most recently written first) → agents/workflows.
    pub fn tree(&self, now: i64) -> Vec<TreeRow> {
        let mut sids: Vec<&String> = self.sessions.keys().collect();
        sids.sort_by_key(|s| std::cmp::Reverse(self.sessions[*s].mtime));
        let mut rows = Vec::new();
        for sid in sids {
            let status = self.session_status(sid, now);
            rows.push(TreeRow {
                depth: 0,
                id: NodeId::Main(sid.clone()),
                label: self.session_label(sid),
                status,
                kind: NodeKind::Main,
                progress: None,
                live: self.sessions[sid].live.clone(),
                mtime: Some(self.sessions[sid].mtime),
                tool: Some(self.sessions[sid].tool),
                open: self.sessions[sid].live.is_some()
                    || (self.sessions[sid].tool == Tool::Codex
                        && now - self.sessions[sid].mtime < ACTIVE_MS),
            });
            self.push_children(sid, None, 1, now, &mut rows);
        }
        rows
    }

    /// Owner of a child: `None` = main thread, `Some(agent_id)` = that agent.
    fn owner_of_tool(&self, sid: &str, tool_id: &str) -> Option<Option<String>> {
        let s = &self.sessions[sid];
        if s.main.has_tool(tool_id) {
            return Some(None);
        }
        s.agents
            .iter()
            .find(|(_, a)| a.thread.has_tool(tool_id))
            .map(|(id, _)| Some(id.clone()))
    }

    fn owner_of_run(&self, sid: &str, run_id: &str) -> Option<String> {
        let s = &self.sessions[sid];
        let link = ThreadLink::Workflow(run_id.to_string());
        if s.main.tool_for_link(&link).is_some() {
            return None;
        }
        s.agents
            .iter()
            .find(|(_, a)| a.thread.tool_for_link(&link).is_some())
            .map(|(id, _)| id.clone())
    }

    fn push_children(
        &self,
        sid: &str,
        owner: Option<&str>,
        depth: u16,
        now: i64,
        rows: &mut Vec<TreeRow>,
    ) {
        if depth > 8 {
            return;
        }
        let s = &self.sessions[sid];
        // Plain subagents owned by `owner`.
        let mut kids: Vec<(&String, &AgentState)> = s
            .agents
            .iter()
            .filter(|(_, a)| a.run_id.is_none())
            .filter(|(_, a)| {
                let o = a
                    .meta
                    .tool_use_id
                    .as_deref()
                    .and_then(|t| self.owner_of_tool(sid, t))
                    .unwrap_or(None);
                o.as_deref() == owner
            })
            .collect();
        kids.sort_by_key(|(_, a)| a.thread.first_ts().unwrap_or(i64::MAX));
        for (aid, a) in kids {
            if owner == Some(aid.as_str()) {
                continue;
            }
            rows.push(TreeRow {
                depth,
                id: NodeId::Agent(sid.into(), aid.clone()),
                label: a
                    .meta
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("agent {aid}")),
                status: self.agent_status(sid, a, now),
                kind: NodeKind::Agent {
                    agent_type: a.meta.agent_type.clone().unwrap_or_else(|| "agent".into()),
                },
                progress: None,
                live: None,
                mtime: None,
                tool: None,
                open: false,
            });
            self.push_children(sid, Some(aid), depth + 1, now, rows);
        }
        // Workflow groups owned by `owner`.
        let mut runs: Vec<&str> = s
            .agents
            .values()
            .filter_map(|a| a.run_id.as_deref())
            .collect();
        runs.sort();
        runs.dedup();
        let mut groups: Vec<(&str, i64)> = runs
            .into_iter()
            .filter(|r| self.owner_of_run(sid, r).as_deref() == owner)
            .map(|r| {
                let first = s
                    .agents
                    .values()
                    .filter(|a| a.run_id.as_deref() == Some(r))
                    .filter_map(|a| a.thread.first_ts())
                    .min()
                    .unwrap_or(i64::MAX);
                (r, first)
            })
            .collect();
        groups.sort_by_key(|g| g.1);
        for (run, _) in groups {
            let mut members: Vec<(&String, &AgentState)> = s
                .agents
                .iter()
                .filter(|(_, a)| a.run_id.as_deref() == Some(run))
                .collect();
            members.sort_by_key(|(_, a)| a.thread.first_ts().unwrap_or(i64::MAX));
            let statuses: Vec<Status> = members
                .iter()
                .map(|(_, a)| self.agent_status(sid, a, now))
                .collect();
            let running = statuses.iter().filter(|s| **s == Status::Running).count();
            let status = if running > 0 {
                Status::Running
            } else if statuses.iter().any(|s| *s == Status::Error) {
                Status::Error
            } else if statuses.iter().all(|s| *s == Status::Done) {
                Status::Done
            } else {
                Status::Idle
            };
            let name = std::iter::once(&s.main)
                .chain(s.agents.values().map(|a| &a.thread))
                .find_map(|t| t.workflow_name_for_run(run))
                .unwrap_or(run);
            rows.push(TreeRow {
                depth,
                id: NodeId::Workflow(sid.into(), run.into()),
                label: format!("wf {name}"),
                status,
                kind: NodeKind::Workflow,
                progress: Some((members.len() - running, members.len())),
                live: None,
                mtime: None,
                tool: None,
                open: false,
            });
            for ((aid, a), st) in members.into_iter().zip(statuses) {
                let mut label = a
                    .meta
                    .description
                    .clone()
                    .unwrap_or_else(|| format!("agent {aid}"));
                if let Some(p) = &a.meta.workflow_phase {
                    label = format!("[{p}] {label}");
                }
                rows.push(TreeRow {
                    depth: depth + 1,
                    id: NodeId::Agent(sid.into(), aid.clone()),
                    label,
                    status: st,
                    kind: NodeKind::WorkflowAgent,
                    progress: None,
                    live: None,
                    mtime: None,
                    tool: None,
                    open: false,
                });
            }
        }
    }

    /// Number of agents currently running across all sessions.
    pub fn running_agents(&self, now: i64) -> usize {
        self.sessions
            .iter()
            .map(|(sid, s)| {
                s.agents
                    .values()
                    .filter(|a| self.agent_status(sid, a, now) == Status::Running)
                    .count()
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::parse_line;

    fn ev(l: &str) -> crate::event::Event {
        parse_line(l).unwrap()
    }

    fn store() -> Store {
        let mut st = Store::default();
        st.apply(Msg::Session {
            sid: "s".into(),
            project: "-p".into(),
            tool: crate::discovery::Tool::Claude,
            path: Default::default(),
        });
        st.apply(Msg::Events {
            key: Key::Main("s".into()),
            reset: false,
            mtime_ms: 1000,
            events: vec![
                ev(r#"{"type":"assistant","uuid":"A","message":{"content":[{"type":"tool_use","id":"t1","name":"Agent","input":{}},{"type":"tool_use","id":"t2","name":"Workflow","input":{}}]}}"#),
                ev(r#"{"type":"user","uuid":"B","parentUuid":"A","toolUseResult":{"runId":"wf_1","workflowName":"rev"},"message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"}]}}"#),
            ],
        });
        st.apply(Msg::Agent {
            sid: "s".into(),
            agent_id: "x".into(),
            meta: AgentMeta {
                tool_use_id: Some("t1".into()),
                description: Some("Audit".into()),
                ..Default::default()
            },
            run_id: None,
        });
        st.apply(Msg::Agent {
            sid: "s".into(),
            agent_id: "w1".into(),
            meta: AgentMeta::default(),
            run_id: Some("wf_1".into()),
        });
        st
    }

    #[test]
    fn links_subagent_by_tool_use_id_and_groups_workflow() {
        let st = store();
        let rows = st.tree(1000);
        let ids: Vec<(u16, NodeId)> = rows.iter().map(|r| (r.depth, r.id.clone())).collect();
        assert_eq!(
            ids,
            vec![
                (0, NodeId::Main("s".into())),
                (1, NodeId::Agent("s".into(), "x".into())),
                (1, NodeId::Workflow("s".into(), "wf_1".into())),
                (2, NodeId::Agent("s".into(), "w1".into())),
            ]
        );
        assert_eq!(rows[2].label, "wf rev");
    }

    #[test]
    fn status_done_after_handback() {
        let mut st = store();
        st.apply(Msg::Events {
            key: Key::Agent("s".into(), "x".into()),
            reset: false,
            mtime_ms: 1000,
            events: vec![
                ev(r#"{"type":"assistant","uuid":"a","message":{"content":[{"type":"tool_use","id":"h","name":"SubagentHandback","input":{}}]}}"#),
            ],
        });
        assert_eq!(st.tree(1500)[1].status, Status::Running);
        st.apply(Msg::Events {
            key: Key::Agent("s".into(), "x".into()),
            reset: false,
            mtime_ms: 1200,
            events: vec![ev(r#"{"type":"user","uuid":"b","parentUuid":"a","message":{"content":[{"type":"tool_result","tool_use_id":"h","content":"ok"}]}}"#)],
        });
        assert_eq!(st.tree(1500)[1].status, Status::Done);
    }

    #[test]
    fn live_before_session_is_applied() {
        let mut st = Store::default();
        let info = LiveInfo {
            pid: 1,
            session_id: "s".into(),
            status: Some("busy".into()),
            name: None,
            tmux: None,
        };
        st.apply(Msg::Live(HashMap::from([("s".to_string(), info)])));
        st.apply(Msg::Session {
            sid: "s".into(),
            project: "-p".into(),
            tool: crate::discovery::Tool::Claude,
            path: Default::default(),
        });
        let rows = st.tree(0);
        assert!(rows[0].live.is_some());
        assert_eq!(rows[0].status, Status::Running);
    }

    #[test]
    fn gone_removes_session() {
        let mut st = store();
        st.apply(Msg::Gone { sid: "s".into() });
        assert!(st.tree(0).is_empty());
    }
}
