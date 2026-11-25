//! Read the tail of a Claude Code background task's output file (`.../tasks/<id>.output`).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Default and maximum number of bytes returned.
pub const TAIL_MAX: u64 = 64 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskTail {
    pub text: String,
    pub truncated: bool,
    pub gone: bool,
    pub size: u64,
}

impl TaskTail {
    fn gone() -> Self {
        Self {
            gone: true,
            ..Self::default()
        }
    }
}

fn under_allowed_root(p: &Path) -> bool {
    let tmp = std::env::temp_dir().canonicalize().ok();
    if tmp.as_ref().is_some_and(|t| p.starts_with(t)) {
        return true;
    }
    // `/tmp/claude-<uid>/...` (also when temp_dir() is elsewhere, e.g. $TMPDIR).
    let slash_tmp = Path::new("/tmp").canonicalize().ok();
    slash_tmp.is_some_and(|t| {
        p.strip_prefix(&t)
            .ok()
            .and_then(|r| r.components().next())
            .is_some_and(|c| c.as_os_str().to_string_lossy().starts_with("claude-"))
    })
}

/// Canonical path when `p` is a `*.output` file inside a directory named `tasks`,
/// under the temp dir or a `/tmp/claude-*` directory. Symlinks and `..` are resolved
/// before checking, so escapes are rejected. Only regular files pass: opening a
/// FIFO (or device) would block forever.
fn resolve_task_output(p: &Path) -> Option<PathBuf> {
    let c = p.canonicalize().ok()?;
    if !std::fs::metadata(&c).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let ok_name = c.extension().is_some_and(|e| e == "output");
    let ok_dir = c
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|n| n == "tasks");
    (ok_name && ok_dir && under_allowed_root(&c)).then_some(c)
}

pub fn is_task_output_path(p: &Path) -> bool {
    resolve_task_output(p).is_some()
}

/// Last `max` bytes (capped at [`TAIL_MAX`]) of a task output file.
/// Paths that fail the guard or cannot be read yield `gone: true`.
pub fn tail_task_output(path: &Path, max: u64) -> TaskTail {
    let Some(p) = resolve_task_output(path) else {
        return TaskTail::gone();
    };
    let max = max.clamp(1, TAIL_MAX);
    let Ok(mut f) = File::open(&p) else {
        return TaskTail::gone();
    };
    let Ok(size) = f.metadata().map(|m| m.len()) else {
        return TaskTail::gone();
    };
    let start = size.saturating_sub(max);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return TaskTail::gone();
    }
    let mut buf = Vec::new();
    if f.take(max).read_to_end(&mut buf).is_err() {
        return TaskTail::gone();
    }
    let truncated = start > 0;
    if truncated {
        // Drop a UTF-8 character cut in half at the start.
        let skip = buf
            .iter()
            .take(3)
            .take_while(|b| (**b & 0xC0) == 0x80)
            .count();
        buf.drain(..skip);
    }
    TaskTail {
        text: String::from_utf8_lossy(&buf).into_owned(),
        truncated,
        gone: false,
        size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tasks_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("ccmon-tasktail-{tag}-{}", std::process::id()))
            .join("tasks");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn tail_missing_file_is_gone() {
        let d = tasks_dir("missing");
        let t = tail_task_output(&d.join("nope.output"), TAIL_MAX);
        assert!(t.gone);
        assert!(t.text.is_empty());
    }

    #[test]
    fn tail_reads_last_64k() {
        let d = tasks_dir("big");
        let f = d.join("x.output");
        std::fs::write(&f, "x".repeat(100 * 1024)).unwrap();
        let t = tail_task_output(&f, TAIL_MAX);
        assert!(!t.gone);
        assert_eq!(t.text.len(), 65536);
        assert!(t.truncated);
        assert_eq!(t.size, 100 * 1024);
        let small = d.join("s.output");
        std::fs::write(&small, "hi\n").unwrap();
        let t = tail_task_output(&small, 10 * TAIL_MAX);
        assert_eq!((t.text.as_str(), t.truncated), ("hi\n", false));
    }

    #[test]
    fn tail_rejects_outside_path() {
        let t = tail_task_output(Path::new("/etc/passwd"), TAIL_MAX);
        assert!(t.gone);
        assert!(t.text.is_empty());
        // right name, wrong directory
        let d = tasks_dir("outside");
        let other = d.parent().unwrap().join("y.output");
        std::fs::write(&other, "secret").unwrap();
        assert!(tail_task_output(&other, TAIL_MAX).gone);
        // `..` and symlink escapes
        #[cfg(unix)]
        {
            let link = d.join("link.output");
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink("/etc/passwd", &link).unwrap();
            assert!(tail_task_output(&link, TAIL_MAX).gone);
        }
        let dots = d
            .join("..")
            .join("..")
            .join("..")
            .join("etc")
            .join("passwd");
        assert!(!is_task_output_path(&dots));
    }

    #[test]
    fn tail_rejects_non_regular_files() {
        let d = tasks_dir("special");
        let dir = d.join("dir.output");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(tail_task_output(&dir, TAIL_MAX).gone);
        #[cfg(unix)]
        {
            let fifo = d.join("fifo.output");
            let _ = std::fs::remove_file(&fifo);
            let ok = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .is_ok_and(|s| s.success());
            if ok {
                assert!(!is_task_output_path(&fifo));
                // Must return instead of blocking in open().
                let (tx, rx) = std::sync::mpsc::channel();
                let p = fifo.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(tail_task_output(&p, TAIL_MAX));
                });
                let t = rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .expect("tail_task_output blocked on a FIFO");
                assert!(t.gone);
            }
        }
    }
}
