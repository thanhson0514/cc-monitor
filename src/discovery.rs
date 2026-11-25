//! Background worker: discovers sessions/subagents/workflows under `~/.claude/projects`,
//! tails their JSONL files and sends parsed events to the UI thread.
//!
//! Change detection is a cheap `stat` of the tracked files every `POLL` (no inotify:
//! `~/.claude/projects` holds thousands of directories, recursive inotify would hit
//! the per-user watch limit). Directory discovery runs every `RESCAN`.

use crate::codex::{parse_meta, CodexMeta, CodexParser};
use crate::config::SharedConfig;
use crate::event::{parse_line, Event};
use crate::tail::{Tail, TailRead};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const POLL: Duration = Duration::from_millis(200);
pub const RESCAN: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct ScanOpts {
    pub session: Option<String>,
    /// Show sessions modified within this many seconds.
    pub window_secs: u64,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AgentMeta {
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub tool_use_id: Option<String>,
    pub workflow_phase: Option<String>,
    /// Codex sub-agent path (`/root/<task>`), matched against `spawn_agent` results.
    pub agent_path: Option<String>,
}

/// A running `claude` process, from `~/.claude/sessions/<pid>.json` (written by Claude Code).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveInfo {
    pub pid: u32,
    pub session_id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tmux: Option<String>,
}

/// Sessions with a live `claude` process (registry entry whose pid is still running).
pub fn read_live(dir: &Path) -> HashMap<String, LiveInfo> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let info = std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str::<LiveInfo>(&s).ok());
        if let Some(info) = info {
            if crate::platform::pid_alive(info.pid) {
                out.insert(info.session_id.clone(), info);
            }
        }
    }
    out
}

/// Session ids the UI asked to track even if old (opened from the history list).
pub type Pinned = Arc<Mutex<HashSet<String>>>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    Main(String),
    Agent(String, String),
}

#[derive(Debug)]
pub enum Msg {
    Session {
        sid: String,
        project: String,
        tool: Tool,
        /// Main transcript file.
        path: PathBuf,
    },
    Agent {
        sid: String,
        agent_id: String,
        meta: AgentMeta,
        run_id: Option<String>,
    },
    Events {
        key: Key,
        reset: bool,
        events: Vec<Event>,
        mtime_ms: i64,
    },
    Gone {
        sid: String,
    },
    /// Current set of live sessions (sent when it changes).
    Live(HashMap<String, LiveInfo>),
    /// Initial scan finished (all known files read once).
    Ready,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn mtime_ms(p: &Path) -> Option<i64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(m.duration_since(UNIX_EPOCH).ok()?.as_millis() as i64)
}

/// `/a/b/.c` → `-a-b--c` (how Claude Code names project directories).
#[cfg(test)]
pub fn project_key(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

pub fn projects_root() -> PathBuf {
    crate::platform::claude_home().join("projects")
}

/// Which coding agent wrote a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Claude,
    Codex,
}

impl Tool {
    pub fn as_str(self) -> &'static str {
        match self {
            Tool::Claude => "claude",
            Tool::Codex => "codex",
        }
    }
}

struct Tracked {
    tail: Tail,
    /// Present for Codex rollouts (they need per-file parser state).
    codex: Option<CodexParser>,
}

impl Tracked {
    fn claude(p: PathBuf) -> Self {
        Self {
            tail: Tail::new(p),
            codex: None,
        }
    }
    fn codex(p: PathBuf) -> Self {
        Self {
            tail: Tail::new(p),
            codex: Some(CodexParser::new()),
        }
    }
}

/// Codex session id from a rollout file name (`rollout-<time>-<uuid>.jsonl`).
pub fn codex_id_from_name(stem: &str) -> Option<String> {
    (stem.starts_with("rollout-") && stem.len() > 36 + 9)
        .then(|| stem[stem.len() - 36..].to_string())
}

fn first_line(p: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let mut line = String::new();
    BufReader::new(std::fs::File::open(p).ok()?)
        .read_line(&mut line)
        .ok()?;
    Some(line)
}

/// All rollout files under `~/.codex/sessions` (YYYY/MM/DD/rollout-*.jsonl).
pub fn codex_rollouts(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                out.push(p);
            }
        }
    }
    out
}

