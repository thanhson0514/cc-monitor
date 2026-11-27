//! Managed-session control: per-session overlay + ccmon-owned state.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

pub mod draft;
pub mod runner;
pub mod state;
pub mod subagent;
pub mod tmux;

/// Per-session launch overlay. Ids: skills = catalog skill id (`name` or
/// `plugin:name`), plugins = `name@marketplace`, MCP = catalog server id,
/// agents = agent name/type.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Overlay {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
    pub disabled_skills: BTreeSet<String>,
    pub disabled_plugins: BTreeSet<String>,
    pub disabled_mcp: BTreeSet<String>,
    pub disabled_agents: BTreeSet<String>,
    pub disabled_workflows: BTreeSet<String>,
    /// Master switch set by the user; never derived from the registry.
    pub workflows_off: bool,
    pub main_agent: Option<String>, // --agent
}

/// ccmon-wide switches; files and history of disabled items are untouched.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Registry {
    pub disabled_agents: BTreeSet<String>,
    pub disabled_workflows: BTreeSet<String>,
    pub notes: BTreeMap<String, String>,
}

/// How to spawn one managed `claude` process.
#[derive(Clone, PartialEq, Debug)]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub settings_file: Option<PathBuf>,
}

fn is_exec(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else {
        return false;
    };
    if !m.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn find_in(dirs: &[PathBuf]) -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["claude.exe", "claude.cmd"]
    } else {
        &["claude"]
    };
    dirs.iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|c| is_exec(c))
}

/// Locate the `claude` binary: PATH, `~/.local/bin`, `~/.claude/local`.
pub fn find_claude() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".claude").join("local"));
    }
    find_in(&dirs)
}

/// Translate an overlay into CLI args (verified mapping: docs/architecture/control.md).
/// Each `--disallowedTools` rule is its own args element. With `sid == None` a
/// new session is started (its id comes from the first stdout event).
pub fn build_args(
    sid: Option<&str>,
    cwd: &Path,
    o: &Overlay,
    state_dir: &Path,
) -> io::Result<LaunchSpec> {
    let program = find_claude().ok_or_else(claude_not_found)?;
    build_args_with(program, sid, cwd, o, state_dir)
}

fn claude_not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "claude binary not found")
}

