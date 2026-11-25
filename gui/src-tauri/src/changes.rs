//! Files changed in a session and side-by-side diffs.
//!
//! Baselines:
//! - "session": the file as it was before the session first changed it, from Claude Code's
//!   own backups (`file-history-delta` lines + `~/.claude/file-history/<sid>/<backup>`);
//!   `backupFileName: null` on the first version means the file was created in the session.
//! - "git": `git show HEAD:<path>` when the file is inside a git work tree.

use serde::Serialize;
use serde_json::Value;
use similar::{ChangeTag, DiffOp, TextDiff};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
const CONTEXT: usize = 3;

/// path → earliest backup file name (None = created in this session).
pub fn session_baselines(transcript: &Path) -> HashMap<String, Option<String>> {
    let mut out: HashMap<String, (u64, Option<String>)> = HashMap::new();
    let Ok(text) = std::fs::read_to_string(transcript) else {
        return HashMap::new();
    };
    for line in text.lines() {
        if !line.contains("\"file-history-delta\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(path) = v.get("trackingPath").and_then(Value::as_str) else {
            continue;
        };
        let b = v.get("backup");
        let version = b
            .and_then(|b| b.get("version"))
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        let name = b
            .and_then(|b| b.get("backupFileName"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let e = out
            .entry(path.to_string())
            .or_insert((version, name.clone()));
        if version < e.0 {
            *e = (version, name);
        }
    }
    out.into_iter().map(|(k, (_, n))| (k, n)).collect()
}

pub fn file_history_dir(sid: &str) -> PathBuf {
    cc_monitor::platform::claude_home()
        .join("file-history")
        .join(sid)
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn repo_root(path: &Path) -> Option<PathBuf> {
    let dir = path.parent()?;
    let mut d = dir;
    while !d.exists() {
        d = d.parent()?;
    }
    git(d, &["rev-parse", "--show-toplevel"]).map(|s| PathBuf::from(s.trim()))
}

/// `git status --porcelain` codes per absolute path, for one repo.
fn repo_status(root: &Path) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Some(out) = git(root, &["status", "--porcelain=v1", "--untracked-files=all"]) {
        for l in out.lines() {
            if l.len() < 4 {
                continue;
            }
            let code = l[..2].trim().to_string();
            let rel = l[3..]
                .rsplit(" -> ")
                .next()
                .unwrap_or(&l[3..])
                .trim_matches('"');
            m.insert(root.join(rel).to_string_lossy().into_owned(), code);
        }
    }
    m
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub path: String,
    pub edits: u32,
    pub last_ts: Option<i64>,
    pub exists: bool,
    pub created_in_session: bool,
    pub has_session_baseline: bool,
    pub repo: Option<String>,
    pub git_status: Option<String>,
    pub added: usize,
    pub removed: usize,
}

pub struct Touch {
    pub edits: u32,
    pub last_ts: Option<i64>,
}

fn read_text(p: &Path) -> Option<String> {
    let bytes = std::fs::read(p).ok()?;
    if bytes.len() > MAX_DIFF_BYTES || bytes.iter().take(8192).any(|&b| b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn baseline_text(
    path: &str,
    mode: &str,
    sid: &str,
    baselines: &HashMap<String, Option<String>>,
) -> Option<String> {
    match mode {
        "git" => {
            let p = Path::new(path);
            let root = repo_root(p)?;
            let rel = p.strip_prefix(&root).ok()?.to_string_lossy().into_owned();
            Some(git(&root, &["show", &format!("HEAD:{rel}")]).unwrap_or_default())
        }
        _ => match baselines.get(path)? {
            None => Some(String::new()),
            Some(name) => read_text(&file_history_dir(sid).join(name)),
        },
    }
}

pub fn list_changed(
    touched: BTreeMap<String, Touch>,
    transcript: &Path,
    sid: &str,
) -> Vec<ChangedFile> {
    let baselines = session_baselines(transcript);
    let mut statuses: HashMap<PathBuf, HashMap<String, String>> = HashMap::new();
    let mut out = Vec::new();
    for (path, t) in touched {
        let p = Path::new(&path);
        let root = repo_root(p);
        let git_status = root.as_ref().and_then(|r| {
            statuses
                .entry(r.clone())
                .or_insert_with(|| repo_status(r))
                .get(&path)
                .cloned()
        });
        let base = baseline_text(&path, "session", sid, &baselines)
            .or_else(|| baseline_text(&path, "git", sid, &baselines));
        let cur = read_text(p).unwrap_or_default();
        let (added, removed) = base.as_deref().map(|b| stats(b, &cur)).unwrap_or((0, 0));
        out.push(ChangedFile {
            exists: p.exists(),
            created_in_session: matches!(baselines.get(&path), Some(None)),
            has_session_baseline: baselines.contains_key(&path),
            repo: root.map(|r| r.display().to_string()),
            git_status,
            path,
            edits: t.edits,
            last_ts: t.last_ts,
            added,
            removed,
        });
    }
    out.sort_by_key(|f| std::cmp::Reverse(f.last_ts));
    out
}

fn stats(a: &str, b: &str) -> (usize, usize) {
    let d = TextDiff::from_lines(a, b);
    let mut add = 0;
    let mut del = 0;
    for c in d.iter_all_changes() {
        match c.tag() {
            ChangeTag::Insert => add += 1,
            ChangeTag::Delete => del += 1,
            ChangeTag::Equal => {}
        }
    }
    (add, del)
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Side {
    pub no: usize,
    pub text: String,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    /// equal | delete | insert | replace | fold
    pub kind: &'static str,
    pub left: Option<Side>,
    pub right: Option<Side>,
    /// For `fold`: number of unchanged lines hidden.
    pub hidden: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffView {
    pub path: String,
    pub baseline: String,
    pub baseline_label: String,
    pub available: Vec<String>,
    pub rows: Vec<Row>,
    pub added: usize,
    pub removed: usize,
    pub note: Option<String>,
}

fn line(s: &str) -> String {
    s.trim_end_matches(['\n', '\r']).to_string()
}

/// Side-by-side rows; long unchanged runs are folded to `CONTEXT` lines each side.
pub fn side_by_side(old: &str, new: &str) -> Vec<Row> {
    let d = TextDiff::from_lines(old, new);
    let ol: Vec<&str> = d.old_slices().to_vec();
    let nl: Vec<&str> = d.new_slices().to_vec();
    let mut rows = Vec::new();
    let ops = d.ops();
    for (oi, op) in ops.iter().enumerate() {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                let first = oi == 0;
                let last = oi == ops.len() - 1;
                let keep_head = if first { 0 } else { CONTEXT };
                let keep_tail = if last { 0 } else { CONTEXT };
                let push = |rows: &mut Vec<Row>, k: usize| {
                    rows.push(Row {
                        kind: "equal",
                        left: Some(Side {
                            no: old_index + k + 1,
                            text: line(ol[old_index + k]),
                        }),
                        right: Some(Side {
                            no: new_index + k + 1,
                            text: line(nl[new_index + k]),
                        }),
                        hidden: 0,
                    })
                };
                if len <= keep_head + keep_tail + 2 {
                    for k in 0..len {
                        push(&mut rows, k);
                    }
                } else {
                    for k in 0..keep_head {
                        push(&mut rows, k);
                    }
                    rows.push(Row {
                        kind: "fold",
                        left: None,
                        right: None,
                        hidden: len - keep_head - keep_tail,
                    });
                    for k in len - keep_tail..len {
                        push(&mut rows, k);
                    }
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for k in 0..old_len {
                    rows.push(Row {
                        kind: "delete",
                        left: Some(Side {
                            no: old_index + k + 1,
                            text: line(ol[old_index + k]),
                        }),
                        right: None,
                        hidden: 0,
                    });
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for k in 0..new_len {
                    rows.push(Row {
                        kind: "insert",
                        left: None,
                        right: Some(Side {
                            no: new_index + k + 1,
                            text: line(nl[new_index + k]),
                        }),
                        hidden: 0,
                    });
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                for k in 0..old_len.max(new_len) {
                    rows.push(Row {
                        kind: "replace",
                        left: (k < old_len).then(|| Side {
                            no: old_index + k + 1,
                            text: line(ol[old_index + k]),
                        }),
                        right: (k < new_len).then(|| Side {
                            no: new_index + k + 1,
                            text: line(nl[new_index + k]),
                        }),
                        hidden: 0,
                    });
                }
            }
        }
    }
    rows
}

pub fn diff_view(path: &str, mode: Option<&str>, transcript: &Path, sid: &str) -> DiffView {
    let baselines = session_baselines(transcript);
    let mut available = Vec::new();
    if baselines.contains_key(path) {
        available.push("session".to_string());
    }
    if repo_root(Path::new(path)).is_some() {
        available.push("git".to_string());
    }
    let mode = mode
        .filter(|m| available.iter().any(|a| a == m))
        .map(str::to_string)
        .or_else(|| available.first().cloned())
        .unwrap_or_else(|| "none".into());
    let cur = read_text(Path::new(path));
    let base = baseline_text(path, &mode, sid, &baselines);
    let label = match mode.as_str() {
        "session" if matches!(baselines.get(path), Some(None)) => {
            "Trước phiên (file chưa tồn tại)".to_string()
        }
        "session" => "Trước phiên".to_string(),
        "git" => "git HEAD".to_string(),
        _ => "Không có bản gốc".to_string(),
    };
    let note = match (&base, &cur) {
        (None, _) if mode != "none" => {
            Some("Không đọc được bản gốc (nhị phân, quá lớn hoặc bản sao đã bị xoá).".into())
        }
        (_, None) if Path::new(path).exists() => {
            Some("File hiện tại là nhị phân hoặc quá lớn.".into())
        }
        (_, None) => Some("File hiện không còn tồn tại.".into()),
        _ => None,
    };
    let old = base.unwrap_or_default();
    let new = cur.unwrap_or_default();
    let (added, removed) = stats(&old, &new);
    let rows = if added == 0 && removed == 0 {
        Vec::new()
    } else {
        side_by_side(&old, &new)
    };
    DiffView {
        path: path.to_string(),
        baseline: mode,
        baseline_label: label,
        available,
        rows,
        added,
        removed,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_by_side_pairs_and_folds() {
        let old: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        let new = old.replace("l10\n", "L10\nextra\n");
        let rows = side_by_side(&old, &new);
        let kinds: Vec<&str> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds.first(), Some(&"fold"));
        assert!(kinds.contains(&"replace"));
        let rep: Vec<&Row> = rows.iter().filter(|r| r.kind == "replace").collect();
        assert_eq!(rep[0].left.as_ref().unwrap().text, "l10");
        assert_eq!(rep[0].right.as_ref().unwrap().text, "L10");
        assert!(rep[1].left.is_none());
        assert_eq!(stats(&old, &new), (2, 1));
    }

    #[test]
    fn baselines_take_earliest_version() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("s.jsonl");
        std::fs::write(
            &t,
            concat!(
                r#"{"type":"file-history-delta","trackingPath":"/a","backup":{"backupFileName":"h@v2","version":2}}"#, "\n",
                r#"{"type":"file-history-delta","trackingPath":"/a","backup":{"backupFileName":"h@v1","version":1}}"#, "\n",
                r#"{"type":"file-history-delta","trackingPath":"/new","backup":{"backupFileName":null,"version":1}}"#, "\n"
            ),
        )
        .unwrap();
        let b = session_baselines(&t);
        assert_eq!(b["/a"].as_deref(), Some("h@v1"));
        assert_eq!(b["/new"], None);
    }
}
