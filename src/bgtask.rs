//! Registry of background tasks (shell, monitor, async agent, workflow) launched in a thread.

use crate::event::TaskNotice;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgKind {
    Shell,
    Monitor,
    Agent,
    Workflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgState {
    Running,
    Completed,
    Failed,
    Killed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BgTask {
    pub id: String,
    pub kind: BgKind,
    pub label: String,
    pub tool_use_id: String,
    pub output_file: Option<String>,
    pub state: BgState,
    pub started: Option<i64>,
    pub ended: Option<i64>,
    pub summary: Option<String>,
}

/// Background tasks by id, in launch order.
#[derive(Debug, Default, Clone)]
pub struct Registry {
    tasks: Vec<BgTask>,
}

impl Registry {
    pub fn launch(&mut self, t: BgTask) {
        match self.tasks.iter_mut().find(|x| x.id == t.id) {
            Some(x) => *x = t,
            None => self.tasks.push(t),
        }
    }

    /// Apply a task notification. Matches by task id, then by launching tool_use id.
    /// Returns false when no known task matches.
    pub fn notice(&mut self, n: &TaskNotice, ts: Option<i64>) -> bool {
        let tu = n.tool_use_id.as_deref();
        let Some(t) = self
            .tasks
            .iter_mut()
            .find(|t| t.id == n.task_id || tu.is_some_and(|u| t.tool_use_id == u))
        else {
            return false;
        };
        t.state = match n.status.as_str() {
            "completed" => BgState::Completed,
            "failed" => BgState::Failed,
            "killed" | "stopped" => BgState::Killed,
            _ => BgState::Unknown,
        };
        t.ended = ts;
        if !n.summary.is_empty() {
            t.summary = Some(n.summary.clone());
        }
        if n.output_file.is_some() {
            t.output_file = n.output_file.clone();
        }
        true
    }

    /// Tasks running at `ts`: started at or before it and not yet ended.
    pub fn pending_at(&self, ts: i64) -> Vec<&BgTask> {
        self.tasks
            .iter()
            .filter(|t| {
                t.state != BgState::Unknown
                    && t.started.is_some_and(|s| s <= ts)
                    && t.ended.is_none_or(|e| e > ts)
            })
            .collect()
    }

    pub fn by_tool_use(&self, tool_use_id: &str) -> Option<&BgTask> {
        self.tasks.iter().find(|t| t.tool_use_id == tool_use_id)
    }

    pub fn get(&self, id: &str) -> Option<&BgTask> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// Session is closed: tasks that never got a notice have an unknown outcome.
    /// Returns true if any task changed. Unknown tasks are not counted by `pending_at`.
    pub fn finalize_unknown(&mut self) -> bool {
        let mut changed = false;
        for t in &mut self.tasks {
            if t.state == BgState::Running {
                t.state = BgState::Unknown;
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, started: i64) -> BgTask {
        BgTask {
            id: id.into(),
            kind: BgKind::Shell,
            label: id.into(),
            tool_use_id: format!("tu_{id}"),
            output_file: None,
            state: BgState::Running,
            started: Some(started),
            ended: None,
            summary: None,
        }
    }

    #[test]
    fn pending_at_respects_start_and_end() {
        let mut r = Registry::default();
        r.launch(task("a", 10));
        r.launch(task("b", 20));
        r.notice(
            &TaskNotice {
                task_id: "a".into(),
                tool_use_id: None,
                status: "completed".into(),
                summary: "done".into(),
                output_file: None,
            },
            Some(30),
        );
        let ids = |ts| {
            r.pending_at(ts)
                .iter()
                .map(|t| t.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(5), Vec::<String>::new());
        assert_eq!(ids(25), vec!["a", "b"]);
        assert_eq!(ids(35), vec!["b"]);
    }

    #[test]
    fn unknown_tasks_are_not_pending() {
        let mut r = Registry::default();
        r.launch(task("a", 10));
        assert!(r.finalize_unknown());
        assert!(!r.finalize_unknown());
        assert!(r.pending_at(50).is_empty());
    }

    #[test]
    fn notice_matches_by_tool_use_id_and_unmatched_returns_false() {
        let mut r = Registry::default();
        r.launch(task("a", 10));
        let mk = |id: &str, tu: Option<&str>| TaskNotice {
            task_id: id.into(),
            tool_use_id: tu.map(Into::into),
            status: "failed".into(),
            summary: String::new(),
            output_file: None,
        };
        assert!(r.notice(&mk("zzz", Some("tu_a")), Some(1)));
        assert_eq!(r.get("a").unwrap().state, BgState::Failed);
        assert!(!r.notice(&mk("nope", None), None));
    }
}
