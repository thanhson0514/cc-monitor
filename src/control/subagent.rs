//! Subagent control: find the main-thread call behind a subagent and build
//! the user-prompt messages ccmon sends to the main thread.
use crate::thread::{Thread, ThreadLink};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SubagentCall {
    pub agent_id: String,
    pub tool_use_id: String,
    pub agent_type: String,
    pub description: String,
    pub prompt: String,
    pub background: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    Managed,
    Terminal,
}

/// The main thread's `Agent`/`Task` tool call linked to `agent_id`.
pub fn find_call(main: &Thread, agent_id: &str) -> Option<SubagentCall> {
    let id = main.tool_for_link(&ThreadLink::Agent(agent_id.to_string()))?;
    for (tid, name, input, _) in main.tool_uses() {
        if tid != id || !matches!(name, "Agent" | "Task") {
            continue;
        }
        let s = |k: &str| input.get(k).and_then(|v| v.as_str());
        return Some(SubagentCall {
            agent_id: agent_id.to_string(),
            tool_use_id: tid.to_string(),
            agent_type: s("subagent_type").unwrap_or("general-purpose").to_string(),
            description: s("description").unwrap_or("").to_string(),
            prompt: s("prompt").unwrap_or("").to_string(),
            background: input
                .get("run_in_background")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        });
    }
    None
}

pub fn pause_msg(c: &SubagentCall, ch: Channel) -> String {
    let (t, id) = (&c.agent_type, &c.agent_id);
    match ch {
        Channel::Managed => format!("[ccmon] I paused subagent {t} ({id}). Do not relaunch it or act on its result; wait for my next instruction."),
        Channel::Terminal => format!("[ccmon] Stop subagent {t} now with TaskStop (task_id: {id}). This is a pause: do not relaunch it or act on its result; wait for my next instruction."),
    }
}

pub fn resume_msg(c: &SubagentCall, transcript: &str) -> String {
    format!(
        "[ccmon] Continue the paused subagent {t} ({id}). A stopped agent cannot be resumed, so start a new Agent call: subagent_type \"{t}\", description \"{d}\", run_in_background: {bg}, with exactly the original prompt below followed by this line: \"A previous attempt at this task was stopped. Its transcript is {transcript} — read it first and continue from where it stopped instead of starting over.\"\n<prompt>\n{p}\n</prompt>\nWhen it finishes, treat its result as the result of the original task.",
        t = c.agent_type, id = c.agent_id, d = c.description, bg = c.background, p = c.prompt
    )
}

pub fn swap_msg(c: &SubagentCall, new_type: &str) -> String {
    format!(
        "[ccmon] Replace the paused subagent {t} ({id}) with a new Agent call: subagent_type \"{new_type}\", description \"{d}\", run_in_background: {bg}, and exactly this prompt, verbatim:\n<prompt>\n{p}\n</prompt>\nWhen it finishes, treat its result as the result of the original task.",
        t = c.agent_type, id = c.agent_id, d = c.description, bg = c.background, p = c.prompt
    )
}

pub fn cancel_msg(c: &SubagentCall, ch: Channel) -> String {
    let (t, id) = (&c.agent_type, &c.agent_id);
    match ch {
        Channel::Managed => format!("[ccmon] I cancelled subagent {t} ({id}). Treat that task as failed: handle it and tell me what you will do next."),
        Channel::Terminal => format!("[ccmon] Stop subagent {t} now with TaskStop (task_id: {id}). I am cancelling it: treat that task as failed, handle it and tell me what you will do next."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::parse_line;

    fn call() -> SubagentCall {
        SubagentCall {
            agent_id: "aa18".into(),
            tool_use_id: "t1".into(),
            agent_type: "Explore".into(),
            description: "Audit".into(),
            prompt: "a \"b\" `c` $(d)\nline2".into(),
            background: true,
        }
    }

    #[test]
    fn find_call_from_main_link() {
        let mut t = Thread::new();
        for l in [
            r#"{"type":"assistant","uuid":"A","message":{"content":[{"type":"tool_use","id":"t1","name":"Agent","input":{"description":"Audit","subagent_type":"Explore","run_in_background":true,"prompt":"look"}}]}}"#,
            r#"{"type":"user","uuid":"B","parentUuid":"A","toolUseResult":{"agentId":"aa18","status":"async_launched"},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
        ] {
            t.push(parse_line(l).unwrap());
        }
        let c = find_call(&t, "aa18").unwrap();
        assert_eq!(c.agent_type, "Explore");
        assert!(c.background);
        assert_eq!(c.prompt, "look");
        assert_eq!(c.tool_use_id, "t1");
        assert_eq!(c.description, "Audit");
        assert_eq!(find_call(&t, "zzzz"), None);
    }

    #[test]
    fn swap_msg_keeps_prompt_verbatim() {
        let c = call();
        let m = swap_msg(&c, "Plan");
        assert!(m.contains(&format!("<prompt>\n{}\n</prompt>", c.prompt)));
        assert!(m.contains("subagent_type \"Plan\"") && m.contains("run_in_background: true"));
        let r = resume_msg(&c, "/x/t.jsonl");
        assert!(r.contains(&format!("<prompt>\n{}\n</prompt>", c.prompt)));
        assert!(r.contains("Its transcript is /x/t.jsonl —"));
    }

    #[test]
    fn messages_have_prefix_and_ids() {
        let c = call();
        let all = [
            pause_msg(&c, Channel::Managed),
            pause_msg(&c, Channel::Terminal),
            resume_msg(&c, "/t"),
            swap_msg(&c, "Plan"),
            cancel_msg(&c, Channel::Managed),
            cancel_msg(&c, Channel::Terminal),
        ];
        for m in &all {
            assert!(m.starts_with("[ccmon] ") && m.contains("aa18"), "{m}");
        }
        assert!(all[1].contains("TaskStop (task_id: aa18)"));
        assert!(all[5].contains("TaskStop (task_id: aa18)"));
    }
}
