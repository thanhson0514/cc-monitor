//! Tauri commands for controlling one subagent through its main thread:
//! pause / resume / swap / cancel. A stopped agent cannot be resumed
//! (docs/architecture/control.md, "Subagent control"), so resume and swap ask
//! main to start a new Agent call; a pause leaves a marker in ccmon state.
//!
//! The plan (`plan_action`) is pure; `Ctx::act` runs it over the session's
//! channel: ccmon's own `-p` child (`stop_task` + queued message), a non-live
//! session (`Manager::send` resumes it) or a tmux pane (main is asked to call
//! TaskStop itself).

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
use std::time::SystemTime;
use tauri::State;

use crate::cmd_agents::{session_agents, BUILTIN_AGENTS};
use crate::cmd_control::{blocking, err_text, lock, reason_text, Ctx, Route};
use crate::Shared;

pub const BUSY_NEW_AGENT: &str = "Session đang bận — chờ lượt hiện tại xong rồi đổi sang agent mới";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentDto {
    pub call: SubagentCall,
    /// running | done | error (tree row status)
    pub status: String,
    /// ccmon holds a pause marker for it.
    pub paused: bool,
    /// "managed" | "tmux"; `None` = not controllable now (see `reason`).
    pub channel: Option<&'static str>,
    pub reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `stop_task` on ccmon's own child.
    StopTask,
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
    let running = status == "running";
    // Managed: ccmon stops the task itself; terminal: main is asked to.
    let stop_and = |msg: String| match ch {
        Channel::Managed => vec![Step::StopTask, Step::Send(msg)],
        Channel::Terminal => vec![Step::Send(msg)],
    };
    match action {
        "pause" => {
            if paused {
                return Err("Subagent đã được tạm dừng".into());
            }
            if !running {
                return Err("Subagent không còn chạy — không tạm dừng được".into());
            }
            let mut v = stop_and(pause_msg(call, ch));
            v.push(Step::MarkPaused);
            Ok(v)
        }
        "resume" | "swap" => {
            if !paused {
                return Err("Subagent chưa được ccmon tạm dừng".into());
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
                Ok(stop_and(cancel_msg(call, ch)))
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
    let status = st
        .tree(discovery::now_ms())
        .into_iter()
        .find(|r| r.id == id)
        .map(|r| crate::status_str(r.status))?;
    let s = st.sessions.get(sid)?;
    let call = find_call(&s.main, aid)?;
    let transcript = s.agents.get(aid)?.path.clone();
    Some(Info {
        sid: sid.clone(),
        call,
        status: status.to_string(),
        transcript,
    })
}

fn is_paused(sid: &str, aid: &str) -> bool {
    state::load_paused(sid)
        .iter()
        .any(|p| p.call.agent_id == aid)
}

fn set_marker(sid: &str, call: &SubagentCall, on: bool) -> Result<(), String> {
    let mut v = state::load_paused(sid);
    v.retain(|p| p.call.agent_id != call.agent_id);
    if on {
        v.push(Paused {
            call: call.clone(),
            at: discovery::now_ms(),
        });
    }
    state::save_paused(sid, &v).map_err(|e| format!("Không lưu được trạng thái tạm dừng: {e}"))
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

fn subagent_dto(ctx: &Ctx, key: &str) -> Option<SubagentDto> {
    let i = info(ctx, key)?;
    let (channel, reason) = match ctx.route(&i.sid) {
        Ok(Route::Tmux(_)) => (Some("tmux"), None),
        Ok(_) => (Some("managed"), None),
        Err(code) => (None, Some(code)),
    };
    Some(SubagentDto {
        paused: is_paused(&i.sid, &i.call.agent_id),
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
        Route::Tmux(p) => tmux::send_text(p, text).map_err(|e| format!("Lỗi tmux: {e}")),
    }
}

fn act(ctx: &Ctx, key: &str, action: &str, agent: Option<&str>) -> Result<(), String> {
    let i = info(ctx, key)
        .ok_or("Không tìm thấy lệnh gọi subagent này (agent lồng nhau hoặc trong workflow)")?;
    let sid = i.sid.as_str();
    let paused = is_paused(sid, &i.call.agent_id);
    // Routed (tmux checked) right before the steps run.
    let mut route = ctx.route(sid).map_err(reason_text)?;
    let ch = match route {
        Route::Tmux(_) => Channel::Terminal,
        _ => Channel::Managed,
    };
    let steps = plan_action(
        action,
        &i.status,
        paused,
        ch,
        &i.call,
        &i.transcript.to_string_lossy(),
        agent,
    )?;
    if action == "swap" {
        let name = agent.unwrap_or_default();
        let ov = effective(&state::load_overlay(sid), &state::load_registry());
        let items = session_agents(ctx.store.clone(), ctx.root.clone(), sid);
        let file = pick_target(&items, &ov.disabled_agents, name)?;
        // tmux sessions hot-reload agents; a ccmon child must be restarted.
        if matches!(route, Route::Managed) {
            let mut m = lock(&ctx.manager);
            let need = restart_needed(
                file.as_deref().and_then(mtime),
                m.started(sid),
                m.state(sid),
            )?;
            if need {
                m.stop(sid);
                route = Route::Resume;
            }
        }
    }
    let mut stopped = false;
    let mut err: Option<String> = None;
    for step in steps {
        match step {
            Step::StopTask if err.is_none() => {
                let r = match route {
                    Route::Managed => lock(&ctx.manager)
                        .stop_task(sid, &i.call.agent_id)
                        .map_err(|e| err_text(&e)),
                    _ => Err("Session không chạy trong ccmon — không dừng được subagent".into()),
                };
                match r {
                    Ok(()) => stopped = true,
                    Err(e) => err = Some(e),
                }
            }
            Step::Send(t) if err.is_none() => err = send(ctx, &route, sid, &t).err(),
            // A stopped agent keeps its marker even if telling main failed,
            // so it can still be resumed.
            Step::MarkPaused if err.is_none() || stopped => {
                if let Err(e) = set_marker(sid, &i.call, true) {
                    err.get_or_insert(e);
                }
            }
            Step::Unmark if err.is_none() => err = set_marker(sid, &i.call, false).err(),
            _ => {}
        }
    }
    err.map_or(Ok(()), Err)
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