/// Codex thread names from `~/.codex/session_index.jsonl` (last entry per id wins).
pub fn codex_titles(index: &Path) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for l in std::fs::read_to_string(index).unwrap_or_default().lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
            if let (Some(id), Some(n)) = (
                v.get("id").and_then(|x| x.as_str()),
                v.get("thread_name").and_then(|x| x.as_str()),
            ) {
                m.insert(id.to_string(), n.to_string());
            }
        }
    }
    m
}

pub struct Worker {
    root: PathBuf,
    codex_root: Option<PathBuf>,
    opts: ScanOpts,
    tx: Sender<Msg>,
    sessions: HashSet<String>,
    session_dirs: HashMap<String, PathBuf>,
    files: HashMap<Key, Tracked>,
    pinned: Pinned,
    live: HashMap<String, LiveInfo>,
    config: SharedConfig,
    codex_meta: HashMap<PathBuf, Option<CodexMeta>>,
    codex_titles: HashMap<String, String>,
    sent_titles: HashMap<String, String>,
}

impl Worker {
    pub fn new(root: PathBuf, opts: ScanOpts, tx: Sender<Msg>) -> Self {
        Self {
            root,
            codex_root: None,
            opts,
            tx,
            sessions: HashSet::new(),
            session_dirs: HashMap::new(),
            files: HashMap::new(),
            pinned: Pinned::default(),
            live: HashMap::new(),
            config: SharedConfig::default(),
            codex_meta: HashMap::new(),
            codex_titles: HashMap::new(),
            sent_titles: HashMap::new(),
        }
    }

    pub fn with_pinned(mut self, pinned: Pinned) -> Self {
        self.pinned = pinned;
        self
    }

    /// Also monitor Codex; `codex_home` is `~/.codex`.
    pub fn with_codex(mut self, codex_home: PathBuf) -> Self {
        self.codex_root = Some(codex_home);
        self
    }

    pub fn with_config(mut self, config: SharedConfig) -> Self {
        self.config = config;
        self
    }

    fn sessions_dir(&self) -> PathBuf {
        self.root
            .parent()
            .map(|p| p.join("sessions"))
            .unwrap_or_default()
    }

    /// Run forever (until the receiver is dropped).
    pub fn run(mut self) {
        if self.rescan().is_err() || self.poll().is_err() || self.tx.send(Msg::Ready).is_err() {
            return;
        }
        let mut last_scan = Instant::now();
        let state = |w: &Self| {
            (
                w.pinned.lock().map(|p| p.len()).unwrap_or(0),
                w.config
                    .lock()
                    .map(|c| (c.claude, c.codex))
                    .unwrap_or((true, true)),
            )
        };
        let mut last_state = state(&self);
        loop {
            std::thread::sleep(POLL);
            let now_state = state(&self);
            if last_scan.elapsed() >= RESCAN || now_state != last_state {
                last_state = now_state;
                last_scan = Instant::now();
                if self.rescan().is_err() {
                    return;
                }
            }
            if self.poll().is_err() {
                return;
            }
        }
    }

    fn send(&self, m: Msg) -> Result<(), ()> {
        self.tx.send(m).map_err(|_| ())
    }

    fn wanted(&self, sid: &str, path: &Path, pinned: &HashSet<String>, now: i64) -> bool {
        match &self.opts.session {
            Some(s) => sid.starts_with(s.as_str()),
            None => {
                self.live.contains_key(sid)
                    || pinned.contains(sid)
                    || mtime_ms(path)
                        .is_some_and(|m| now - m <= self.opts.window_secs as i64 * 1000)
            }
        }
    }

    pub fn rescan(&mut self) -> Result<(), ()> {
        let now = now_ms();
        let cfg = self.config.lock().map(|c| *c).unwrap_or_default();
        let mut seen = HashSet::new();
        let pinned = self.pinned.lock().map(|p| p.clone()).unwrap_or_default();
        if cfg.claude {
            let live = read_live(&self.sessions_dir());
            if live != self.live {
                self.live = live.clone();
                self.send(Msg::Live(live))?;
            }
            self.scan_claude(now, &pinned, &mut seen)?;
        }
        if cfg.codex {
            self.scan_codex(now, &pinned, &mut seen)?;
        }
        let gone: Vec<String> = self.sessions.difference(&seen).cloned().collect();
        for sid in gone {
            self.sessions.remove(&sid);
            self.session_dirs.remove(&sid);
            self.files.retain(|k, _| match k {
                Key::Main(s) | Key::Agent(s, _) => s != &sid,
            });
            self.send(Msg::Gone { sid })?;
        }
        let dirs: Vec<(String, PathBuf)> = self
            .session_dirs
            .iter()
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
        for (sid, dir) in dirs {
            self.discover_agents(&sid, &dir.join("subagents"), None)?;
            let wf_root = dir.join("subagents").join("workflows");
            if let Ok(wfs) = std::fs::read_dir(&wf_root) {
                for wf in wfs.flatten() {
                    let run_id = wf.file_name().to_string_lossy().into_owned();
                    self.discover_agents(&sid, &wf.path(), Some(run_id))?;
                }
            }
        }
        Ok(())
    }

