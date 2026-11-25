//! Rendering. View data (timeline lines, metrics) is prepared before `Terminal::draw`
//! so the draw closure only borrows immutably.

use crate::app::{App, Focus};
use cc_monitor::metrics::{human_bytes, human_tokens, Metrics};
use cc_monitor::store::{NodeId, NodeKind, Status};
use cc_monitor::thread::{Item, ToolItem, ToolState};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MAX_EXPANDED_LINES: usize = 400;
const PERSISTED_MAX_BYTES: u64 = 256 * 1024;
pub const MIN_WIDTH_FOR_TREE: u16 = 70;

pub struct TimelineView {
    pub title: String,
    pub lines: Vec<Line<'static>>,
    /// First line index of each item.
    pub item_line: Vec<usize>,
}

pub struct Layouts {
    pub tree: Option<Rect>,
    pub timeline: Rect,
    pub status: Rect,
    pub hints: Rect,
}

pub fn layouts(area: Rect, app: &App) -> Layouts {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(area);
    let show_tree = app.tree_visible.unwrap_or(area.width >= MIN_WIDTH_FOR_TREE);
    let (tree, timeline) = if show_tree && area.width >= 30 {
        let w = (area.width * 28 / 100).clamp(24, 48).min(area.width / 2);
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(w), Constraint::Min(10)])
            .split(v[0]);
        (Some(h[0]), h[1])
    } else {
        (None, v[0])
    };
    Layouts {
        tree,
        timeline,
        status: v[1],
        hints: v[2],
    }
}

fn hms(ts: Option<i64>, tz_offset_ms: i64) -> String {
    match ts {
        Some(t) => {
            let s = ((t + tz_offset_ms) / 1000).rem_euclid(86_400);
            format!("{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
        }
        None => "        ".into(),
    }
}

fn dur(ms: i64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, ms % 60_000 / 1000)
    }
}

fn truncate_width(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max {
            if max > 0 {
                out.pop();
                out.push('…');
            }
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}

fn first_line(s: &str) -> &str {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

/// Hard-wrap text to `width` display columns.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    for raw in text.lines() {
        let raw = raw.replace('\t', "    ");
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut w = 0;
        for c in raw.chars() {
            let cw = c.width().unwrap_or(0);
            if w + cw > width {
                out.push(std::mem::take(&mut cur));
                w = 0;
            }
            cur.push(c);
            w += cw;
        }
        out.push(cur);
    }
    out
}

fn status_glyph(s: Status) -> (&'static str, Color) {
    match s {
        Status::Running => ("▶", Color::Green),
        Status::Done => ("✓", Color::DarkGray),
        Status::Error => ("✗", Color::Red),
        Status::Idle => ("·", Color::DarkGray),
    }
}

fn tool_header(t: &ToolItem, now: i64) -> (Vec<Span<'static>>, Color) {
    let (mark, color, time) = match t.state {
        ToolState::Running => (
            "…",
            Color::Yellow,
            t.started.map(|s| dur((now - s).max(0))).unwrap_or_default(),
        ),
        ToolState::Ok => (
            "✓",
            Color::Green,
            match (t.started, t.ended) {
                (Some(a), Some(b)) => dur((b - a).max(0)),
                _ => String::new(),
            },
        ),
        ToolState::Err => (
            "✗",
            Color::Red,
            match (t.started, t.ended) {
                (Some(a), Some(b)) => dur((b - a).max(0)),
                _ => String::new(),
            },
        ),
    };
    let link = if t.link.is_some() { " ↪" } else { "" };
    (
        vec![
            Span::styled(
                format!("▸ {:<9}", t.name),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" {}", t.summary)),
            Span::styled(format!("  {time} {mark}{link}"), Style::default().fg(color)),
        ],
        color,
    )
}

fn expanded_body(item: &Item) -> String {
    match item {
        Item::Prompt { text, .. } | Item::Text { text, .. } | Item::System { text, .. } => {
            text.clone()
        }
        Item::Thinking { text, .. } => {
            if text.trim().is_empty() {
                "(nội dung suy nghĩ không được lưu)".into()
            } else {
                text.clone()
            }
        }
        Item::Compact { pre, post, .. } => format!("Context {} → {} token", pre, post),
        Item::Turn { usage, .. } => format!("{usage:?}"),
        Item::Tool(t) => {
            let input = serde_json::to_string_pretty(&t.input).unwrap_or_default();
            let mut result = t.result.clone();
            if let Some(p) = &t.persisted {
                if let Ok(f) = std::fs::File::open(p) {
                    use std::io::Read;
                    let mut s = String::new();
                    let _ = f.take(PERSISTED_MAX_BYTES).read_to_string(&mut s);
                    result = format!("[đầy đủ từ {}]\n{s}", p.display());
                }
            }
            format!("── input ──\n{input}\n── kết quả ──\n{result}")
        }
    }
}

