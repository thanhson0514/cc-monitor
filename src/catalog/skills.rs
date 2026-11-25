//! Skill provider: user, project and (enabled) plugin skills.

use super::{frontmatter, plugins, CapKind, Capability, Provider, ScanCtx, Scope};
use serde_json::Map;
use std::path::Path;

pub struct SkillProvider;

fn scan_dir(dir: &Path, prefix: Option<&str>, scope: &Scope, out: &mut Vec<Capability>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dirs: Vec<_> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in dirs {
        let file = d.join("SKILL.md");
        // unreadable / non-UTF8 files are skipped
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let (fm, _) = frontmatter::parse(&text);
        let dir_name = d
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let name = frontmatter::str_of(&fm, "name")
            .filter(|n| !n.is_empty())
            .unwrap_or(dir_name);
        if name.is_empty() {
            continue;
        }
        let id = match prefix {
            Some(p) => format!("{p}:{name}"),
            None => name.clone(),
        };
        out.push(Capability {
            kind: CapKind::Skill,
            id,
            name,
            description: frontmatter::str_of(&fm, "description").unwrap_or_default(),
            scope: scope.clone(),
            path: Some(file),
            enabled_globally: true,
            meta: Map::new(),
        });
    }
}

impl Provider for SkillProvider {
    fn kind(&self) -> CapKind {
        CapKind::Skill
    }

    fn scan(&self, ctx: &ScanCtx) -> Vec<Capability> {
        let mut out = Vec::new();
        scan_dir(
            &ctx.claude_home.join("skills"),
            None,
            &Scope::User,
            &mut out,
        );
        if let Some(cwd) = ctx
            .cwd
            .as_ref()
            .filter(|c| c.join(".claude") != ctx.claude_home)
        {
            scan_dir(&cwd.join(".claude/skills"), None, &Scope::Project, &mut out);
        }
        for p in plugins::installed(ctx).into_iter().filter(|p| p.enabled) {
            scan_dir(
                &p.install_path.join("skills"),
                Some(&p.name),
                &Scope::Plugin(p.id.clone()),
                &mut out,
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::write;
    use super::*;

    fn ctx(root: &Path) -> ScanCtx {
        let home = root.join("home/.claude");
        let inst = root.join("cache/foo");
        write(
            &home.join("skills/u/SKILL.md"),
            "---\nname: usr\ndescription: user one\n---\nbody",
        );
        write(&home.join("skills/nofm/SKILL.md"), "no frontmatter");
        write(
            &root.join("proj/.claude/skills/p/SKILL.md"),
            "---\nname: prj\ndescription: proj one\n---\n",
        );
        write(
            &inst.join("skills/ps/SKILL.md"),
            "---\nname: ps\ndescription: plug one\n---\n",
        );
        write(
            &home.join("plugins/installed_plugins.json"),
            &format!(
                r#"{{"plugins":{{"foo@mk":[{{"scope":"user","installPath":"{}"}}]}}}}"#,
                inst.display()
            ),
        );
        write(
            &home.join("settings.json"),
            r#"{"enabledPlugins":{"foo@mk":true}}"#,
        );
        ScanCtx {
            claude_home: home,
            claude_json: root.join("home/.claude.json"),
            cwd: Some(root.join("proj")),
            registry: Default::default(),
        }
    }

    #[test]
    fn skills_from_user_project_and_plugin() {
        let t = tempfile::tempdir().unwrap();
        let c = ctx(t.path());
        let caps = SkillProvider.scan(&c);
        let get = |id: &str| {
            caps.iter()
                .find(|x| x.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
        };
        assert_eq!(get("usr").scope, Scope::User);
        assert_eq!(get("usr").description, "user one");
        assert_eq!(get("nofm").scope, Scope::User); // falls back to dir name
        assert_eq!(get("prj").scope, Scope::Project);
        assert_eq!(get("foo:ps").scope, Scope::Plugin("foo@mk".into()));
        assert_eq!(get("foo:ps").name, "ps");

        // disabled plugin: its skills disappear
        write(
            &c.claude_home.join("settings.json"),
            r#"{"enabledPlugins":{"foo@mk":false}}"#,
        );
        assert!(!SkillProvider.scan(&c).iter().any(|x| x.id == "foo:ps"));
    }

    #[test]
    fn malformed_skill_md_is_skipped_not_panic() {
        let t = tempfile::tempdir().unwrap();
        let c = ctx(t.path());
        let bad = c.claude_home.join("skills/bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("SKILL.md"), [0xff, 0xfe, 0x00, 0x9f]).unwrap();
        std::fs::create_dir_all(c.claude_home.join("skills/empty")).unwrap();
        write(
            &c.claude_home.join("skills/weird/SKILL.md"),
            "---\n: : :\n\u{0}---\n",
        );
        let caps = SkillProvider.scan(&c);
        assert!(!caps.iter().any(|x| x.id == "bad" || x.id == "empty"));
        assert!(caps.iter().any(|x| x.id == "usr"));
    }

    #[test]
    fn catalog_scans_all_providers() {
        let t = tempfile::tempdir().unwrap();
        let c = ctx(t.path());
        let cat = super::super::Catalog::scan(&c);
        assert_eq!(cat.of(CapKind::Plugin).count(), 1);
        assert_eq!(cat.of(CapKind::Skill).count(), 4);
    }

    #[test]
    fn cwd_equal_home_does_not_duplicate() {
        let t = tempfile::tempdir().unwrap();
        let mut c = ctx(t.path());
        c.cwd = Some(t.path().join("home"));
        let caps = SkillProvider.scan(&c);
        assert_eq!(caps.iter().filter(|x| x.id == "usr").count(), 1);
        assert!(!caps.iter().any(|x| x.id == "prj"));
    }
}
