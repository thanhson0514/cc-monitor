//! Tauri commands for controlling one subagent through its main thread:
//! pause / resume / swap / cancel. A stopped agent cannot be resumed
//! (docs/architecture/control.md, "Subagent control"), so resume and swap ask
//! main to start a new Agent call; a pause leaves a marker in ccmon state.
//!
//! The plan (`plan_action`) is pure; `Ctx::act` runs it over the session's
//! channel: ccmon's own `-p` child (`stop_task` + queued message), a non-live
//! session (`Manager::send` resumes it) or a tmux pane (main is asked to call
//! TaskStop itself; a foreground agent is interrupted with Esc first).

use cc_monitor::catalog::{CapKind, Capability, Scope};
use cc_monitor::control::runner::RunState;
use cc_monitor::control::state::{self, effective, Paused};
use cc_monitor::control::subagent::{
    cancel_msg, find_call, pause_msg, resume_msg, swap_msg, Channel, SubagentCall,
};
use cc_monitor::control::tmux;
use cc_monitor::discovery;
use cc_monitor::store::NodeId;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tauri::State;

use crate::cmd_agents::{session_agents, BUILTIN_AGENTS};
use crate::cmd_control::{blocking, err_text, lock, reason_text, Ctx, Route};
use crate::Shared;

pub const BUSY_NEW_AGENT: &str = "Session đang bận — chờ lượt hiện tại xong rồi đổi sang agent mới";
/// Resume/swap while the agent has not stopped yet (terminal pause pending).
pub const STILL_RUNNING: &str = "Subagent vẫn đang chạy — chờ nó dừng hẳn rồi thử lại";
pub const IN_FLIGHT_TEXT: &str = "Đang xử lý thao tác trước, thử lại sau";
/// Time Claude Code gets to handle an Esc before the message is pasted.
const AFTER_ESCAPE: Duration = Duration::from_millis(400);

/// Sessions with a subagent action in progress (one at a time per session).
static IN_FLIGHT: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Held while an action runs on a session; released on drop.
pub struct InFlight(String);

