//! Session history listing, per-session assets (user files, images, touched files,
//! scratchpad, command log) and file previews for the sidebar. Read-only.

use cc_monitor::thread::{Item, Thread, ToolState, UserFileKind};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"];
const TEXT_PREVIEW_MAX: u64 = 512 * 1024;
const IMAGE_MAX: u64 = 20 * 1024 * 1024;
const SCRATCH_MAX_FILES: usize = 500;

pub fn is_image(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXT.contains(&e.to_ascii_lowercase().as_str()))
}

fn mtime_ms(m: &std::fs::Metadata) -> Option<i64> {
    Some(
        m.modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis() as i64,
    )
}

// ---------- history ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySession {
    pub sid: String,
    pub mtime: i64,
    pub size: u64,
    pub title: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryProject {
    pub project: String,
    pub label: String,
    pub tool: &'static str,
    pub sessions: Vec<HistorySession>,
}

/// Last `"key":"value"` string occurrence in the tail of a file (cheap metadata lookup).
fn tail_string(path: &Path, key: &str) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(256 * 1024);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let pat = format!("\"{key}\":\"");
    let i = text.rfind(&pat)? + pat.len();
    let rest = &text[i..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn project_label(project: &str, sample: Option<&Path>) -> String {
    sample
        .and_then(|p| tail_string(p, "cwd"))
        .and_then(|c| cc_monitor::platform::basename(&c).map(str::to_string))
        .unwrap_or_else(|| project.trim_start_matches('-').to_string())
}

pub fn list_history(root: &Path, days: u32, now_ms: i64) -> Vec<HistoryProject> {
    let cutoff = now_ms - days as i64 * 86_400_000;
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(root) else {
        return out;
    };
    for proj in projects.flatten() {
        let pdir = proj.path();
        if !pdir.is_dir() {
            continue;
        }
        let project = proj.file_name().to_string_lossy().into_owned();
        let mut sessions = Vec::new();
        let mut newest: Option<(i64, PathBuf)> = None;
        for e in std::fs::read_dir(&pdir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(m) = e.metadata() else { continue };
            let Some(mt) = mtime_ms(&m) else { continue };
            if mt < cutoff || m.len() == 0 {
                continue;
            }
            if newest.as_ref().map_or(true, |n| mt > n.0) {
                newest = Some((mt, p.clone()));
            }
            sessions.push(HistorySession {
                sid: p
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                mtime: mt,
                size: m.len(),
                title: tail_string(&p, "aiTitle"),
            });
        }
        if sessions.is_empty() {
            continue;
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.mtime));
        out.push(HistoryProject {
            tool: "claude",
            label: project_label(&project, newest.as_ref().map(|n| n.1.as_path())),
            project,
            sessions,
        });
    }
    out.sort_by_key(|p| std::cmp::Reverse(p.sessions.first().map(|s| s.mtime).unwrap_or(0)));
    out
}

/// Codex sessions (main threads only) grouped by working directory.
pub fn list_codex_history(codex_home: &Path, days: u32, now_ms: i64) -> Vec<HistoryProject> {
    use cc_monitor::discovery::{codex_id_from_name, codex_rollouts, codex_titles};
    let cutoff = now_ms - days as i64 * 86_400_000;
    let titles = codex_titles(&codex_home.join("session_index.jsonl"));
    let mut groups: BTreeMap<String, Vec<HistorySession>> = BTreeMap::new();
    for p in codex_rollouts(&codex_home.join("sessions")) {
        let Ok(m) = std::fs::metadata(&p) else {
            continue;
        };
        let Some(mt) = mtime_ms(&m) else { continue };
        if mt < cutoff {
            continue;
        }
        let Some(id) = p
            .file_stem()
            .and_then(|s| codex_id_from_name(&s.to_string_lossy()))
        else {
            continue;
        };
        let first = std::fs::File::open(&p).ok().and_then(|f| {
            use std::io::BufRead;
            let mut l = String::new();
            std::io::BufReader::new(f).read_line(&mut l).ok()?;
            Some(l)
        });
        let meta = first.as_deref().and_then(cc_monitor::codex::parse_meta);
        if meta.as_ref().is_some_and(|m| m.parent_id.is_some()) {
            continue;
        }
        let cwd = meta.and_then(|m| m.cwd).unwrap_or_default();
        groups.entry(cwd).or_default().push(HistorySession {
            title: titles.get(&id).cloned(),
            sid: id,
            mtime: mt,
            size: m.len(),
        });
    }
    groups
        .into_iter()
        .map(|(cwd, mut sessions)| {
            sessions.sort_by_key(|s| std::cmp::Reverse(s.mtime));
            HistoryProject {
                label: cc_monitor::platform::basename(&cwd)
                    .unwrap_or("codex")
                    .to_string(),
                project: format!("codex:{cwd}"),
                tool: "codex",
                sessions,
            }
        })
        .collect()
}

