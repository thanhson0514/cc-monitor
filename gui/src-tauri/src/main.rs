// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! ccmon GUI: Tauri shell around the `cc_monitor` core. Read-only.
//!
//! A background worker (core `discovery::Worker`) tails transcripts; a pump thread
//! applies its messages to the shared `Store` and emits `store-changed` to the
//! webview, which re-fetches what it shows through the commands below.

mod assets;
mod changes;

use cc_monitor::bgtask::{BgKind, BgState, BgTask};
use cc_monitor::config::{self, SharedConfig};
use cc_monitor::discovery::{self, Key, Msg, Pinned, ScanOpts};
use cc_monitor::metrics;
use cc_monitor::store::{NodeId, NodeKind, Status, Store, TreeRow, ACTIVE_MS};
use cc_monitor::thread::{Item, ThreadLink, ToolState};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager, State};

const PREVIEW: usize = 2000;
const DETAIL_MAX: u64 = 512 * 1024;
const EMIT_EVERY: Duration = Duration::from_millis(250);

struct Shared {
    store: Arc<Mutex<Store>>,
    tee_dir: PathBuf,
    pinned: Pinned,
    root: PathBuf,
    config: SharedConfig,
}

const INPUT_STR_MAX: usize = 20_000;

/// Tool input for display: long strings (file contents, edits) are clipped.
fn clip_value(v: &Value) -> Value {
    match v {
        Value::String(s) if s.len() > INPUT_STR_MAX => {
            let (c, _) = clip(s, INPUT_STR_MAX);
            Value::String(format!("{c}\n… (cắt bớt, {} ký tự)", s.chars().count()))
        }
        Value::Array(a) => Value::Array(a.iter().map(clip_value).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), clip_value(v))).collect())
        }
        _ => v.clone(),
    }
}

// ---------- node keys ----------

fn key_of(id: &NodeId) -> String {
    match id {
        NodeId::Main(s) => format!("m:{s}"),
        NodeId::Agent(s, a) => format!("a:{s}:{a}"),
        NodeId::Workflow(s, w) => format!("w:{s}:{w}"),
    }
}

fn parse_key(k: &str) -> Option<NodeId> {
    let mut it = k.splitn(3, ':');
    let kind = it.next()?;
    let sid = it.next()?.to_string();
    match kind {
        "m" => Some(NodeId::Main(sid)),
        "a" => Some(NodeId::Agent(sid, it.next()?.to_string())),
        "w" => Some(NodeId::Workflow(sid, it.next()?.to_string())),
        _ => None,
    }
}

fn status_str(s: Status) -> &'static str {
    match s {
        Status::Running => "running",
        Status::Done => "done",
        Status::Error => "error",
        Status::Idle => "idle",
    }
}

fn clip(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    (s[..cut].to_string(), true)
}

// ---------- DTOs ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TreeRowDto {
    depth: u16,
    key: String,
    label: String,
    status: &'static str,
    kind: &'static str,
    agent_type: Option<String>,
    progress: Option<(usize, usize)>,
    /// `busy` / `idle` when the session's `claude` process is running.
    live: Option<String>,
    live_name: Option<String>,
    tmux: Option<String>,
    mtime: Option<i64>,
    tool: Option<&'static str>,
    open: bool,
}