impl InFlight {
    pub fn take(sid: &str) -> Result<Self, String> {
        if lock(&IN_FLIGHT).insert(sid.to_string()) {
            Ok(Self(sid.to_string()))
        } else {
            Err(IN_FLIGHT_TEXT.into())
        }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        lock(&IN_FLIGHT).remove(&self.0);
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentDto {
    pub call: SubagentCall,
    /// running | done | error (tree row status)
    pub status: String,
    /// ccmon holds a pause marker for it.
    pub paused: bool,
    /// Paused, but main was only asked to stop it and the agent still runs:
    /// resume/swap are refused until it stops.
    pub stopping: bool,
    /// "managed" | "tmux"; `None` = not controllable now (see `reason`).
    pub channel: Option<&'static str>,
    pub reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `stop_task` on ccmon's own child.
    StopTask,
    /// Esc into the tmux pane: interrupts main's turn and the foreground
    /// subagent inside it.
    Escape,
    /// Message to main.
    Send(String),
    MarkPaused,
    Unmark,
}

/// What `action` does for a subagent in `status` (`paused` = marker held),
/// reached over `ch`. `transcript` = the agent's own jsonl; `agent` = swap target.
pub fn plan_action(
    action: &str,
    status: &str,
    paused: bool,
    ch: Channel,
    call: &SubagentCall,
    transcript: &str,
    agent: Option<&str>,
) -> Result<Vec<Step>, String> {
    // `idle`: pending tools but quiet for a while (long Bash call): still running.
    let running = matches!(status, "running" | "idle");
    // Managed: ccmon stops the task itself. Terminal: a foreground agent
    // blocks main inside the Agent call, so Esc interrupts main's turn and the
    // agent with it; a background agent is stopped by main (TaskStop).
    let stop_and = |msg: fn(&SubagentCall, Channel) -> String| match ch {
        Channel::Managed => vec![Step::StopTask, Step::Send(msg(call, Channel::Managed))],
        Channel::Terminal if !call.background => {
            vec![Step::Escape, Step::Send(msg(call, Channel::Managed))]
        }
        Channel::Terminal => vec![Step::Send(msg(call, Channel::Terminal))],
    };
    match action {
        "pause" => {
            if paused {
                return Err("Subagent đã được tạm dừng".into());
            }
            if !running {
                return Err("Subagent không còn chạy — không tạm dừng được".into());
            }
            let mut v = stop_and(pause_msg);
            v.push(Step::MarkPaused);
            Ok(v)
        }
        "resume" | "swap" => {
            if !paused {
                return Err("Subagent chưa được ccmon tạm dừng".into());
            }
            if running {
                return Err(STILL_RUNNING.into());
            }
            let msg = if action == "resume" {
                resume_msg(call, transcript)
            } else {
                let a = agent
                    .filter(|a| !a.trim().is_empty())
                    .ok_or("Chưa chọn agent để đổi sang")?;
                swap_msg(call, a)
            };
            Ok(vec![Step::Send(msg), Step::Unmark])
        }
        "cancel" => {
            if paused {
                // Already stopped: only tell main.
                Ok(vec![
                    Step::Send(cancel_msg(call, Channel::Managed)),
                    Step::Unmark,
                ])
            } else if running {
                Ok(stop_and(cancel_msg))
            } else {
                Err("Subagent đã kết thúc — không còn gì để huỷ".into())
            }
        }
        _ => Err(format!("Thao tác không hợp lệ: {action}")),
    }
}

/// A `-p` child loads agent files only at start: a target file newer than the
/// child needs a restart (`--resume` on the next send). `Ok(false)` without a
/// user file or a ccmon child; `Err` while a turn runs.
pub fn restart_needed(
    file_mtime: Option<SystemTime>,
    child_start: Option<SystemTime>,
    st: Option<RunState>,
) -> Result<bool, String> {
    let (Some(f), Some(c)) = (file_mtime, child_start) else {
        return Ok(false);
    };
    if f <= c {
        return Ok(false);
    }
    match st {
        Some(RunState::Busy | RunState::Restarting) => Err(BUSY_NEW_AGENT.into()),
        Some(RunState::Idle | RunState::Starting) => Ok(true),
        _ => Ok(false),
    }
}

/// Swap target from the session's agent listing (`items`, built-ins always
/// count), not disabled for the session. Returns the user/project agent file.
pub fn pick_target(
    items: &[Capability],
    disabled: &BTreeSet<String>,
    name: &str,
) -> Result<Option<PathBuf>, String> {
    if disabled.contains(name) {
        return Err(format!("Agent {name} đang bị tắt cho session này"));
    }
    if let Some(c) = items
        .iter()
        .find(|c| c.kind == CapKind::Agent && c.id == name)
    {
        return Ok(c
            .path
            .clone()
            .filter(|_| matches!(c.scope, Scope::User | Scope::Project)));
    }
    if BUILTIN_AGENTS.contains(&name) {
        return Ok(None);
    }
    Err(format!("Không có agent {name} trong danh sách của session"))
}

/// Status `plan_action` sees: a marker whose task ccmon stopped itself reads
/// `done` even while the tree row still says running (the row only turns done
/// once the transcript goes quiet).
pub fn effective_status<'a>(status: &'a str, marker: Option<&Paused>) -> &'a str {
    if marker.is_some_and(|m| m.stopped) {
        "done"
    } else {
        status
    }
}