// ---------- assets ----------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub path: String,
    pub kind: &'static str,
    pub ts: Option<i64>,
    pub exists: bool,
    pub size: Option<u64>,
    pub source: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FileHit {
    pub thread_key: String,
    pub tool_id: String,
    pub idx: usize,
    pub ts: Option<i64>,
    pub op: String,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TouchedFile {
    pub path: String,
    pub reads: u32,
    pub edits: u32,
    pub writes: u32,
    pub last_ts: Option<i64>,
    pub last_op: String,
    pub threads: Vec<String>,
    pub exists: bool,
    pub hits: Vec<FileHit>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub ts: Option<i64>,
    pub thread: String,
    pub thread_key: String,
    pub tool_id: String,
    pub tool: String,
    pub title: String,
    pub command: String,
    pub state: &'static str,
    pub dur_ms: Option<i64>,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Assets {
    pub tmp_dir: Option<String>,
    pub user_files: Vec<FileEntry>,
    pub images: Vec<FileEntry>,
    pub files: Vec<TouchedFile>,
    pub scratch: Vec<FileEntry>,
    pub log: Vec<LogEntry>,
}

/// `/tmp/claude-<uid>/<project>/<sid>` — per-session temp dir (images/, scratchpad/, tasks/).
pub fn session_tmp_dir(project: &str, sid: &str) -> Option<PathBuf> {
    let tmp = std::env::temp_dir();
    for e in std::fs::read_dir(&tmp).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("claude-") {
            continue;
        }
        let d = e.path().join(project).join(sid);
        if d.is_dir() {
            return Some(d);
        }
    }
    None
}

fn entry(path: &str, kind: &'static str, ts: Option<i64>, source: Option<String>) -> FileEntry {
    let meta = std::fs::metadata(path).ok();
    FileEntry {
        path: path.to_string(),
        kind,
        ts: ts.or_else(|| meta.as_ref().and_then(mtime_ms)),
        exists: meta.is_some(),
        size: meta.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
        source,
    }
}

fn walk(dir: &Path, out: &mut Vec<FileEntry>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if out.len() >= SCRATCH_MAX_FILES {
            return;
        }
        let p = e.path();
        let Ok(m) = e.metadata() else { continue };
        if m.is_dir() {
            let name = e.file_name();
            if !matches!(
                name.to_str(),
                Some("target" | "node_modules" | ".git" | "dist")
            ) {
                walk(&p, out);
            }
        } else {
            let s = p.to_string_lossy().into_owned();
            let kind = if is_image(&s) { "image" } else { "file" };
            out.push(FileEntry {
                path: s,
                kind,
                ts: mtime_ms(&m),
                exists: true,
                size: Some(m.len()),
                source: Some("scratchpad".into()),
            });
        }
    }
}

pub struct ThreadRef<'a> {
    pub key: String,
    pub label: String,
    pub thread: &'a mut Thread,
}

