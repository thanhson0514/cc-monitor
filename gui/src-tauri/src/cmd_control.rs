//! Tauri commands for managed sessions: composer (send / interrupt / stop) and
//! per-session overlay (model, effort, permission mode, capability toggles).
//!
//! Every command that can block (Manager lock held during a restart, process
//! spawn/kill, disk scans) is `async` and runs its work in `spawn_blocking`.
//! "Disabled" in the UI always comes from the overlay, never from transcripts
//! (deny rules act at call time; disabled skills/agents stay listed).

use cc_monitor::control::runner::{ControlError, Manager as CtlManager, RunState};
use cc_monitor::control::state::{self, effective};
use cc_monitor::control::{find_claude, Overlay, Registry};
use cc_monitor::discovery::{self, LiveInfo, Pinned};
use cc_monitor::store::Store;
use serde::Serialize;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tauri::State;

use crate::Shared;

pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
pub const MODEL_ALIASES: [&str; 4] = ["opus", "sonnet", "haiku", "fable"];
const SID_TIMEOUT: Duration = Duration::from_secs(30);
const HISTORY_DAYS: i64 = 30;

pub type SharedManager = Arc<Mutex<CtlManager>>;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlDto {
    pub overlay: Overlay,
    /// `None` = not run by ccmon (yet).
    pub run_state: Option<RunState>,
    pub can_control: bool,
    /// "liveElsewhere" | "claudeNotFound"
    pub reason: Option<&'static str>,
    pub models: Vec<String>,
    pub efforts: Vec<String>,
    /// Saved overlay differs from the one the running process was launched
    /// with: it applies from the next turn (restart between turns).
    pub pending: bool,
    /// ccmon-wide disabled agents/workflows (registry): always off here.
    pub registry_agents: Vec<String>,
    pub registry_workflows: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlChanged {
    pub sid: String,
    pub run_state: RunState,
}

/// Everything `control_dto` needs, gathered by the caller.
pub struct DtoInputs<'a> {
    pub overlay: Overlay,
    pub registry: &'a Registry,
    pub run_state: Option<RunState>,
    /// Overlay the managed process was launched with.
    pub launched: Option<&'a Overlay>,
    /// Pid of ccmon's own child for this session.
    pub own_pid: Option<u32>,
    /// `discovery::read_live` entry for this session.
    pub live: Option<&'a LiveInfo>,
    pub claude_found: bool,
    pub seen_models: Vec<String>,
}

pub fn control_dto(i: DtoInputs) -> ControlDto {
    let elsewhere = i.live.is_some_and(|l| i.own_pid != Some(l.pid));
    let reason = if elsewhere {
        Some("liveElsewhere")
    } else if !i.claude_found {
        Some("claudeNotFound")
    } else {
        None
    };
    let alive = matches!(
        i.run_state,
        Some(RunState::Idle | RunState::Starting | RunState::Busy | RunState::Restarting)
    );
    let pending = alive
        && i.launched
            .is_some_and(|l| *l != effective(&i.overlay, i.registry));
    ControlDto {
        overlay: i.overlay,
        run_state: i.run_state,
        can_control: reason.is_none(),
        reason,
        models: model_list(i.seen_models.iter().map(String::as_str)),
        efforts: EFFORTS.map(String::from).to_vec(),
        pending,
        registry_agents: i.registry.disabled_agents.iter().cloned().collect(),
        registry_workflows: i.registry.disabled_workflows.iter().cloned().collect(),
    }
}

/// Aliases first, then models seen in transcripts (deduplicated, sorted).
pub fn model_list<'a>(seen: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = MODEL_ALIASES.map(String::from).to_vec();
    let mut rest: Vec<String> = seen
        .into_iter()
        .map(str::trim)
        .filter(|m| !m.is_empty() && !m.starts_with('<') && !out.iter().any(|a| a == m))
        .map(String::from)
        .collect();
    rest.sort();
    rest.dedup();
    out.extend(rest);
    out
}

/// Save a session overlay and return what is now stored.
pub fn persist_overlay(dir: &Path, sid: &str, o: &Overlay) -> io::Result<Overlay> {
    state::save_overlay_in(dir, sid, o)?;
    Ok(state::load_overlay_in(dir, sid))
}

/// A new session may only start in a project ccmon already knows (from the
/// store or transcript history) that still exists.
pub fn check_new_cwd(cwd: &str, known: &[String]) -> Result<PathBuf, String> {
    if !known.iter().any(|k| k == cwd) {
        return Err("Thư mục không nằm trong danh sách project đã biết".into());
    }
    let p = PathBuf::from(cwd);
    if !p.is_dir() {
        return Err(format!("Thư mục không còn tồn tại: {cwd}"));
    }
    Ok(p)
}

