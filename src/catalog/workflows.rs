//! Workflow provider: `workflows/*.js` in the user home and the project.

use super::{CapKind, Capability, Provider, ScanCtx, Scope};
use serde_json::Map;
use std::path::Path;

pub struct WorkflowProvider;

/// First comment line of the file (`// text`, `/* text */` or ` * text`).
fn describe(src: &str) -> String {
    for l in src
        .lines()
        .take(20)
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        let t = l
            .strip_prefix("//")
            .or_else(|| l.strip_prefix("/**"))
            .or_else(|| l.strip_prefix("/*"))
            .or_else(|| l.strip_prefix('*'));
        match t {
            Some(t) => {
                let t = t.trim().trim_end_matches("*/").trim();
                if !t.is_empty() {
                    return t.to_string();
                }
            }
            None => break,
        }
    }
    String::new()
}

fn scan_dir(dir: &Path, scope: &Scope, out: &mut Vec<Capability>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "js"))
        .collect();
    files.sort();
    for f in files {
        let Some(name) = f.file_stem().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        // unreadable / non-UTF8 files still count as a workflow, just undescribed
        let description = std::fs::read_to_string(&f)
            .map(|s| describe(&s))
            .unwrap_or_default();
        out.push(Capability {
            kind: CapKind::Workflow,
            id: name.clone(),
            name,
            description,
            scope: scope.clone(),
            path: Some(f),
            enabled_globally: true,
            // `meta.runs` (run history) is added later by the GUI layer.
            meta: Map::new(),
        });
    }
}

impl Provider for WorkflowProvider {
    fn kind(&self) -> CapKind {
        CapKind::Workflow
    }

    fn scan(&self, ctx: &ScanCtx) -> Vec<Capability> {
        let mut out = Vec::new();
        scan_dir(&ctx.claude_home.join("workflows"), &Scope::User, &mut out);
        if let Some(cwd) = ctx
            .cwd
            .as_ref()
            .filter(|c| c.join(".claude") != ctx.claude_home)
        {
            scan_dir(&cwd.join(".claude/workflows"), &Scope::Project, &mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::{write, Scope};
    use super::*;

    #[test]
    fn workflows_from_js_files() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        let home = root.join("home/.claude");
        write(
            &home.join("workflows/ship.js"),
            "// Ship a release\nexport default 1;",
        );
        write(&home.join("workflows/readme.md"), "x");
        write(
            &root.join("proj/.claude/workflows/lint.js"),
            "/**\n * Lint everything\n */\nrun()",
        );
        std::fs::write(home.join("workflows/bin.js"), [0xff, 0xfe]).unwrap();
        let c = ScanCtx {
            claude_home: home,
            claude_json: root.join("c.json"),
            cwd: Some(root.join("proj")),
            registry: Default::default(),
        };
        let caps = WorkflowProvider.scan(&c);
        let get = |id: &str| {
            caps.iter()
                .find(|x| x.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
        };
        assert_eq!(get("ship").scope, Scope::User);
        assert_eq!(get("ship").description, "Ship a release");
        assert_eq!(get("lint").scope, Scope::Project);
        assert_eq!(get("lint").description, "Lint everything");
        assert!(get("bin").description.is_empty());
        assert!(!caps.iter().any(|x| x.id == "readme"));
    }
}
