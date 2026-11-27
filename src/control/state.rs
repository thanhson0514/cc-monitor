//! ccmon state files under the parent dir of `config::config_path()`:
//! `overlays/<sid>.json`, `defaults.json`, `registry.json`.
use super::{Overlay, Registry};
use std::io;
use std::path::{Path, PathBuf};

pub fn state_dir() -> PathBuf {
    crate::config::config_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}
pub fn load_overlay(sid: &str) -> Overlay {
    load_overlay_in(&state_dir(), sid)
}
pub fn save_overlay(sid: &str, o: &Overlay) -> io::Result<()> {
    save_overlay_in(&state_dir(), sid, o)
}
/// Overlay for a session not started yet (`defaults.json`, else default).
pub fn load_defaults() -> Overlay {
    load_defaults_in(&state_dir())
}
pub fn load_registry() -> Registry {
    load_registry_in(&state_dir())
}
pub fn save_registry(r: &Registry) -> io::Result<()> {
    save_registry_in(&state_dir(), r)
}

/// Merge ccmon-wide switches into a session overlay. The overlay's
/// `workflows_off` stays a user-set master switch and is never derived.
/// A main agent disabled in the registry is dropped (`--agent` not passed).
pub fn effective(o: &Overlay, r: &Registry) -> Overlay {
    let mut e = o.clone();
    if e.main_agent
        .as_ref()
        .is_some_and(|a| r.disabled_agents.contains(a))
    {
        e.main_agent = None;
    }
    e.disabled_agents.extend(r.disabled_agents.iter().cloned());
    e.disabled_workflows
        .extend(r.disabled_workflows.iter().cloned());
    e
}

pub(crate) fn valid_sid(sid: &str) -> bool {
    !sid.is_empty() && sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn read_json<T: serde::de::DeserializeOwned>(p: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

pub(crate) fn write_atomic<T: serde::Serialize>(p: &Path, v: &T) -> io::Result<()> {
    let dir = p
        .parent()
        .ok_or_else(|| io::Error::other("no parent dir"))?;
    std::fs::create_dir_all(dir)?;
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("state");
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}.{seq}.tmp", std::process::id()));
    let body = serde_json::to_vec_pretty(v).map_err(io::Error::other)?;
    let res = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, p));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

pub fn load_overlay_in(dir: &Path, sid: &str) -> Overlay {
    if !valid_sid(sid) {
        return Overlay::default();
    }
    read_json(&dir.join("overlays").join(format!("{sid}.json")))
        .or_else(|| read_json(&dir.join("defaults.json")))
        .unwrap_or_default()
}
pub fn save_overlay_in(dir: &Path, sid: &str, o: &Overlay) -> io::Result<()> {
    if !valid_sid(sid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid session id",
        ));
    }
    write_atomic(&dir.join("overlays").join(format!("{sid}.json")), o)
}
pub fn load_defaults_in(dir: &Path) -> Overlay {
    read_json(&dir.join("defaults.json")).unwrap_or_default()
}
pub fn load_registry_in(dir: &Path) -> Registry {
    read_json(&dir.join("registry.json")).unwrap_or_default()
}
pub fn save_registry_in(dir: &Path, r: &Registry) -> io::Result<()> {
    write_atomic(&dir.join("registry.json"), r)
}

/// A subagent the user paused, kept so it can be continued or swapped.
#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
pub struct Paused {
    pub call: super::subagent::SubagentCall,
    pub at: i64,
}

