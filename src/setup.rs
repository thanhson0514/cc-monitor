//! `ccmon setup` / `ccmon uninstall`: wrap/unwrap the user's `statusLine.command`
//! in `~/.claude/settings.json` with `ccmon tee --`. Other keys are preserved.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

const PREFIX: &str = "ccmon tee --";

pub fn settings_path() -> PathBuf {
    crate::platform::claude_home().join("settings.json")
}

/// `<exe> tee --` prefix for a specific executable (the GUI passes its own path).
pub fn tee_prefix_for(exe: &Path) -> String {
    let e = exe.display().to_string();
    if e.contains(' ') {
        format!("\"{e}\" tee --")
    } else {
        format!("{e} tee --")
    }
}

/// (prefix, original command) of a status-line command wrapped by ccmon:
/// `ccmon tee -- …`, `/path/ccmon-gui tee -- …`, `"C:\…\ccmon-gui.exe" tee -- …`.
fn split_wrapped(cmd: &str) -> Option<(&str, &str)> {
    let i = cmd.find(" tee --")?;
    let head = &cmd[..i];
    head.to_ascii_lowercase()
        .contains("ccmon")
        .then(|| (&cmd[..i + 7], cmd[i + 7..].trim()))
}

pub fn is_wrapped(cmd: &str) -> bool {
    split_wrapped(cmd).is_some()
}

pub fn wrap_command(old: &str) -> String {
    wrap_with(PREFIX, old)
}

pub fn wrap_with(prefix: &str, old: &str) -> String {
    if is_wrapped(old) {
        return old.to_string();
    }
    let old = old.trim();
    if old.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix} {old}")
    }
}

pub fn unwrap_command(cmd: &str) -> Option<String> {
    split_wrapped(cmd).map(|(_, rest)| rest.to_string())
}

fn ask(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).is_ok() && matches!(s.trim(), "y" | "Y" | "yes")
}

/// Settings object; a missing file is an empty object (fresh installs).
fn load(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("đọc {}", path.display()))?;
    let v: Value = serde_json::from_str(&text)
        .with_context(|| format!("{} không phải JSON hợp lệ", path.display()))?;
    if !v.is_object() {
        bail!("{} không phải object JSON", path.display());
    }
    Ok(v)
}

fn save(path: &Path, v: &Value) -> Result<()> {
    if path.exists() {
        let backup = path.with_extension("json.ccmon.bak");
        std::fs::copy(path, &backup)
            .with_context(|| format!("sao lưu sang {}", backup.display()))?;
        println!("Bản sao lưu: {}", backup.display());
    } else if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.ccmon.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(v)? + "\n")?;
    std::fs::rename(&tmp, path)?;
    println!("Đã lưu {}", path.display());
    Ok(())
}

/// Current status-line command, if any.
pub fn current_command(path: &Path) -> Option<String> {
    let v = load(path).ok()?;
    v.get("statusLine")?
        .get("command")?
        .as_str()
        .map(str::to_string)
}

pub fn setup(path: &Path, assume_yes: bool) -> Result<()> {
    setup_with(path, PREFIX, assume_yes)
}

pub fn setup_with(path: &Path, prefix: &str, assume_yes: bool) -> Result<()> {
    let mut v = load(path)?;
    let current = v
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(Value::as_str)
        .map(str::to_string);
    match &current {
        Some(c) if is_wrapped(c) => {
            println!("statusLine đã được bọc bởi ccmon: {c}");
            return Ok(());
        }
        Some(c) => {
            let new = wrap_with(prefix, c);
            println!("statusLine.command hiện tại: {c}\nSẽ đổi thành:              {new}");
            if !assume_yes && !ask("Áp dụng?") {
                println!("Huỷ, không sửa gì.");
                return Ok(());
            }
            v["statusLine"]["command"] = Value::String(new);
        }
        None => {
            println!("Chưa có statusLine. Sẽ thêm lệnh `{prefix}` (không in gì, chỉ lấy dữ liệu cho ccmon).");
            if !assume_yes && !ask("Áp dụng?") {
                println!("Huỷ, không sửa gì.");
                return Ok(());
            }
            v["statusLine"] = json!({"type": "command", "command": prefix});
        }
    }
    save(path, &v)
}

pub fn uninstall(path: &Path) -> Result<()> {
    let mut v = load(path)?;
    let Some(cmd) = v
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        println!("Không có statusLine, không cần gỡ.");
        return Ok(());
    };
    let Some(orig) = unwrap_command(&cmd) else {
        println!("statusLine không do ccmon bọc, giữ nguyên.");
        return Ok(());
    };
    if orig.is_empty() {
        v.as_object_mut().unwrap().remove("statusLine");
    } else {
        v["statusLine"]["command"] = Value::String(orig);
    }
    save(path, &v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_is_idempotent_and_roundtrips() {
        let w = wrap_command("ccstatusline");
        assert_eq!(w, "ccmon tee -- ccstatusline");
        assert_eq!(wrap_command(&w), w);
        assert_eq!(unwrap_command(&w).as_deref(), Some("ccstatusline"));
        assert_eq!(unwrap_command("ccstatusline"), None);
    }

    #[test]
    fn wraps_with_gui_executable_path() {
        let p = tee_prefix_for(Path::new(r"C:\Program Files\ccmon\ccmon-gui.exe"));
        assert_eq!(p, r#""C:\Program Files\ccmon\ccmon-gui.exe" tee --"#);
        let w = wrap_with(&p, "ccstatusline");
        assert!(is_wrapped(&w));
        assert_eq!(unwrap_command(&w).as_deref(), Some("ccstatusline"));
        assert_eq!(wrap_with(&p, &w), w);
        assert!(!is_wrapped("bash ~/.claude/statusline.sh"));
    }

    #[test]
    fn setup_creates_missing_settings() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub/settings.json");
        setup(&p, true).unwrap();
        assert_eq!(current_command(&p).as_deref(), Some("ccmon tee --"));
    }

    #[test]
    fn setup_preserves_other_keys_and_uninstall_restores() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        let orig = r#"{"model":"opus","statusLine":{"type":"command","command":"ccstatusline","padding":0,"refreshInterval":10},"hooks":{"a":1}}"#;
        std::fs::write(&p, orig).unwrap();
        setup(&p, true).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["statusLine"]["command"], "ccmon tee -- ccstatusline");
        assert_eq!(v["statusLine"]["refreshInterval"], 10);
        assert_eq!(v["hooks"]["a"], 1);
        assert_eq!(v.as_object().unwrap().keys().next().unwrap(), "model");
        assert!(p.with_extension("json.ccmon.bak").exists());
        setup(&p, true).unwrap(); // idempotent
        uninstall(&p).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["statusLine"]["command"], "ccstatusline");
    }

    #[test]
    fn setup_without_statusline_adds_bare_tee() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        std::fs::write(&p, "{}").unwrap();
        setup(&p, true).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["statusLine"]["command"], "ccmon tee --");
        uninstall(&p).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v.get("statusLine").is_none());
    }
}
