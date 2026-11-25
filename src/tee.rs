//! `ccmon tee -- <statusline command>`: snapshot Claude Code's status-line JSON
//! (documented at https://code.claude.com/docs/en/statusline.md) for ccmon, then pass
//! the same stdin to the user's original status-line command and print its output.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Status-line snapshots. Override the base with `CCMON_CACHE_DIR` (tests, demos).
pub fn cache_dir() -> PathBuf {
    std::env::var_os("CCMON_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("ccmon")
        })
        .join("statusline")
}

/// Write `<dir>/<session_id>.json` atomically. Returns the session id if found.
pub fn snapshot(input: &[u8], dir: &Path) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(input).ok()?;
    let sid = v.get("session_id")?.as_str()?;
    if sid.is_empty() || sid.contains('/') || sid.contains("..") {
        return None;
    }
    std::fs::create_dir_all(dir).ok()?;
    let tmp = dir.join(format!(".{sid}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, input).ok()?;
    std::fs::rename(&tmp, dir.join(format!("{sid}.json"))).ok()?;
    Some(sid.to_string())
}

pub fn run_tee(cmd: &[String]) -> i32 {
    let mut input = Vec::new();
    if std::io::stdin().read_to_end(&mut input).is_err() {
        return 1;
    }
    snapshot(&input, &cache_dir());
    if cmd.is_empty() {
        return 0;
    }
    let child = crate::platform::shell(&cmd.join(" "))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn();
    let Ok(mut child) = child else { return 127 };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&input);
    }
    match child.wait_with_output() {
        Ok(out) => {
            let _ = std::io::stdout().write_all(&out.stdout);
            out.status.code().unwrap_or(1)
        }
        Err(_) => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_writes_by_session_id() {
        let d = tempfile::tempdir().unwrap();
        let sid = snapshot(br#"{"session_id":"abc","rate_limits":{}}"#, d.path());
        assert_eq!(sid.as_deref(), Some("abc"));
        assert!(d.path().join("abc.json").exists());
        assert_eq!(snapshot(br#"{"session_id":"../x"}"#, d.path()), None);
        assert_eq!(snapshot(b"not json", d.path()), None);
    }
}
