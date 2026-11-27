//! Drive the tmux pane of a terminal-run Claude Code session: paste a prompt
//! (bracketed) and press Enter. Argv only, never a shell.
use crate::discovery::LiveInfo;
use serde::Serialize;
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// Refusal when the pane shows a Claude Code permission/selection dialog: the
/// Enter after a paste would answer it.
pub const DIALOG_PENDING: &str =
    "Terminal đang chờ bạn duyệt một quyền — xử lý trong terminal trước rồi gửi lại";

/// Lines of the pane bottom that `dialog_pending` looks at.
const DIALOG_LINES: usize = 15;

/// Markers of Claude Code's permission and selection dialogs.
const DIALOG_MARKERS: [&str; 5] = [
    "Do you want to proceed?",
    "Do you want to make this edit",
    "Do you want to create ",
    "❯ 1. Yes",
    "Enter to select",
];

/// `io::Error` payload for a pending dialog (see `error_text`).
#[derive(Debug)]
struct DialogPending;
impl std::fmt::Display for DialogPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(DIALOG_PENDING)
    }
}
impl std::error::Error for DialogPending {}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Block {
    NotInTmux,
    PaneGone,
    NotForeground,
    Unsupported,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pane {
    pub id: String,
    pub in_mode: bool,
}

/// `"cde:@1.%4"` -> `"%4"`.
pub fn pane_id(tmux_field: &str) -> Option<String> {
    let i = tmux_field.rfind('%')?;
    let digits: String = tmux_field[i + 1..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!digits.is_empty()).then(|| format!("%{digits}"))
}

/// Parse `/proc/<pid>/stat`: true iff `pgrp == tpgid` and `tty_nr != 0`.
pub fn foreground_ok(stat: &str) -> bool {
    let Some(rest) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
        return false;
    };
    // rest: state ppid pgrp session tty_nr tpgid ...
    let f: Vec<&str> = rest.split_whitespace().collect();
    let (Some(pgrp), Some(tty), Some(tpgid)) = (f.get(2), f.get(4), f.get(5)) else {
        return false;
    };
    let (Ok(pgrp), Ok(tty), Ok(tpgid)) = (
        pgrp.parse::<i64>(),
        tty.parse::<i64>(),
        tpgid.parse::<i64>(),
    ) else {
        return false;
    };
    tty != 0 && pgrp == tpgid
}

#[cfg(target_os = "linux")]
pub fn check(live: &LiveInfo) -> Result<Pane, Block> {
    let id = live
        .tmux
        .as_deref()
        .and_then(pane_id)
        .ok_or(Block::NotInTmux)?;
    let out = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            &id,
            "#{pane_tty} #{pane_in_mode}",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| Block::Unsupported)?;
    if !out.status.success() {
        return Err(Block::PaneGone);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut it = text.split_whitespace();
    let tty = it.next().ok_or(Block::PaneGone)?;
    let in_mode = it.next() == Some("1");
    let fd0 =
        std::fs::read_link(format!("/proc/{}/fd/0", live.pid)).map_err(|_| Block::NotForeground)?;
    if fd0.to_string_lossy() != tty {
        return Err(Block::NotForeground);
    }
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", live.pid))
        .map_err(|_| Block::NotForeground)?;
    if !foreground_ok(&stat) {
        return Err(Block::NotForeground);
    }
    Ok(Pane { id, in_mode })
}

#[cfg(not(target_os = "linux"))]
pub fn check(_live: &LiveInfo) -> Result<Pane, Block> {
    Err(Block::Unsupported)
}

/// Argv lists (after `tmux`) that paste buffer `buf` into the pane and submit.
pub fn send_cmds(p: &Pane, buf: &str) -> Vec<Vec<String>> {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut out = Vec::new();
    if p.in_mode {
        out.push(v(&["send-keys", "-t", &p.id, "-X", "cancel"]));
    }
    out.push(v(&["paste-buffer", "-p", "-d", "-b", buf, "-t", &p.id]));
    out.push(v(&["send-keys", "-t", &p.id, "Enter"]));
    out
}

/// True when the last ~15 non-blank lines of a `capture-pane -p` dump show a
/// permission or selection dialog.
pub fn dialog_pending(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(DIALOG_LINES)..];
    tail.iter()
        .any(|l| DIALOG_MARKERS.iter().any(|m| l.contains(m)))
}

/// User-facing text of a `send_text` / `ensure_no_dialog` error.
pub fn error_text(e: &io::Error) -> String {
    if e.get_ref().is_some_and(|i| i.is::<DialogPending>()) {
        DIALOG_PENDING.to_string()
    } else {
        format!("Lỗi tmux: {e}")
    }
}

/// `Err` (see `error_text`) when the pane shows a dialog the Enter of a paste
/// would confirm. Reads the visible pane (`capture-pane -p`), argv only.
pub fn ensure_no_dialog(p: &Pane) -> io::Result<()> {
    let out = Command::new("tmux")
        .args(["capture-pane", "-p", "-t", &p.id])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("tmux capture-pane failed"));
    }
    if dialog_pending(&String::from_utf8_lossy(&out.stdout)) {
        return Err(io::Error::other(DialogPending));
    }
    Ok(())
}

