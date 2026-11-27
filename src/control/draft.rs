//! Live draft of the assistant message a managed session is streaming.
//!
//! Claude Code writes an assistant message to the transcript only once the
//! whole API response (or its last block) is complete, so a long reply or a
//! long tool input can sit unseen for many seconds. With
//! `--include-partial-messages`, stdout carries `stream_event` lines
//! (`message_start`, `content_block_*`, `message_stop`) as the model streams;
//! [`Draft`] accumulates them so the UI can show the reply as it is written.

use serde::Serialize;
use serde_json::Value;

/// Characters of a tool's partial input JSON kept for display (the tail).
pub const TOOL_INPUT_KEEP: usize = 4000;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    /// API message id (`msg_…`), the same id the transcript lines carry.
    pub msg_id: String,
    pub blocks: Vec<DraftBlock>,
    /// `message_stop` seen: the message is complete (the transcript may lag).
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftBlock {
    /// `text`, `thinking` or `tool`.
    pub kind: &'static str,
    /// Text so far; for a tool, the tail of its partial input JSON.
    pub text: String,
    /// Tool name.
    pub name: Option<String>,
    /// Full length of a tool's partial input so far (chars).
    pub len: usize,
    pub done: bool,
}

/// How urgently a change should reach the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Structure changed (message/block start or stop): emit now.
    Now,
    /// More text in a block: emit when the throttle allows.
    Delta,
}

impl Draft {
    /// Apply one stdout line. Only main-thread stream events are used
    /// (subagent streams carry a `parent_tool_use_id`).
    pub fn apply(&mut self, v: &Value) -> Option<Change> {
        if v.get("type").and_then(Value::as_str) != Some("stream_event") {
            return None;
        }
        if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
            return None;
        }
        let e = v.get("event")?;
        let idx = e.get("index").and_then(Value::as_u64).map(|i| i as usize);
        match e.get("type").and_then(Value::as_str)? {
            "message_start" => {
                let id = e.pointer("/message/id").and_then(Value::as_str)?;
                *self = Draft {
                    msg_id: id.to_string(),
                    ..Draft::default()
                };
                Some(Change::Now)
            }
            "content_block_start" => {
                let b = e.get("content_block")?;
                let (kind, name) = match b.get("type").and_then(Value::as_str)? {
                    "text" => ("text", None),
                    "thinking" | "redacted_thinking" => ("thinking", None),
                    "tool_use" | "server_tool_use" => (
                        "tool",
                        b.get("name").and_then(Value::as_str).map(str::to_string),
                    ),
                    _ => return None,
                };
                let i = idx?;
                if self.msg_id.is_empty() {
                    return None;
                }
                while self.blocks.len() <= i {
                    self.blocks.push(DraftBlock {
                        kind,
                        text: String::new(),
                        name: None,
                        len: 0,
                        done: false,
                    });
                }
                self.blocks[i] = DraftBlock {
                    kind,
                    text: String::new(),
                    name,
                    len: 0,
                    done: false,
                };
                Some(Change::Now)
            }
            "content_block_delta" => {
                let b = self.blocks.get_mut(idx?)?;
                let d = e.get("delta")?;
                let piece = match d.get("type").and_then(Value::as_str)? {
                    "text_delta" => d.get("text"),
                    "thinking_delta" => d.get("thinking"),
                    "input_json_delta" => d.get("partial_json"),
                    _ => None,
                }
                .and_then(Value::as_str)?;
                if piece.is_empty() {
                    return None;
                }
                b.text.push_str(piece);
                if b.kind == "tool" {
                    b.len += piece.chars().count();
                    let n = b.text.chars().count();
                    if n > TOOL_INPUT_KEEP * 2 {
                        let cut = b
                            .text
                            .char_indices()
                            .nth(n - TOOL_INPUT_KEEP)
                            .map_or(0, |(i, _)| i);
                        b.text.drain(..cut);
                    }
                }
                Some(Change::Delta)
            }
            "content_block_stop" => {
                self.blocks.get_mut(idx?)?.done = true;
                Some(Change::Now)
            }
            "message_stop" => {
                if self.msg_id.is_empty() {
                    return None;
                }
                self.done = true;
                for b in &mut self.blocks {
                    b.done = true;
                }
                Some(Change::Now)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn se(event: &str) -> Value {
        serde_json::from_str(&format!(
            r#"{{"type":"stream_event","session_id":"s","parent_tool_use_id":null,"event":{event}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn accumulates_text_and_tool_input() {
        let mut d = Draft::default();
        assert_eq!(
            d.apply(&se(r#"{"type":"message_start","message":{"id":"msg_1"}}"#)),
            Some(Change::Now)
        );
        d.apply(&se(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ));
        d.apply(&se(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#));
        assert_eq!(
            d.apply(&se(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#)),
            Some(Change::Delta)
        );
        d.apply(&se(r#"{"type":"content_block_stop","index":0}"#));
        d.apply(&se(r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t","name":"Bash","input":{}}}"#));
        d.apply(&se(r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\": \"ec"}}"#));
        assert_eq!(d.msg_id, "msg_1");
        assert_eq!(d.blocks[0].text, "Hello");
        assert!(d.blocks[0].done);
        assert_eq!(d.blocks[1].kind, "tool");
        assert_eq!(d.blocks[1].name.as_deref(), Some("Bash"));
        assert_eq!(d.blocks[1].text, "{\"command\": \"ec");
        assert!(!d.done);
        d.apply(&se(r#"{"type":"message_stop"}"#));
        assert!(d.done && d.blocks[1].done);
        // The next message replaces the draft.
        d.apply(&se(r#"{"type":"message_start","message":{"id":"msg_2"}}"#));
        assert_eq!(d.msg_id, "msg_2");
        assert!(d.blocks.is_empty());
    }

    #[test]
    fn ignores_subagent_and_other_lines() {
        let mut d = Draft::default();
        let sub: Value = serde_json::from_str(
            r#"{"type":"stream_event","parent_tool_use_id":"toolu_x","event":{"type":"message_start","message":{"id":"m"}}}"#,
        )
        .unwrap();
        assert_eq!(d.apply(&sub), None);
        assert_eq!(d.apply(&serde_json::json!({"type":"assistant"})), None);
        assert!(d.msg_id.is_empty());
    }

    #[test]
    fn keeps_only_tail_of_long_tool_input() {
        let mut d = Draft::default();
        d.apply(&se(r#"{"type":"message_start","message":{"id":"m"}}"#));
        d.apply(&se(r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","name":"Write"}}"#));
        let chunk = "ă".repeat(1000);
        for _ in 0..20 {
            d.apply(&se(&format!(
                r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"input_json_delta","partial_json":"{chunk}"}}}}"#
            )));
        }
        let b = &d.blocks[0];
        assert_eq!(b.len, 20_000);
        assert!(b.text.chars().count() <= TOOL_INPUT_KEEP * 2);
        assert!(b.text.chars().count() >= TOOL_INPUT_KEEP);
    }
}