pub fn load_paused(sid: &str) -> Vec<Paused> {
    load_paused_in(&state_dir(), sid)
}
pub fn save_paused(sid: &str, p: &[Paused]) -> io::Result<()> {
    save_paused_in(&state_dir(), sid, p)
}
pub fn load_paused_in(dir: &Path, sid: &str) -> Vec<Paused> {
    if !valid_sid(sid) {
        return Vec::new();
    }
    read_json(&dir.join("paused").join(format!("{sid}.json"))).unwrap_or_default()
}
pub fn save_paused_in(dir: &Path, sid: &str, p: &[Paused]) -> io::Result<()> {
    if !valid_sid(sid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid session id",
        ));
    }
    let path = dir.join("paused").join(format!("{sid}.json"));
    if p.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    write_atomic(&path, &p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ccmon-state-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn overlay_falls_back_to_defaults() {
        let d = tmp("fallback");
        assert_eq!(load_overlay_in(&d, "abc"), Overlay::default());
        let def = Overlay {
            model: Some("opus".into()),
            ..Default::default()
        };
        std::fs::write(
            d.join("defaults.json"),
            serde_json::to_string(&def).unwrap(),
        )
        .unwrap();
        assert_eq!(load_overlay_in(&d, "abc"), def);
        // corrupt per-session file -> falls back, no panic
        std::fs::create_dir_all(d.join("overlays")).unwrap();
        std::fs::write(d.join("overlays/abc.json"), "{not json").unwrap();
        assert_eq!(load_overlay_in(&d, "abc"), def);
        std::fs::write(d.join("defaults.json"), "garbage").unwrap();
        assert_eq!(load_overlay_in(&d, "abc"), Overlay::default());
    }

    #[test]
    fn save_is_atomic_and_roundtrips() {
        let d = tmp("roundtrip");
        let mut o = Overlay {
            model: Some("sonnet".into()),
            workflows_off: true,
            ..Default::default()
        };
        o.disabled_skills.insert("p:x".into());
        save_overlay_in(&d, "s-1", &o).unwrap();
        assert_eq!(load_overlay_in(&d, "s-1"), o);
        o.effort = Some("high".into());
        save_overlay_in(&d, "s-1", &o).unwrap();
        assert_eq!(load_overlay_in(&d, "s-1"), o);
        let names: Vec<_> = std::fs::read_dir(d.join("overlays"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["s-1.json".to_string()], "no tmp leftovers");
        let r = Registry {
            disabled_agents: ["a".to_string()].into(),
            ..Default::default()
        };
        save_registry_in(&d, &r).unwrap();
        assert_eq!(load_registry_in(&d), r);
        std::fs::write(d.join("registry.json"), "xx").unwrap();
        assert_eq!(load_registry_in(&d), Registry::default());
    }

    #[test]
    fn defaults_load_without_sid() {
        let d = tmp("defaults");
        assert_eq!(load_defaults_in(&d), Overlay::default());
        let def = Overlay {
            effort: Some("high".into()),
            ..Default::default()
        };
        std::fs::write(
            d.join("defaults.json"),
            serde_json::to_string(&def).unwrap(),
        )
        .unwrap();
        assert_eq!(load_defaults_in(&d), def);
    }

    #[test]
    fn rejects_path_traversal_sid() {
        let d = tmp("traversal");
        for bad in ["../x", "a/b", "", "a.b", "..", "a\\b", "é"] {
            assert!(
                save_overlay_in(&d, bad, &Overlay::default()).is_err(),
                "{bad}"
            );
            assert_eq!(load_overlay_in(&d, bad), Overlay::default());
        }
        assert!(!d.join("x.json").exists());
    }

    #[test]
    fn registry_disabled_agents_merge_into_overlay() {
        let o = Overlay {
            disabled_agents: ["a".to_string()].into(),
            main_agent: Some("b".into()),
            ..Default::default()
        };
        let r = Registry {
            disabled_agents: ["b".to_string(), "c".to_string()].into(),
            ..Default::default()
        };
        let e = effective(&o, &r);
        assert_eq!(e.disabled_agents, ["a", "b", "c"].map(String::from).into());
        // a ccmon-disabled agent is never used as the main agent
        assert_eq!(e.main_agent, None);
        assert_eq!(
            effective(&o, &Registry::default()).main_agent,
            Some("b".into())
        );
        let r2 = Registry {
            disabled_workflows: ["w".to_string()].into(),
            ..Default::default()
        };
        let e2 = effective(&o, &r2);
        assert!(!e2.workflows_off);
        assert_eq!(e2.disabled_workflows, ["w".to_string()].into());
    }

    #[test]
    fn paused_roundtrip() {
        use super::super::subagent::SubagentCall;
        let d = tmp("paused");
        let mk = |id: &str| Paused {
            call: SubagentCall {
                agent_id: id.into(),
                tool_use_id: "t".into(),
                agent_type: "Explore".into(),
                description: "d".into(),
                prompt: "p".into(),
                background: false,
            },
            at: 5,
        };
        let v = vec![mk("a1"), mk("a2")];
        save_paused_in(&d, "s-1", &v).unwrap();
        assert_eq!(load_paused_in(&d, "s-1"), v);
        save_paused_in(&d, "s-1", &[]).unwrap();
        assert!(!d.join("paused/s-1.json").exists());
        assert!(load_paused_in(&d, "s-1").is_empty());
        assert!(load_paused_in(&d, "../x").is_empty());
        assert!(save_paused_in(&d, "../x", &v).is_err());
    }
}
