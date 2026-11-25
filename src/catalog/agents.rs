//! Agent provider: user, project and plugin agent markdown files.

use super::{frontmatter, plugins, CapKind, Capability, Provider, ScanCtx, Scope};
use serde_json::Map;
use std::path::Path;

pub struct AgentProvider;

fn scan_dir(dir: &Path, prefix: Option<&str>, scope: &Scope, out: &mut Vec<Capability>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<_> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let (fm, _) = frontmatter::parse(&text);
        let stem = f.file_stem().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let name = frontmatter::str_of(&fm, "name").filter(|n| !n.is_empty()).unwrap_or(stem);
        if name.is_empty() {
            continue;
        }
        let id = match prefix {
            Some(p) => format!("{p}:{name}"),
            None => name.clone(),
        };
        let mut meta = Map::new();
        for k in ["tools", "model", "color"] {
            if let Some(v) = fm.get(k) {
                meta.insert(k.into(), v.clone());
            }
        }
        out.push(Capability {
            kind: CapKind::Agent,
            id,
            name,
            description: frontmatter::str_of(&fm, "description").unwrap_or_default(),
            scope: scope.clone(),
            path: Some(f),
            enabled_globally: true,
            meta,
        });
    }
}

impl Provider for AgentProvider {
    fn kind(&self) -> CapKind {
        CapKind::Agent
    }

    /// Built-in agents (Explore, general-purpose...) are not on disk; they come
    /// from the session inventory (see `usage::inventory_only`).
    fn scan(&self, ctx: &ScanCtx) -> Vec<Capability> {
        let mut out = Vec::new();
        scan_dir(&ctx.claude_home.join("agents"), None, &Scope::User, &mut out);
        if let Some(cwd) = ctx.cwd.as_ref().filter(|c| c.join(".claude") != ctx.claude_home) {
            scan_dir(&cwd.join(".claude/agents"), None, &Scope::Project, &mut out);
        }
        for p in plugins::installed(ctx).into_iter().filter(|p| p.enabled) {
            scan_dir(&p.install_path.join("agents"), Some(&p.name), &Scope::Plugin(p.id.clone()), &mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::{write, Scope};
    use super::*;

    #[test]
    fn agents_from_md_frontmatter() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        let home = root.join("home/.claude");
        let inst = root.join("cache/pl");
        write(&home.join("agents/rev.md"), "---\nname: reviewer\ndescription: reviews\ntools: Read, Grep\nmodel: opus\ncolor: red\n---\nbody");
        write(&home.join("agents/nofm.md"), "just text");
        write(&home.join("agents/notes.txt"), "ignored");
        write(&root.join("proj/.claude/agents/p.md"), "---\nname: proj-agent\ndescription: d\ntools: [Read, Bash]\n---\n");
        write(&inst.join("agents/doc.md"), "---\nname: documenter\ndescription: docs\n---\n");
        write(
            &home.join("plugins/installed_plugins.json"),
            &format!(r#"{{"plugins":{{"pl@mk":[{{"scope":"user","installPath":"{}"}}]}}}}"#, inst.display()),
        );
        write(&home.join("settings.json"), r#"{"enabledPlugins":{"pl@mk":true}}"#);
        std::fs::write(home.join("agents/bad.md"), [0xff, 0xfe]).unwrap();
        let c = ScanCtx { claude_home: home, claude_json: root.join("c.json"), cwd: Some(root.join("proj")) };
        let caps = AgentProvider.scan(&c);
        let get = |id: &str| caps.iter().find(|x| x.id == id).unwrap_or_else(|| panic!("missing {id}"));
        let r = get("reviewer");
        assert_eq!(r.scope, Scope::User);
        assert_eq!(r.description, "reviews");
        assert_eq!(r.meta["tools"], "Read, Grep");
        assert_eq!(r.meta["model"], "opus");
        assert_eq!(r.meta["color"], "red");
        assert_eq!(get("nofm").scope, Scope::User);
        assert_eq!(get("proj-agent").scope, Scope::Project);
        assert_eq!(get("proj-agent").meta["tools"], serde_json::json!(["Read", "Bash"]));
        assert_eq!(get("pl:documenter").scope, Scope::Plugin("pl@mk".into()));
        assert!(!caps.iter().any(|x| x.id == "notes" || x.id == "bad"));
    }
}
