//! Run history of agents (subagent types) and workflows, read from
//! `~/.claude/projects` without parsing transcripts.
//!
//! Layout read (all read-only):
//! - subagent: `<proj>/<sid>/subagents/agent-<id>.{jsonl,meta.json}`
//! - workflow agents: `<proj>/<sid>/subagents/workflows/<runId>/agent-*.jsonl`
//! - workflow name/status: `<proj>/<sid>/workflows/<runId>.json` (`workflowName`,
//!   `status`, `startTime`), falling back to the main transcript's
//!   `toolUseResult.{runId,workflowName}` line.
//!
//! Cost: directories are listed and files `stat`ed; `since_days` filters on the
//! agent jsonl mtime *before* anything is opened. Per agent only `.meta.json`,
//! the first line and the tail of the jsonl are read. History is returned
//! regardless of registry state (disabling never hides history).

use crate::discovery::AgentMeta;
use crate::event::parse_ts;
use serde::Serialize;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub session_id: String,
    pub project: String,
    /// `a:<sid>:<agentId>` or `w:<sid>:<runId>`; same as the GUI `key_of`.
    pub node_key: String,
    pub started: Option<i64>,
    pub ended: Option<i64>,
    pub status: String,
    pub description: String,
}

/// Last-line read window; a line longer than this yields no end timestamp.
const TAIL_BYTES: u64 = 256 * 1024;
/// A transcript touched within this window is considered still running.
const RUNNING_MS: i64 = 60_000;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn mtime_ms(p: &Path) -> Option<i64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(m.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_millis() as i64)
}

/// Oldest mtime (ms) still inside the window; `since_days == 0` means no limit.
fn cutoff_ms(since_days: u32) -> i64 {
    if since_days == 0 {
        return i64::MIN;
    }
    now_ms() - Duration::from_secs(since_days as u64 * 86_400).as_millis() as i64
}

fn ts_of_line(line: &str) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    parse_ts(v.get("timestamp")?.as_str()?)
}

fn first_ts(p: &Path) -> Option<i64> {
    let mut r = BufReader::new(std::fs::File::open(p).ok()?);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    ts_of_line(&line)
}

fn last_ts(p: &Path) -> Option<i64> {
    let mut f = std::fs::File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .and_then(ts_of_line)
}

fn dirs_in(p: &Path) -> Vec<std::fs::DirEntry> {
    std::fs::read_dir(p)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn name_of(e: &std::fs::DirEntry) -> String {
    e.file_name().to_string_lossy().into_owned()
}

/// `agent-<id>.jsonl` files in `dir` that are inside the time window.
fn recent_agent_files(dir: &Path, cutoff: i64) -> Vec<(String, std::path::PathBuf, i64)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut v: Vec<_> = rd
        .flatten()
        .filter_map(|e| {
            let n = name_of(&e);
            let id = n
                .strip_prefix("agent-")?
                .strip_suffix(".jsonl")?
                .to_string();
            let p = e.path();
            let m = mtime_ms(&p)?;
            (m >= cutoff).then_some((id, p, m))
        })
        .collect();
    v.sort();
    v
}

fn read_meta(dir: &Path, id: &str) -> AgentMeta {
    std::fs::read_to_string(dir.join(format!("agent-{id}.meta.json")))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn status_of(mtime: i64) -> &'static str {
    if now_ms() - mtime < RUNNING_MS {
        "running"
    } else {
        "completed"
    }
}

/// `(project, sid, session_dir)` for every session directory under the root.
fn sessions(root: &Path) -> Vec<(String, String, std::path::PathBuf)> {
    let mut out = vec![];
    for proj in dirs_in(root) {
        for s in dirs_in(&proj.path()) {
            out.push((name_of(&proj), name_of(&s), s.path()));
        }
    }
    out.sort();
    out
}

pub fn agent_history(projects_root: &Path, agent: &str, since_days: u32) -> Vec<RunRecord> {
    let cutoff = cutoff_ms(since_days);
    let mut out = vec![];
    for (project, sid, sdir) in sessions(projects_root) {
        let dir = sdir.join("subagents");
        for (id, path, mtime) in recent_agent_files(&dir, cutoff) {
            let meta = read_meta(&dir, &id);
            if meta.agent_type.as_deref().unwrap_or("general-purpose") != agent {
                continue;
            }
            let running = status_of(mtime) == "running";
            out.push(RunRecord {
                session_id: sid.clone(),
                project: project.clone(),
                node_key: format!("a:{sid}:{id}"),
                started: first_ts(&path),
                ended: if running { None } else { last_ts(&path) },
                status: status_of(mtime).into(),
                description: meta.description.unwrap_or_default(),
            });
        }
    }
    out.sort_by(|a, b| b.started.cmp(&a.started));
    out
}