pub fn err_text(e: &ControlError) -> String {
    match e {
        ControlError::SessionLiveElsewhere => "Session đang chạy ở terminal — chỉ xem".into(),
        ControlError::ClaudeNotFound => {
            "Không tìm thấy lệnh claude (PATH, ~/.local/bin, ~/.claude/local)".into()
        }
        ControlError::Busy => "Session đang chạy một lượt — chờ xong hoặc bấm Dừng".into(),
        ControlError::Io(m) => format!("Lỗi khi chạy claude: {m}"),
    }
}

/// `cwd` of a transcript (first lines that carry one).
pub fn transcript_cwd(p: &Path) -> Option<String> {
    let f = std::fs::File::open(p).ok()?;
    BufReader::new(f)
        .lines()
        .take(50)
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
        .find_map(|v| v.get("cwd")?.as_str().map(String::from))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn sessions_dir(root: &Path) -> PathBuf {
    root.parent()
        .map(|p| p.join("sessions"))
        .unwrap_or_default()
}

/// Project cwds ccmon knows: tracked sessions (most recent first), then the
/// newest transcript of each project in the last 30 days.
fn known_cwds(store: &Mutex<Store>, root: &Path) -> Vec<String> {
    let mut out: Vec<(i64, String)> = {
        let st = lock(store);
        st.sessions
            .values()
            .filter_map(|s| Some((s.mtime, s.main.cwd()?.to_string())))
            .collect()
    };
    let cutoff = discovery::now_ms() - HISTORY_DAYS * 86_400_000;
    for proj in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let mut newest: Option<(i64, PathBuf)> = None;
        for e in std::fs::read_dir(proj.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(mt) = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
            else {
                continue;
            };
            if mt >= cutoff && newest.as_ref().map_or(true, |n| mt > n.0) {
                newest = Some((mt, p));
            }
        }
        if let Some((mt, p)) = newest {
            if let Some(c) = transcript_cwd(&p) {
                out.push((mt, c));
            }
        }
    }
    out.sort_by_key(|(mt, _)| std::cmp::Reverse(*mt));
    let mut seen = std::collections::HashSet::new();
    out.into_iter()
        .map(|(_, c)| c)
        .filter(|c| seen.insert(c.clone()))
        .collect()
}

fn seen_models(store: &Mutex<Store>) -> Vec<String> {
    let st = lock(store);
    let mut v = Vec::new();
    for s in st.sessions.values() {
        v.extend(s.main.model().map(String::from));
        v.extend(
            s.agents
                .values()
                .filter_map(|a| a.thread.model().map(String::from)),
        );
    }
    v
}

fn session_cwd(store: &Mutex<Store>, sid: &str) -> Option<String> {
    lock(store).sessions.get(sid)?.main.cwd().map(String::from)
}

#[derive(Clone)]
struct Ctx {
    store: Arc<Mutex<Store>>,
    manager: SharedManager,
    root: PathBuf,
    pinned: Pinned,
}

impl Ctx {
    fn of(s: &Shared) -> Self {
        Self {
            store: s.store.clone(),
            manager: s.manager.clone(),
            root: s.root.clone(),
            pinned: s.pinned.clone(),
        }
    }

    fn live(&self) -> HashMap<String, LiveInfo> {
        discovery::read_live(&sessions_dir(&self.root))
    }

    fn dto(&self, sid: Option<&str>) -> ControlDto {
        let overlay = sid
            .map(state::load_overlay)
            .unwrap_or_else(state::load_defaults);
        let registry = state::load_registry();
        let live = self.live();
        let (run_state, launched, own_pid) = match sid {
            Some(s) => {
                let m = lock(&self.manager);
                (m.state(s), m.overlay(s).cloned(), m.pid(s))
            }
            None => (None, None, None),
        };
        control_dto(DtoInputs {
            overlay,
            registry: &registry,
            run_state,
            launched: launched.as_ref(),
            own_pid,
            live: sid.and_then(|s| live.get(s)),
            claude_found: find_claude().is_some(),
            seen_models: seen_models(&self.store),
        })
    }

