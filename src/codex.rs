//! Parse Codex rollout files (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`) into the same
//! `Event` model used for Claude Code, so threads, the tree and the UI work unchanged.
//!
//! Observed format (Codex VS Code extension, 2026-10): one JSON object per line with
//! `timestamp`, `type` and `payload`. Lines have no uuid chain, so the parser numbers events
//! sequentially and links each to the previous one. Undocumented format: every field is optional.

use crate::event::{
    parse_ts, truncate, Block, Event, Kind, RateInfo, ToolLinkInfo, Usage, RESULT_PREVIEW_BYTES,
};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Metadata from a rollout's first line (`session_meta`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CodexMeta {
    pub id: String,
    pub cwd: Option<String>,
    /// Set for sub-agents spawned by another thread.
    pub parent_id: Option<String>,
    pub agent_path: Option<String>,
    pub nickname: Option<String>,
}

pub fn parse_meta(first_line: &str) -> Option<CodexMeta> {
    let v: Value = serde_json::from_str(first_line).ok()?;
    if v.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    let p = v.get("payload")?;
    let spawn = p.pointer("/source/subagent/thread_spawn");
    let s = |v: Option<&Value>, k: &str| {
        v.and_then(|v| v.get(k))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    Some(CodexMeta {
        id: s(Some(p), "id").or_else(|| s(Some(p), "session_id"))?,
        cwd: s(Some(p), "cwd"),
        parent_id: s(spawn, "parent_thread_id"),
        agent_path: s(spawn, "agent_path"),
        nickname: s(spawn, "agent_nickname").or_else(|| s(Some(p), "agent_nickname")),
    })
}

/// Per-file parser state.
#[derive(Debug, Default)]
pub struct CodexParser {
    seq: u64,
    last: Option<String>,
    /// call_id → tool name (to interpret outputs).
    calls: HashMap<String, String>,
}

/// Lines that are large and never displayed.
fn quick_skip(line: &str) -> bool {
    let head = &line.as_bytes()[..line.len().min(160)];
    let has = |pat: &[u8]| head.windows(pat.len()).any(|w| w == pat);
    has(b"\"type\":\"world_state\"")
        || has(b"\"type\":\"token_usage_record\"")
        || (has(b"\"type\":\"response_item\"") && has(b"\"type\":\"message\""))
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `exec` custom tool input is JavaScript calling `tools.exec_command({"cmd": ...})`.
fn exec_cmd(js: &str) -> Option<String> {
    let i = js.find("\"cmd\":\"")? + 7;
    let rest = &js[i..];
    let mut end = None;
    let mut esc = false;
    for (k, c) in rest.char_indices() {
        if esc {
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            end = Some(k);
            break;
        }
    }
    serde_json::from_str::<String>(&format!("\"{}\"", &rest[..end?])).ok()
}

/// Map a Codex tool call onto the names the UI renders specially.
fn normalize_tool(name: &str, input: Value) -> (String, Value) {
    match name {
        "exec" => match input.as_str().and_then(exec_cmd) {
            Some(cmd) => ("Bash".into(), json!({ "command": cmd })),
            None => ("exec".into(), json!({ "code": input })),
        },
        "exec_command" | "shell" | "local_shell" => {
            let cmd = input
                .get("cmd")
                .or_else(|| input.get("command"))
                .map(|c| match c {
                    Value::Array(a) => a
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "),
                    other => text_of(other),
                })
                .unwrap_or_default();
            ("Bash".into(), json!({ "command": cmd }))
        }
        "spawn_agent" => {
            let task = input
                .get("task_name")
                .and_then(Value::as_str)
                .unwrap_or("agent");
            (
                "Agent".into(),
                json!({ "description": task, "subagent_type": "codex" }),
            )
        }
        _ => (name.to_string(), input),
    }
}

fn is_error_output(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("script failed")
        || t.contains("process exited with code 1")
        || t.starts_with("error")
}

impl CodexParser {
    pub fn new() -> Self {
        Self::default()
    }

    fn chain(&mut self, mut ev: Event) -> Event {
        self.seq += 1;
        let id = format!("c{}", self.seq);
        ev.parent = self.last.replace(id.clone());
        ev.uuid = Some(id);
        ev
    }

    /// Events for one line (zero, one or two).
    pub fn parse(&mut self, line: &str) -> Vec<Event> {
        if line.trim().is_empty() || quick_skip(line) {
            return vec![];
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return vec![];
        };
        let ts = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_ts);
        let ty = v.get("type").and_then(Value::as_str).unwrap_or_default();
        let Some(p) = v.get("payload") else {
            return vec![];
        };
        let pty = p.get("type").and_then(Value::as_str).unwrap_or_default();
        let turn = p
            .get("turn_id")
            .or_else(|| p.pointer("/internal_chat_message_metadata_passthrough/turn_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut out = Vec::new();
        let assistant = |blocks: Vec<Block>| {
            let mut e = Event::new(Kind::Assistant);
            e.ts = ts;
            e.msg_id = turn.clone();
            e.blocks = blocks;
            e
        };
        match (ty, pty) {
            ("session_meta", _) => {
                let mut e = Event::new(Kind::Meta);
                e.cwd = p.get("cwd").and_then(Value::as_str).map(str::to_string);
                e.git_branch = p
                    .pointer("/git/branch")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                out.push(e);
            }
            ("turn_context", _) | ("event_msg", "thread_settings_applied") => {
                let mut e = Event::new(Kind::Meta);
                e.model = p
                    .get("model")
                    .or_else(|| p.pointer("/thread_settings/model"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                e.effort = p
                    .get("effort")
                    .or_else(|| p.pointer("/thread_settings/reasoning_effort"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                out.push(e);
            }
            ("event_msg", "user_message") => {
                let mut e = Event::new(Kind::User);
                e.ts = ts;
                let mut text = p
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                for img in p
                    .get("local_images")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(path) = img
                        .as_str()
                        .or_else(|| img.get("path").and_then(Value::as_str))
                    {
                        text.push_str(&format!("\n[Image: source: {path}]"));
                    }
                }
                e.prompt = Some(text);
                out.push(self.chain(e));
            }
            ("event_msg", "agent_message") => {
                let msg = p.get("message").and_then(Value::as_str).unwrap_or_default();
                if !msg.trim().is_empty() {
                    out.push(self.chain(assistant(vec![Block::Text(msg.to_string())])));
                }
            }
            ("response_item", "reasoning") => {
                let summary = p
                    .get("summary")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|s| text_of(s.get("text").unwrap_or(s)))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                out.push(self.chain(assistant(vec![Block::Thinking(summary)])));
            }
            ("response_item", "custom_tool_call") | ("response_item", "function_call") => {
                let Some(id) = p.get("call_id").and_then(Value::as_str) else {
                    return vec![];
                };
                let name = p.get("name").and_then(Value::as_str).unwrap_or("tool");
                let raw = if pty == "function_call" {
                    let a = p.get("arguments").and_then(Value::as_str).unwrap_or("{}");
                    serde_json::from_str(a).unwrap_or_else(|_| json!({ "arguments": a }))
                } else {
                    p.get("input").cloned().unwrap_or(Value::Null)
                };
                let (name, input) = normalize_tool(name, raw);
                self.calls.insert(id.to_string(), name.clone());
                out.push(self.chain(assistant(vec![Block::ToolUse {
                    id: id.to_string(),
                    name,
                    input,
                }])));
            }
            ("response_item", "custom_tool_call_output")
            | ("response_item", "function_call_output") => {
                let Some(id) = p.get("call_id").and_then(Value::as_str) else {
                    return vec![];
                };
                let text = text_of(p.get("output").unwrap_or(&Value::Null));
                let mut e = Event::new(Kind::User);
                e.ts = ts;
                if self.calls.get(id).map(String::as_str) == Some("Agent") {
                    let task = serde_json::from_str::<Value>(&text).ok().and_then(|v| {
                        v.get("task_name")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    });
                    if let Some(t) = task {
                        e.link = Some(ToolLinkInfo {
                            agent_id: Some(format!("path:{t}")),
                            ..Default::default()
                        });
                    }
                }
                e.blocks = vec![Block::ToolResult {
                    tool_use_id: id.to_string(),
                    is_error: is_error_output(&text),
                    content: truncate(text, RESULT_PREVIEW_BYTES),
                }];
                out.push(self.chain(e));
            }
            ("event_msg", "patch_apply_end") => {
                let id = p
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("patch")
                    .to_string();
                let files: Vec<String> = match p.get("changes") {
                    Some(Value::Object(m)) => m.keys().cloned().collect(),
                    _ => vec![],
                };
                let ok = p.get("success").and_then(Value::as_bool).unwrap_or(true);
                out.push(self.chain(assistant(vec![Block::ToolUse {
                    id: id.clone(),
                    name: "Patch".into(),
                    input: json!({ "files": files }),
                }])));
                let mut r = Event::new(Kind::User);
                r.ts = ts;
                let text = format!(
                    "{}{}",
                    p.get("stdout").and_then(Value::as_str).unwrap_or_default(),
                    p.get("stderr").and_then(Value::as_str).unwrap_or_default()
                );
                r.blocks = vec![Block::ToolResult {
                    tool_use_id: id,
                    is_error: !ok,
                    content: truncate(text, RESULT_PREVIEW_BYTES),
                }];
                out.push(self.chain(r));
            }
            ("event_msg", "web_search_end") => {
                let id = p
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("search")
                    .to_string();
                let q = p.get("query").and_then(Value::as_str).unwrap_or_default();
                out.push(self.chain(assistant(vec![Block::ToolUse {
                    id: id.clone(),
                    name: "WebSearch".into(),
                    input: json!({ "query": q }),
                }])));
                let mut r = Event::new(Kind::User);
                r.ts = ts;
                r.blocks = vec![Block::ToolResult {
                    tool_use_id: id,
                    is_error: false,
                    content: String::new(),
                }];
                out.push(self.chain(r));
            }
            ("event_msg", "token_count") => {
                let mut e = Event::new(Kind::Meta);
                if let Some(u) = p.pointer("/info/last_token_usage") {
                    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                    let cached = n("cached_input_tokens");
                    e.usage = Some(Usage {
                        input: n("input_tokens").saturating_sub(cached),
                        cache_read: cached,
                        cache_create: n("cache_write_input_tokens"),
                        output: n("output_tokens"),
                    });
                }
                e.window = p
                    .pointer("/info/model_context_window")
                    .and_then(Value::as_u64);
                if let Some(r) = p.get("rate_limits").filter(|r| r.is_object()) {
                    e.rate = Some(RateInfo {
                        five_h: r.pointer("/primary/used_percent").and_then(Value::as_f64),
                        seven_d: r.pointer("/secondary/used_percent").and_then(Value::as_f64),
                    });
                }
                out.push(e);
            }
            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::{Item, Thread, ToolState};

    const LINES: &[&str] = &[
        r#"{"timestamp":"2026-10-13T06:24:52.754Z","type":"session_meta","payload":{"id":"S1","cwd":"/home/u/proj"}}"#,
        r#"{"timestamp":"2026-10-13T06:24:53.000Z","type":"turn_context","payload":{"turn_id":"T1","model":"gpt-5"}}"#,
        r#"{"timestamp":"2026-10-13T06:24:54.130Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"huge"}]}}"#,
        r#"{"timestamp":"2026-10-13T06:24:54.142Z","type":"event_msg","payload":{"type":"user_message","message":"Setup SWC","local_images":["/tmp/a.png"]}}"#,
        r#"{"timestamp":"2026-10-13T06:24:56.087Z","type":"response_item","payload":{"type":"reasoning","summary":[],"encrypted_content":"gAAA"}}"#,
        r#"{"timestamp":"2026-10-13T06:24:56.356Z","type":"event_msg","payload":{"type":"agent_message","message":"Mình sẽ kiểm tra","turn_id":"T1"}}"#,
        r#"{"timestamp":"2026-10-13T06:24:57.475Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"c1","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"pwd && rg --files -g 'a b'\"})"}}"#,
        r#"{"timestamp":"2026-10-13T06:24:57.810Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"c1","output":[{"type":"input_text","text":"Script completed\n"},{"type":"input_text","text":"/home/u/proj"}]}}"#,
        r#"{"timestamp":"2026-10-13T06:24:57.900Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":22312,"cached_input_tokens":6912,"output_tokens":255},"model_context_window":258400},"rate_limits":{"primary":{"used_percent":12.5},"secondary":{"used_percent":40.0}}}}"#,
        r#"{"timestamp":"2026-10-13T06:25:00.000Z","type":"response_item","payload":{"type":"function_call","call_id":"c2","name":"spawn_agent","arguments":"{\"task_name\":\"review\",\"message\":\"gAAA\"}"}}"#,
        r#"{"timestamp":"2026-10-13T06:25:01.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c2","output":"{\"task_name\":\"/root/review\"}"}}"#,
        r#"{"timestamp":"2026-10-13T06:26:00.000Z","type":"event_msg","payload":{"type":"patch_apply_end","call_id":"p1","success":true,"stdout":"Success. Updated","changes":{"/home/u/proj/a.ts":{"type":"update"}}}}"#,
    ];

    fn thread() -> Thread {
        let mut parser = CodexParser::new();
        let mut t = Thread::new();
        for l in LINES {
            for e in parser.parse(l) {
                t.push(e);
            }
        }
        t
    }

    #[test]
    fn maps_codex_rollout_to_items() {
        let mut t = thread();
        assert_eq!(t.model(), Some("gpt-5"));
        assert_eq!(t.cwd(), Some("/home/u/proj"));
        assert_eq!(t.last_usage().unwrap().context(), 22312);
        assert_eq!(t.window(), Some(258400));
        assert_eq!(t.rate().unwrap().seven_d, Some(40.0));
        assert_eq!(t.user_files()[0].path, "/tmp/a.png");
        let items: Vec<Item> = t
            .items()
            .iter()
            .filter(|i| !matches!(i, Item::Turn { .. }))
            .cloned()
            .collect();
        assert!(matches!(&items[0], Item::Prompt { text, .. } if text.starts_with("Setup SWC")));
        assert!(matches!(&items[1], Item::Thinking { .. }));
        assert!(matches!(&items[2], Item::Text { text, .. } if text == "Mình sẽ kiểm tra"));
        match &items[3] {
            Item::Tool(tool) => {
                assert_eq!(tool.name, "Bash");
                assert_eq!(tool.input["command"], "pwd && rg --files -g 'a b'");
                assert_eq!(tool.state, ToolState::Ok);
                assert!(tool.result.contains("/home/u/proj"));
            }
            i => panic!("{i:?}"),
        }
        assert!(matches!(&items[4], Item::Tool(tool) if tool.name == "Agent"
            && tool.link == Some(crate::thread::ThreadLink::Agent("path:/root/review".into()))));
        assert!(
            matches!(&items[5], Item::Tool(tool) if tool.name == "Patch" && tool.input["files"][0] == "/home/u/proj/a.ts")
        );
    }

    #[test]
    fn meta_of_subagent() {
        let m = parse_meta(r#"{"type":"session_meta","payload":{"id":"C","cwd":"/p","source":{"subagent":{"thread_spawn":{"parent_thread_id":"P","agent_path":"/root/review","agent_nickname":"James"}}}}}"#).unwrap();
        assert_eq!(m.parent_id.as_deref(), Some("P"));
        assert_eq!(m.agent_path.as_deref(), Some("/root/review"));
        assert_eq!(m.nickname.as_deref(), Some("James"));
        assert!(parse_meta(r#"{"type":"event_msg","payload":{}}"#).is_none());
    }
}

#[cfg(test)]
mod real_data {
    use super::*;
    use crate::thread::{Item, Thread};

    #[test]
    #[ignore = "reads the real ~/.codex/sessions"]
    fn parses_all_local_rollouts() {
        let root = crate::platform::codex_home().join("sessions");
        let mut stack = vec![root];
        let (mut files, mut items, mut tools, mut bash) = (0, 0, 0, 0);
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "jsonl") {
                    let text = std::fs::read_to_string(&p).unwrap_or_default();
                    let mut parser = CodexParser::new();
                    let mut t = Thread::new();
                    for l in text.lines() {
                        for ev in parser.parse(l) {
                            t.push(ev);
                        }
                    }
                    files += 1;
                    for it in t.items() {
                        items += 1;
                        if let Item::Tool(tool) = it {
                            tools += 1;
                            if tool.name == "Bash" {
                                bash += 1;
                            }
                        }
                    }
                }
            }
        }
        println!("{files} file, {items} mục, {tools} tool ({bash} Bash)");
        assert!(files == 0 || items > 0);
    }
}