/// Run `steps` in order through `exec`, stopping at the first failure, except
/// for markers: once `StopTask`/`Escape` succeeded the agent is stopped, so a later
/// failure (telling main) still saves a pause marker — resume, swap or cancel
/// can then resend. Returns the first error.
pub fn run_steps(
    steps: Vec<Step>,
    mut exec: impl FnMut(&Step) -> Result<(), String>,
) -> Result<(), String> {
    let mut stopped = false;
    let mut marked = false;
    let mut err: Option<String> = None;
    for step in &steps {
        let run = match step {
            Step::MarkPaused => err.is_none() || stopped,
            _ => err.is_none(),
        };
        if !run {
            continue;
        }
        match exec(step) {
            Ok(()) => match step {
                Step::StopTask | Step::Escape => stopped = true,
                Step::MarkPaused => marked = true,
                _ => {}
            },
            Err(e) => {
                err.get_or_insert(e);
            }
        }
    }
    if stopped && err.is_some() && !marked {
        if let Err(e) = exec(&Step::MarkPaused) {
            err.get_or_insert(e);
        }
    }
    err.map_or(Ok(()), Err)
}

/// What the store knows about one subagent node.
struct Info {
    sid: String,
    call: SubagentCall,
    status: String,
    transcript: PathBuf,
}

fn info(ctx: &Ctx, key: &str) -> Option<Info> {
    let Some(id @ NodeId::Agent(..)) = crate::parse_key(key) else {
        return None;
    };
    let NodeId::Agent(sid, aid) = &id else {
        return None;
    };
    let st = lock(&ctx.store);
    let s = st.sessions.get(sid)?;
    let call = find_call(&s.main, aid)?;
    let transcript = s.agents.get(aid)?.path.clone();
    let status = crate::status_str(st.agent_status_of(sid, aid, discovery::now_ms())?);
    Some(Info {
        sid: sid.clone(),
        call,
        status: status.to_string(),
        transcript,
    })
}

fn marker(sid: &str, aid: &str) -> Option<Paused> {
    state::load_paused(sid)
        .into_iter()
        .find(|p| p.call.agent_id == aid)
}

