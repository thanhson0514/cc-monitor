//! Parse one line of a Claude Code transcript (JSONL) into a compact `Event`.
//!
//! The transcript format is internal to Claude Code and undocumented; every field is
//! optional and unknown line types are ignored. Parsing must never panic.

use serde::Deserialize;
use serde_json::Value;

/// Max bytes of a tool result kept in memory for display.
pub const RESULT_PREVIEW_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Text(String),
    Thinking(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        is_error: bool,
        content: String,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub cache_read: u64,
    pub cache_create: u64,
    pub output: u64,
}

impl Usage {
    /// Tokens occupying the context window for this request.
    pub fn context(&self) -> u64 {
        self.input + self.cache_read + self.cache_create
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// Session metadata only (model, cwd, usage, limits); not shown as an item.
    Meta,
    /// A line we do not display (attachment, unknown type) that still sits in the
    /// `parentUuid` chain; kept so the live branch is not cut.
    Link,
    /// A file or directory the user attached (`@path` mention).
    Attachment {
        path: String,
        is_dir: bool,
    },
    User,
    Assistant,
    System {
        subtype: String,
        text: String,
    },
    Title(String),
    CostState {
        added: u64,
        removed: u64,
    },
    /// A `<task-notification>` about a finished background task. Stays in the
    /// `parentUuid` chain like `Link`.
    TaskNotice(TaskNotice),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskNotice {
    pub task_id: String,
    pub tool_use_id: Option<String>,
    pub status: String,
    pub summary: String,
    pub output_file: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompactInfo {
    pub pre: u64,
    pub post: u64,
}

/// Fields of `toolUseResult` that link a tool call to another thread.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolLinkInfo {
    pub agent_id: Option<String>,
    pub run_id: Option<String>,
    pub workflow_name: Option<String>,
    /// `toolUseResult.status` (e.g. `async_launched`).
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub kind: Kind,
    pub uuid: Option<String>,
    pub parent: Option<String>,
    pub logical_parent: Option<String>,
    /// Milliseconds since Unix epoch.
    pub ts: Option<i64>,
    pub blocks: Vec<Block>,
    pub prompt: Option<String>,
    pub model: Option<String>,
    /// API message id (one assistant response may span several lines).
    pub msg_id: Option<String>,
    pub usage: Option<Usage>,
    pub effort: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub link: Option<ToolLinkInfo>,
    pub compact: Option<CompactInfo>,
    pub is_compact_summary: bool,
    pub is_meta: bool,
    /// `thinkingDurationMs` of an assistant line.
    pub thinking_ms: Option<u64>,
    /// Plan usage limits reported by the tool (Codex `rate_limits`).
    pub rate: Option<RateInfo>,
    /// Context window size reported by the tool (Codex `model_context_window`).
    pub window: Option<u64>,
    /// `toolUseResult.backgroundTaskId` of a tool result that started a background task.
    pub bg_task_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RateInfo {
    /// 5-hour window used %.
    pub five_h: Option<f64>,
    /// 7-day window used %.
    pub seven_d: Option<f64>,
}

impl Event {
    /// An event with every optional field empty.
    pub fn new(kind: Kind) -> Self {
        Event {
            kind,
            uuid: None,
            parent: None,
            logical_parent: None,
            ts: None,
            blocks: Vec::new(),
            prompt: None,
            model: None,
            msg_id: None,
            usage: None,
            effort: None,
            cwd: None,
            git_branch: None,
            link: None,
            compact: None,
            is_compact_summary: false,
            is_meta: false,
            thinking_ms: None,
            rate: None,
            window: None,
            bg_task_id: None,
        }
    }
}

const IGNORED_PREFIXES: &[&str] = &[
    "api-request",
    "attachment",
    "file-history",
    "queue-operation",
    "mode",
    "session-owner",
    "atis-latch",
    "last-prompt",
    "tools-used",
];

#[derive(Deserialize)]
struct Head<'a> {
    #[serde(borrow, rename = "type")]
    kind: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow, default)]
    uuid: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow, default, rename = "parentUuid")]
    parent_uuid: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow, default, rename = "logicalParentUuid")]
    logical_parent_uuid: Option<std::borrow::Cow<'a, str>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Raw {
    uuid: Option<String>,
    parent_uuid: Option<String>,
    logical_parent_uuid: Option<String>,
    timestamp: Option<String>,
    message: Option<RawMsg>,
    effort: Option<String>,
    cwd: Option<String>,
    git_branch: Option<String>,
    tool_use_result: Option<Value>,
    is_compact_summary: bool,
    is_meta: bool,
    subtype: Option<String>,
    content: Option<Value>,
    compact_metadata: Option<Value>,
    ai_title: Option<String>,
    total_lines_added: Option<u64>,
    total_lines_removed: Option<u64>,
    thinking_duration_ms: Option<u64>,
    attachment: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawMsg {
    id: Option<String>,
    model: Option<String>,
    usage: Option<RawUsage>,
    content: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawUsage {
    input_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    output_tokens: u64,
}

fn is_ignored(kind: &str) -> bool {
    IGNORED_PREFIXES.iter().any(|p| kind.starts_with(p))
}

/// Cheap pre-check on the first bytes so multi-MB `api-request-*` lines (never part of
/// the conversation chain) are skipped without tokenizing the whole line.
fn quick_skip(line: &str) -> bool {
    let head = &line.as_bytes()[..line.len().min(256)];
    String::from_utf8_lossy(head).contains("\"type\":\"api-request")
}

fn unescape_html(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = s.find(&open)? + open.len();
    let end = start + s[start..].find(&format!("</{name}>"))?;
    Some(s[start..end].trim())
}

/// Parse the `<task-notification>` XML-ish block Claude Code queues when a background
/// task finishes. Tolerant: only `task-id` is required.
pub fn parse_task_notification(s: &str) -> Option<TaskNotice> {
    let body = &s[s.find("<task-notification>")?..];
    let t = |n: &str| tag(body, n).filter(|v| !v.is_empty()).map(unescape_html);
    Some(TaskNotice {
        task_id: t("task-id")?,
        tool_use_id: t("tool-use-id"),
        status: t("status").unwrap_or_default(),
        summary: t("summary").unwrap_or_default(),
        output_file: t("output-file"),
    })
}

fn attachment_event(line: &str, head: &Head) -> Option<Event> {
    let mut ev = link_event(head)?;
    if line.contains("queued_command") && line.contains("<task-notification>") {
        if let Ok(raw) = serde_json::from_str::<Raw>(line) {
            let notice = raw
                .attachment
                .as_ref()
                .filter(|a| a.get("type").and_then(Value::as_str) == Some("queued_command"))
                .and_then(|a| a.get("prompt").and_then(Value::as_str))
                .and_then(parse_task_notification);
            if let Some(n) = notice {
                ev.kind = Kind::TaskNotice(n);
                ev.ts = raw.timestamp.as_deref().and_then(parse_ts);
                return Some(ev);
            }
        }
    }
    let head_bytes = &line.as_bytes()[..line.len().min(4096)];
    let has = |pat: &[u8]| head_bytes.windows(pat.len()).any(|w| w == pat);
    if !(has(b"\"type\":\"file\"") || has(b"\"type\":\"directory\"")) {
        return Some(ev);
    }
    let Ok(raw) = serde_json::from_str::<Raw>(line) else {
        return Some(ev);
    };
    let found = raw.attachment.as_ref().and_then(|a| {
        let s = |k: &str| a.get(k).and_then(Value::as_str).map(str::to_string);
        match a.get("type").and_then(Value::as_str) {
            Some("file") => s("filename").map(|p| (p, false)),
            Some("directory") => s("path").map(|p| (p, true)),
            _ => None,
        }
    });
    if let Some((path, is_dir)) = found {
        ev.kind = Kind::Attachment { path, is_dir };
        ev.ts = raw.timestamp.as_deref().and_then(parse_ts);
    }
    Some(ev)
}

fn link_event(head: &Head) -> Option<Event> {
    let mut ev = Event::new(Kind::Link);
    ev.uuid = Some(head.uuid.as_ref()?.to_string());
    ev.parent = head.parent_uuid.as_ref().map(|c| c.to_string());
    ev.logical_parent = head.logical_parent_uuid.as_ref().map(|c| c.to_string());
    Some(ev)
}

pub fn parse_line(line: &str) -> Option<Event> {
    let line = line.trim();
    if line.is_empty() || quick_skip(line) {
        return None;
    }
    let head: Head = serde_json::from_str(line).ok()?;
    let kind_str = head.kind.as_deref()?.to_string();
    if kind_str == "attachment" {
        return attachment_event(line, &head);
    }
    if is_ignored(&kind_str) {
        return link_event(&head);
    }
    let raw: Raw = serde_json::from_str(line).ok()?;
    let kind = match kind_str.as_str() {
        "user" => Kind::User,
        "assistant" => Kind::Assistant,
        "system" => Kind::System {
            subtype: raw.subtype.clone().unwrap_or_default(),
            text: raw
                .content
                .as_ref()
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        "ai-title" => Kind::Title(raw.ai_title.clone()?),
        "cost-state" => Kind::CostState {
            added: raw.total_lines_added.unwrap_or(0),
            removed: raw.total_lines_removed.unwrap_or(0),
        },
        _ => return link_event(&head),
    };

    let mut ev = Event {
        kind,
        uuid: raw.uuid,
        parent: raw.parent_uuid,
        logical_parent: raw.logical_parent_uuid,
        ts: raw.timestamp.as_deref().and_then(parse_ts),
        blocks: Vec::new(),
        prompt: None,
        model: None,
        msg_id: None,
        usage: None,
        effort: raw.effort,
        cwd: raw.cwd,
        git_branch: raw.git_branch,
        link: None,
        compact: None,
        is_compact_summary: raw.is_compact_summary,
        is_meta: raw.is_meta,
        thinking_ms: raw.thinking_duration_ms,
        rate: None,
        window: None,
        bg_task_id: None,
    };

    if let Some(meta) = raw.compact_metadata {
        ev.compact = Some(CompactInfo {
            pre: meta.get("preTokens").and_then(Value::as_u64).unwrap_or(0),
            post: meta.get("postTokens").and_then(Value::as_u64).unwrap_or(0),
        });
    }

    if let Some(tur) = raw.tool_use_result.as_ref().filter(|v| v.is_object()) {
        let s = |k: &str| tur.get(k).and_then(Value::as_str).map(str::to_string);
        let link = ToolLinkInfo {
            agent_id: s("agentId"),
            run_id: s("runId"),
            workflow_name: s("workflowName"),
            status: s("status"),
        };
        if link.agent_id.is_some() || link.run_id.is_some() {
            ev.link = Some(link);
        }
        ev.bg_task_id = s("backgroundTaskId");
    }

    if let Some(msg) = raw.message {
        ev.model = msg.model;
        ev.msg_id = msg.id;
        ev.usage = msg.usage.map(|u| Usage {
            input: u.input_tokens,
            cache_read: u.cache_read_input_tokens,
            cache_create: u.cache_creation_input_tokens,
            output: u.output_tokens,
        });
        match msg.content {
            Some(Value::String(s)) => ev.prompt = Some(s),
            Some(Value::Array(items)) => {
                for it in items {
                    if let Some(b) = parse_block(it) {
                        ev.blocks.push(b);
                    }
                }
            }
            _ => {}
        }
    }
    // A human prompt with attachments (text + images) arrives as a content array.
    if ev.kind == Kind::User
        && ev.prompt.is_none()
        && !ev.blocks.is_empty()
        && ev.blocks.iter().all(|b| matches!(b, Block::Text(_)))
    {
        let text: Vec<&str> = ev
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        ev.prompt = Some(text.join("\n"));
        ev.blocks.clear();
    }
    if ev.kind == Kind::User {
        let notice = ev
            .prompt
            .as_deref()
            .filter(|p| p.trim_start().starts_with("<task-notification>"))
            .and_then(parse_task_notification);
        if let Some(n) = notice {
            ev.kind = Kind::TaskNotice(n);
        }
    }
    Some(ev)
}

fn parse_block(v: Value) -> Option<Block> {
    let ty = v.get("type")?.as_str()?;
    match ty {
        "text" => Some(Block::Text(v.get("text")?.as_str()?.to_string())),
        "thinking" => Some(Block::Thinking(
            v.get("thinking")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )),
        "tool_use" => Some(Block::ToolUse {
            id: v.get("id")?.as_str()?.to_string(),
            name: v.get("name")?.as_str()?.to_string(),
            input: v.get("input").cloned().unwrap_or(Value::Null),
        }),
        "tool_result" => Some(Block::ToolResult {
            tool_use_id: v.get("tool_use_id")?.as_str()?.to_string(),
            is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            content: truncate(result_text(v.get("content")), RESULT_PREVIEW_BYTES),
        }),
        _ => None,
    }
}

fn result_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                Some("text") => p.get("text").and_then(Value::as_str).map(str::to_string),
                Some("image") => Some("[image]".to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n… (cắt bớt)");
    }
    s
}

/// Parse an RFC 3339 UTC timestamp such as `2026-11-10T02:05:00.422Z` into epoch ms.
pub fn parse_ts(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r)?.parse().ok() };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut ms = 0i64;
    if b.get(19) == Some(&b'.') {
        let frac: String = s[20..].chars().take_while(|c| c.is_ascii_digit()).collect();
        let f3 = format!("{:0<3}", &frac[..frac.len().min(3)]);
        ms = f3.parse().ok()?;
    }
    // days_from_civil (Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + h) * 60 + mi) * 60_000 + sec * 1000 + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_ignored_types() {
        assert!(parse_line(r#"{"type":"api-request-blob","hash":"x","message":{}}"#).is_none());
        assert!(parse_line(r#"{"hash":"x","type":"attachment"}"#).is_none());
        assert!(parse_line("{not json").is_none());
        assert!(parse_line("").is_none());
        assert!(parse_line(r#"{"type":"something-new","x":1}"#).is_none());
        // Chain members we don't display are kept as links.
        let l = parse_line(r#"{"type":"attachment","uuid":"u","parentUuid":"p","attachment":{}}"#)
            .unwrap();
        assert_eq!(
            (l.kind, l.uuid.as_deref(), l.parent.as_deref()),
            (Kind::Link, Some("u"), Some("p"))
        );
    }

    #[test]
    fn parses_assistant_tool_use() {
        let l = r#"{"parentUuid":"p","type":"assistant","uuid":"u1","timestamp":"2026-11-10T02:05:00.422Z","effort":"high","message":{"model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_read_input_tokens":30142,"cache_creation_input_tokens":41677,"output_tokens":168},"content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}]}}"#;
        let e = parse_line(l).unwrap();
        assert_eq!(e.kind, Kind::Assistant);
        assert_eq!(e.uuid.as_deref(), Some("u1"));
        assert_eq!(e.parent.as_deref(), Some("p"));
        assert_eq!(e.effort.as_deref(), Some("high"));
        assert_eq!(e.usage.unwrap().cache_read, 30142);
        assert_eq!(e.usage.unwrap().context(), 2 + 30142 + 41677);
        assert_eq!(e.ts, Some(1_794_276_300_422));
        match &e.blocks[0] {
            Block::ToolUse { id, name, .. } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "Bash");
            }
            b => panic!("{b:?}"),
        }
    }

    #[test]
    fn parses_user_prompt_and_tool_result() {
        let p = parse_line(
            r#"{"type":"user","uuid":"a","message":{"role":"user","content":"xin chào"}}"#,
        )
        .unwrap();
        assert_eq!(p.prompt.as_deref(), Some("xin chào"));
        let r = parse_line(r#"{"type":"user","uuid":"b","toolUseResult":{"agentId":"abc","status":"async_launched"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":true,"content":[{"type":"text","text":"l1"},{"type":"text","text":"l2"}]}]}}"#).unwrap();
        assert_eq!(
            r.blocks[0],
            Block::ToolResult {
                tool_use_id: "toolu_1".into(),
                is_error: true,
                content: "l1\nl2".into()
            }
        );
        assert_eq!(r.link.unwrap().agent_id.as_deref(), Some("abc"));
    }

    #[test]
    fn user_content_array_without_tool_result_is_a_prompt() {
        let e = parse_line(r#"{"type":"user","uuid":"a","message":{"content":[{"type":"text","text":"xem ảnh"},{"type":"image","source":{}}]}}"#).unwrap();
        assert_eq!(e.prompt.as_deref(), Some("xem ảnh"));
        assert!(e.blocks.is_empty());
    }

    #[test]
    fn parses_file_attachment() {
        let e = parse_line(r#"{"type":"attachment","uuid":"u","parentUuid":"p","timestamp":"2026-11-25T00:00:00.000Z","attachment":{"type":"file","filename":"/a/b.md","content":{}}}"#).unwrap();
        assert_eq!(
            e.kind,
            Kind::Attachment {
                path: "/a/b.md".into(),
                is_dir: false
            }
        );
        assert_eq!(e.parent.as_deref(), Some("p"));
        let o =
            parse_line(r#"{"type":"attachment","uuid":"u","attachment":{"type":"output_style"}}"#)
                .unwrap();
        assert_eq!(o.kind, Kind::Link);
    }

    #[test]
    fn attachment_with_multibyte_at_4096_does_not_panic() {
        let pad = "ệ".repeat(2000);
        let l = format!(
            r#"{{"type":"attachment","uuid":"u","x":"{pad}","attachment":{{"type":"file","filename":"/a"}}}}"#
        );
        for cut in 0..4 {
            let line = format!("{}{}", " ".repeat(cut), l);
            assert!(parse_line(&line).is_some());
        }
    }

    #[test]
    fn parses_compact_boundary() {
        let e = parse_line(r#"{"parentUuid":null,"logicalParentUuid":"prev","type":"system","subtype":"compact_boundary","content":"Conversation compacted","compactMetadata":{"trigger":"manual","preTokens":936115,"postTokens":21590},"uuid":"cb"}"#).unwrap();
        assert!(
            matches!(e.kind, Kind::System { ref subtype, .. } if subtype == "compact_boundary")
        );
        assert_eq!(e.parent, None);
        assert_eq!(e.logical_parent.as_deref(), Some("prev"));
        assert_eq!(
            e.compact,
            Some(CompactInfo {
                pre: 936115,
                post: 21590
            })
        );
    }

    #[test]
    fn parses_title_and_cost() {
        assert_eq!(
            parse_line(r#"{"type":"ai-title","aiTitle":"T"}"#)
                .unwrap()
                .kind,
            Kind::Title("T".into())
        );
        assert_eq!(
            parse_line(r#"{"type":"cost-state","totalLinesAdded":29,"totalLinesRemoved":1}"#)
                .unwrap()
                .kind,
            Kind::CostState {
                added: 29,
                removed: 1
            }
        );
    }

    #[test]
    fn truncate_respects_char_boundary() {
        let s = truncate("ăăăă".to_string(), 3);
        assert!(s.starts_with('ă'));
    }

    #[test]
    fn parses_task_notification_attachment() {
        let l = r#"{"type":"attachment","uuid":"u1","parentUuid":"p","attachment":{"type":"queued_command","prompt":"<task-notification>\n<task-id>b9b5g4q3w</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n<output-file>/tmp/claude-1000/x/tasks/b9b5g4q3w.output</output-file>\n<status>failed</status>\n<summary>Background command \"a &amp;&amp; b\" failed (exit code 1)</summary>\n</task-notification>"}}"#;
        let e = parse_line(l).unwrap();
        assert_eq!(
            e.kind,
            Kind::TaskNotice(TaskNotice {
                task_id: "b9b5g4q3w".into(),
                tool_use_id: Some("toolu_1".into()),
                status: "failed".into(),
                summary: "Background command \"a && b\" failed (exit code 1)".into(),
                output_file: Some("/tmp/claude-1000/x/tasks/b9b5g4q3w.output".into())
            })
        );
        assert_eq!(e.uuid.as_deref(), Some("u1"));
        assert_eq!(e.parent.as_deref(), Some("p"));
    }

    #[test]
    fn queued_command_without_notification_stays_link() {
        let e = parse_line(r#"{"type":"attachment","uuid":"u","attachment":{"type":"queued_command","prompt":"hello"}}"#).unwrap();
        assert_eq!(e.kind, Kind::Link);
    }

    #[test]
    fn reads_background_task_id_from_tool_use_result() {
        let l = r#"{"type":"user","uuid":"u2","toolUseResult":{"backgroundTaskId":"bqat3cqah"},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Command running in background with ID: bqat3cqah."}]}}"#;
        assert_eq!(
            parse_line(l).unwrap().bg_task_id.as_deref(),
            Some("bqat3cqah")
        );
    }

    #[test]
    fn quoted_notification_tag_mid_text_stays_user() {
        let l = r#"{"type":"user","uuid":"u","message":{"content":"look at this: <task-notification><task-id>x</task-id></task-notification>"}}"#;
        assert_eq!(parse_line(l).unwrap().kind, Kind::User);
    }

    #[test]
    fn notification_also_found_in_user_text() {
        let body = "<task-notification>\\n<task-id>t9</task-id>\\n<status>completed</status>\\n<summary>it&#39;s done</summary>\\n</task-notification>";
        for content in [
            format!(r#""{body}""#),
            format!(r#"[{{"type":"text","text":"{body}"}}]"#),
        ] {
            let l = format!(
                r#"{{"type":"user","uuid":"u","parentUuid":"p","message":{{"content":{content}}}}}"#
            );
            let e = parse_line(&l).unwrap();
            let Kind::TaskNotice(n) = e.kind else {
                panic!("not a notice")
            };
            assert_eq!(
                (n.task_id.as_str(), n.status.as_str(), n.summary.as_str()),
                ("t9", "completed", "it's done")
            );
            assert_eq!(n.tool_use_id, None);
        }
    }
}
