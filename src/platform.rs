//! Cross-platform helpers (Linux, Windows): data directories, memory, process liveness,
//! local time offset, path names and shell invocation.

use std::path::PathBuf;
use std::process::Command;

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

/// Claude Code data dir (`~/.claude`). Override with `CCMON_CLAUDE_DIR` (tests, demos).
pub fn claude_home() -> PathBuf {
    std::env::var_os("CCMON_CLAUDE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"))
}

/// Codex data dir (`~/.codex`). Override with `CCMON_CODEX_DIR`.
pub fn codex_home() -> PathBuf {
    std::env::var_os("CCMON_CODEX_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".codex"))
}

/// (used, total) physical memory in bytes.
pub fn memory() -> Option<(u64, u64)> {
    let mut s = sysinfo::System::new();
    s.refresh_memory();
    let total = s.total_memory();
    (total > 0).then(|| (s.used_memory(), total))
}

/// Whether a process with this pid is running.
pub fn pid_alive(pid: u32) -> bool {
    let mut s = sysinfo::System::new();
    s.refresh_process(sysinfo::Pid::from_u32(pid))
}

/// Local UTC offset in milliseconds.
pub fn local_offset_ms() -> i64 {
    use chrono::Offset;
    chrono::Local::now().offset().fix().local_minus_utc() as i64 * 1000
}

/// Last path component, for both `/` and `\` separators.
pub fn basename(p: &str) -> Option<&str> {
    p.rsplit(['/', '\\']).find(|s| !s.is_empty())
}

/// Run a command line through the platform shell (`sh -c` / `cmd /C`).
pub fn shell(cmdline: &str) -> Command {
    if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cmdline);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmdline);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_both_separators() {
        assert_eq!(basename("/a/b/c"), Some("c"));
        assert_eq!(basename(r"C:\Users\me\proj\"), Some("proj"));
        assert_eq!(basename(""), None);
    }

    #[test]
    fn this_process_is_alive_and_memory_known() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(999_999_999));
        let (used, total) = memory().unwrap();
        assert!(used <= total && total > 0);
    }

    #[test]
    fn shell_runs() {
        let out = shell("echo hi").output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("hi"));
    }
}