impl From<&TreeRow> for TreeRowDto {
    fn from(r: &TreeRow) -> Self {
        let (kind, agent_type) = match &r.kind {
            NodeKind::Main => ("main", None),
            NodeKind::Agent { agent_type } => ("agent", Some(agent_type.clone())),
            NodeKind::Workflow => ("workflow", None),
            NodeKind::WorkflowAgent => ("workflowAgent", None),
        };
        Self {
            depth: r.depth,
            key: key_of(&r.id),
            label: r.label.clone(),
            status: status_str(r.status),
            kind,
            agent_type,
            progress: r.progress,
            live: match &r.live {
                Some(l) => Some(l.status.clone().unwrap_or_else(|| "idle".into())),
                // Codex has no process registry: an open Codex session is busy while running.
                None if r.open => Some(
                    if r.status == Status::Running {
                        "busy"
                    } else {
                        "idle"
                    }
                    .into(),
                ),
                None => None,
            },
            tool: r.tool.map(|t| t.as_str()),
            open: r.open,
            live_name: r.live.as_ref().and_then(|l| l.name.clone()),
            tmux: r.live.as_ref().and_then(|l| l.tmux.clone()),
            mtime: r.mtime,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BgTaskDto {
    id: String,
    kind: &'static str,
    label: String,
    state: &'static str,
    started: Option<i64>,
    ended: Option<i64>,
    summary: Option<String>,
    output_file: Option<String>,
}

impl BgTaskDto {
    /// `open` = the owning session is live or recently written. A `Running` task of a
    /// session that is not open is reported as `unknown` (finalization is not timer-driven).
    fn from_task(t: &BgTask, open: bool) -> Self {
        Self {
            id: t.id.clone(),
            kind: match t.kind {
                BgKind::Shell => "shell",
                BgKind::Monitor => "monitor",
                BgKind::Agent => "agent",
                BgKind::Workflow => "workflow",
            },
            label: t.label.clone(),
            state: match t.state {
                BgState::Running if open => "running",
                BgState::Running | BgState::Unknown => "unknown",
                BgState::Completed => "completed",
                BgState::Failed => "failed",
                BgState::Killed => "killed",
            },
            started: t.started,
            ended: t.ended,
            summary: t.summary.clone(),
            output_file: t.output_file.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TailDto {
    text: String,
    truncated: bool,
    gone: bool,
    size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkDto {
    kind: &'static str,
    key: Option<String>,
    label: Option<String>,
    status: Option<&'static str>,
    progress: Option<(usize, usize)>,
    report: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ItemDto {
    Prompt {
        idx: usize,
        ts: Option<i64>,
        text: String,
    },
    Turn {
        idx: usize,
        ts: Option<i64>,
        model: Option<String>,
        input: u64,
        output: u64,
    },
    Thinking {
        idx: usize,
        ts: Option<i64>,
        text: String,
        ms: Option<u64>,
    },
    Text {
        idx: usize,
        ts: Option<i64>,
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Tool {
        idx: usize,
        id: String,
        name: String,
        summary: String,
        state: &'static str,
        started: Option<i64>,
        ended: Option<i64>,
        prompt: Option<String>,
        /// Report text when this is the agent's `SubagentHandback` call.
        handback: Option<String>,
        input: Value,
        result_preview: String,
        has_more: bool,
        link: Option<LinkDto>,
        bg: Option<BgTaskDto>,
        waits: Vec<BgTaskDto>,
        woke_by: Option<String>,
    },
    Compact {
        idx: usize,
        ts: Option<i64>,
        pre: u64,
        post: u64,
    },
    System {
        idx: usize,
        ts: Option<i64>,
        text: String,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadDto {
    key: String,
    kind: &'static str,
    title: String,
    /// Who speaks as "user" in this thread: the human (main) or Claude (agents).
    user_label: String,
    assistant_label: String,
    total: usize,
    items: Vec<ItemDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusDto {
    model: Option<String>,
    ctx: Option<u64>,
    win: Option<u64>,
    win_estimated: bool,
    ctx_pct: Option<f64>,
    branch: Option<String>,
    lines: Option<(u64, u64)>,
    mem: Option<(u64, u64)>,
    effort: Option<String>,
    five_h: Option<f64>,
    seven_d: Option<f64>,
    compactions: u32,
    running_agents: usize,
    sessions: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolDetailDto {
    input: String,
    result: String,
    source: Option<String>,
}

// ---------- helpers ----------

/// Report an agent sent back: last `SubagentHandback` message, else its last text.
fn agent_report(store: &mut Store, id: &NodeId) -> Option<String> {
    let t = store.thread_mut(id)?;
    let items = t.items();
    for it in items.iter().rev() {
        match it {
            Item::Tool(tool) if tool.name == "SubagentHandback" => {
                if let Some(m) = tool.input.get("message").and_then(Value::as_str) {
                    return Some(m.to_string());
                }
            }
            Item::Text { text, .. } => return Some(text.clone()),
            _ => {}
        }
    }
    None
}

// ---------- commands ----------

#[tauri::command]
fn get_tree(state: State<Shared>) -> Vec<TreeRowDto> {
    let store = state.store.lock().unwrap();
    store
        .tree(discovery::now_ms())
        .iter()
        .map(TreeRowDto::from)
        .collect()
}

#[tauri::command]
fn get_thread(state: State<Shared>, key: String, tail: Option<usize>) -> Option<ThreadDto> {
    let id = parse_key(&key)?;
    let mut store = state.store.lock().unwrap();
    let now = discovery::now_ms();
    let rows: HashMap<NodeId, TreeRow> = store
        .tree(now)
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();
    let row = rows.get(&id);
    let title = row.map(|r| r.label.clone()).unwrap_or_default();
    let agent_name = match store.sessions.get(id.session()).map(|s| s.tool) {
        Some(discovery::Tool::Codex) => "Codex",
        _ => "Claude",
    };
    let (kind, user_label, assistant_label) = match row.map(|r| &r.kind) {
        Some(NodeKind::Main) | None => ("main", "Bạn".to_string(), agent_name.to_string()),
        Some(NodeKind::Agent { agent_type }) => (
            "agent",
            format!("{agent_name} giao việc"),
            agent_type.clone(),
        ),
        Some(NodeKind::WorkflowAgent) => (
            "agent",
            "Workflow giao việc".to_string(),
            "Agent".to_string(),
        ),
        Some(NodeKind::Workflow) => ("workflow", String::new(), String::new()),
    };
    if let NodeId::Workflow(..) = id {
        return Some(ThreadDto {
            key,
            kind,
            title,
            user_label,
            assistant_label,
            total: 0,
            items: vec![],
        });
    }
    let sid = id.session().to_string();
    let items: Vec<Item> = store.thread_mut(&id)?.items().to_vec();
    let total = items.len();
    let start = total.saturating_sub(tail.unwrap_or(400));
    let mut out = Vec::with_capacity(total - start);
    for (idx, it) in items.into_iter().enumerate().skip(start) {
        out.push(match it {
            Item::Prompt { ts, text } => ItemDto::Prompt { idx, ts, text },
            Item::Turn { ts, model, usage } => ItemDto::Turn {
                idx,
                ts,
                model: model.as_deref().map(metrics::model_label),
                input: usage.map(|u| u.input).unwrap_or(0),
                output: usage.map(|u| u.output).unwrap_or(0),
            },
            Item::Thinking { ts, text, ms } => ItemDto::Thinking { idx, ts, text, ms },
            Item::Text { ts, text } => ItemDto::Text { idx, ts, text },
            Item::Compact { ts, pre, post } => ItemDto::Compact { idx, ts, pre, post },
            Item::System { ts, text } => ItemDto::System { idx, ts, text },
            Item::Tool(t) => {
                let link = t.link.as_ref().map(|l| {
                    let node = store.node_for_link(&sid, l);
                    let r = node.as_ref().and_then(|n| rows.get(n));
                    let report = match (l, &node) {
                        (ThreadLink::Agent(_), Some(n)) => agent_report(&mut store, n),
                        _ => None,
                    };
                    LinkDto {
                        kind: match l {
                            ThreadLink::Agent(_) => "agent",
                            ThreadLink::Workflow(_) => "workflow",
                        },
                        key: node.as_ref().map(key_of),
                        label: r.map(|r| r.label.clone()),
                        status: r.map(|r| status_str(r.status)),
                        progress: r.and_then(|r| r.progress),
                        report,
                    }
                });
                let (result_preview, has_more) = clip(&t.result, PREVIEW);
                let open = store.session_is_open(&sid, now);
                let bg = t.bg.as_ref().map(|b| BgTaskDto::from_task(b, open));
                let waits = t
                    .waits
                    .iter()
                    .map(|b| BgTaskDto::from_task(b, open))
                    .collect();
                ItemDto::Tool {
                    idx,
                    prompt: t
                        .input
                        .get("prompt")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    input: clip_value(&t.input),
                    handback: (t.name == "SubagentHandback")
                        .then(|| {
                            t.input
                                .get("message")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .flatten(),
                    id: t.id,
                    name: t.name,
                    summary: t.summary,
                    state: match t.state {
                        ToolState::Running => "running",
                        ToolState::Ok => "ok",
                        ToolState::Err => "error",
                    },
                    started: t.started,
                    ended: t.ended,
                    result_preview,
                    has_more: has_more || t.persisted.is_some(),
                    link,
                    bg,
                    waits,
                    woke_by: t.woke_by,
                }
            }
        });
    }
    Some(ThreadDto {
        key,
        kind,
        title,
        user_label,
        assistant_label,
        total,
        items: out,
    })
}

#[tauri::command]
fn get_tool_detail(state: State<Shared>, key: String, tool_id: String) -> Option<ToolDetailDto> {
    let id = parse_key(&key)?;
    let mut store = state.store.lock().unwrap();
    let t = store.thread_mut(&id)?;
    let tool = t.items().iter().find_map(|i| match i {
        Item::Tool(tool) if tool.id == tool_id => Some(tool.clone()),
        _ => None,
    })?;
    drop(store);
    let mut result = tool.result.clone();
    let mut source = None;
    if let Some(p) = &tool.persisted {
        if let Ok(f) = std::fs::File::open(p) {
            use std::io::Read;
            let mut s = String::new();
            let _ = f.take(DETAIL_MAX).read_to_string(&mut s);
            result = s;
            source = Some(p.display().to_string());
        }
    }
    Some(ToolDetailDto {
        input: serde_json::to_string_pretty(&tool.input).unwrap_or_default(),
        result,
        source,
    })
}

#[tauri::command]
fn get_status(state: State<Shared>, key: Option<String>) -> StatusDto {
    let store = state.store.lock().unwrap();
    let now = discovery::now_ms();
    let mem = cc_monitor::platform::memory();
    let sid = key
        .as_deref()
        .and_then(parse_key)
        .map(|id| id.session().to_string());
    let m = sid
        .as_ref()
        .and_then(|sid| {
            let t = store.thread(&NodeId::Main(sid.clone()))?;
            let tee = metrics::load_tee(&state.tee_dir, sid, now);
            Some(metrics::collect(t, tee.as_ref(), mem))
        })
        .unwrap_or_else(|| metrics::Metrics {
            mem,
            ..Default::default()
        });
    StatusDto {
        model: m.model,
        ctx: m.ctx,
        win: m.win.map(|w| w.0),
        win_estimated: m.win.is_some_and(|w| w.1),
        ctx_pct: m.ctx_pct,
        branch: m.branch,
        lines: m.lines,
        mem: m.mem,
        effort: m.effort,
        five_h: m.five_h,
        seven_d: m.seven_d,
        compactions: m.compactions,
        running_agents: store.running_agents(now),
        sessions: store
            .sessions
            .values()
            .filter(|s| now - s.mtime < ACTIVE_MS)
            .count(),
    }
}

fn arg_value(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Node to open at startup (`--select <key>` or env `CCMON_SELECT`), e.g. `m:<session id>`.
#[tauri::command]
fn get_initial_select() -> Option<String> {
    arg_value("--select").or_else(|| std::env::var("CCMON_SELECT").ok())
}

/// Startup shortcuts: `--diff <path>` opens the changes panel on that file's diff,
/// `--view <path>` opens the file viewer.
#[derive(Serialize)]
struct Startup {
    diff: Option<String>,
    view: Option<String>,
}

#[tauri::command]
fn get_startup() -> Startup {
    Startup {
        diff: arg_value("--diff"),
        view: arg_value("--view"),
    }
}

#[tauri::command]
fn list_history(state: State<Shared>, days: Option<u32>) -> Vec<assets::HistoryProject> {
    let cfg = *state.config.lock().unwrap();
    let now = discovery::now_ms();
    let days = days.unwrap_or(30);
    let mut out = if cfg.claude {
        assets::list_history(&state.root, days, now)
    } else {
        vec![]
    };
    if cfg.codex && config::CODEX_ENABLED {
        out.extend(assets::list_codex_history(
            &cc_monitor::platform::codex_home(),
            days,
            now,
        ));
    }
    out.sort_by_key(|p| std::cmp::Reverse(p.sessions.first().map(|s| s.mtime).unwrap_or(0)));
    out
}

/// Start tracking an old session so it can be viewed (picked up by the next rescan).
#[tauri::command]
fn open_session(state: State<Shared>, sid: String) {
    state.pinned.lock().unwrap().insert(sid);
}

/// Sidebar data. `scope`: "session" (main + all agents) or "thread" (selected node only).
#[tauri::command]
fn get_assets(state: State<Shared>, key: String, scope: Option<String>) -> Option<assets::Assets> {
    let id = parse_key(&key)?;
    let sid = id.session().to_string();
    let mut store = state.store.lock().unwrap();
    let rows: HashMap<NodeId, String> = store
        .tree(discovery::now_ms())
        .into_iter()
        .map(|r| (r.id.clone(), r.label))
        .collect();
    let session = store.sessions.get_mut(&sid)?;
    let tmp = assets::session_tmp_dir(&session.project, &sid);
    let only_thread = scope.as_deref() == Some("thread");
    let mut refs = Vec::new();
    let main_id = NodeId::Main(sid.clone());
    if !only_thread || id == main_id {
        refs.push(assets::ThreadRef {
            key: key_of(&main_id),
            label: "Luồng chính".into(),
            thread: &mut session.main,
        });
    }
    for (aid, a) in session.agents.iter_mut() {
        let nid = NodeId::Agent(sid.clone(), aid.clone());
        let in_wf =
            matches!(&id, NodeId::Workflow(_, w) if a.run_id.as_deref() == Some(w.as_str()));
        if only_thread && nid != id && !in_wf {
            continue;
        }
        refs.push(assets::ThreadRef {
            label: rows.get(&nid).cloned().unwrap_or_else(|| aid.clone()),
            key: key_of(&nid),
            thread: &mut a.thread,
        });
    }
    Some(assets::collect(refs, tmp))
}

fn transcript_of(_state: &Shared, store: &Store, sid: &str) -> Option<PathBuf> {
    Some(store.sessions.get(sid)?.path.clone())
}

/// Files the session edited or created (all threads), with git status and +/- vs baseline.
#[tauri::command]
fn get_changes(state: State<Shared>, key: String) -> Option<Vec<changes::ChangedFile>> {
    let sid = parse_key(&key)?.session().to_string();
    let mut store = state.store.lock().unwrap();
    let transcript = transcript_of(&state, &store, &sid)?;
    let session = store.sessions.get_mut(&sid)?;
    let mut touched: std::collections::BTreeMap<String, changes::Touch> = Default::default();
    let threads = std::iter::once(&mut session.main)
        .chain(session.agents.values_mut().map(|a| &mut a.thread));
    for t in threads {
        for it in t.items().iter() {
            let Item::Tool(tool) = it else { continue };
            if tool.state == ToolState::Err {
                continue;
            }
            if tool.name == "Patch" {
                for f in tool
                    .input
                    .get("files")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    let e = touched.entry(f.to_string()).or_insert(changes::Touch {
                        edits: 0,
                        last_ts: None,
                    });
                    e.edits += 1;
                    e.last_ts = e.last_ts.max(tool.started);
                }
                continue;
            }
            if !matches!(
                tool.name.as_str(),
                "Edit" | "MultiEdit" | "Write" | "NotebookEdit"
            ) {
                continue;
            }
            let Some(path) = tool
                .input
                .get("file_path")
                .or_else(|| tool.input.get("notebook_path"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let e = touched.entry(path.to_string()).or_insert(changes::Touch {
                edits: 0,
                last_ts: None,
            });
            e.edits += 1;
            e.last_ts = e.last_ts.max(tool.started);
        }
    }
    drop(store);
    Some(changes::list_changed(touched, &transcript, &sid))
}

#[tauri::command]
fn get_diff(
    state: State<Shared>,
    key: String,
    path: String,
    baseline: Option<String>,
) -> Option<changes::DiffView> {
    let sid = parse_key(&key)?.session().to_string();
    let transcript = {
        let store = state.store.lock().unwrap();
        transcript_of(&state, &store, &sid)?
    };
    Some(changes::diff_view(
        &path,
        baseline.as_deref(),
        &transcript,
        &sid,
    ))
}

/// Last bytes of a background task's output file. Only `*/tasks/*.output` under the
/// temp dir or `/tmp/claude-*` are read (see `cc_monitor::tasktail`); anything else is `gone`.
#[tauri::command]
fn tail_task_output(path: String, max: Option<u64>) -> TailDto {
    let t = cc_monitor::tasktail::tail_task_output(
        Path::new(&path),
        max.unwrap_or(cc_monitor::tasktail::TAIL_MAX),
    );
    TailDto {
        text: t.text,
        truncated: t.truncated,
        gone: t.gone,
        size: t.size,
    }
}

#[tauri::command]
fn preview_file(path: String) -> assets::Preview {
    assets::preview(&path)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SetupInfo {
    config: config::Config,
    claude_dir: String,
    claude_found: bool,
    codex_dir: String,
    codex_found: bool,
    statusline: Option<String>,
    statusline_wrapped: bool,
}

/// What is installed on this machine and the current settings (first-run screen).
#[tauri::command]
fn get_setup(state: State<Shared>) -> SetupInfo {
    let claude = cc_monitor::platform::claude_home();
    let codex = cc_monitor::platform::codex_home();
    let statusline = cc_monitor::setup::current_command(&cc_monitor::setup::settings_path());
    SetupInfo {
        config: *state.config.lock().unwrap(),
        claude_found: claude.join("projects").is_dir(),
        claude_dir: claude.display().to_string(),
        codex_found: codex.join("sessions").is_dir(),
        codex_dir: codex.display().to_string(),
        statusline_wrapped: statusline
            .as_deref()
            .is_some_and(cc_monitor::setup::is_wrapped),
        statusline,
    }
}

/// Save tool choices; optionally wrap Claude Code's status line with `<this exe> tee --`
/// to read Session % / Weekly % / context window (user consented in the setup screen).
#[tauri::command]
fn save_setup(
    state: State<Shared>,
    claude: bool,
    codex: bool,
    statusline: bool,
) -> Result<SetupInfo, String> {
    let codex = codex && config::CODEX_ENABLED;
    let cfg = config::Config {
        setup_done: true,
        claude: claude || !codex,
        codex,
    };
    config::save(&cfg).map_err(|e| e.to_string())?;
    *state.config.lock().unwrap() = cfg;
    if claude && statusline {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let prefix = cc_monitor::setup::tee_prefix_for(&exe);
        cc_monitor::setup::setup_with(&cc_monitor::setup::settings_path(), &prefix, true)
            .map_err(|e| e.to_string())?;
    }
    Ok(get_setup(state))
}

/// Remove ccmon's wrapper from Claude Code's status line.
#[tauri::command]
fn remove_statusline(state: State<Shared>) -> Result<SetupInfo, String> {
    cc_monitor::setup::uninstall(&cc_monitor::setup::settings_path()).map_err(|e| e.to_string())?;
    Ok(get_setup(state))
}

// ---------- main ----------

#[derive(Clone, Serialize)]
struct Changed {
    keys: Vec<String>,
    tree: bool,
}

fn main() {
    // `ccmon-gui tee -- <cmd>`: status-line passthrough (see cc_monitor::tee), no window.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("tee") {
        let cmd: Vec<String> = args[2..]
            .iter()
            .skip_while(|a| *a == "--")
            .cloned()
            .collect();
        std::process::exit(cc_monitor::tee::run_tee(&cmd));
    }
    let shared_config: SharedConfig = Arc::new(Mutex::new(config::load()));
    let store = Arc::new(Mutex::new(Store::default()));
    let (tx, rx) = channel::<Msg>();
    let window_secs = std::env::args()
        .any(|a| a == "--all")
        .then_some(86_400)
        .unwrap_or(1_800);
    let opts = ScanOpts {
        session: None,
        window_secs,
    };
    let pinned = Pinned::default();
    let worker_pinned = pinned.clone();
    let worker_config = shared_config.clone();
    std::thread::spawn(move || {
        let mut worker = discovery::Worker::new(discovery::projects_root(), opts, tx)
            .with_pinned(worker_pinned)
            .with_config(worker_config);
        if config::CODEX_ENABLED {
            worker = worker.with_codex(cc_monitor::platform::codex_home());
        }
        worker.run()
    });

    let pump_store = store.clone();
    tauri::Builder::default()
        .manage(Shared {
            store,
            tee_dir: cc_monitor::tee::cache_dir(),
            pinned,
            root: discovery::projects_root(),
            config: shared_config,
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let mut keys = BTreeSet::new();
                let mut tree = false;
                let mut last = Instant::now();
                loop {
                    match rx.recv_timeout(EMIT_EVERY) {
                        Ok(m) => {
                            match &m {
                                Msg::Events { key, .. } => {
                                    keys.insert(match key {
                                        Key::Main(s) => format!("m:{s}"),
                                        Key::Agent(s, a) => format!("a:{s}:{a}"),
                                    });
                                    tree = true;
                                }
                                _ => tree = true,
                            }
                            pump_store.lock().unwrap().apply(m);
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                    if last.elapsed() >= EMIT_EVERY {
                        last = Instant::now();
                        if tree || !keys.is_empty() {
                            let payload = Changed {
                                keys: std::mem::take(&mut keys).into_iter().collect(),
                                tree,
                            };
                            tree = false;
                            let _ = handle.emit("store-changed", payload);
                        }
                    }
                }
            });
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title("ccmon — Claude Code Monitor");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_tree,
            get_thread,
            get_tool_detail,
            get_status,
            get_initial_select,
            get_startup,
            list_history,
            open_session,
            get_assets,
            get_changes,
            get_setup,
            save_setup,
            remove_statusline,
            get_diff,
            preview_file,
            tail_task_output
        ])
        .run(tauri::generate_context!())
        .expect("không khởi động được ccmon GUI");
}
