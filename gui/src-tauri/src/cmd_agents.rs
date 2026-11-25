//! Tauri commands for the Agents page: agents and workflows, their run
//! history, the ccmon-wide on/off switch (registry) and writing user/project
//! agent files. Writing an agent file is the only place ccmon edits user files,
//! and it never overwrites an existing file unless the user confirmed it.

use cc_monitor::catalog::history::{self, RunRecord, RunStat, RunStats};
use cc_monitor::catalog::usage::inventory_only;
use cc_monitor::catalog::{frontmatter, CapKind, Capability, Catalog, Scope};
use cc_monitor::control::state;
use cc_monitor::control::Registry;
use cc_monitor::platform;
use cc_monitor::store::Store;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::State;

use crate::cmd_catalog::{self, CapabilityDto};
use crate::Shared;

/// Error prefix telling the UI the target file exists (ask, then retry with `overwrite`).
pub const EXISTS_PREFIX: &str = "EXISTS:";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    pub tools: Option<Vec<String>>,
    pub model: Option<String>,
    pub prompt: String,
}

/// `^[a-z0-9][a-z0-9-]{0,63}$`
pub fn valid_agent_name(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// A tool entry of the comma list: `Read`, `Bash(git diff:*)`, `mcp__s__t`.
fn valid_tool(t: &str) -> bool {
    let ok_char = |c: char| c.is_ascii_alphanumeric() || "_-:.*()@/ ".contains(c);
    t.len() <= 200
        && t.starts_with(|c: char| c.is_ascii_alphanumeric())
        && t.chars().all(ok_char)
        && t == t.trim()
        && !t.contains(": ")
}

fn valid_model(m: &str) -> bool {
    m.len() <= 100
        && m.starts_with(|c: char| c.is_ascii_alphanumeric())
        && m.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:-[]".contains(c))
}

/// YAML for a free-text value: plain when that is unambiguous, otherwise a
/// `|-` literal block (no escaping needed; also read back by
/// `catalog::frontmatter`). The value is trimmed first.
fn yaml_text(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let keyword = matches!(
        lower.as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~"
    ) || s.parse::<f64>().is_ok();
    let plain = !keyword
        && !s.contains('\n')
        && !s.contains('\t')
        && !s.contains(": ")
        && !s.contains(" #")
        && !s.ends_with(':')
        && !s.starts_with(|c: char| "-?:,[]{}#&*!|>'\"%@`".contains(c));
    if plain {
        return s.to_string();
    }
    let mut out = String::from("|-");
    for l in s.lines() {
        out.push('\n');
        let l = l.trim_end();
        if !l.is_empty() {
            out.push_str("  ");
            out.push_str(l);
        }
    }
    out
}

/// Agent markdown: frontmatter `name`, `description`, `tools` (comma list,
/// omitted when empty), `model` (omitted when unset), then the prompt.
pub fn render_agent(def: &AgentDef) -> Result<String, String> {
    if !valid_agent_name(&def.name) {
        return Err(
            "Tên agent chỉ gồm chữ thường, số và dấu '-' (tối đa 64 ký tự, không bắt đầu bằng '-')"
                .into(),
        );
    }
    if def.description.trim().is_empty() {
        return Err("Cần nhập mô tả cho agent".into());
    }
    let tools: Vec<&str> = def.tools.iter().flatten().map(|t| t.as_str()).collect();
    if let Some(bad) = tools.iter().find(|t| !valid_tool(t)) {
        return Err(format!("Tên tool không hợp lệ: {bad:?}"));
    }
    let model = def
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    if let Some(m) = model.filter(|m| !valid_model(m)) {
        return Err(format!("Model không hợp lệ: {m:?}"));
    }
    let mut out = format!(
        "---\nname: {}\ndescription: {}\n",
        def.name,
        yaml_text(&def.description)
    );
    if !tools.is_empty() {
        out.push_str(&format!("tools: {}\n", tools.join(", ")));
    }
    if let Some(m) = model {
        out.push_str(&format!("model: {m}\n"));
    }
    out.push_str("---\n\n");
    let prompt = def.prompt.replace("\r\n", "\n");
    out.push_str(prompt.trim_end());
    out.push('\n');
    Ok(out)
}

/// Target file for `scope` (`user` | `project`); other scopes are read-only.
pub fn agent_path(
    scope: &str,
    claude_home: &Path,
    project: Option<&Path>,
    name: &str,
) -> Result<PathBuf, String> {
    if !valid_agent_name(name) {
        return Err("Tên agent không hợp lệ".into());
    }
    let dir = match scope {
        "user" => claude_home.join("agents"),
        "project" => project
            .ok_or("Chọn một dự án để lưu agent cấp dự án")?
            .join(".claude/agents"),
        _ => return Err("Agent built-in và plugin chỉ đọc".into()),
    };
    Ok(dir.join(format!("{name}.md")))
}

/// Write via a temp file in the same directory. Without `overwrite`, the file
/// is linked into place only if nothing exists there (no check-then-write race).
fn write_agent_file(path: &Path, body: &str, overwrite: bool) -> Result<(), String> {
    let dir = path.parent().ok_or("Đường dẫn không hợp lệ")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("Không tạo được thư mục: {e}"))?;
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("agent");
    let tmp = dir.join(format!(".{name}.ccmon-{}-{seq}.tmp", std::process::id()));
    std::fs::write(&tmp, body).map_err(|e| format!("Không ghi được file: {e}"))?;
    let exists = || format!("{EXISTS_PREFIX}{}", path.display());
    let res = if overwrite {
        std::fs::rename(&tmp, path).map_err(|e| format!("Không ghi được file: {e}"))
    } else {
        match std::fs::hard_link(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(exists()),
            // filesystems without hard links
            Err(_) if path.symlink_metadata().is_ok() => Err(exists()),
            Err(_) => std::fs::rename(&tmp, path).map_err(|e| format!("Không ghi được file: {e}")),
        }
    };
    let _ = std::fs::remove_file(&tmp);
    res
}