pub fn collect(threads: Vec<ThreadRef>, tmp_dir: Option<PathBuf>) -> Assets {
    let mut a = Assets {
        tmp_dir: tmp_dir.as_ref().map(|d| d.display().to_string()),
        ..Default::default()
    };
    let mut touched: BTreeMap<String, TouchedFile> = BTreeMap::new();
    let mut seen_user = std::collections::HashSet::new();
    for tr in threads {
        for f in tr.thread.user_files() {
            if seen_user.insert(f.path.clone()) {
                let kind = match f.kind {
                    UserFileKind::Image => "image",
                    UserFileKind::File => "file",
                    UserFileKind::Directory => "dir",
                };
                a.user_files
                    .push(entry(&f.path, kind, f.ts, Some(tr.label.clone())));
            }
        }
        for (idx, it) in tr.thread.items().iter().enumerate() {
            let Item::Tool(t) = it else { continue };
            let path = t
                .input
                .get("file_path")
                .or_else(|| t.input.get("notebook_path"))
                .and_then(Value::as_str);
            if let Some(path) = path {
                let op = match t.name.as_str() {
                    "Read" => "read",
                    "Edit" | "MultiEdit" | "NotebookEdit" => "edit",
                    "Write" => "write",
                    _ => "",
                };
                if !op.is_empty() {
                    let e = touched
                        .entry(path.to_string())
                        .or_insert_with(|| TouchedFile {
                            path: path.to_string(),
                            ..Default::default()
                        });
                    match op {
                        "read" => e.reads += 1,
                        "edit" => e.edits += 1,
                        _ => e.writes += 1,
                    }
                    e.hits.push(FileHit {
                        thread_key: tr.key.clone(),
                        tool_id: t.id.clone(),
                        idx,
                        ts: t.started,
                        op: op.to_string(),
                    });
                    if t.started >= e.last_ts {
                        e.last_ts = t.started;
                        e.last_op = op.to_string();
                    }
                    if !e.threads.contains(&tr.label) {
                        e.threads.push(tr.label.clone());
                    }
                    if is_image(path) && !seen_user.contains(path) {
                        a.images.push(entry(
                            path,
                            "image",
                            t.started,
                            Some(format!("{} · {}", t.name, tr.label)),
                        ));
                    }
                }
            }
            if t.name == "Patch" {
                let files = t
                    .input
                    .get("files")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for f in files.iter().filter_map(Value::as_str) {
                    let e = touched.entry(f.to_string()).or_insert_with(|| TouchedFile {
                        path: f.to_string(),
                        ..Default::default()
                    });
                    e.edits += 1;
                    e.hits.push(FileHit {
                        thread_key: tr.key.clone(),
                        tool_id: t.id.clone(),
                        idx,
                        ts: t.started,
                        op: "edit".into(),
                    });
                    if t.started >= e.last_ts {
                        e.last_ts = t.started;
                        e.last_op = "edit".into();
                    }
                    if !e.threads.contains(&tr.label) {
                        e.threads.push(tr.label.clone());
                    }
                }
            }
            if t.name == "Bash" || t.name == "PowerShell" {
                let cmd = t
                    .input
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let title = t
                    .input
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| cmd.lines().next().unwrap_or_default().to_string());
                a.log.push(LogEntry {
                    ts: t.started,
                    thread: tr.label.clone(),
                    thread_key: tr.key.clone(),
                    tool_id: t.id.clone(),
                    tool: t.name.clone(),
                    title,
                    command: cmd.chars().take(400).collect(),
                    state: match t.state {
                        ToolState::Running => "running",
                        ToolState::Ok => "ok",
                        ToolState::Err => "error",
                    },
                    dur_ms: t.started.zip(t.ended).map(|(s, e)| e - s),
                });
            }
        }
    }
    if let Some(dir) = &tmp_dir {
        walk(&dir.join("scratchpad"), &mut a.scratch);
        // Pasted images live in <tmp>/images even when a prompt marker was not parsed.
        let mut pasted = Vec::new();
        walk(&dir.join("images"), &mut pasted);
        for p in pasted {
            if seen_user.insert(p.path.clone()) {
                a.user_files.push(FileEntry {
                    source: Some("ảnh dán".into()),
                    ..p
                });
            }
        }
    }
    for s in a.scratch.iter().filter(|s| s.kind == "image") {
        if !a.images.iter().any(|i| i.path == s.path) {
            a.images.push(s.clone());
        }
    }
    for f in touched.values_mut() {
        f.exists = Path::new(&f.path).exists();
        f.hits.sort_by_key(|h| h.ts);
    }
    a.files = touched.into_values().collect();
    a.files.sort_by_key(|f| std::cmp::Reverse(f.last_ts));
    a.images.sort_by_key(|f| std::cmp::Reverse(f.ts));
    a.scratch.sort_by_key(|f| std::cmp::Reverse(f.ts));
    a.user_files.sort_by_key(|f| std::cmp::Reverse(f.ts));
    a.log.sort_by_key(|l| std::cmp::Reverse(l.ts));
    a
}

// ---------- preview ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub kind: &'static str,
    pub path: String,
    pub size: u64,
    pub data_url: Option<String>,
    pub text: Option<String>,
    pub truncated: bool,
    pub entries: Vec<String>,
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

fn mime(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// A path as written in a transcript: `~/x` is under the home directory, a
/// relative path is under `base` (the session cwd).
pub fn resolve_path(path: &str, base: Option<&str>) -> String {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return cc_monitor::platform::home()
            .join(rest)
            .display()
            .to_string();
    }
    let p = Path::new(path);
    match base {
        Some(b) if p.is_relative() && !b.is_empty() => Path::new(b).join(p).display().to_string(),
        _ => path.to_string(),
    }
}