/// Paste buffer name, unique within this process even for sends in the same ms.
fn buffer_name(pid: u32, millis: u128) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("ccmon-{pid}-{millis}-{n}")
}

fn tmux_run(args: &[String]) -> io::Result<()> {
    let st = Command::new("tmux")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if st.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("tmux {} failed", args[0])))
    }
}

/// Paste `text` into the pane and press Enter; refuses while a dialog is open.
pub fn send_text(p: &Pane, text: &str) -> io::Result<()> {
    ensure_no_dialog(p)?;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let buf = buffer_name(std::process::id(), millis);
    let mut child = Command::new("tmux")
        .args(["load-buffer", "-b", &buf, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("no stdin"))?
        .write_all(text.as_bytes())?;
    if !child.wait()?.success() {
        return Err(io::Error::other("tmux load-buffer failed"));
    }
    let res = (|| {
        for argv in send_cmds(p, &buf) {
            let cancel = argv.get(3).map(String::as_str) == Some("-X");
            match tmux_run(&argv) {
                Err(e) if !cancel => return Err(e),
                _ => {}
            }
        }
        Ok(())
    })();
    if res.is_err() {
        let _ = tmux_run(&["delete-buffer".into(), "-b".into(), buf]);
    }
    res
}

pub fn send_escape(p: &Pane) -> io::Result<()> {
    tmux_run(&[
        "send-keys".into(),
        "-t".into(),
        p.id.clone(),
        "Escape".into(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_id_parses() {
        assert_eq!(pane_id("cde:@1.%4").as_deref(), Some("%4"));
        assert_eq!(pane_id("main:@0.%12").as_deref(), Some("%12"));
        assert_eq!(pane_id("garbage"), None);
    }

    #[test]
    fn foreground_ok_matches_tpgid() {
        assert!(foreground_ok("123 (claude) S 1 123 123 34817 123 4194560"));
        assert!(!foreground_ok("123 (claude) S 1 123 123 34817 999 4194560"));
        assert!(!foreground_ok("123 (claude) S 1 123 123 0 123 4194560"));
        assert!(foreground_ok(
            "123 (my (weird) name) S 1 123 123 34817 123 4194560"
        ));
    }

    #[test]
    fn send_cmds_cancels_copy_mode() {
        let p = Pane {
            id: "%4".into(),
            in_mode: true,
        };
        let c = send_cmds(&p, "b");
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], ["send-keys", "-t", "%4", "-X", "cancel"]);
        let p = Pane {
            id: "%4".into(),
            in_mode: false,
        };
        let c = send_cmds(&p, "b");
        assert_eq!(c.len(), 2);
        assert_eq!(c[1], ["send-keys", "-t", "%4", "Enter"]);
    }

    #[test]
    fn buffer_names_are_unique_in_the_same_millisecond() {
        let a = buffer_name(7, 1000);
        let b = buffer_name(7, 1000);
        assert_ne!(a, b);
        assert!(a.starts_with("ccmon-7-1000-") && b.starts_with("ccmon-7-1000-"));
    }

    #[test]
    fn dialog_pending_detects_permission_prompts() {
        let bash = "● Bash(rm -rf build)\n\n Bash command\n   rm -rf build\n Do you want to proceed?\n ❯ 1. Yes\n   2. No, and tell Claude what to do differently (esc)\n\n\n";
        assert!(dialog_pending(bash));
        assert!(dialog_pending("Do you want to make this edit to main.rs?\n❯ 1. Yes\n"));
        assert!(dialog_pending("Which option?\n❯ 1. Red\n  2. Blue\nEnter to select · ↑/↓ to navigate · Esc to cancel\n"));
        // Normal input box: nothing pending.
        assert!(!dialog_pending("● Done.\n\n╭────╮\n│ ❯ hello │\n╰────╯\n  ? for shortcuts\n"));
        assert!(!dialog_pending(""));
        // An old prompt scrolled above the last 15 lines no longer counts.
        let mut old = String::from("Do you want to proceed?\n❯ 1. Yes\n");
        for i in 0..20 {
            old.push_str(&format!("line {i}\n"));
        }
        assert!(!dialog_pending(&old));
    }

    #[test]
    fn dialog_error_text_is_the_refusal() {
        let e = io::Error::other(DialogPending);
        assert_eq!(error_text(&e), DIALOG_PENDING);
        assert_eq!(
            DIALOG_PENDING,
            "Terminal đang chờ bạn duyệt một quyền — xử lý trong terminal trước rồi gửi lại"
        );
        assert_eq!(error_text(&io::Error::other("x")), "Lỗi tmux: x");
    }

    #[test]
    fn paste_cmds_no_shell() {
        let p = Pane {
            id: "%4".into(),
            in_mode: true,
        };
        for argv in send_cmds(&p, "ccmon-1-2") {
            for a in &argv {
                assert!(!["sh", "bash", "-c"].contains(&a.as_str()));
                assert!(!a.contains("secret prompt"));
            }
        }
    }
}