pub fn save_agent_in(
    claude_home: &Path,
    project: Option<&Path>,
    scope: &str,
    def: &AgentDef,
    overwrite: bool,
) -> Result<PathBuf, String> {
    let body = render_agent(def)?;
    let path = agent_path(scope, claude_home, project, &def.name)?;
    write_agent_file(&path, &body, overwrite)?;
    Ok(path)
}

/// A project cwd coming from the webview is accepted only when it is one of
/// the projects ccmon already knows (sessions / transcript history).
pub fn resolve_project(cwd: Option<&str>, known: &[String]) -> Result<Option<PathBuf>, String> {
    match cwd.filter(|c| !c.is_empty()) {
        None => Ok(None),
        Some(c) => crate::cmd_control::check_new_cwd(c, known).map(Some),
    }
}

/// Window for the run counts shown in the lists.
pub const STATS_DAYS: u32 = 30;
const STATS_TTL: Duration = Duration::from_secs(10);
/// Max bytes of an agent prompt / workflow script shown on the page.
const BODY_MAX: usize = 64 * 1024;

/// One agent or workflow on the page.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemDto {
    #[serde(flatten)]
    pub cap: CapabilityDto,
    /// Off for all of ccmon (registry); files and history are kept.
    pub disabled: bool,
    /// Runs in the last [`STATS_DAYS`] days.
    pub runs: u32,
    pub last_run: Option<i64>,
    /// Agent prompt (frontmatter stripped) or workflow script, capped.
    pub body: Option<String>,
    /// User/project agent files may be edited from the page.
    pub editable: bool,
}

pub type AgentDto = ItemDto;
pub type WorkflowDto = ItemDto;

fn read_body(c: &Capability) -> Option<String> {
    let text = std::fs::read_to_string(c.path.as_ref()?).ok()?;
    let mut body = if c.kind == CapKind::Agent {
        frontmatter::parse(&text).1.trim().to_string()
    } else {
        text
    };
    if body.len() > BODY_MAX {
        let mut cut = BODY_MAX;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str("\n…");
    }
    Some(body)
}

/// Claude Code's own agent types (no file on disk), for names known only from history.
const BUILTIN_AGENTS: [&str; 6] = [
    "general-purpose",
    "Explore",
    "Plan",
    "statusline-setup",
    "claude-code-guide",
    "output-style-setup",
];