pub fn build_timeline(app: &mut App, width: u16, now: i64, tz: i64) -> TimelineView {
    let Some(id) = app.selected.clone() else {
        return TimelineView {
            title: " Chưa có phiên nào đang hoạt động ".into(),
            lines: vec![],
            item_line: vec![],
        };
    };
    if let NodeId::Workflow(..) = &id {
        return build_workflow_view(app, &id);
    }
    let title = app
        .rows
        .iter()
        .find(|r| r.id == id)
        .map(|r| r.label.clone())
        .unwrap_or_default();
    let Some(thread) = app.store.thread_mut(&id) else {
        return TimelineView {
            title,
            lines: vec![],
            item_line: vec![],
        };
    };
    let items = thread.items().to_vec();
    let inner = width.saturating_sub(2) as usize;
    let mut lines = Vec::new();
    let mut item_line = Vec::with_capacity(items.len());
    for (i, it) in items.iter().enumerate() {
        item_line.push(lines.len());
        let ts = match it {
            Item::Prompt { ts, .. }
            | Item::Thinking { ts, .. }
            | Item::Text { ts, .. }
            | Item::Compact { ts, .. }
            | Item::System { ts, .. }
            | Item::Turn { ts, .. } => *ts,
            Item::Tool(t) => t.started,
        };
        let mut spans = vec![Span::styled(
            format!("{} ", hms(ts, tz)),
            Style::default().fg(Color::DarkGray),
        )];
        let budget = inner.saturating_sub(10);
        match it {
            Item::Prompt { text, .. } => spans.push(Span::styled(
                truncate_width(&format!("❯ {}", first_line(text)), budget),
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Item::Thinking { text, .. } => spans.push(Span::styled(
                format!(
                    "✻ suy nghĩ{}",
                    if text.is_empty() {
                        String::new()
                    } else {
                        format!(" ({} ký tự)", text.chars().count())
                    }
                ),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
            Item::Text { text, .. } => spans.push(Span::raw(truncate_width(
                &format!("● {}", first_line(text)),
                budget,
            ))),
            Item::Compact { pre, post, .. } => spans.push(Span::styled(
                format!(
                    "──── compact {} → {} ────",
                    human_tokens(*pre),
                    human_tokens(*post)
                ),
                Style::default().fg(Color::Magenta),
            )),
            Item::Turn { model, usage, .. } => spans.push(Span::styled(
                format!(
                    "Claude {} ↓{} ↑{}",
                    model
                        .as_deref()
                        .map(cc_monitor::metrics::model_label)
                        .unwrap_or_default(),
                    usage.map(|u| human_tokens(u.input)).unwrap_or_default(),
                    usage.map(|u| human_tokens(u.output)).unwrap_or_default()
                ),
                Style::default().fg(Color::Blue),
            )),
            Item::System { text, .. } => spans.push(Span::styled(
                truncate_width(&format!("· {}", first_line(text)), budget),
                Style::default().fg(Color::DarkGray),
            )),
            Item::Tool(t) => {
                let (mut s, _) = tool_header(t, now);
                // Truncate the summary span so the timing stays visible.
                let fixed = UnicodeWidthStr::width(s[0].content.as_ref())
                    + UnicodeWidthStr::width(s[2].content.as_ref());
                let room = budget.saturating_sub(fixed);
                s[1] = Span::raw(truncate_width(&s[1].content, room));
                spans.extend(s);
            }
        }
        lines.push(Line::from(spans));
        if app.expanded.contains(&(id.clone(), i)) {
            let body = expanded_body(it);
            let wrapped = wrap(&body, inner.saturating_sub(4));
            let n = wrapped.len();
            for l in wrapped.into_iter().take(MAX_EXPANDED_LINES) {
                lines.push(Line::from(Span::styled(
                    format!("    {l}"),
                    Style::default().fg(Color::Gray),
                )));
            }
            if n > MAX_EXPANDED_LINES {
                lines.push(Line::from(Span::styled(
                    format!("    … còn {} dòng", n - MAX_EXPANDED_LINES),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }
    TimelineView {
        title,
        lines,
        item_line,
    }
}

fn build_workflow_view(app: &App, id: &NodeId) -> TimelineView {
    let title = app
        .rows
        .iter()
        .find(|r| &r.id == id)
        .map(|r| r.label.clone())
        .unwrap_or_default();
    let mut lines = Vec::new();
    let mut item_line = Vec::new();
    for m in app.workflow_members(id) {
        let Some(row) = app.rows.iter().find(|r| r.id == m) else {
            continue;
        };
        let (g, c) = status_glyph(row.status);
        let last = app
            .store
            .thread(&m)
            .and_then(|t| t.last_tool_name().map(str::to_string))
            .unwrap_or_default();
        item_line.push(lines.len());
        lines.push(Line::from(vec![
            Span::styled(format!("{g} "), Style::default().fg(c)),
            Span::raw(row.label.clone()),
            Span::styled(format!("   {last}"), Style::default().fg(Color::DarkGray)),
        ]));
    }
    TimelineView {
        title,
        lines,
        item_line,
    }
}

fn seg(text: String, bg: Color) -> (String, Color) {
    (text, bg)
}

fn powerline(segs: Vec<(String, Color)>, plain: bool) -> Line<'static> {
    let mut spans = Vec::new();
    let fg = Color::Black;
    for (i, (t, bg)) in segs.iter().enumerate() {
        spans.push(Span::styled(
            format!(" {t} "),
            Style::default().fg(fg).bg(*bg),
        ));
        if plain {
            spans.push(Span::raw(" "));
        } else {
            let next = segs.get(i + 1).map(|s| s.1);
            let mut st = Style::default().fg(*bg);
            if let Some(n) = next {
                st = st.bg(n);
            }
            spans.push(Span::styled("\u{e0b0}", st));
        }
    }
    Line::from(spans)
}

const BLUE: Color = Color::Rgb(130, 170, 255);
const GRAY: Color = Color::Rgb(170, 170, 170);
const PURPLE: Color = Color::Rgb(210, 140, 225);
const YELLOW: Color = Color::Rgb(255, 205, 100);
const TEAL: Color = Color::Rgb(100, 210, 205);

pub fn status_lines(m: &Metrics, plain: bool) -> Vec<Line<'static>> {
    let dash = || "—".to_string();
    let pct = |p: Option<f64>| p.map(|v| format!("{v:.1}%")).unwrap_or_else(dash);
    let mut l1 = vec![
        seg(
            format!("Model: {}", m.model.clone().unwrap_or_else(dash)),
            BLUE,
        ),
        seg(
            format!("Ctx: {}", m.ctx.map(human_tokens).unwrap_or_else(dash)),
            GRAY,
        ),
    ];
    if let Some(b) = &m.branch {
        l1.push(seg(format!("⎇ {b}"), PURPLE));
    }
    if let Some((a, r)) = m.lines {
        l1.push(seg(format!("(+{a},-{r})"), YELLOW));
    }
    let mut l2 = Vec::new();
    if let Some((u, t)) = m.mem {
        l2.push(seg(
            format!("Mem: {}/{}", human_bytes(u), human_bytes(t)),
            YELLOW,
        ));
    }
    l2.push(seg(
        format!("Thinking: {}", m.effort.clone().unwrap_or_else(dash)),
        TEAL,
    ));
    l2.push(seg(format!("Session: {}", pct(m.five_h)), YELLOW));
    l2.push(seg(format!("Weekly: {}", pct(m.seven_d)), TEAL));
    l2.push(seg(format!("Ctx(u) Used: {}", pct(m.ctx_pct)), TEAL));
    l2.push(seg(
        format!(
            "Win: {}",
            m.win
                .map(|(w, est)| format!("{}{}", human_tokens(w), if est { "?" } else { "" }))
                .unwrap_or_else(dash)
        ),
        BLUE,
    ));
    l2.push(seg(format!("↻ {}", m.compactions), GRAY));
    vec![powerline(l1, plain), powerline(l2, plain)]
}

pub struct Frame0<'a> {
    pub timeline: &'a TimelineView,
    pub metrics: &'a Metrics,
    pub running_agents: usize,
    pub ready: bool,
    pub plain: bool,
    pub tree_scroll: usize,
    pub timeline_scroll: usize,
}

pub fn draw(f: &mut Frame, app: &App, v: &Frame0) {
    let lay = layouts(f.area(), app);
    if let Some(area) = lay.tree {
        draw_tree(f, app, area, v.tree_scroll, v.ready);
    }
    draw_timeline(f, app, lay.timeline, v);
    f.render_widget(Paragraph::new(status_lines(v.metrics, v.plain)), lay.status);
    let follow = if app.follow { "ON" } else { "off" };
    let hints = format!(
        " Tab đổi pane · ↑↓ chọn · Enter mở/nhảy luồng · Space mở rộng · Esc quay lại · f follow({follow}) · t ẩn/hiện cây · q thoát │ {} agent đang chạy",
        v.running_agents
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_width(&hints, lay.hints.width as usize),
            Style::default().fg(Color::DarkGray),
        ))),
        lay.hints,
    );
}

fn border(title: String, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title(title)
}

fn draw_tree(f: &mut Frame, app: &App, area: Rect, scroll: usize, ready: bool) {
    let inner_w = area.width.saturating_sub(2) as usize;
    let mut lines = Vec::new();
    if app.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            if ready {
                " Không có phiên nào hoạt động trong 30 phút qua"
            } else {
                " Đang quét…"
            },
            Style::default().fg(Color::DarkGray),
        )));
    }
    for r in app.rows.iter().skip(scroll) {
        let (g, c) = status_glyph(r.status);
        let indent = "  ".repeat(r.depth as usize);
        let icon = match &r.kind {
            NodeKind::Main => "◆ ",
            NodeKind::Agent { .. } => "",
            NodeKind::Workflow => "⚙ ",
            NodeKind::WorkflowAgent => "",
        };
        let prog = r
            .progress
            .map(|(d, t)| format!(" {d}/{t}"))
            .unwrap_or_default();
        let agent_type = match &r.kind {
            NodeKind::Agent { agent_type } => format!("{agent_type}: "),
            _ => String::new(),
        };
        let text = format!("{indent}{g} {icon}{agent_type}{}{prog}", r.label);
        let mut style = Style::default();
        if r.depth == 0 {
            style = style.add_modifier(Modifier::BOLD);
        }
        if app.selected.as_ref() == Some(&r.id) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let text = truncate_width(&text, inner_w);
        let gpos = indent.len();
        if text.len() < gpos + g.len()
            || !text.is_char_boundary(gpos)
            || !text.is_char_boundary(gpos + g.len())
        {
            lines.push(Line::from(Span::styled(text, style)));
            continue;
        }
        lines.push(Line::from(vec![
            Span::styled(text[..gpos].to_string(), style),
            Span::styled(g.to_string(), style.fg(c)),
            Span::styled(text[gpos + g.len()..].to_string(), style),
        ]));
    }
    f.render_widget(
        Paragraph::new(lines).block(border(" Luồng ".into(), app.focus == Focus::Tree)),
        area,
    );
}