fn build_args_with(
    program: PathBuf,
    sid: Option<&str>,
    cwd: &Path,
    o: &Overlay,
    state_dir: &Path,
) -> io::Result<LaunchSpec> {
    let mut args: Vec<String> = [
        "-p",
        "--verbose",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        // Token-level `stream_event`s: the transcript lags whole messages behind.
        "--include-partial-messages",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(sid) = sid {
        if !state::valid_sid(sid) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid session id",
            ));
        }
        args.extend(["--resume".into(), sid.into()]);
    }
    for (flag, val) in [
        ("--model", &o.model),
        ("--effort", &o.effort),
        ("--permission-mode", &o.permission_mode),
        ("--agent", &o.main_agent),
    ] {
        if let Some(v) = val.as_ref().filter(|v| !v.is_empty()) {
            args.extend([flag.into(), v.clone()]);
        }
    }
    let mut settings_file = None;
    if !o.disabled_plugins.is_empty() {
        let name = match sid {
            Some(s) => s.to_string(),
            None => format!("new-{}", chrono::Utc::now().timestamp_millis()),
        };
        let path = state_dir
            .join("launch")
            .join(format!("{name}.settings.json"));
        let enabled: BTreeMap<&String, bool> =
            o.disabled_plugins.iter().map(|p| (p, false)).collect();
        state::write_atomic(&path, &serde_json::json!({ "enabledPlugins": enabled }))?;
        args.extend(["--settings".into(), path.to_string_lossy().into_owned()]);
        settings_file = Some(path);
    }
    let mut rules: Vec<String> = Vec::new();
    rules.extend(o.disabled_skills.iter().map(|s| format!("Skill({s})")));
    rules.extend(
        o.disabled_mcp
            .iter()
            .map(|s| format!("mcp__{}", crate::catalog::mcp::normalize(s))),
    );
    rules.extend(o.disabled_agents.iter().map(|a| format!("Agent({a})")));
    if o.workflows_off {
        rules.push("Workflow".into());
    } else {
        rules.extend(
            o.disabled_workflows
                .iter()
                .map(|w| format!("Workflow({w})")),
        );
    }
    if !rules.is_empty() {
        // Variadic flag: keep it last.
        args.push("--disallowedTools".into());
        args.extend(rules);
    }
    Ok(LaunchSpec {
        program,
        args,
        cwd: cwd.to_path_buf(),
        settings_file,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(non_snake_case)]
    fn P() -> PathBuf {
        PathBuf::from("claude")
    }

    fn set(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn args_basic_resume() {
        let d = tempfile::tempdir().unwrap();
        let o = Overlay {
            model: Some("haiku".into()),
            effort: Some("low".into()),
            permission_mode: Some("plan".into()),
            main_agent: Some("Explore".into()),
            ..Default::default()
        };
        let s = build_args_with(P(), Some("abc-123"), Path::new("/w"), &o, d.path()).unwrap();
        assert_eq!(s.cwd, Path::new("/w"));
        assert_eq!(
            s.args,
            [
                "-p",
                "--verbose",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--resume",
                "abc-123",
                "--model",
                "haiku",
                "--effort",
                "low",
                "--permission-mode",
                "plan",
                "--agent",
                "Explore"
            ]
        );
        assert!(s.settings_file.is_none());
    }

    #[test]
    fn args_handle_colons_and_spaces() {
        let d = tempfile::tempdir().unwrap();
        let o = Overlay {
            disabled_skills: set(&["superpowers:brainstorming"]),
            disabled_mcp: set(&["claude.ai Claude Docs", "plugin:gitlab:gitlab"]),
            disabled_agents: set(&["Explore"]),
            disabled_workflows: set(&["deep-research"]),
            ..Default::default()
        };
        let s = build_args_with(P(), Some("s1"), Path::new("/w"), &o, d.path()).unwrap();
        let i = s
            .args
            .iter()
            .position(|a| a == "--disallowedTools")
            .unwrap();
        let mut rules = s.args[i + 1..].to_vec();
        rules.sort();
        assert_eq!(
            rules,
            [
                "Agent(Explore)",
                "Skill(superpowers:brainstorming)",
                "Workflow(deep-research)",
                "mcp__claude_ai_Claude_Docs",
                "mcp__plugin_gitlab_gitlab",
            ]
        );
        assert_eq!(s.args.len(), i + 6);
    }

    #[test]
    fn workflows_off_emits_bare_workflow_only() {
        let d = tempfile::tempdir().unwrap();
        let o = Overlay {
            workflows_off: true,
            disabled_workflows: set(&["x"]),
            ..Default::default()
        };
        let s = build_args_with(P(), Some("s1"), Path::new("/w"), &o, d.path()).unwrap();
        assert_eq!(
            &s.args[s.args.len() - 2..],
            ["--disallowedTools", "Workflow"]
        );
    }

    #[test]
    fn settings_file_only_when_plugins_disabled() {
        let d = tempfile::tempdir().unwrap();
        let none = build_args_with(
            P(),
            Some("s1"),
            Path::new("/w"),
            &Overlay::default(),
            d.path(),
        )
        .unwrap();
        assert!(none.settings_file.is_none() && !none.args.contains(&"--settings".to_string()));
        assert!(!d.path().join("launch").exists());
        assert!(!none.args.contains(&"--disallowedTools".to_string()));
        let o = Overlay {
            disabled_plugins: set(&["superpowers@claude-plugins-official"]),
            ..Default::default()
        };
        let s = build_args_with(P(), Some("s1"), Path::new("/w"), &o, d.path()).unwrap();
        let f = s.settings_file.unwrap();
        assert_eq!(f, d.path().join("launch/s1.settings.json"));
        assert!(s
            .args
            .windows(2)
            .any(|w| w[0] == "--settings" && w[1] == f.to_string_lossy()));
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(
            v["enabledPlugins"]["superpowers@claude-plugins-official"],
            false
        );
    }

    #[test]
    fn new_session_has_no_resume() {
        let d = tempfile::tempdir().unwrap();
        let o = Overlay {
            disabled_plugins: set(&["p@m"]),
            ..Default::default()
        };
        let s = build_args_with(P(), None, Path::new("/w"), &o, d.path()).unwrap();
        assert!(!s.args.contains(&"--resume".to_string()));
        let f = s.settings_file.unwrap();
        assert!(f.file_name().unwrap().to_string_lossy().starts_with("new-"));
    }

    #[test]
    fn hyphenated_mcp_rule_keeps_hyphens() {
        let d = tempfile::tempdir().unwrap();
        let o = Overlay {
            disabled_mcp: set(&[
                "docs-langchain",
                "plugin:chrome-devtools-mcp:chrome-devtools",
            ]),
            ..Default::default()
        };
        let s = build_args_with(P(), Some("s1"), Path::new("/w"), &o, d.path()).unwrap();
        assert!(s.args.contains(&"mcp__docs-langchain".to_string()));
        assert!(s
            .args
            .contains(&"mcp__plugin_chrome-devtools-mcp_chrome-devtools".to_string()));
    }

    #[test]
    fn find_in_requires_existing_executable() {
        let d = tempfile::tempdir().unwrap();
        let dirs = vec![d.path().to_path_buf()];
        assert!(find_in(&dirs).is_none());
        let name = if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        };
        let f = d.path().join(name);
        std::fs::write(&f, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(find_in(&dirs).is_none(), "non-executable must be skipped");
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(find_in(&dirs), Some(f));
    }

    #[test]
    fn missing_binary_is_not_found_error() {
        assert_eq!(claude_not_found().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn invalid_sid_rejected() {
        let d = tempfile::tempdir().unwrap();
        assert!(build_args_with(
            P(),
            Some("../x"),
            Path::new("/w"),
            &Overlay::default(),
            d.path()
        )
        .is_err());
    }
}
