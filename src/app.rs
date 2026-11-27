//! Application state and key handling (terminal-independent, unit-testable).

use cc_monitor::store::{NodeId, Store, TreeRow};
use cc_monitor::thread::Item;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Timeline,
}

pub struct App {
    pub store: Store,
    pub rows: Vec<TreeRow>,
    pub selected: Option<NodeId>,
    pub history: Vec<NodeId>,
    pub cursor: usize,
    pub follow: bool,
    pub expanded: HashSet<(NodeId, usize)>,
    pub focus: Focus,
    /// User override of tree visibility (None = automatic by width).
    pub tree_visible: Option<bool>,
    pub quit: bool,
    /// Number of items in the selected timeline (set when the view is built).
    pub item_count: usize,
    pub page: usize,
    /// Until the user picks a node, keep showing the most recently active session.
    pub user_selected: bool,
}

impl App {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            rows: Vec::new(),
            selected: None,
            history: Vec::new(),
            cursor: 0,
            follow: true,
            expanded: HashSet::new(),
            focus: Focus::Tree,
            tree_visible: None,
            quit: false,
            item_count: 0,
            page: 10,
            user_selected: false,
        }
    }

    pub fn refresh_rows(&mut self, now: i64) {
        self.rows = self.store.tree(now);
        let valid = self
            .selected
            .as_ref()
            .is_some_and(|s| self.rows.iter().any(|r| &r.id == s));
        let auto =
            !self.user_selected && self.rows.first().map(|r| &r.id) != self.selected.as_ref();
        if !valid || auto {
            self.selected = self.rows.first().map(|r| r.id.clone());
            self.follow = true;
        }
    }

    fn selected_index(&self) -> Option<usize> {
        let s = self.selected.as_ref()?;
        self.rows.iter().position(|r| &r.id == s)
    }

    pub fn select(&mut self, id: NodeId) {
        if self.selected.as_ref() != Some(&id) {
            if let Some(cur) = self.selected.take() {
                self.history.push(cur);
            }
            self.selected = Some(id);
            self.follow = true;
            self.cursor = usize::MAX;
        }
    }

    fn move_tree(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let i = self.selected_index().unwrap_or(0) as isize;
        let n = (i + delta).clamp(0, self.rows.len() as isize - 1) as usize;
        let id = self.rows[n].id.clone();
        self.selected = Some(id);
        self.follow = true;
        self.cursor = usize::MAX;
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.item_count == 0 {
            return;
        }
        let last = self.item_count - 1;
        let cur = self.cursor.min(last) as isize;
        let n = (cur + delta).clamp(0, last as isize) as usize;
        self.cursor = n;
        self.follow = n == last && delta > 0 || (self.follow && n == last);
    }

    /// Clamp cursor after the view knows how many items exist.
    pub fn clamp_cursor(&mut self, count: usize) {
        self.item_count = count;
        if count == 0 {
            self.cursor = 0;
        } else if self.follow || self.cursor >= count {
            self.cursor = count - 1;
        }
    }

    fn current_item(&mut self) -> Option<Item> {
        let id = self.selected.clone()?;
        let t = self.store.thread_mut(&id)?;
        t.items().get(self.cursor).cloned()
    }

    fn activate(&mut self) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        if let NodeId::Workflow(sid, _) = &id {
            // In a workflow view the cursor indexes its member rows.
            let members: Vec<NodeId> = self.workflow_members(&id);
            if let Some(m) = members.get(self.cursor) {
                let _ = sid;
                self.select(m.clone());
            }
            return;
        }
        if let Some(Item::Tool(t)) = self.current_item() {
            if let Some(link) = &t.link {
                if let Some(node) = self.store.node_for_link(id.session(), link) {
                    self.select(node);
                    return;
                }
            }
        }
        self.toggle_expand();
    }

    pub fn workflow_members(&self, wf: &NodeId) -> Vec<NodeId> {
        let Some(i) = self.rows.iter().position(|r| &r.id == wf) else {
            return vec![];
        };
        let depth = self.rows[i].depth;
        self.rows[i + 1..]
            .iter()
            .take_while(|r| r.depth > depth)
            .map(|r| r.id.clone())
            .collect()
    }

    fn toggle_expand(&mut self) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let key = (id, self.cursor);
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
    }

    pub fn on_key(&mut self, k: KeyEvent, tree_shown: bool) {
        if !matches!(
            k.code,
            KeyCode::Char('q') | KeyCode::Char('f') | KeyCode::Char('t')
        ) {
            self.user_selected = true;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Tab | KeyCode::BackTab => {
                if !tree_shown {
                    self.tree_visible = Some(true);
                    self.focus = Focus::Tree;
                } else {
                    self.focus = if self.focus == Focus::Tree {
                        Focus::Timeline
                    } else {
                        Focus::Tree
                    };
                }
            }
            KeyCode::Char('t') => {
                self.tree_visible = Some(!tree_shown);
                if tree_shown {
                    self.focus = Focus::Timeline;
                }
            }
            KeyCode::Char('f') => {
                self.follow = !self.follow;
                if self.follow {
                    self.cursor = usize::MAX;
                }
            }
            KeyCode::Esc => {
                if let Some(prev) = self.history.pop() {
                    self.selected = Some(prev);
                    self.follow = true;
                    self.cursor = usize::MAX;
                } else {
                    self.focus = Focus::Tree;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.nav(-1),
            KeyCode::Down | KeyCode::Char('j') => self.nav(1),
            KeyCode::PageUp => self.nav(-(self.page as isize)),
            KeyCode::PageDown => self.nav(self.page as isize),
            KeyCode::Char('g') | KeyCode::Home => self.nav(isize::MIN / 2),
            KeyCode::Char('G') | KeyCode::End => self.nav(isize::MAX / 2),
            KeyCode::Enter => match self.focus {
                Focus::Tree => self.focus = Focus::Timeline,
                Focus::Timeline => self.activate(),
            },
            KeyCode::Char(' ') if self.focus == Focus::Timeline => self.toggle_expand(),
            KeyCode::Right | KeyCode::Char('l') => self.focus = Focus::Timeline,
            KeyCode::Left | KeyCode::Char('h') if tree_shown => self.focus = Focus::Tree,
            _ => {}
        }
    }

    fn nav(&mut self, d: isize) {
        match self.focus {
            Focus::Tree => self.move_tree(d),
            Focus::Timeline => self.move_cursor(d),
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent, tree_width: u16) {
        self.user_selected = true;
        let over_tree = m.column < tree_width;
        let d = match m.kind {
            MouseEventKind::ScrollUp => -3isize,
            MouseEventKind::ScrollDown => 3,
            _ => return,
        };
        if over_tree {
            self.move_tree(d.signum());
        } else {
            self.move_cursor(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_monitor::discovery::{AgentMeta, Key, Msg};
    use cc_monitor::event::parse_line;
    use crossterm::event::KeyEvent;

    fn app() -> App {
        let mut st = Store::default();
        st.apply(Msg::Session {
            sid: "s".into(),
            project: "-p".into(),
            tool: cc_monitor::discovery::Tool::Claude,
            path: Default::default(),
        });
        st.apply(Msg::Events {
            key: Key::Main("s".into()),
            reset: false,
            mtime_ms: 0,
            events: vec![
                parse_line(r#"{"type":"user","uuid":"P","message":{"content":"hi"}}"#).unwrap(),
                parse_line(r#"{"type":"assistant","uuid":"A","parentUuid":"P","message":{"content":[{"type":"tool_use","id":"t1","name":"Agent","input":{"description":"d"}}]}}"#).unwrap(),
                parse_line(r#"{"type":"user","uuid":"B","parentUuid":"A","toolUseResult":{"agentId":"x"},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#).unwrap(),
                parse_line(r#"{"type":"assistant","uuid":"C","parentUuid":"B","message":{"content":[{"type":"text","text":"done"}]}}"#).unwrap(),
            ],
        });
        st.apply(Msg::Agent {
            sid: "s".into(),
            agent_id: "x".into(),
            meta: AgentMeta {
                tool_use_id: Some("t1".into()),
                ..Default::default()
            },
            run_id: None,
            path: "agent-x.jsonl".into(),
        });
        let mut a = App::new(st);
        a.refresh_rows(0);
        a.focus = Focus::Timeline;
        // Items: Prompt, Turn, Tool(Agent), Turn, Text
        a.clamp_cursor(5);
        a
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn follow_turns_off_on_scroll_up() {
        let mut a = app();
        assert!(a.follow);
        assert_eq!(a.cursor, 4);
        a.on_key(key(KeyCode::Up), true);
        assert!(!a.follow);
        assert_eq!(a.cursor, 3);
        a.on_key(key(KeyCode::Char('f')), true);
        a.clamp_cursor(5);
        assert!(a.follow);
        assert_eq!(a.cursor, 4);
    }

    #[test]
    fn enter_on_linked_tool_selects_thread_and_esc_returns() {
        let mut a = app();
        a.on_key(key(KeyCode::Up), true);
        a.on_key(key(KeyCode::Up), true); // cursor on the Agent tool item
        a.on_key(key(KeyCode::Enter), true);
        assert_eq!(a.selected, Some(NodeId::Agent("s".into(), "x".into())));
        a.on_key(key(KeyCode::Esc), true);
        assert_eq!(a.selected, Some(NodeId::Main("s".into())));
    }

    #[test]
    fn space_toggles_expand() {
        let mut a = app();
        a.on_key(key(KeyCode::Char(' ')), true);
        assert!(a.expanded.contains(&(NodeId::Main("s".into()), 4)));
        a.on_key(key(KeyCode::Char(' ')), true);
        assert!(a.expanded.is_empty());
    }
}