#[derive(serde::Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RunFile {
    workflow_name: Option<String>,
    status: Option<String>,
    summary: Option<String>,
}

fn read_run_file(sdir: &Path, run: &str) -> RunFile {
    std::fs::read_to_string(sdir.join("workflows").join(format!("{run}.json")))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Cap on bytes read from one main transcript while looking for workflow names.
const SCAN_CAP: u64 = 32 * 1024 * 1024;
/// A "running" run untouched for this long is reported as `unknown`.
const STALE_MS: i64 = 10 * 60_000;

/// Fallback names: ONE bounded scan of the main transcript per session,
/// collecting every `toolUseResult.{runId,workflowName}` pair. Lines are
/// pre-filtered with a substring test; non-UTF8 and unparsable lines are skipped.
fn names_from_transcript(proj_dir: &Path, sid: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Ok(f) = std::fs::File::open(proj_dir.join(format!("{sid}.jsonl"))) else {
        return map;
    };
    let mut r = BufReader::new(f.take(SCAN_CAP));
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        if !line.contains("\"runId\"") || !line.contains("workflowName") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let res = &v["toolUseResult"];
        if let (Some(id), Some(n)) = (res["runId"].as_str(), res["workflowName"].as_str()) {
            map.entry(id.to_string()).or_insert_with(|| n.to_string());
        }
    }
    map
}

