//! User settings shared by the GUI and the TUI: which tools to monitor.
//! Stored at `<config dir>/ccmon/config.json` (e.g. `~/.config/ccmon` on Linux,
//! `%APPDATA%\ccmon` on Windows).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Codex monitoring is switched off for now: ccmon focuses on Claude Code. The parser
/// (`crate::codex`) stays in place; flip this to bring Codex back.
pub const CODEX_ENABLED: bool = false;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// First-run setup finished.
    pub setup_done: bool,
    /// Monitor Claude Code sessions.
    pub claude: bool,
    /// Monitor Codex sessions.
    pub codex: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            setup_done: false,
            claude: true,
            codex: CODEX_ENABLED,
        }
    }
}

/// Live, shared copy of the settings (the worker re-reads it on every rescan).
pub type SharedConfig = Arc<Mutex<Config>>;

pub fn config_path() -> PathBuf {
    std::env::var_os("CCMON_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::config_dir()
                .unwrap_or_default()
                .join("ccmon")
                .join("config.json")
        })
}

pub fn load() -> Config {
    let mut c: Config = std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    // An older config may still have Codex on: ignore it while Codex is disabled.
    c.codex &= CODEX_ENABLED;
    if !c.codex && !c.claude {
        c.claude = true;
    }
    c
}

pub fn save(c: &Config) -> std::io::Result<()> {
    let p = config_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, serde_json::to_string_pretty(c)? + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_default() {
        let c: Config = serde_json::from_str(r#"{"codex":false}"#).unwrap();
        assert_eq!(
            c,
            Config {
                setup_done: false,
                claude: true,
                codex: false
            }
        );
    }
}
