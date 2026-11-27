//! Drive the tmux pane of a terminal-run Claude Code session: paste a prompt
//! (bracketed) and press Enter. Argv only, never a shell.
use crate::discovery::LiveInfo;
use serde::Serialize;
use std::io::{self, Write};
use std::process::{Command, Stdio};

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
        .map_err(|_| Block::PaneGone)?;
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

pub fn send_text(p: &Pane, text: &str) -> io::Result<()> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let buf = format!("ccmon-{}-{millis}", std::process::id());
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
    for argv in send_cmds(p, &buf) {
        tmux_run(&argv)?;
    }
    Ok(())
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