pub fn workflow_history(projects_root: &Path, workflow: &str, since_days: u32) -> Vec<RunRecord> {
    let cutoff = cutoff_ms(since_days);
    let mut out = vec![];
    for (project, sid, sdir) in sessions(projects_root) {
        let proj_dir = projects_root.join(&project);
        let mut transcript_names = None; // scanned lazily, at most once per session
        for run in dirs_in(&sdir.join("subagents").join("workflows")) {
            let run_id = name_of(&run);
            let files = recent_agent_files(&run.path(), cutoff);
            if files.is_empty() {
                continue;
            }
            let rf = read_run_file(&sdir, &run_id);
            let name = rf.workflow_name.clone().or_else(|| {
                transcript_names
                    .get_or_insert_with(|| names_from_transcript(&proj_dir, &sid))
                    .get(&run_id)
                    .cloned()
            });
            if name.as_deref() != Some(workflow) {
                continue;
            }
            let started = files.iter().filter_map(|(_, p, _)| first_ts(p)).min();
            let newest = files.iter().map(|f| f.2).max().unwrap_or(0);
            let run_json = sdir.join("workflows").join(format!("{run_id}.json"));
            let latest = newest.max(mtime_ms(&run_json).unwrap_or(0));
            let mut status = match rf.status {
                Some(s) if !s.is_empty() => s,
                _ => status_of(newest).into(),
            };
            let mut ended = None;
            if status == "running" && now_ms() - latest >= STALE_MS {
                // crashed/stale run: it will never report completion
                status = "unknown".into();
                ended = Some(latest);
            } else if status != "running" {
                ended = files.iter().filter_map(|(_, p, _)| last_ts(p)).max();
            }
            out.push(RunRecord {
                session_id: sid.clone(),
                project: project.clone(),
                node_key: format!("w:{sid}:{run_id}"),
                started,
                ended,
                status,
                description: rf.summary.unwrap_or_else(|| workflow.to_string()),
            });
        }
    }
    out.sort_by(|a, b| b.started.cmp(&a.started));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::write;

    fn line(ts: &str) -> String {
        format!("{{\"type\":\"user\",\"timestamp\":\"{ts}\"}}\n")
    }

    fn agent(root: &Path, sub: &str, id: &str, meta: &str, t0: &str, t1: &str) {
        write(
            &root.join(format!("{sub}/agent-{id}.jsonl")),
            &format!("{}{}", line(t0), line(t1)),
        );
        // finished runs: transcript last touched a day ago
        std::fs::File::options()
            .write(true)
            .open(root.join(format!("{sub}/agent-{id}.jsonl")))
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(86_400))
            .unwrap();
        if !meta.is_empty() {
            write(&root.join(format!("{sub}/agent-{id}.meta.json")), meta);
        }
    }

    #[test]
    fn history_lists_runs_of_agent_type_across_sessions() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        agent(
            r,
            "p1/s1/subagents",
            "a1",
            r#"{"agentType":"Explore","description":"look"}"#,
            "2026-11-25T07:00:00.000Z",
            "2026-11-25T07:01:00.000Z",
        );
        agent(
            r,
            "p2/s2/subagents",
            "a2",
            r#"{"agentType":"Explore","description":"again"}"#,
            "2026-11-25T08:00:00.000Z",
            "2026-11-25T08:05:00.000Z",
        );
        agent(
            r,
            "p1/s1/subagents",
            "a3",
            r#"{"agentType":"Plan"}"#,
            "2026-11-25T07:00:00.000Z",
            "2026-11-25T07:01:00.000Z",
        );
        agent(
            r,
            "p1/s1/subagents",
            "a4",
            "not json",
            "2026-11-25T07:00:00.000Z",
            "2026-11-25T07:01:00.000Z",
        );
        write(&r.join("p1/s1/subagents/agent-bad.jsonl"), "\u{0}garbage");
        let h = agent_history(r, "Explore", 0);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].node_key, "a:s2:a2"); // newest first
        assert_eq!(h[0].project, "p2");
        assert_eq!(h[0].description, "again");
        assert_eq!(h[0].started, parse_ts("2026-11-25T08:00:00.000Z"));
        assert_eq!(h[0].ended, parse_ts("2026-11-25T08:05:00.000Z"));
        assert_eq!(h[0].status, "completed");
        assert_eq!(h[1].node_key, "a:s1:a1");
        // missing/unparseable meta => general-purpose
        let g = agent_history(r, "general-purpose", 0);
        assert_eq!(g.len(), 2); // a4 (bad meta) and `bad` (no meta)
    }

    #[test]
    fn since_days_filters_by_mtime() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        agent(
            r,
            "p/s/subagents",
            "old",
            r#"{"agentType":"X"}"#,
            "2026-01-01T00:00:00.000Z",
            "2026-01-01T00:01:00.000Z",
        );
        agent(
            r,
            "p/s/subagents",
            "new",
            r#"{"agentType":"X"}"#,
            "2026-01-01T00:00:00.000Z",
            "2026-01-01T00:01:00.000Z",
        );
        let old = std::fs::File::options()
            .write(true)
            .open(r.join("p/s/subagents/agent-old.jsonl"))
            .unwrap();
        old.set_modified(SystemTime::now() - Duration::from_secs(10 * 86_400))
            .unwrap();
        let h = agent_history(r, "X", 3);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].status, "completed");
        assert_eq!(h[0].node_key, "a:s:new");
        assert_eq!(agent_history(r, "X", 0).len(), 2);
    }

    #[test]
    fn disabled_agent_still_has_history() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        agent(
            r,
            "p/s/subagents",
            "a1",
            r#"{"agentType":"reviewer"}"#,
            "2026-11-25T07:00:00.000Z",
            "2026-11-25T07:01:00.000Z",
        );
        let home = r.join("home/.claude");
        write(
            &home.join("agents/reviewer.md"),
            "---\nname: reviewer\n---\n",
        );
        let mut reg = crate::control::Registry::default();
        reg.disabled_agents.insert("reviewer".into());
        let ctx = crate::catalog::ScanCtx {
            claude_home: home,
            claude_json: r.join("c.json"),
            cwd: None,
            registry: reg,
        };
        let cat = crate::catalog::Catalog::scan(&ctx);
        let cap = cat
            .of(crate::catalog::CapKind::Agent)
            .find(|c| c.id == "reviewer")
            .unwrap();
        assert!(!cap.enabled_globally);
        assert!(cap.path.as_ref().unwrap().exists()); // files kept
        assert_eq!(agent_history(r, "reviewer", 0).len(), 1);
    }

    #[test]
    fn workflow_runs_grouped_by_run_id() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        // run 1: name from run json; two agents
        let wf = "p/s/subagents/workflows";
        agent(
            r,
            &format!("{wf}/wf_1"),
            "x",
            "",
            "2026-11-25T07:00:00.000Z",
            "2026-11-25T07:02:00.000Z",
        );
        agent(
            r,
            &format!("{wf}/wf_1"),
            "y",
            "",
            "2026-11-25T07:01:00.000Z",
            "2026-11-25T07:03:00.000Z",
        );
        write(
            &r.join("p/s/workflows/wf_1.json"),
            r#"{"runId":"wf_1","workflowName":"review","status":"completed","script":"x"}"#,
        );
        // run 2: no run json; name from main transcript
        agent(
            r,
            &format!("{wf}/wf_2"),
            "z",
            "",
            "2026-11-25T09:00:00.000Z",
            "2026-11-25T09:01:00.000Z",
        );
        write(
            &r.join("p/s.jsonl"),
            &format!(
                "{}{}\n",
                line("2026-11-25T06:00:00.000Z"),
                r#"{"type":"user","toolUseResult":{"runId":"wf_2","workflowName":"review"}}"#
            ),
        );
        // run 3: other workflow
        agent(
            r,
            &format!("{wf}/wf_3"),
            "w",
            "",
            "2026-11-25T09:00:00.000Z",
            "2026-11-25T09:01:00.000Z",
        );
        write(
            &r.join("p/s/workflows/wf_3.json"),
            r#"{"workflowName":"other"}"#,
        );
        let h = workflow_history(r, "review", 0);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].node_key, "w:s:wf_2");
        assert_eq!(h[1].node_key, "w:s:wf_1");
        assert_eq!(h[1].started, parse_ts("2026-11-25T07:00:00.000Z"));
        assert_eq!(h[1].ended, parse_ts("2026-11-25T07:03:00.000Z"));
        assert_eq!(h[1].status, "completed");
        assert_eq!(workflow_history(r, "other", 0).len(), 1);
        assert!(workflow_history(r, "nope", 0).is_empty());
    }

    #[test]
    fn one_transcript_resolves_many_runs_tolerating_bad_lines() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        let wf = "p/s/subagents/workflows";
        for run in ["wf_a", "wf_b"] {
            agent(
                r,
                &format!("{wf}/{run}"),
                "x",
                "",
                "2026-11-25T07:00:00.000Z",
                "2026-11-25T07:01:00.000Z",
            );
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"{\"runId\": broken workflowName\n");
        bytes.extend_from_slice(&[0xff, 0xfe, b'\n']);
        bytes.extend_from_slice(br#"{"toolUseResult":{"runId":"wf_a","workflowName":"review"}}"#);
        bytes.push(b'\n');
        bytes.extend_from_slice(br#"{"toolUseResult":{"runId":"wf_b","workflowName":"review"}}"#);
        bytes.push(b'\n');
        std::fs::write(r.join("p/s.jsonl"), bytes).unwrap();
        assert_eq!(workflow_history(r, "review", 0).len(), 2);
    }

    #[test]
    fn stale_running_workflow_becomes_unknown() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        let wf = "p/s/subagents/workflows";
        agent(
            r,
            &format!("{wf}/wf_s"),
            "x",
            "",
            "2026-11-25T07:00:00.000Z",
            "2026-11-25T07:01:00.000Z",
        );
        agent(
            r,
            &format!("{wf}/wf_f"),
            "x",
            "",
            "2026-11-25T07:00:00.000Z",
            "2026-11-25T07:01:00.000Z",
        );
        let j = r#"{"workflowName":"w","status":"running"}"#;
        write(&r.join("p/s/workflows/wf_s.json"), j);
        write(&r.join("p/s/workflows/wf_f.json"), j);
        let old = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(r.join("p/s/workflows/wf_s.json"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(r.join(format!("{wf}/wf_s/agent-x.jsonl")))
            .unwrap()
            .set_modified(old)
            .unwrap();
        // wf_f: agent file is 1 day old but the run json is fresh => still running
        let h = workflow_history(r, "w", 0);
        let s = h.iter().find(|x| x.node_key == "w:s:wf_s").unwrap();
        assert_eq!(s.status, "unknown");
        assert!(s.ended.is_some());
        let f = h.iter().find(|x| x.node_key == "w:s:wf_f").unwrap();
        assert_eq!(f.status, "running");
        assert_eq!(f.ended, None);
    }
}