pub fn preview(path: &str) -> Preview {
    let mut p = Preview {
        kind: "missing",
        path: path.to_string(),
        size: 0,
        data_url: None,
        text: None,
        truncated: false,
        entries: vec![],
    };
    let Ok(m) = std::fs::metadata(path) else {
        return p;
    };
    if m.is_dir() {
        p.kind = "dir";
        let mut names: Vec<String> = std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                if e.path().is_dir() {
                    format!("{n}/")
                } else {
                    n
                }
            })
            .collect();
        names.sort();
        p.entries = names;
        return p;
    }
    p.size = m.len();
    if is_image(path) {
        if m.len() > IMAGE_MAX {
            p.kind = "binary";
            return p;
        }
        if let Ok(bytes) = std::fs::read(path) {
            p.kind = "image";
            p.data_url = Some(format!("data:{};base64,{}", mime(path), b64(&bytes)));
        }
        return p;
    }
    let Ok(f) = std::fs::File::open(path) else {
        return p;
    };
    let mut buf = Vec::new();
    let _ = f.take(TEXT_PREVIEW_MAX).read_to_end(&mut buf);
    if buf.iter().take(8192).any(|&b| b == 0) {
        p.kind = "binary";
        return p;
    }
    p.kind = "text";
    p.truncated = m.len() > TEXT_PREVIEW_MAX;
    p.text = Some(String::from_utf8_lossy(&buf).into_owned());
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    #[ignore = "reads the real ~/.claude/projects"]
    fn history_on_real_data() {
        let h = list_history(
            &cc_monitor::discovery::projects_root(),
            30,
            cc_monitor::discovery::now_ms(),
        );
        let n: usize = h.iter().map(|p| p.sessions.len()).sum();
        println!(
            "{} project, {} phiên; ví dụ: {:?}",
            h.len(),
            n,
            h.first()
                .map(|p| (&p.label, p.sessions.first().map(|s| &s.title)))
        );
    }

    #[test]
    fn preview_kinds() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("a.txt");
        std::fs::write(&t, "xin chào").unwrap();
        assert_eq!(
            preview(t.to_str().unwrap()).text.as_deref(),
            Some("xin chào")
        );
        assert_eq!(preview(d.path().to_str().unwrap()).entries, vec!["a.txt"]);
        assert_eq!(preview("/no/such/file").kind, "missing");
    }

    #[test]
    fn touched_file_hits_in_order() {
        use cc_monitor::event::parse_line;
        let mk = |lines: &[&str]| {
            let mut t = Thread::new();
            for l in lines {
                t.push(parse_line(l).expect(l));
            }
            t
        };
        let tool = |uuid: &str, parent: &str, ts: &str, id: &str, name: &str| {
            format!(
                r#"{{"type":"assistant","uuid":"{uuid}","parentUuid":"{parent}","timestamp":"2026-11-25T00:00:{ts}.000Z","message":{{"content":[{{"type":"tool_use","id":"{id}","name":"{name}","input":{{"file_path":"/a.rs"}}}}]}}}}"#
            )
        };
        let mut main = mk(&[
            &tool("A", "", "01", "r1", "Read"),
            &tool("B", "A", "05", "e1", "Edit"),
        ]);
        let mut agent = mk(&[&tool("C", "", "03", "r2", "Read")]);
        let a = collect(
            vec![
                ThreadRef {
                    key: "m:s".into(),
                    label: "main".into(),
                    thread: &mut main,
                },
                ThreadRef {
                    key: "a:s:x".into(),
                    label: "agent".into(),
                    thread: &mut agent,
                },
            ],
            None,
        );
        let f = a.files.iter().find(|f| f.path == "/a.rs").unwrap();
        let got: Vec<_> = f
            .hits
            .iter()
            .map(|h| (h.thread_key.as_str(), h.tool_id.as_str(), h.op.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("m:s", "r1", "read"),
                ("a:s:x", "r2", "read"),
                ("m:s", "e1", "edit")
            ]
        );
        assert!(f.hits.windows(2).all(|w| w[0].ts <= w[1].ts));
        let idx_of = |t: &mut Thread, id: &str| {
            t.items()
                .iter()
                .position(|i| matches!(i, Item::Tool(x) if x.id == id))
                .unwrap()
        };
        assert_eq!(f.hits[0].idx, idx_of(&mut main, "r1"));
        assert_eq!(f.hits[2].idx, idx_of(&mut main, "e1"));
        assert_eq!(f.hits[1].idx, idx_of(&mut agent, "r2"));
    }
}