/// `on`: save a marker (`stopped` = ccmon stopped the task itself); else drop it.
fn set_marker(sid: &str, call: &SubagentCall, on: bool, stopped: bool) -> Result<(), String> {
    let mut v = state::load_paused(sid);
    v.retain(|p| p.call.agent_id != call.agent_id);
    if on {
        v.push(Paused {
            call: call.clone(),
            at: discovery::now_ms(),
            stopped,
        });
    }
    state::save_paused(sid, &v).map_err(|e| format!("Không lưu được trạng thái tạm dừng: {e}"))
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

fn subagent_dto(ctx: &Ctx, key: &str) -> Option<SubagentDto> {
    let i = info(ctx, key)?;
    let m = marker(&i.sid, &i.call.agent_id);
    let active = matches!(
        effective_status(&i.status, m.as_ref()),
        "running" | "idle"
    );
    // No controls for a finished, unpaused agent: skip the channel check
    // (tmux forks) entirely.
    let (channel, reason) = if active || m.is_some() {
        match ctx.route(&i.sid) {
            Ok(Route::Tmux(_)) => (Some("tmux"), None),
            Ok(_) => (Some("managed"), None),
            Err(code) => (None, Some(code)),
        }
    } else {
        (None, None)
    };
    Some(SubagentDto {
        paused: m.is_some(),
        stopping: m.is_some() && active,
        call: i.call,
        status: i.status,
        channel,
        reason,
    })
}

/// Message to main over `route`: queued into ccmon's child (written even
/// mid-turn), a resume of a non-live session, or pasted into the tmux pane.
fn send(ctx: &Ctx, route: &Route, sid: &str, text: &str) -> Result<(), String> {
    match route {
        Route::Managed => lock(&ctx.manager)
            .send_queued(sid, text)
            .map_err(|e| err_text(&e)),
        Route::Resume => ctx.send_resume(sid, text).map(|_| ()),
        Route::Tmux(p) => tmux::send_text(p, text).map_err(|e| tmux::error_text(&e)),
    }
}

const NO_CALL: &str = "Không tìm thấy lệnh gọi subagent này (agent lồng nhau hoặc trong workflow)";

/// Swap target `name` for session `sid`, checked against its agent listing.
fn swap_target(ctx: &Ctx, sid: &str, name: &str) -> Result<Option<PathBuf>, String> {
    let ov = effective(&state::load_overlay(sid), &state::load_registry());
    let items = session_agents(ctx.store.clone(), ctx.root.clone(), sid);
    pick_target(&items, &ov.disabled_agents, name)
}

fn act(ctx: &Ctx, key: &str, action: &str, agent: Option<&str>) -> Result<(), String> {
    let i = info(ctx, key).ok_or(NO_CALL)?;
    let sid = i.sid.as_str();
    let _guard = InFlight::take(sid)?;
    let m = marker(sid, &i.call.agent_id);
    let paused = m.is_some();
    let status = effective_status(&i.status, m.as_ref());
    // Validate the swap target (catalog scan) before routing, so the tmux
    // check stays right before the send.
    let file = if action == "swap" {
        Some(swap_target(ctx, sid, agent.unwrap_or_default())?)
    } else {
        None
    };
    let mut route = ctx.route(sid).map_err(reason_text)?;
    let ch = match route {
        Route::Tmux(_) => Channel::Terminal,
        _ => Channel::Managed,
    };
    let steps = plan_action(
        action,
        status,
        paused,
        ch,
        &i.call,
        &i.transcript.to_string_lossy(),
        agent,
    )?;
    if let Some(file) = file {
        // tmux sessions hot-reload agents; a ccmon child must be restarted.
        if matches!(route, Route::Managed) {
            let file_mtime = file.as_deref().and_then(mtime);
            let mut m = lock(&ctx.manager);
            let need = restart_needed(file_mtime, m.started(sid), m.state(sid))?;
            if need {
                m.stop(sid);
                route = Route::Resume;
            }
        }
    }
    let mut stopped = false;
    run_steps(steps, |step| match step {
        Step::StopTask => match route {
            Route::Managed => {
                let r = lock(&ctx.manager)
                    .stop_task(sid, &i.call.agent_id)
                    .map_err(|e| err_text(&e));
                stopped = r.is_ok();
                r
            }
            _ => Err("Session không chạy trong ccmon — không dừng được subagent".into()),
        },
        Step::Escape => match &route {
            // Check for a dialog first: Esc would dismiss it (a refusal).
            Route::Tmux(p) => tmux::ensure_no_dialog(p)
                .and_then(|_| tmux::send_escape(p))
                .map(|_| std::thread::sleep(AFTER_ESCAPE))
                .map_err(|e| tmux::error_text(&e)),
            _ => Err("Session không chạy trong tmux — không gửi được Esc".into()),
        },
        Step::Send(t) => send(ctx, &route, sid, t),
        Step::MarkPaused => set_marker(sid, &i.call, true, stopped),
        Step::Unmark => set_marker(sid, &i.call, false, false),
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwapCheck {
    /// Swapping to this agent restarts ccmon's `-p` child (and stops the
    /// session's other background tasks).
    pub restart: bool,
}

/// Dry run of a swap to `agent`: whether it would restart the session.
fn swap_check(ctx: &Ctx, key: &str, agent: &str) -> Result<SwapCheck, String> {
    let i = info(ctx, key).ok_or(NO_CALL)?;
    let file_mtime = swap_target(ctx, &i.sid, agent)?.as_deref().and_then(mtime);
    let m = lock(&ctx.manager);
    let restart = restart_needed(file_mtime, m.started(&i.sid), m.state(&i.sid))?;
    Ok(SwapCheck { restart })
}

/// The main-thread call behind subagent node `key` (`a:<sid>:<agentId>`);
/// `None` for nested/workflow agents (no call found on main).
#[tauri::command]
pub async fn get_subagent(
    state: State<'_, Shared>,
    key: String,
) -> Result<Option<SubagentDto>, String> {
    let c = Ctx::of(&state);
    blocking(move || subagent_dto(&c, &key)).await
}

/// Would swapping subagent `key` to `agent` restart the session?
#[tauri::command]
pub async fn subagent_swap_check(
    state: State<'_, Shared>,
    key: String,
    agent: String,
) -> Result<SwapCheck, String> {
    let c = Ctx::of(&state);
    blocking(move || swap_check(&c, &key, &agent)).await?
}

/// `action`: pause | resume | swap (with `agent`) | cancel.
#[tauri::command]
pub async fn subagent_action(
    state: State<'_, Shared>,
    key: String,
    action: String,
    agent: Option<String>,
) -> Result<(), String> {
    let c = Ctx::of(&state);
    blocking(move || act(&c, &key, &action, agent.as_deref())).await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn call() -> SubagentCall {
        SubagentCall {
            agent_id: "a1".into(),
            tool_use_id: "t1".into(),
            agent_type: "Explore".into(),
            description: "Audit".into(),
            prompt: "look".into(),
            background: true,
        }
    }

    fn plan(action: &str, status: &str, paused: bool, ch: Channel) -> Result<Vec<Step>, String> {
        plan_action(
            action,
            status,
            paused,
            ch,
            &call(),
            "/t/agent-a1.jsonl",
            Some("rev"),
        )
    }

    #[test]
    fn action_plan_pause_requires_running() {
        assert!(plan("pause", "done", false, Channel::Managed).is_err());
        assert!(plan("pause", "running", true, Channel::Managed).is_err());
        assert_eq!(
            plan("pause", "running", false, Channel::Managed).unwrap(),
            vec![
                Step::StopTask,
                Step::Send(pause_msg(&call(), Channel::Managed)),
                Step::MarkPaused
            ]
        );
        assert_eq!(
            plan("pause", "running", false, Channel::Terminal).unwrap(),
            vec![
                Step::Send(pause_msg(&call(), Channel::Terminal)),
                Step::MarkPaused
            ]
        );
        // swap without a marker
        assert!(plan("swap", "done", false, Channel::Managed).is_err());
    }

    #[test]
    fn action_plan_resume_swap_cancel() {
        assert!(plan("resume", "done", false, Channel::Managed).is_err());
        assert_eq!(
            plan("resume", "done", true, Channel::Terminal).unwrap(),
            vec![
                Step::Send(resume_msg(&call(), "/t/agent-a1.jsonl")),
                Step::Unmark
            ]
        );
        assert_eq!(
            plan("swap", "done", true, Channel::Managed).unwrap(),
            vec![Step::Send(swap_msg(&call(), "rev")), Step::Unmark]
        );
        let no_agent = plan_action("swap", "done", true, Channel::Managed, &call(), "", None);
        assert!(no_agent.is_err());
        assert_eq!(
            plan("cancel", "running", false, Channel::Managed).unwrap(),
            vec![
                Step::StopTask,
                Step::Send(cancel_msg(&call(), Channel::Managed))
            ]
        );
        assert_eq!(
            plan("cancel", "running", false, Channel::Terminal).unwrap(),
            vec![Step::Send(cancel_msg(&call(), Channel::Terminal))]
        );
        // Paused: the agent is already stopped, whatever the channel.
        assert_eq!(
            plan("cancel", "done", true, Channel::Terminal).unwrap(),
            vec![
                Step::Send(cancel_msg(&call(), Channel::Managed)),
                Step::Unmark
            ]
        );
        assert!(plan("cancel", "done", false, Channel::Managed).is_err());
        assert!(plan("explode", "running", false, Channel::Managed).is_err());
    }

    #[test]
    fn idle_agent_counts_as_running() {
        assert_eq!(
            plan("pause", "idle", false, Channel::Managed).unwrap(),
            vec![
                Step::StopTask,
                Step::Send(pause_msg(&call(), Channel::Managed)),
                Step::MarkPaused
            ]
        );
        assert_eq!(
            plan("cancel", "idle", false, Channel::Terminal).unwrap(),
            vec![Step::Send(cancel_msg(&call(), Channel::Terminal))]
        );
    }

    fn fg() -> SubagentCall {
        SubagentCall {
            background: false,
            ..call()
        }
    }

    #[test]
    fn foreground_agent_on_tmux_is_interrupted_with_escape() {
        let p = |action: &str| {
            plan_action(action, "running", false, Channel::Terminal, &fg(), "/t", None).unwrap()
        };
        // Esc interrupts main's turn and with it the agent: main is told it is stopped.
        assert_eq!(
            p("pause"),
            vec![
                Step::Escape,
                Step::Send(pause_msg(&fg(), Channel::Managed)),
                Step::MarkPaused
            ]
        );
        assert_eq!(
            p("cancel"),
            vec![Step::Escape, Step::Send(cancel_msg(&fg(), Channel::Managed))]
        );
        // Managed: stop_task works on a foreground agent too (control.md).
        assert_eq!(
            plan_action("pause", "running", false, Channel::Managed, &fg(), "/t", None).unwrap()[0],
            Step::StopTask
        );
        // Background agent on tmux: no Escape.
        assert!(!plan("pause", "running", false, Channel::Terminal)
            .unwrap()
            .contains(&Step::Escape));
    }

    #[test]
    fn resume_and_swap_wait_until_the_agent_stopped() {
        for st in ["running", "idle"] {
            for a in ["resume", "swap"] {
                assert_eq!(
                    plan(a, st, true, Channel::Terminal),
                    Err(STILL_RUNNING.to_string()),
                    "{a} {st}"
                );
            }
            // Cancel stays possible.
            assert!(plan("cancel", st, true, Channel::Terminal).is_ok());
        }
        assert_eq!(
            STILL_RUNNING,
            "Subagent vẫn đang chạy — chờ nó dừng hẳn rồi thử lại"
        );
        assert!(plan("resume", "error", true, Channel::Terminal).is_ok());
    }

    #[test]
    fn marker_stopped_by_ccmon_reads_done() {
        let m = |stopped| Paused {
            call: call(),
            at: 1,
            stopped,
        };
        assert_eq!(effective_status("running", Some(&m(true))), "done");
        assert_eq!(effective_status("running", Some(&m(false))), "running");
        assert_eq!(effective_status("idle", None), "idle");
        // A managed pause can be resumed at once; a terminal one waits.
        let st = effective_status("running", Some(&m(true)));
        assert!(plan("resume", st, true, Channel::Managed).is_ok());
    }

    #[test]
    fn one_action_per_session_at_a_time() {
        let g = InFlight::take("s-guard").unwrap();
        assert_eq!(
            InFlight::take("s-guard").err(),
            Some("Đang xử lý thao tác trước, thử lại sau".to_string())
        );
        assert!(InFlight::take("s-other").is_ok());
        drop(g);
        assert!(InFlight::take("s-guard").is_ok());
    }

    /// Runs steps with `fail` failing; returns the executed steps.
    fn run_with(steps: Vec<Step>, fail: fn(&Step) -> bool) -> (Result<(), String>, Vec<Step>) {
        let mut done = Vec::new();
        let r = run_steps(steps, |s| {
            done.push(s.clone());
            if fail(s) {
                Err("x".into())
            } else {
                Ok(())
            }
        });
        (r, done)
    }

    #[test]
    fn stopped_agent_keeps_a_marker_when_telling_main_fails() {
        let send = |s: &Step| matches!(s, Step::Send(_));
        // Cancel: stop ok, message fails -> marker so a retry can resend.
        let cancel = plan("cancel", "running", false, Channel::Managed).unwrap();
        let (r, done) = run_with(cancel.clone(), send);
        assert!(r.is_err());
        assert_eq!(done.last(), Some(&Step::MarkPaused));
        // Pause: marker once, from the plan.
        let pause = plan("pause", "running", false, Channel::Managed).unwrap();
        let (r, done) = run_with(pause, send);
        assert!(r.is_err());
        assert_eq!(done.iter().filter(|s| **s == Step::MarkPaused).count(), 1);
        // Stop failed: nothing else runs, no marker.
        let (r, done) = run_with(cancel.clone(), |s| *s == Step::StopTask);
        assert!(r.is_err());
        assert_eq!(done, vec![Step::StopTask]);
        // Success: no extra marker.
        let (r, done) = run_with(cancel.clone(), |_| false);
        assert!(r.is_ok());
        assert_eq!(done, cancel);
        // Tmux: nothing stopped by ccmon, no marker on failure.
        let t = plan("cancel", "running", false, Channel::Terminal).unwrap();
        let (_, done) = run_with(t, send);
        assert!(!done.contains(&Step::MarkPaused));
        // Tmux foreground: Esc went through, message failed -> marker kept.
        let f = plan_action("pause", "running", false, Channel::Terminal, &fg(), "/t", None).unwrap();
        let (_, done) = run_with(f, send);
        assert_eq!(done.last(), Some(&Step::MarkPaused));
        // Paused cancel: send fails -> keep the existing marker (no Unmark).
        let p = plan("cancel", "done", true, Channel::Managed).unwrap();
        let (_, done) = run_with(p, send);
        assert!(!done.contains(&Step::Unmark));
    }

    #[test]
    fn restart_only_for_newer_agent_file_on_idle_child() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let older = Some(t0 - Duration::from_secs(5));
        let newer = Some(t0 + Duration::from_secs(5));
        let idle = Some(RunState::Idle);
        assert_eq!(restart_needed(newer, Some(t0), idle.clone()), Ok(true));
        assert_eq!(restart_needed(older, Some(t0), idle.clone()), Ok(false));
        // Built-in/plugin agent: no user file.
        assert_eq!(restart_needed(None, Some(t0), idle), Ok(false));
        // No ccmon child: the next send launches fresh anyway.
        assert_eq!(restart_needed(newer, None, None), Ok(false));
        assert_eq!(
            restart_needed(newer, Some(t0), Some(RunState::Busy)),
            Err(BUSY_NEW_AGENT.to_string())
        );
        assert_eq!(
            BUSY_NEW_AGENT,
            "Session đang bận — chờ lượt hiện tại xong rồi đổi sang agent mới"
        );
    }

    fn cap(id: &str, path: Option<&str>) -> Capability {
        Capability {
            kind: CapKind::Agent,
            id: id.into(),
            name: id.into(),
            description: String::new(),
            scope: Scope::User,
            path: path.map(PathBuf::from),
            enabled_globally: true,
            meta: Default::default(),
        }
    }

    #[test]
    fn swap_target_must_be_listed_and_enabled() {
        let items = vec![cap("rev", Some("/a/rev.md")), cap("plug:x", None)];
        let off: BTreeSet<String> = ["off".to_string()].into();
        assert_eq!(
            pick_target(&items, &off, "rev").unwrap(),
            Some(PathBuf::from("/a/rev.md"))
        );
        assert_eq!(pick_target(&items, &off, "plug:x").unwrap(), None);
        // Built-ins are always listed.
        assert_eq!(pick_target(&items, &off, "Explore").unwrap(), None);
        assert!(pick_target(&items, &off, "ghost").is_err());
        let items = vec![cap("off", Some("/a/off.md"))];
        assert!(pick_target(&items, &off, "off").is_err());
        let mut w = cap("wf", None);
        w.kind = CapKind::Workflow;
        assert!(pick_target(&[w], &off, "wf").is_err());
    }
}