    fn send(
        &self,
        sid: Option<String>,
        cwd: String,
        text: String,
        overlay: Option<Overlay>,
    ) -> Result<String, String> {
        if text.trim().is_empty() {
            return Err("Nội dung trống".into());
        }
        let registry = state::load_registry();
        if let Some(sid) = sid {
            // The working directory always comes from the store, never the webview.
            let cwd = session_cwd(&self.store, &sid)
                .ok_or_else(|| "Không rõ thư mục làm việc của session".to_string())?;
            let eff = effective(&state::load_overlay(&sid), &registry);
            let live = self.live();
            return lock(&self.manager)
                .send(Some(&sid), Path::new(&cwd), &text, &eff, &live)
                .map_err(|e| err_text(&e));
        }
        let path = check_new_cwd(&cwd, &known_cwds(&self.store, &self.root))?;
        let raw = overlay.unwrap_or_else(state::load_defaults);
        let eff = effective(&raw, &registry);
        // Lock-free start: the lock is only held for the spawn, never while
        // waiting for the session id.
        let s = lock(&self.manager)
            .start_new(&path, &text, &eff)
            .map_err(|e| err_text(&e))?;
        let Some(new) = s.wait_session_id(SID_TIMEOUT) else {
            let msg = match s.state() {
                RunState::Exited { stderr_tail, .. } => {
                    format!("claude đã thoát: {}", stderr_tail.trim())
                }
                _ => "claude không trả về session id".to_string(),
            };
            drop(s);
            return Err(msg);
        };
        let _ = state::save_overlay(&new, &raw);
        lock(&self.pinned).insert(new.clone());
        lock(&self.manager).adopt(new.clone(), s, eff);
        Ok(new)
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_control(
    state: State<'_, Shared>,
    sid: Option<String>,
) -> Result<ControlDto, String> {
    let c = Ctx::of(&state);
    blocking(move || c.dto(sid.as_deref())).await
}

#[tauri::command]
pub async fn set_overlay(
    state: State<'_, Shared>,
    sid: String,
    overlay: Overlay,
) -> Result<ControlDto, String> {
    let c = Ctx::of(&state);
    blocking(move || {
        persist_overlay(&state::state_dir(), &sid, &overlay).map_err(|e| e.to_string())?;
        Ok(c.dto(Some(&sid)))
    })
    .await?
}

/// `sid = None` starts a new session in `cwd` (with `overlay`, else the
/// defaults); returns the session id.
#[tauri::command]
pub async fn send_prompt(
    state: State<'_, Shared>,
    sid: Option<String>,
    cwd: String,
    text: String,
    overlay: Option<Overlay>,
) -> Result<String, String> {
    let c = Ctx::of(&state);
    blocking(move || c.send(sid, cwd, text, overlay)).await?
}

#[tauri::command]
pub async fn interrupt(state: State<'_, Shared>, sid: String) -> Result<(), String> {
    let m = state.manager.clone();
    blocking(move || lock(&m).interrupt(&sid).map_err(|e| err_text(&e))).await?
}

#[tauri::command]
pub async fn stop_managed(state: State<'_, Shared>, sid: String) -> Result<bool, String> {
    let m = state.manager.clone();
    blocking(move || lock(&m).stop(&sid)).await
}

/// Project directories a new session can start in.
#[tauri::command]
pub async fn list_projects(state: State<'_, Shared>) -> Result<Vec<String>, String> {
    let c = Ctx::of(&state);
    blocking(move || known_cwds(&c.store, &c.root)).await
}

/// Kill every managed child (their own process groups outlive ccmon otherwise).
pub fn stop_all(m: &SharedManager) {
    let mut g = lock(m);
    for (sid, _) in g.sessions() {
        g.stop(&sid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(pid: u32) -> LiveInfo {
        serde_json::from_value(serde_json::json!({"pid": pid, "sessionId": "s1"})).unwrap()
    }

    fn inputs<'a>(reg: &'a Registry) -> DtoInputs<'a> {
        DtoInputs {
            overlay: Overlay::default(),
            registry: reg,
            run_state: None,
            launched: None,
            own_pid: None,
            live: None,
            claude_found: true,
            seen_models: vec![],
        }
    }

    #[test]
    fn control_dto_reports_live_elsewhere() {
        let reg = Registry::default();
        let l = live(42);
        // Open in a terminal, not ours: read-only.
        let d = control_dto(DtoInputs {
            live: Some(&l),
            ..inputs(&reg)
        });
        assert!(!d.can_control);
        assert_eq!(d.reason, Some("liveElsewhere"));
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["canControl"], false);
        assert_eq!(v["reason"], "liveElsewhere");
        assert_eq!(v["runState"], serde_json::Value::Null);
        // Our own child shows up in the live registry too: still controllable.
        let d = control_dto(DtoInputs {
            live: Some(&l),
            own_pid: Some(42),
            run_state: Some(RunState::Idle),
            ..inputs(&reg)
        });
        assert!(d.can_control);
        assert_eq!(d.reason, None);
        assert_eq!(
            serde_json::to_value(&d).unwrap()["runState"]["kind"],
            "idle"
        );
        // Not live at all.
        let d = control_dto(inputs(&reg));
        assert!(d.can_control && d.reason.is_none());
        assert_eq!(d.efforts, EFFORTS.map(String::from).to_vec());
    }

    #[test]
    fn control_dto_reports_missing_claude() {
        let reg = Registry::default();
        let d = control_dto(DtoInputs {
            claude_found: false,
            ..inputs(&reg)
        });
        assert!(!d.can_control);
        assert_eq!(d.reason, Some("claudeNotFound"));
        // Live elsewhere wins over a missing binary.
        let l = live(7);
        let d = control_dto(DtoInputs {
            claude_found: false,
            live: Some(&l),
            ..inputs(&reg)
        });
        assert_eq!(d.reason, Some("liveElsewhere"));
    }

    #[test]
    fn pending_when_saved_overlay_differs_from_launched() {
        let reg = Registry {
            disabled_agents: ["Explore".to_string()].into(),
            ..Default::default()
        };
        let saved = Overlay {
            effort: Some("high".into()),
            ..Default::default()
        };
        let launched_same = effective(&saved, &reg);
        let d = control_dto(DtoInputs {
            overlay: saved.clone(),
            launched: Some(&launched_same),
            run_state: Some(RunState::Busy),
            ..inputs(&reg)
        });
        assert!(!d.pending, "registry merge must not count as a change");
        assert_eq!(d.registry_agents, vec!["Explore".to_string()]);
        let launched_old = Overlay::default();
        let d = control_dto(DtoInputs {
            overlay: saved.clone(),
            launched: Some(&launched_old),
            run_state: Some(RunState::Busy),
            ..inputs(&reg)
        });
        assert!(d.pending);
        // Exited process: the next send relaunches anyway.
        let d = control_dto(DtoInputs {
            overlay: saved,
            launched: Some(&launched_old),
            run_state: Some(RunState::Exited {
                code: Some(0),
                stderr_tail: String::new(),
            }),
            ..inputs(&reg)
        });
        assert!(!d.pending);
    }

    #[test]
    fn set_overlay_persists() {
        let d = tempfile::tempdir().unwrap();
        let mut o = Overlay {
            model: Some("sonnet".into()),
            permission_mode: Some("plan".into()),
            ..Default::default()
        };
        o.disabled_skills.insert("superpowers:brainstorming".into());
        o.disabled_mcp.insert("docs-langchain".into());
        let back = persist_overlay(d.path(), "abc-1", &o).unwrap();
        assert_eq!(back, o);
        assert_eq!(state::load_overlay_in(d.path(), "abc-1"), o);
        assert!(d.path().join("overlays/abc-1.json").is_file());
        assert!(persist_overlay(d.path(), "../x", &o).is_err());
    }

    #[test]
    fn models_start_with_aliases_then_seen() {
        let m = model_list([
            "claude-opus-5-5",
            "sonnet",
            "",
            "<synthetic>",
            "claude-opus-5-5",
            "claude-haiku-4-5",
        ]);
        assert_eq!(
            m,
            [
                "opus",
                "sonnet",
                "haiku",
                "fable",
                "claude-haiku-4-5",
                "claude-opus-5-5"
            ]
            .map(String::from)
            .to_vec()
        );
    }

    #[test]
    fn new_cwd_must_be_known_and_exist() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().to_string_lossy().into_owned();
        assert!(check_new_cwd(&p, &[]).is_err());
        assert_eq!(check_new_cwd(&p, &[p.clone()]).unwrap(), d.path());
        let gone = d.path().join("gone").to_string_lossy().into_owned();
        assert!(check_new_cwd(&gone, &[gone.clone()]).is_err());
    }

    #[test]
    fn transcript_cwd_reads_first_cwd() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("s.jsonl");
        std::fs::write(
            &f,
            "not json\n{\"type\":\"summary\"}\n{\"type\":\"user\",\"cwd\":\"/w/p\"}\n",
        )
        .unwrap();
        assert_eq!(transcript_cwd(&f), Some("/w/p".into()));
        assert_eq!(transcript_cwd(&d.path().join("none.jsonl")), None);
    }

    #[test]
    fn error_texts_are_vietnamese() {
        assert!(err_text(&ControlError::SessionLiveElsewhere).contains("terminal"));
        assert!(err_text(&ControlError::ClaudeNotFound).contains("claude"));
        assert!(err_text(&ControlError::Io("x".into())).contains('x'));
    }
}