    fn scan_claude(
        &mut self,
        now: i64,
        pinned: &HashSet<String>,
        seen: &mut HashSet<String>,
    ) -> Result<(), ()> {
        let Ok(projects) = std::fs::read_dir(&self.root) else {
            return Ok(());
        };
        for proj in projects.flatten() {
            let pdir = proj.path();
            if !pdir.is_dir() {
                continue;
            }
            let project = proj.file_name().to_string_lossy().into_owned();
            let Ok(entries) = std::fs::read_dir(&pdir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(sid) = p.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                    continue;
                };
                if !self.wanted(&sid, &p, pinned, now) {
                    continue;
                }
                seen.insert(sid.clone());
                if self.sessions.insert(sid.clone()) {
                    self.send(Msg::Session {
                        sid: sid.clone(),
                        project: project.clone(),
                        tool: Tool::Claude,
                        path: p.clone(),
                    })?;
                    self.files
                        .insert(Key::Main(sid.clone()), Tracked::claude(p.clone()));
                    self.session_dirs.insert(sid.clone(), pdir.join(&sid));
                }
            }
        }
        Ok(())
    }

    fn scan_codex(
        &mut self,
        now: i64,
        pinned: &HashSet<String>,
        seen: &mut HashSet<String>,
    ) -> Result<(), ()> {
        let Some(home) = self.codex_root.clone() else {
            return Ok(());
        };
        let files = codex_rollouts(&home.join("sessions"));
        let by_id: HashMap<String, PathBuf> = files
            .iter()
            .filter_map(|p| {
                Some((
                    codex_id_from_name(&p.file_stem()?.to_string_lossy())?,
                    p.clone(),
                ))
            })
            .collect();
        // Wanted rollouts, plus the parents of wanted sub-agents.
        let mut want: Vec<(String, PathBuf)> = by_id
            .iter()
            .filter(|(id, p)| self.wanted(id, p, pinned, now))
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
        let mut i = 0;
        while i < want.len() {
            let (_, p) = want[i].clone();
            let meta = self
                .codex_meta
                .entry(p.clone())
                .or_insert_with(|| first_line(&p).as_deref().and_then(parse_meta))
                .clone();
            if let Some(parent) = meta.and_then(|m| m.parent_id) {
                if !want.iter().any(|(id, _)| id == &parent) {
                    if let Some(pp) = by_id.get(&parent) {
                        want.push((parent, pp.clone()));
                    }
                }
            }
            i += 1;
        }
        let index = home.join("session_index.jsonl");
        self.codex_titles = codex_titles(&index);
        // Mains first so children find their parent session.
        want.sort_by_key(|(_, p)| {
            self.codex_meta
                .get(p)
                .cloned()
                .flatten()
                .and_then(|m| m.parent_id)
                .is_some()
        });
        for (id, p) in want {
            let meta = self.codex_meta.get(&p).cloned().flatten();
            let parent = meta
                .as_ref()
                .and_then(|m| m.parent_id.clone())
                .filter(|pid| self.sessions.contains(pid));
            match parent {
                Some(pid) => {
                    let key = Key::Agent(pid.clone(), id.clone());
                    if !self.files.contains_key(&key) {
                        let m = meta.clone().unwrap_or_default();
                        self.send(Msg::Agent {
                            sid: pid.clone(),
                            agent_id: id.clone(),
                            meta: AgentMeta {
                                agent_type: Some("codex".into()),
                                description: m.nickname.clone().or(m.agent_path.clone()),
                                agent_path: m.agent_path.clone(),
                                ..Default::default()
                            },
                            run_id: None,
                        })?;
                        self.files.insert(key, Tracked::codex(p.clone()));
                    }
                }
                None => {
                    seen.insert(id.clone());
                    if self.sessions.insert(id.clone()) {
                        let project = format!(
                            "codex:{}",
                            meta.as_ref()
                                .and_then(|m| m.cwd.clone())
                                .unwrap_or_default()
                        );
                        self.send(Msg::Session {
                            sid: id.clone(),
                            project,
                            tool: Tool::Codex,
                            path: p.clone(),
                        })?;
                        self.files
                            .insert(Key::Main(id.clone()), Tracked::codex(p.clone()));
                    }
                    if let Some(t) = self.codex_titles.get(&id).cloned() {
                        if self.sent_titles.get(&id) != Some(&t) {
                            self.sent_titles.insert(id.clone(), t.clone());
                            self.send(Msg::Events {
                                key: Key::Main(id.clone()),
                                reset: false,
                                events: vec![Event::new(crate::event::Kind::Title(t))],
                                mtime_ms: mtime_ms(&p).unwrap_or(0),
                            })?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn discover_agents(&mut self, sid: &str, dir: &Path, run_id: Option<String>) -> Result<(), ()> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(());
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(agent_id) = name
                .strip_prefix("agent-")
                .and_then(|n| n.strip_suffix(".jsonl"))
            else {
                continue;
            };
            let key = Key::Agent(sid.to_string(), agent_id.to_string());
            if self.files.contains_key(&key) {
                continue;
            }
            let meta: AgentMeta =
                std::fs::read_to_string(dir.join(format!("agent-{agent_id}.meta.json")))
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
            self.send(Msg::Agent {
                sid: sid.to_string(),
                agent_id: agent_id.to_string(),
                meta,
                run_id: run_id.clone(),
            })?;
            self.files.insert(key, Tracked::claude(e.path()));
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Result<(), ()> {
        let mut out = Vec::new();
        for (key, t) in self.files.iter_mut() {
            if !t.tail.changed() {
                continue;
            }
            let Ok(r) = t.tail.read_new() else { continue };
            let (reset, lines) = match r {
                TailRead::Lines(l) => (false, l),
                TailRead::Reset(l) => (true, l),
            };
            if lines.is_empty() && !reset {
                continue;
            }
            let events: Vec<Event> = match &mut t.codex {
                Some(parser) => {
                    if reset {
                        *parser = CodexParser::new();
                    }
                    lines.iter().flat_map(|l| parser.parse(l)).collect()
                }
                None => lines.iter().filter_map(|l| parse_line(l)).collect(),
            };
            let mtime_ms = mtime_ms(t.tail.path()).unwrap_or(0);
            out.push(Msg::Events {
                key: key.clone(),
                reset,
                events,
                mtime_ms,
            });
        }
        for m in out {
            self.send(m)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn project_key_encoding() {
        assert_eq!(project_key("/a/b/.c"), "-a-b--c");
        assert_eq!(
            project_key("/home/me/repo/.claude/worktrees/x"),
            "-home-me-repo--claude-worktrees-x"
        );
    }

    #[test]
    fn discovers_session_subagent_workflow() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("-p");
        let sdir = proj.join("s1");
        std::fs::create_dir_all(sdir.join("subagents/workflows/wf_1")).unwrap();
        std::fs::write(
            proj.join("s1.jsonl"),
            "{\"type\":\"user\",\"uuid\":\"a\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .unwrap();
        std::fs::write(sdir.join("subagents/agent-x.jsonl"), "").unwrap();
        std::fs::write(
            sdir.join("subagents/agent-x.meta.json"),
            r#"{"agentType":"Explore","toolUseId":"t1"}"#,
        )
        .unwrap();
        std::fs::write(sdir.join("subagents/workflows/wf_1/agent-y.jsonl"), "").unwrap();
        let (tx, rx) = channel();
        let mut w = Worker::new(
            root.path().to_path_buf(),
            ScanOpts {
                session: None,
                window_secs: 600,
            },
            tx,
        );
        w.rescan().unwrap();
        w.poll().unwrap();
        let msgs: Vec<Msg> = rx.try_iter().collect();
        assert!(msgs.iter().any(
            |m| matches!(m, Msg::Session { sid, project, .. } if sid == "s1" && project == "-p")
        ));
        assert!(msgs.iter().any(|m| matches!(m, Msg::Agent { agent_id, meta, run_id: None, .. } if agent_id == "x" && meta.tool_use_id.as_deref() == Some("t1"))));
        assert!(msgs.iter().any(|m| matches!(m, Msg::Agent { agent_id, run_id: Some(r), .. } if agent_id == "y" && r == "wf_1")));
        assert!(msgs.iter().any(|m| matches!(m, Msg::Events { key: Key::Main(s), events, .. } if s == "s1" && events.len() == 1)));
    }

    #[test]
    fn read_live_keeps_only_running_pids() {
        let d = tempfile::tempdir().unwrap();
        let me = std::process::id();
        std::fs::write(
            d.path().join("a.json"),
            format!(r#"{{"pid":{me},"sessionId":"alive","status":"busy"}}"#),
        )
        .unwrap();
        std::fs::write(
            d.path().join("b.json"),
            r#"{"pid":999999999,"sessionId":"dead"}"#,
        )
        .unwrap();
        std::fs::write(d.path().join("c.key"), "x").unwrap();
        let live = read_live(d.path());
        assert_eq!(live.len(), 1);
        assert_eq!(live["alive"].status.as_deref(), Some("busy"));
    }

    #[test]
    fn discovers_codex_parent_and_subagent() {
        let home = tempfile::tempdir().unwrap();
        let day = home.path().join("sessions/2026/11/25");
        std::fs::create_dir_all(&day).unwrap();
        let parent = "01a00000-0000-7000-8000-000000000001";
        let child = "01a00000-0000-7000-8000-000000000002";
        std::fs::write(
            day.join(format!("rollout-2026-11-25T01-00-00-{parent}.jsonl")),
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{parent}\",\"cwd\":\"/p\"}}}}\n"),
        )
        .unwrap();
        std::fs::write(
            day.join(format!("rollout-2026-11-25T01-01-00-{child}.jsonl")),
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{child}\",\"source\":{{\"subagent\":{{\"thread_spawn\":{{\"parent_thread_id\":\"{parent}\",\"agent_path\":\"/root/x\"}}}}}}}}}}\n"),
        )
        .unwrap();
        std::fs::write(
            home.path().join("session_index.jsonl"),
            format!("{{\"id\":\"{parent}\",\"thread_name\":\"Tên phiên\"}}\n"),
        )
        .unwrap();
        let (tx, rx) = channel();
        let claude = tempfile::tempdir().unwrap();
        let mut w = Worker::new(
            claude.path().to_path_buf(),
            ScanOpts {
                session: None,
                window_secs: 600,
            },
            tx,
        )
        .with_codex(home.path().to_path_buf())
        // Codex is off by default (config::CODEX_ENABLED); the parser is still tested.
        .with_config(SharedConfig::new(std::sync::Mutex::new(
            crate::config::Config {
                setup_done: true,
                claude: true,
                codex: true,
            },
        )));
        w.rescan().unwrap();
        let msgs: Vec<Msg> = rx.try_iter().collect();
        assert!(msgs
            .iter()
            .any(|m| matches!(m, Msg::Session { sid, tool: Tool::Codex, .. } if sid == parent)));
        assert!(msgs.iter().any(|m| matches!(m, Msg::Agent { sid, agent_id, meta, .. }
            if sid == parent && agent_id == child && meta.agent_path.as_deref() == Some("/root/x"))));
        assert!(msgs.iter().any(|m| matches!(m, Msg::Events { events, .. }
            if matches!(&events[0].kind, crate::event::Kind::Title(t) if t == "Tên phiên"))));
        assert!(!msgs
            .iter()
            .any(|m| matches!(m, Msg::Session { sid, .. } if sid == child)));
    }

    #[test]
    fn disabled_tool_is_not_scanned() {
        let claude = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(claude.path().join("-p")).unwrap();
        std::fs::write(claude.path().join("-p/s.jsonl"), "").unwrap();
        let cfg = SharedConfig::default();
        cfg.lock().unwrap().claude = false;
        let (tx, rx) = channel();
        let mut w = Worker::new(
            claude.path().to_path_buf(),
            ScanOpts {
                session: None,
                window_secs: 600,
            },
            tx,
        )
        .with_config(cfg);
        w.rescan().unwrap();
        assert!(rx.try_iter().next().is_none());
    }

    #[test]
    fn old_sessions_excluded() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("-p");
        std::fs::create_dir_all(&proj).unwrap();
        let f = proj.join("old.jsonl");
        std::fs::write(&f, "").unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let (tx, rx) = channel();
        let mut w = Worker::new(
            root.path().to_path_buf(),
            ScanOpts {
                session: None,
                window_secs: 1800,
            },
            tx,
        );
        w.rescan().unwrap();
        assert!(rx.try_iter().next().is_none());
    }
}
