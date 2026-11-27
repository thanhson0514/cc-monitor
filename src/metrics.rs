//! Status-bar metrics for the selected session.

use crate::thread::Thread;
use serde_json::Value;
use std::path::Path;

/// Status-line snapshots older than this are ignored.
const TEE_FRESH_MS: i64 = 600_000;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metrics {
    pub model: Option<String>,
    pub ctx: Option<u64>,
    /// (window size, estimated?)
    pub win: Option<(u64, bool)>,
    pub ctx_pct: Option<f64>,
    pub branch: Option<String>,
    pub lines: Option<(u64, u64)>,
    pub mem: Option<(u64, u64)>,
    pub effort: Option<String>,
    pub five_h: Option<f64>,
    pub seven_d: Option<f64>,
    pub compactions: u32,
}

/// (used, total) bytes from `/proc/meminfo` text; used = MemTotal − MemAvailable.
#[allow(dead_code)]
pub fn read_meminfo(text: &str) -> Option<(u64, u64)> {
    let field = |name: &str| -> Option<u64> {
        let l = text.lines().find(|l| l.starts_with(name))?;
        l.split_whitespace()
            .nth(1)?
            .parse::<u64>()
            .ok()
            .map(|kb| kb * 1024)
    };
    let total = field("MemTotal:")?;
    let avail = field("MemAvailable:")?;
    Some((total.saturating_sub(avail), total))
}

/// `claude-opus-5-5` → `Opus 5.5`; `claude-sonnet-4-5-20250929` → `Sonnet 4.5`.
pub fn model_label(id: &str) -> String {
    if !id.starts_with("claude-") {
        return id.to_string();
    }
    let core = id.split('[').next().unwrap_or(id);
    let parts: Vec<&str> = core.trim_start_matches("claude-").split('-').collect();
    let Some((family, rest)) = parts.split_first() else {
        return id.to_string();
    };
    let nums: Vec<&str> = rest
        .iter()
        .copied()
        .filter(|p| p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit()))
        .collect();
    let mut f = family.to_string();
    if let Some(c) = f.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    if nums.is_empty() {
        f
    } else {
        format!("{f} {}", nums.join("."))
    }
}

fn win_estimate(model: &str, ctx: Option<u64>) -> u64 {
    if model.contains("[1m]") || ctx.is_some_and(|c| c > 200_000) {
        1_000_000
    } else {
        200_000
    }
}