fn draw_timeline(f: &mut Frame, app: &App, area: Rect, v: &Frame0) {
    let height = area.height.saturating_sub(2) as usize;
    let cursor_line = v.timeline.item_line.get(app.cursor).copied();
    let lines: Vec<Line> = v
        .timeline
        .lines
        .iter()
        .enumerate()
        .skip(v.timeline_scroll)
        .take(height)
        .map(|(i, l)| {
            if Some(i) == cursor_line && app.focus == Focus::Timeline {
                l.clone().style(Style::default().bg(Color::Rgb(45, 50, 65)))
            } else {
                l.clone()
            }
        })
        .collect();
    let title = format!(
        " {} ",
        truncate_width(&v.timeline.title, area.width.saturating_sub(6) as usize)
    );
    f.render_widget(
        Paragraph::new(lines).block(border(title, app.focus == Focus::Timeline)),
        area,
    );
}

/// Keep the cursor visible; when following, pin to the bottom.
pub fn timeline_scroll(
    prev: usize,
    view: &TimelineView,
    cursor: usize,
    follow: bool,
    height: usize,
) -> usize {
    let total = view.lines.len();
    if total <= height {
        return 0;
    }
    let max = total - height;
    if follow {
        return max;
    }
    let start = view.item_line.get(cursor).copied().unwrap_or(0);
    let end = view
        .item_line
        .get(cursor + 1)
        .copied()
        .unwrap_or(total)
        .saturating_sub(1);
    let mut s = prev.min(max);
    if start < s {
        s = start;
    } else if end >= s + height {
        s = (end + 1 - height).min(start);
    }
    s.min(max)
}