/// Rows for one kind: catalog items, then announced-only ones (built-in agents,
/// inline workflows), then names that only exist in run history (deleted
/// files, other projects) as `Scope::Managed`, so their history stays visible.
pub fn item_rows(
    kind: CapKind,
    items: Vec<Capability>,
    stats: &std::collections::HashMap<String, RunStat>,
    disabled: &BTreeSet<String>,
    with_body: bool,
) -> Vec<ItemDto> {
    let mut seen = HashSet::new();
    let mut caps: Vec<Capability> = items
        .into_iter()
        .filter(|c| c.kind == kind && seen.insert(c.id.clone()))
        .collect();
    let mut extra: Vec<&String> = stats.keys().filter(|n| !seen.contains(*n)).collect();
    extra.sort();
    for n in extra {
        let builtin = kind == CapKind::Agent && BUILTIN_AGENTS.contains(&n.as_str());
        caps.push(Capability {
            kind,
            id: n.clone(),
            name: n.clone(),
            description: String::new(),
            scope: if builtin {
                Scope::BuiltIn
            } else {
                Scope::Managed
            },
            path: None,
            enabled_globally: true,
            meta: Default::default(),
        });
    }
    caps.into_iter()
        .map(|c| {
            let st = stats.get(&c.id).copied().unwrap_or_default();
            let editable = kind == CapKind::Agent
                && c.path.is_some()
                && matches!(c.scope, Scope::User | Scope::Project);
            ItemDto {
                disabled: disabled.contains(&c.id),
                runs: st.runs,
                last_run: st.last,
                body: if with_body { read_body(&c) } else { None },
                editable,
                cap: CapabilityDto::from(&c),
            }
        })
        .collect()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

static STATS: std::sync::OnceLock<Mutex<Option<(Instant, RunStats)>>> = std::sync::OnceLock::new();

/// Run counts for every name, one pass over the projects root, cached briefly.
fn stats_cached(root: &Path) -> RunStats {
    let cell = STATS.get_or_init(Default::default);
    if let Some((at, s)) = lock(cell).as_ref() {
        if at.elapsed() < STATS_TTL {
            return s.clone();
        }
    }
    let s = history::run_stats(root, STATS_DAYS);
    *lock(cell) = Some((Instant::now(), s.clone()));
    s
}

struct Ctx {
    store: Arc<Mutex<Store>>,
    root: PathBuf,
}

impl Ctx {
    fn of(s: &Shared) -> Self {
        Self {
            store: s.store.clone(),
            root: s.root.clone(),
        }
    }

    /// Project cwd: an explicit one must be a known project; otherwise the
    /// session's own cwd from the store.
    fn project(&self, sid: Option<&str>, cwd: Option<&str>) -> Result<Option<PathBuf>, String> {
        if let Some(c) = cwd.filter(|c| !c.is_empty()) {
            let in_store = lock(&self.store)
                .sessions
                .values()
                .any(|s| s.main.cwd() == Some(c));
            let known = if in_store {
                vec![c.to_string()]
            } else {
                crate::cmd_control::known_cwds(&self.store, &self.root)
            };
            return resolve_project(Some(c), &known);
        }
        let st = lock(&self.store);
        Ok(sid
            .and_then(|s| st.sessions.get(s))
            .and_then(|s| s.main.cwd())
            .map(PathBuf::from))
    }

    /// Catalog plus what sessions announced/ran without a file on disk.
    fn items(&self, project: Option<&Path>) -> Vec<Capability> {
        let cwd = project.map(|p| p.to_string_lossy().into_owned());
        let catalog: Catalog = cmd_catalog::catalog_for(cwd.as_deref());
        let extra = {
            let st = lock(&self.store);
            let threads: Vec<_> = st.sessions.values().map(|s| &s.main).collect();
            inventory_only(&threads, &catalog)
        };
        catalog.items.into_iter().chain(extra).collect()
    }

    fn list(
        &self,
        kind: CapKind,
        sid: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<Vec<ItemDto>, String> {
        let project = self.project(sid, cwd)?;
        let items = self.items(project.as_deref());
        let stats = stats_cached(&self.root);
        let reg = state::load_registry();
        Ok(match kind {
            CapKind::Agent => item_rows(kind, items, &stats.agents, &reg.disabled_agents, true),
            _ => item_rows(kind, items, &stats.workflows, &reg.disabled_workflows, true),
        })
    }

    /// Tool names for the agent form: common built-ins plus every tool seen.
    fn tools(&self) -> Vec<String> {
        let mut v: BTreeSet<String> = BASE_TOOLS.iter().map(|s| s.to_string()).collect();
        let st = lock(&self.store);
        for s in st.sessions.values() {
            let threads = std::iter::once(&s.main).chain(s.agents.values().map(|a| &a.thread));
            for t in threads {
                v.extend(t.tool_uses().map(|(_, n, _, _)| n.to_string()));
            }
        }
        v.into_iter().filter(|t| valid_tool(t)).collect()
    }
}

const BASE_TOOLS: [&str; 13] = [
    "Agent",
    "Bash",
    "Edit",
    "Glob",
    "Grep",
    "NotebookEdit",
    "Read",
    "Skill",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
    "Workflow",
    "Write",
];

fn valid_registry_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 200 && !n.chars().any(char::is_control)
}

/// Flip one name in the registry (ccmon state only; files untouched).
pub fn set_disabled_in(
    dir: &Path,
    kind: CapKind,
    name: &str,
    disabled: bool,
) -> Result<Registry, String> {
    if !valid_registry_name(name) {
        return Err("Tên không hợp lệ".into());
    }
    let mut r = state::load_registry_in(dir);
    let set = match kind {
        CapKind::Agent => &mut r.disabled_agents,
        CapKind::Workflow => &mut r.disabled_workflows,
        _ => return Err("Chỉ bật/tắt được agent hoặc workflow".into()),
    };
    if disabled {
        set.insert(name.to_string());
    } else {
        set.remove(name);
    }
    state::save_registry_in(dir, &r).map_err(|e| format!("Không lưu được registry: {e}"))?;
    Ok(r)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_agents(
    state: State<'_, Shared>,
    sid: Option<String>,
    cwd: Option<String>,
) -> Result<Vec<AgentDto>, String> {
    let c = Ctx::of(&state);
    blocking(move || c.list(CapKind::Agent, sid.as_deref(), cwd.as_deref())).await?
}

#[tauri::command]
pub async fn list_workflows(
    state: State<'_, Shared>,
    sid: Option<String>,
    cwd: Option<String>,
) -> Result<Vec<WorkflowDto>, String> {
    let c = Ctx::of(&state);
    blocking(move || c.list(CapKind::Workflow, sid.as_deref(), cwd.as_deref())).await?
}

#[tauri::command]
pub async fn agent_tools(state: State<'_, Shared>) -> Result<Vec<String>, String> {
    let c = Ctx::of(&state);
    blocking(move || c.tools()).await
}

/// Project cwd of a session, from the store (default project of the page).
#[tauri::command]
pub fn session_project(state: State<'_, Shared>, sid: String) -> Option<String> {
    let st = lock(&state.store);
    st.sessions.get(&sid)?.main.cwd().map(str::to_string)
}

/// `days = 0`: all history.
#[tauri::command]
pub async fn agent_history(
    state: State<'_, Shared>,
    name: String,
    days: Option<u32>,
) -> Result<Vec<RunRecord>, String> {
    let root = state.root.clone();
    blocking(move || history::agent_history(&root, &name, days.unwrap_or(STATS_DAYS))).await
}

#[tauri::command]
pub async fn workflow_history(
    state: State<'_, Shared>,
    name: String,
    days: Option<u32>,
) -> Result<Vec<RunRecord>, String> {
    let root = state.root.clone();
    blocking(move || history::workflow_history(&root, &name, days.unwrap_or(STATS_DAYS))).await
}

fn set_disabled(kind: CapKind, name: String, disabled: bool) -> Result<(), String> {
    set_disabled_in(&state::state_dir(), kind, &name, disabled)?;
    cmd_catalog::invalidate_catalog();
    Ok(())
}

#[tauri::command]
pub async fn set_agent_disabled(name: String, disabled: bool) -> Result<(), String> {
    blocking(move || set_disabled(CapKind::Agent, name, disabled)).await?
}

#[tauri::command]
pub async fn set_workflow_disabled(name: String, disabled: bool) -> Result<(), String> {
    blocking(move || set_disabled(CapKind::Workflow, name, disabled)).await?
}

/// Write a user/project agent file. Refuses to replace an existing file
/// (error starting with [`EXISTS_PREFIX`]) unless `overwrite` is set after the
/// user confirmed. The project cwd is resolved server-side (known projects only).
#[tauri::command]
pub async fn save_agent(
    state: State<'_, Shared>,
    scope: String,
    cwd: Option<String>,
    def: AgentDef,
    overwrite: bool,
) -> Result<String, String> {
    let c = Ctx::of(&state);
    blocking(move || {
        let project = if scope == "project" {
            c.project(None, cwd.as_deref())?
        } else {
            None
        };
        let p = save_agent_in(
            &platform::claude_home(),
            project.as_deref(),
            &scope,
            &def,
            overwrite,
        )?;
        cmd_catalog::invalidate_catalog();
        Ok(p.display().to_string())
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_monitor::catalog::frontmatter;

    fn def(name: &str) -> AgentDef {
        AgentDef {
            name: name.into(),
            description: "Reviews diffs".into(),
            tools: Some(vec!["Read".into(), "Grep".into()]),
            model: Some("sonnet".into()),
            prompt: "You review code.\n\n## Steps\n- read".into(),
        }
    }

    #[test]
    fn save_agent_writes_frontmatter() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join(".claude");
        let p = save_agent_in(&home, None, "user", &def("reviewer"), false).unwrap();
        assert_eq!(p, home.join("agents/reviewer.md"));
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.starts_with("---\nname: reviewer\n"), "{text}");
        assert!(text.contains("\ntools: Read, Grep\n"), "{text}");
        assert!(text.contains("\nmodel: sonnet\n"), "{text}");
        let (fm, body) = frontmatter::parse(&text);
        assert_eq!(fm["name"], "reviewer");
        assert_eq!(fm["description"], "Reviews diffs");
        assert_eq!(body.trim(), "You review code.\n\n## Steps\n- read");
        // tools and model omitted when not set
        let mut d = def("plain");
        d.tools = Some(vec![]);
        d.model = None;
        let text = std::fs::read_to_string(save_agent_in(&home, None, "user", &d, false).unwrap())
            .unwrap();
        assert!(
            !text.contains("tools:") && !text.contains("model:"),
            "{text}"
        );
        // no temp files left behind
        let left: Vec<_> = std::fs::read_dir(home.join("agents"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
    }

    #[test]
    fn save_agent_refuses_overwrite_without_flag() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join(".claude");
        let p = home.join("agents/reviewer.md");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "mine").unwrap();
        let e = save_agent_in(&home, None, "user", &def("reviewer"), false).unwrap_err();
        assert!(e.starts_with(EXISTS_PREFIX), "{e}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "mine");
        save_agent_in(&home, None, "user", &def("reviewer"), true).unwrap();
        assert!(std::fs::read_to_string(&p)
            .unwrap()
            .contains("name: reviewer"));
    }

    #[test]
    fn save_agent_rejects_bad_name() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join(".claude");
        for n in [
            "",
            "-x",
            "Up",
            "a b",
            "../x",
            "a/b",
            "x.md",
            "tiếng",
            &"a".repeat(65),
        ] {
            assert!(!valid_agent_name(n), "{n}");
            assert!(
                save_agent_in(&home, None, "user", &def(n), false).is_err(),
                "{n}"
            );
        }
        for n in ["a", "0", "code-reviewer", &"a".repeat(64)] {
            assert!(valid_agent_name(n), "{n}");
        }
        assert!(!home.join("agents").exists());
    }

    #[test]
    fn description_with_colons_newlines_and_quotes_round_trips() {
        for d in [
            "Use when: reviewing # code",
            "line one\nline two: yes\n\n- not a list",
            "\"quoted\" and 'single'",
            "true",
            "123",
            "- dash first",
            "trailing colon:",
            "  padded  ",
        ] {
            let mut a = def("x");
            a.description = d.into();
            let text = render_agent(&a).unwrap();
            let (fm, body) = frontmatter::parse(&text);
            assert_eq!(fm["description"], d.trim(), "{text}");
            assert_eq!(fm["name"], "x", "{text}");
            assert_eq!(fm["model"], "sonnet", "{text}");
            assert!(body.contains("You review code."), "{text}");
        }
        let mut a = def("x");
        a.description = "  ".into();
        assert!(render_agent(&a).is_err(), "description required");
    }

    #[test]
    fn rejects_unsafe_tools_and_model() {
        for bad in ["Read\nmodel: x", "A, B", "", " #x"] {
            let mut a = def("x");
            a.tools = Some(vec![bad.into()]);
            assert!(render_agent(&a).is_err(), "{bad:?}");
        }
        let mut a = def("x");
        a.tools = Some(vec!["Bash(git diff:*)".into(), "mcp__s__t".into()]);
        assert!(render_agent(&a).is_ok());
        for bad in ["opus\nx", "a b", "#x"] {
            let mut a = def("x");
            a.model = Some(bad.into());
            assert!(render_agent(&a).is_err(), "{bad:?}");
        }
        let mut a = def("x");
        a.model = Some("claude-opus-4-5[1m]".into());
        assert!(render_agent(&a).is_ok());
    }

    #[test]
    fn agent_path_resolution() {
        let home = Path::new("/h/.claude");
        let proj = Path::new("/w/p");
        assert_eq!(
            agent_path("user", home, Some(proj), "a").unwrap(),
            PathBuf::from("/h/.claude/agents/a.md")
        );
        assert_eq!(
            agent_path("project", home, Some(proj), "a").unwrap(),
            PathBuf::from("/w/p/.claude/agents/a.md")
        );
        assert!(agent_path("project", home, None, "a").is_err());
        assert!(agent_path("plugin", home, Some(proj), "a").is_err());
        assert!(agent_path("user", home, None, "../a").is_err());
    }

    #[test]
    fn project_must_be_known() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().to_string_lossy().into_owned();
        let known = vec![p.clone()];
        assert_eq!(resolve_project(None, &known).unwrap(), None);
        assert_eq!(resolve_project(Some(""), &known).unwrap(), None);
        assert_eq!(
            resolve_project(Some(&p), &known).unwrap(),
            Some(PathBuf::from(&p))
        );
        assert!(resolve_project(Some("/etc"), &known).is_err());
    }

    fn cap(id: &str, scope: Scope, path: Option<PathBuf>) -> Capability {
        Capability {
            kind: CapKind::Agent,
            id: id.into(),
            name: id.into(),
            description: String::new(),
            scope,
            path,
            enabled_globally: true,
            meta: Default::default(),
        }
    }

    #[test]
    fn rows_merge_registry_stats_and_history_only_names() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("rev.md");
        std::fs::write(&f, "---\nname: rev\n---\n\nBody here\n").unwrap();
        let items = vec![
            cap("rev", Scope::User, Some(f)),
            cap("Explore", Scope::BuiltIn, None),
            cap(
                "p:x",
                Scope::Plugin("p@m".into()),
                Some(t.path().join("none.md")),
            ),
            cap("rev", Scope::Project, None), // duplicate id: first wins
        ];
        let mut stats = std::collections::HashMap::new();
        stats.insert(
            "Explore".to_string(),
            RunStat {
                runs: 3,
                last: Some(9),
            },
        );
        stats.insert(
            "gone".to_string(),
            RunStat {
                runs: 1,
                last: Some(1),
            },
        );
        stats.insert(
            "Plan".to_string(),
            RunStat {
                runs: 2,
                last: Some(2),
            },
        );
        let disabled: BTreeSet<String> = ["rev".to_string(), "gone".to_string()].into();
        let rows = item_rows(CapKind::Agent, items, &stats, &disabled, true);
        let ids: Vec<_> = rows.iter().map(|r| r.cap.id.as_str()).collect();
        assert_eq!(ids, ["rev", "Explore", "p:x", "Plan", "gone"]);
        assert!(rows[0].disabled && rows[0].editable);
        assert_eq!(rows[0].body.as_deref(), Some("Body here"));
        assert_eq!((rows[1].runs, rows[1].last_run), (3, Some(9)));
        assert!(!rows[1].editable && !rows[2].editable);
        // history-only: a known built-in type stays built-in, anything else is "managed"
        assert_eq!(rows[3].cap.scope, Scope::BuiltIn);
        assert!(rows[4].disabled);
        assert_eq!(rows[4].cap.scope, Scope::Managed);
        let v = serde_json::to_value(&rows[1]).unwrap();
        assert_eq!(v["lastRun"], 9);
        assert_eq!(v["scope"], "builtIn");
        assert_eq!(v["disabled"], false);
    }

    #[test]
    fn registry_toggle_keeps_other_entries() {
        let t = tempfile::tempdir().unwrap();
        set_disabled_in(t.path(), CapKind::Agent, "a", true).unwrap();
        set_disabled_in(t.path(), CapKind::Workflow, "w", true).unwrap();
        let r = set_disabled_in(t.path(), CapKind::Agent, "b", true).unwrap();
        assert_eq!(r.disabled_agents.len(), 2);
        let r = set_disabled_in(t.path(), CapKind::Agent, "a", false).unwrap();
        assert!(!r.disabled_agents.contains("a") && r.disabled_workflows.contains("w"));
        assert!(set_disabled_in(t.path(), CapKind::Skill, "s", true).is_err());
        assert!(set_disabled_in(t.path(), CapKind::Agent, "", true).is_err());
    }
}