pub fn collect(thread: &Thread, tee: Option<&Value>, mem: Option<(u64, u64)>) -> Metrics {
    let mut m = Metrics {
        model: thread.model().map(model_label),
        ctx: thread.last_usage().map(|u| u.context()),
        branch: thread.git_branch().map(str::to_string),
        lines: thread.lines(),
        effort: thread.effort().map(str::to_string),
        compactions: thread.compactions(),
        mem,
        five_h: thread.rate().and_then(|r| r.five_h),
        seven_d: thread.rate().and_then(|r| r.seven_d),
        win: thread.window().map(|w| (w, false)),
        ..Default::default()
    };
    if let Some(t) = tee {
        let f = |path: &[&str]| -> Option<&Value> { path.iter().try_fold(t, |v, k| v.get(*k)) };
        if let Some(w) = f(&["context_window", "context_window_size"]).and_then(Value::as_u64) {
            m.win = Some((w, false));
        }
        m.ctx_pct = f(&["context_window", "used_percentage"]).and_then(Value::as_f64);
        m.five_h = f(&["rate_limits", "five_hour", "used_percentage"])
            .and_then(Value::as_f64)
            .or(m.five_h);
        m.seven_d = f(&["rate_limits", "seven_day", "used_percentage"])
            .and_then(Value::as_f64)
            .or(m.seven_d);
        if let (Some(a), Some(r)) = (
            f(&["cost", "total_lines_added"]).and_then(Value::as_u64),
            f(&["cost", "total_lines_removed"]).and_then(Value::as_u64),
        ) {
            m.lines = Some((a, r));
        }
        if let Some(e) = f(&["effort", "level"]).and_then(Value::as_str) {
            m.effort = Some(e.to_string());
        }
        if m.model.is_none() {
            m.model = f(&["model", "display_name"])
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
    if m.win.is_none() {
        m.win = thread.model().map(|id| (win_estimate(id, m.ctx), true));
    }
    if m.ctx_pct.is_none() {
        if let (Some(c), Some((w, _))) = (m.ctx, m.win) {
            m.ctx_pct = Some(c as f64 * 100.0 / w as f64);
        }
    }
    m
}

/// Model and effort of the selected node (main thread, subagent or workflow agent).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeModel {
    /// Display label, same formatting as the status bar (`Opus 5.5`).
    pub model: Option<String>,
    /// Raw model id (`claude-opus-5-5`), if known.
    pub model_id: Option<String>,
    pub effort: Option<String>,
    /// The values come from a subagent thread rather than the main session.
    pub sub: bool,
}

/// `main` = the session's metrics, `main_id` = its raw model id; `node` = the
/// selected subagent thread, if any. A subagent's own model/effort win; what it
/// has not recorded is inherited from the session.
pub fn node_model(main: &Metrics, main_id: Option<&str>, node: Option<&Thread>) -> NodeModel {
    let own_id = node.and_then(Thread::model);
    NodeModel {
        model: own_id.map(model_label).or_else(|| main.model.clone()),
        model_id: own_id.or(main_id).map(str::to_string),
        effort: node
            .and_then(Thread::effort)
            .map(str::to_string)
            .or_else(|| main.effort.clone()),
        sub: node.is_some(),
    }
}

/// Load a fresh status-line snapshot for this session, if any.
pub fn load_tee(dir: &Path, sid: &str, now_ms: i64) -> Option<Value> {
    let p = dir.join(format!("{sid}.json"));
    let mt = std::fs::metadata(&p).ok()?.modified().ok()?;
    let mt = mt.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64;
    if now_ms - mt > TEE_FRESH_MS {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

pub fn human_bytes(b: u64) -> String {
    format!("{:.1}G", b as f64 / (1024.0 * 1024.0 * 1024.0))
}

pub fn human_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::parse_line;
    use serde_json::json;

    #[test]
    fn meminfo_parse() {
        let t = "MemTotal:       32000000 kB\nMemFree: 1 kB\nMemAvailable:   16000000 kB\n";
        assert_eq!(
            read_meminfo(t),
            Some((16_000_000 * 1024, 32_000_000 * 1024))
        );
        assert_eq!(read_meminfo("junk"), None);
    }

    #[test]
    fn model_labels() {
        assert_eq!(model_label("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_label("claude-sonnet-4-5-20250929"), "Sonnet 4.5");
        assert_eq!(model_label("claude-opus-5-5[1m]"), "Opus 5.5");
    }

    fn thread() -> Thread {
        let mut t = Thread::new();
        t.push(parse_line(r#"{"type":"assistant","uuid":"a","effort":"medium","gitBranch":"feat/x","message":{"model":"claude-opus-5-5","usage":{"input_tokens":6000,"cache_read_input_tokens":94000,"output_tokens":5},"content":[]}}"#).unwrap());
        t
    }

    #[test]
    fn prefers_tee_cache() {
        let tee = json!({"context_window":{"context_window_size":1000000,"used_percentage":13.3},
            "rate_limits":{"five_hour":{"used_percentage":9.0},"seven_day":{"used_percentage":62.0}},
            "cost":{"total_lines_added":1,"total_lines_removed":1}});
        let m = collect(&thread(), Some(&tee), None);
        assert_eq!(m.win, Some((1_000_000, false)));
        assert_eq!(m.ctx_pct, Some(13.3));
        assert_eq!(m.five_h, Some(9.0));
        assert_eq!(m.seven_d, Some(62.0));
        assert_eq!(m.lines, Some((1, 1)));
        assert_eq!(m.ctx, Some(100_000));
        assert_eq!(m.effort.as_deref(), Some("medium"));
        assert_eq!(m.branch.as_deref(), Some("feat/x"));
    }

    #[test]
    fn win_fallback_marked_estimated() {
        let m = collect(&thread(), None, None);
        assert_eq!(m.win, Some((200_000, true)));
        assert_eq!(m.ctx_pct, Some(50.0));
        assert_eq!(m.five_h, None);
    }

    #[test]
    fn node_model_prefers_node_then_main() {
        let main = collect(&thread(), None, None);
        // Main thread itself: its own label, id and effort.
        let n = node_model(&main, Some("claude-opus-5-5"), None);
        assert_eq!(n.model.as_deref(), Some("Opus 5.5"));
        assert_eq!(n.model_id.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(n.effort.as_deref(), Some("medium"));
        assert!(!n.sub);
        // Subagent with its own model and no effort: model from the subagent,
        // effort inherited from the session.
        let mut sub = Thread::new();
        sub.push(parse_line(r#"{"type":"assistant","uuid":"s","message":{"model":"claude-haiku-4-5-20251001","content":[]}}"#).unwrap());
        let n = node_model(&main, Some("claude-opus-5-5"), Some(&sub));
        assert_eq!(n.model.as_deref(), Some("Haiku 4.5"));
        assert_eq!(n.model_id.as_deref(), Some("claude-haiku-4-5-20251001"));
        assert_eq!(n.effort.as_deref(), Some("medium"));
        assert!(n.sub);
        // Subagent with nothing recorded yet: everything from the session.
        let empty = Thread::new();
        let n = node_model(&main, Some("claude-opus-5-5"), Some(&empty));
        assert_eq!(n.model.as_deref(), Some("Opus 5.5"));
        assert_eq!(n.model_id.as_deref(), Some("claude-opus-5-5"));
        assert!(n.sub);
    }

    #[test]
    fn token_format() {
        assert_eq!(human_tokens(106_200), "106.2k");
        assert_eq!(human_tokens(1_000_000), "1.0M");
    }
}