pub fn tree_scroll(prev: usize, selected: usize, height: usize) -> usize {
    if selected < prev {
        selected
    } else if selected >= prev + height {
        selected + 1 - height
    } else {
        prev
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_handles_wide_chars() {
        assert_eq!(wrap("abcdefghij", 8), vec!["abcdefgh", "ij"]);
        let w = wrap("ăăăăăăăăă", 8);
        assert_eq!(w.len(), 2);
    }

    fn sample_app() -> App {
        use cc_monitor::discovery::{AgentMeta, Key, Msg};
        use cc_monitor::event::parse_line;
        let mut st = cc_monitor::store::Store::default();
        st.apply(Msg::Session {
            sid: "s".into(),
            project: "-p".into(),
            tool: cc_monitor::discovery::Tool::Claude,
            path: Default::default(),
        });
        let lines = [
            r#"{"type":"user","uuid":"P","cwd":"/home/u/proj","message":{"content":"Xin chào, hãy đọc file rất dài 文字 ăn"}}"#,
            r#"{"type":"assistant","uuid":"A","parentUuid":"P","gitBranch":"feat/x","message":{"model":"claude-opus-5-5","usage":{"input_tokens":5,"cache_read_input_tokens":100000},"content":[{"type":"thinking","thinking":"hmm"},{"type":"tool_use","id":"t1","name":"Agent","input":{"description":"Kiểm tra rất rất rất dài dài dài","subagent_type":"Explore"}}]}}"#,
            r#"{"type":"user","uuid":"B","parentUuid":"A","toolUseResult":{"agentId":"x"},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
        ];
        st.apply(Msg::Events {
            key: Key::Main("s".into()),
            reset: false,
            mtime_ms: 0,
            events: lines.iter().map(|l| parse_line(l).unwrap()).collect(),
        });
        st.apply(Msg::Agent {
            sid: "s".into(),
            agent_id: "x".into(),
            meta: AgentMeta {
                tool_use_id: Some("t1".into()),
                description: Some("Kiểm tra".into()),
                agent_type: Some("Explore".into()),
                ..Default::default()
            },
            run_id: None,
        });
        let mut a = App::new(st);
        a.refresh_rows(0);
        a
    }

    #[test]
    fn renders_without_panic_at_various_sizes() {
        use ratatui::backend::TestBackend;
        for (w, h) in [(20u16, 5u16), (60, 10), (200, 50)] {
            let mut app = sample_app();
            app.expanded.insert((NodeId::Main("s".into()), 0));
            let mut term = ratatui::Terminal::new(TestBackend::new(w, h)).unwrap();
            let lay = layouts(Rect::new(0, 0, w, h), &app);
            let view = build_timeline(&mut app, lay.timeline.width, 0, 0);
            app.clamp_cursor(view.item_line.len());
            let m = cc_monitor::metrics::collect(
                app.store.thread(&NodeId::Main("s".into())).unwrap(),
                None,
                None,
            );
            let fr = Frame0 {
                timeline: &view,
                metrics: &m,
                running_agents: 1,
                ready: true,
                plain: false,
                tree_scroll: 0,
                timeline_scroll: 0,
            };
            term.draw(|f| draw(f, &app, &fr)).unwrap();
            if w == 60 {
                assert!(lay.tree.is_none(), "tree hidden below 70 cols");
            }
        }
    }

    #[test]
    fn scroll_keeps_cursor_visible() {
        let view = TimelineView {
            title: String::new(),
            lines: vec![Line::from(""); 100],
            item_line: (0..100).collect(),
        };
        assert_eq!(timeline_scroll(0, &view, 50, false, 10), 41);
        assert_eq!(timeline_scroll(41, &view, 45, false, 10), 41);
        assert_eq!(timeline_scroll(41, &view, 10, false, 10), 10);
        assert_eq!(timeline_scroll(0, &view, 0, true, 10), 90);
    }
}
