//! Managed `claude` sessions: one long-lived
//! `claude -p --input-format stream-json --output-format stream-json` child per
//! session (std::process + threads, no async).
//!
//! Stdout is used for state only (`system/init`, `result`, session id); the
//! conversation itself is still rendered from the transcript JSONL.
//!
//! Change notification: `Manager` takes an [`OnChange`] callback
//! `Fn(sid, &RunState)`. It is invoked from the per-child stdout reader thread
//! (and from the thread calling `Manager` methods for `Busy`/`Restarting`).
//! The reader thread never touches the `Manager`, so the callback must not
//! lock the `Manager` either: just forward/emit the event. `sid` is `""` until
//! a new session's id is known.
use super::{LaunchSpec, Overlay};
use crate::discovery::LiveInfo;
use serde::Serialize;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Bytes of child stderr kept for [`RunState::Exited`].
pub const STDERR_TAIL: usize = 4096;
/// How long `Manager::send` waits for a new session's id.
const SID_TIMEOUT: Duration = Duration::from_secs(30);

/// State-change observer: `(sid, new_state)`. See module docs.
pub type OnChange = dyn Fn(&str, &RunState) + Send + Sync;

/// Serialized as `{"kind":"idle"}` … `{"kind":"exited","code":1,"stderrTail":"…"}`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RunState {
    /// Process up, no turn running.
    Idle,
    /// Spawned, no stdout event seen yet.
    Starting,
    /// From `send` until the turn's `{"type":"result"}`.
    Busy,
    /// Being stopped to relaunch with a new overlay (Manager only).
    Restarting,
    Exited {
        code: Option<i32>,
        #[serde(rename = "stderrTail")]
        stderr_tail: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "camelCase")]
pub enum ControlError {
    /// Open in a terminal (live pid not ours): read-only.
    SessionLiveElsewhere,
    ClaudeNotFound,
    /// A turn is running; retry after its `result`.
    Busy,
    Io(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionLiveElsewhere => f.write_str("session is running in another terminal"),
            Self::ClaudeNotFound => f.write_str("claude binary not found"),
            Self::Busy => f.write_str("session is busy"),
            Self::Io(m) => write!(f, "io error: {m}"),
        }
    }
}
impl std::error::Error for ControlError {}

impl From<io::Error> for ControlError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::NotFound {
            Self::ClaudeNotFound
        } else {
            Self::Io(e.to_string())
        }
    }
}

struct Shared {
    state: RunState,
    sid: Option<String>,
    /// Set by `stop`/Drop: the reader then stays silent and leaves cleanup to Drop.
    stopped: bool,
}

struct Inner {
    m: Mutex<Shared>,
    cv: Condvar,
    tail: Mutex<Vec<u8>>,
    settings_file: Option<PathBuf>,
    on_change: Option<Arc<OnChange>>,
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.m.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Apply `f`; notify (outside the lock) if the state changed.
    fn update(&self, f: impl FnOnce(&mut Shared)) {
        let note = {
            let mut g = self.lock();
            let before = g.state.clone();
            f(&mut g);
            self.cv.notify_all();
            (g.state != before && !g.stopped).then(|| (g.sid.clone(), g.state.clone()))
        };
        if let (Some((sid, st)), Some(cb)) = (note, &self.on_change) {
            cb(sid.as_deref().unwrap_or(""), &st);
        }
    }
    fn tail(&self) -> String {
        let t = self.tail.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&t).into_owned()
    }
}

/// One running `claude` child.
pub struct ManagedSession {
    child: Arc<Mutex<Child>>,
    pid: u32,
    stdin: Option<ChildStdin>,
    inner: Arc<Inner>,
    req_seq: u64,
}

impl ManagedSession {
    pub fn spawn(spec: LaunchSpec) -> io::Result<Self> {
        Self::spawn_observed(spec, None)
    }

    /// `spawn` with a state-change callback (called from the reader thread).
    pub fn spawn_observed(spec: LaunchSpec, on_change: Option<Arc<OnChange>>) -> io::Result<Self> {
        let spawned = spawn_retrying(&spec);
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                if let Some(f) = &spec.settings_file {
                    let _ = std::fs::remove_file(f);
                }
                return Err(e);
            }
        };
        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let inner = Arc::new(Inner {
            m: Mutex::new(Shared {
                state: RunState::Starting,
                sid: None,
                stopped: false,
            }),
            cv: Condvar::new(),
            tail: Mutex::new(Vec::new()),
            settings_file: spec.settings_file.clone(),
            on_change,
        });
        let child = Arc::new(Mutex::new(child));
        let err_h = stderr.map(|e| {
            let inner = inner.clone();
            std::thread::spawn(move || drain_stderr(e, &inner))
        });
        {
            let inner = inner.clone();
            let child = child.clone();
            std::thread::spawn(move || read_stdout(stdout, err_h, &child, &inner));
        }
        Ok(Self {
            child,
            pid,
            stdin,
            inner,
            req_seq: 0,
        })
    }

    fn write_line(&mut self, v: &serde_json::Value) -> io::Result<()> {
        let w = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "stdin closed"))?;
        let mut line = v.to_string();
        line.push('\n');
        w.write_all(line.as_bytes())?;
        w.flush()
    }

    /// Write one stream-json user message; state is `Busy` until its `result`.
    pub fn send(&mut self, text: &str) -> io::Result<()> {
        if matches!(self.state(), RunState::Exited { .. }) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "session exited"));
        }
        // Busy before writing so a fast `result` cannot be overwritten.
        self.inner.update(|s| s.state = RunState::Busy);
        self.write_line(&serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]}
        }))
    }

    /// Ask the running turn to abort (stream-json control_request). The turn
    /// ends with `result` (`error_during_execution`); the process stays alive.
    pub fn interrupt(&mut self) -> io::Result<()> {
        self.req_seq += 1;
        let id = format!("ccmon-int-{}-{}", self.pid, self.req_seq);
        self.write_line(&serde_json::json!({
            "type": "control_request",
            "request_id": id,
            "request": {"subtype": "interrupt"}
        }))
    }

    pub fn state(&self) -> RunState {
        self.inner.lock().state.clone()
    }

    /// From the first stdout event carrying `session_id` (or `system/init`).
    pub fn session_id(&self) -> Option<String> {
        self.inner.lock().sid.clone()
    }

    /// OS pid of the child (compare with `LiveInfo::pid`).
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Block until the session id is known, the child exits, or `timeout`.
    pub fn wait_session_id(&self, timeout: Duration) -> Option<String> {
        let g = self.inner.lock();
        let (g, _) = self
            .inner
            .cv
            .wait_timeout_while(g, timeout, |s| {
                s.sid.is_none() && !matches!(s.state, RunState::Exited { .. })
            })
            .unwrap_or_else(|e| e.into_inner());
        g.sid.clone()
    }

    /// Kill the child and delete its launch settings file.
    pub fn stop(self) {
        drop(self)
    }
}

impl Drop for ManagedSession {
    fn drop(&mut self) {
        self.inner.lock().stopped = true;
        drop(self.stdin.take());
        {
            let mut c = self.child.lock().unwrap_or_else(|e| e.into_inner());
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(f) = &self.inner.settings_file {
            let _ = std::fs::remove_file(f);
        }
    }
}

/// Spawn, retrying briefly on ETXTBSY (binary being replaced, e.g. by
/// claude's auto-updater, or a script still open for writing in a forked
/// sibling).
fn spawn_retrying(spec: &LaunchSpec) -> io::Result<Child> {
    let mut tries = 0;
    loop {
        let r = Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match r {
            Err(e) if cfg!(unix) && e.raw_os_error() == Some(26) && tries < 20 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            r => return r,
        }
    }
}

fn drain_stderr(mut e: impl Read, inner: &Inner) {
    let mut buf = [0u8; 4096];
    loop {
        match e.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut t = inner.tail.lock().unwrap_or_else(|e| e.into_inner());
                t.extend_from_slice(&buf[..n]);
                if t.len() > STDERR_TAIL {
                    let cut = t.len() - STDERR_TAIL;
                    t.drain(..cut);
                }
            }
        }
    }
}

fn read_stdout(
    stdout: Option<impl Read>,
    err_h: Option<JoinHandle<()>>,
    child: &Mutex<Child>,
    inner: &Inner,
) {
    if let Some(out) = stdout {
        let mut r = BufReader::new(out);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            // Non-JSON lines (warnings etc.) are ignored.
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&buf) else {
                continue;
            };
            on_event(&v, inner);
        }
    }
    if let Some(h) = err_h {
        let _ = h.join();
    }
    let code = loop {
        {
            let mut c = child.lock().unwrap_or_else(|e| e.into_inner());
            match c.try_wait() {
                Ok(Some(st)) => break st.code(),
                Ok(None) => {}
                Err(_) => break None,
            }
        }
        if inner.lock().stopped {
            return; // Drop kills and reaps.
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stderr_tail = inner.tail();
    inner.update(|s| {
        // Under the lock so it cannot race a restart writing the same path.
        if !s.stopped {
            if let Some(f) = &inner.settings_file {
                let _ = std::fs::remove_file(f);
            }
        }
        s.state = RunState::Exited { code, stderr_tail };
    });
}

fn on_event(v: &serde_json::Value, inner: &Inner) {
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let sid = v
        .get("session_id")
        .and_then(|s| s.as_str())
        .filter(|s| super::state::valid_sid(s))
        .map(String::from);
    inner.update(|s| {
        if s.sid.is_none() {
            s.sid = sid;
        }
        if s.state == RunState::Starting {
            s.state = RunState::Idle;
        }
        if ty == "result" && s.state == RunState::Busy {
            s.state = RunState::Idle;
        }
    });
}

struct Entry {
    s: ManagedSession,
    overlay: Overlay,
}

/// All sessions ccmon runs, keyed by session id. Dropping it kills every child.
pub struct Manager {
    sessions: HashMap<String, Entry>,
    on_change: Arc<OnChange>,
    state_dir: PathBuf,
    /// `None` = locate `claude` via `find_claude()` on each launch.
    program: Option<PathBuf>,
}

impl Manager {
    /// Uses the real `claude` binary and `state::state_dir()`.
    pub fn new(on_change: Box<OnChange>) -> Self {
        Self {
            sessions: HashMap::new(),
            on_change: Arc::from(on_change),
            state_dir: super::state::state_dir(),
            program: None,
        }
    }

    /// Explicit program and state dir (tests, custom binary).
    pub fn with_program(program: PathBuf, state_dir: PathBuf, on_change: Box<OnChange>) -> Self {
        Self {
            sessions: HashMap::new(),
            on_change: Arc::from(on_change),
            state_dir,
            program: Some(program),
        }
    }

    fn notify(&self, sid: &str, st: &RunState) {
        (self.on_change)(sid, st)
    }

    fn launch(
        &self,
        sid: Option<&str>,
        cwd: &Path,
        o: &Overlay,
    ) -> Result<ManagedSession, ControlError> {
        let spec = match &self.program {
            Some(p) => super::build_args_with(p.clone(), sid, cwd, o, &self.state_dir)?,
            None => super::build_args(sid, cwd, o, &self.state_dir)?,
        };
        Ok(ManagedSession::spawn_observed(
            spec,
            Some(self.on_change.clone()),
        )?)
    }

    /// Send one user message. `sid == None` starts a new session and returns
    /// its id. A changed overlay restarts the process between turns
    /// (`--resume <sid>`); while a turn runs that is `Err(Busy)`, as is a send
    /// during a turn. `live` = `discovery::read_live` (our own children appear
    /// there too and are told apart by pid).
    pub fn send(
        &mut self,
        sid: Option<&str>,
        cwd: &Path,
        text: &str,
        o: &Overlay,
        live: &HashMap<String, LiveInfo>,
    ) -> Result<String, ControlError> {
        let Some(sid) = sid else {
            let mut s = self.launch(None, cwd, o)?;
            s.send(text)?;
            let Some(new) = s.wait_session_id(SID_TIMEOUT) else {
                let msg = match s.state() {
                    RunState::Exited { stderr_tail, .. } => {
                        format!("claude exited: {}", stderr_tail.trim())
                    }
                    _ => "no session id from claude".to_string(),
                };
                return Err(ControlError::Io(msg));
            };
            self.sessions.insert(
                new.clone(),
                Entry {
                    s,
                    overlay: o.clone(),
                },
            );
            return Ok(new);
        };
        if let Some(info) = live.get(sid) {
            if self.sessions.get(sid).map(|e| e.s.pid()) != Some(info.pid) {
                return Err(ControlError::SessionLiveElsewhere);
            }
        }
        if let Some(e) = self.sessions.get_mut(sid) {
            match e.s.state() {
                RunState::Exited { .. } => {
                    self.sessions.remove(sid);
                }
                RunState::Busy | RunState::Restarting => return Err(ControlError::Busy),
                RunState::Idle | RunState::Starting if e.overlay != *o => {
                    self.notify(sid, &RunState::Restarting);
                    self.sessions.remove(sid); // Drop: kill + cleanup
                }
                RunState::Idle | RunState::Starting => {
                    e.s.send(text)?;
                    return Ok(sid.to_string());
                }
            }
        }
        let mut s = self.launch(Some(sid), cwd, o)?;
        s.send(text)?;
        self.sessions.insert(
            sid.to_string(),
            Entry {
                s,
                overlay: o.clone(),
            },
        );
        Ok(sid.to_string())
    }

    pub fn interrupt(&mut self, sid: &str) -> Result<(), ControlError> {
        match self.sessions.get_mut(sid) {
            Some(e) => Ok(e.s.interrupt()?),
            None => Err(ControlError::Io("session not managed".into())),
        }
    }

    /// Kill a managed session. Returns false if it was not managed.
    pub fn stop(&mut self, sid: &str) -> bool {
        let Some(e) = self.sessions.remove(sid) else {
            return false;
        };
        e.s.stop();
        self.notify(
            sid,
            &RunState::Exited {
                code: None,
                stderr_tail: String::new(),
            },
        );
        true
    }

    pub fn state(&self, sid: &str) -> Option<RunState> {
        self.sessions.get(sid).map(|e| e.s.state())
    }

    pub fn pid(&self, sid: &str) -> Option<u32> {
        self.sessions.get(sid).map(|e| e.s.pid())
    }

    /// Overlay the session's current process was launched with.
    pub fn overlay(&self, sid: &str) -> Option<&Overlay> {
        self.sessions.get(sid).map(|e| &e.overlay)
    }

    /// `(sid, state)` of every managed session.
    pub fn sessions(&self) -> Vec<(String, RunState)> {
        self.sessions
            .iter()
            .map(|(k, e)| (k.clone(), e.s.state()))
            .collect()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    const NEW_SID: &str = "11111111-2222-3333-4444-555555555555";

    struct Fake {
        dir: tempfile::TempDir,
        program: PathBuf,
    }

    impl Fake {
        fn log(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("spawns.log"))
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect()
        }
        fn stdin_lines(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("stdin.log"))
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect()
        }
        fn gate(&self) -> PathBuf {
            self.dir.path().join("gate")
        }
        fn state_dir(&self) -> PathBuf {
            self.dir.path().join("state")
        }
        fn spec(&self, sid: Option<&str>, o: &Overlay) -> LaunchSpec {
            super::super::build_args_with(
                self.program.clone(),
                sid,
                self.dir.path(),
                o,
                &self.state_dir(),
            )
            .unwrap()
        }
    }

    /// Fake `claude`: logs its args, prints a hook event with the session id,
    /// then answers every stdin line like stream-json mode does.
    /// `gated`: each turn waits for the gate file before its `result`.
    fn fake(gated: bool) -> Fake {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().display();
        let gate = if gated {
            format!(r#"while [ ! -e "{d}/gate" ]; do sleep 0.01; done; rm -f "{d}/gate""#)
        } else {
            String::from(":")
        };
        let script = format!(
            r#"#!/bin/sh
echo "$*" >> "{d}/spawns.log"
SID={NEW_SID}
prev=""
for a in "$@"; do [ "$prev" = "--resume" ] && SID="$a"; prev="$a"; done
echo '{{"type":"system","subtype":"hook_started","session_id":"'$SID'"}}'
while IFS= read -r line; do
  echo "$line" >> "{d}/stdin.log"
  case "$line" in
    *control_request*)
      echo '{{"type":"control_response","response":{{"subtype":"success"}}}}'
      echo '{{"type":"result","subtype":"error_during_execution","session_id":"'$SID'"}}' ;;
    *)
      echo '{{"type":"system","subtype":"init","session_id":"'$SID'"}}'
      echo 'not json at all'
      echo '{{"type":"assistant","session_id":"'$SID'"}}'
      {gate}
      echo '{{"type":"result","subtype":"success","session_id":"'$SID'"}}' ;;
  esac
done
"#
        );
        let program = dir.path().join("fake-claude");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        Fake { dir, program }
    }

    fn wait_for(what: &str, f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(5), "timed out: {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn manager(f: &Fake) -> Manager {
        Manager::with_program(f.program.clone(), f.state_dir(), Box::new(|_, _| {}))
    }

    fn live(sid: &str, pid: u32) -> HashMap<String, LiveInfo> {
        let mut m = HashMap::new();
        m.insert(
            sid.to_string(),
            LiveInfo {
                pid,
                session_id: sid.to_string(),
                status: None,
                name: None,
                tmux: None,
            },
        );
        m
    }

    #[test]
    fn refuses_resume_when_session_live() {
        let f = fake(false);
        let mut m = manager(&f);
        let o = Overlay::default();
        // Open in a terminal (some other pid) -> refuse, nothing spawned.
        let r = m.send(Some("abc"), f.dir.path(), "hi", &o, &live("abc", 1));
        assert_eq!(r, Err(ControlError::SessionLiveElsewhere));
        assert!(f.log().is_empty());
        // Our own managed child shows up as live too: allowed.
        let sid = m
            .send(Some("abc"), f.dir.path(), "hi", &o, &HashMap::new())
            .unwrap();
        assert_eq!(sid, "abc");
        wait_for("idle", || m.state("abc") == Some(RunState::Idle));
        let pid = m.pid("abc").unwrap();
        m.send(Some("abc"), f.dir.path(), "again", &o, &live("abc", pid))
            .unwrap();
        wait_for("second turn", || f.stdin_lines().len() == 2);
        assert_eq!(f.log().len(), 1);
    }

    #[test]
    fn new_session_takes_sid_from_first_event() {
        let f = fake(false);
        let mut m = manager(&f);
        let sid = m
            .send(
                None,
                f.dir.path(),
                "hello",
                &Overlay::default(),
                &HashMap::new(),
            )
            .unwrap();
        assert_eq!(sid, NEW_SID);
        assert!(!f.log()[0].contains("--resume"));
        wait_for("stdin line", || f.stdin_lines().len() == 1);
        let line: serde_json::Value = serde_json::from_str(&f.stdin_lines()[0]).unwrap();
        assert_eq!(line["type"], "user");
        assert_eq!(line["message"]["role"], "user");
        assert_eq!(line["message"]["content"][0]["type"], "text");
        assert_eq!(line["message"]["content"][0]["text"], "hello");
    }

    #[test]
    fn restarts_between_turns_when_overlay_changes() {
        let f = fake(true);
        let mut m = manager(&f);
        let a = Overlay::default();
        let b = Overlay {
            model: Some("haiku".into()),
            ..Default::default()
        };
        m.send(Some("s1"), f.dir.path(), "one", &a, &HashMap::new())
            .unwrap();
        assert_eq!(m.state("s1"), Some(RunState::Busy));
        wait_for("turn reached the fake", || f.stdin_lines().len() == 1);
        // Mid-turn: no restart.
        assert_eq!(
            m.send(Some("s1"), f.dir.path(), "two", &b, &HashMap::new()),
            Err(ControlError::Busy)
        );
        assert_eq!(f.log().len(), 1);
        std::fs::write(f.gate(), "").unwrap();
        wait_for("idle", || m.state("s1") == Some(RunState::Idle));
        let old_pid = m.pid("s1").unwrap();
        // Same overlay: reuse the process.
        m.send(Some("s1"), f.dir.path(), "two", &a, &HashMap::new())
            .unwrap();
        std::fs::write(f.gate(), "").unwrap();
        wait_for("idle 2", || {
            m.state("s1") == Some(RunState::Idle) && f.stdin_lines().len() == 2
        });
        assert_eq!(f.log().len(), 1);
        // Changed overlay while idle: stop + respawn with --resume.
        m.send(Some("s1"), f.dir.path(), "three", &b, &HashMap::new())
            .unwrap();
        wait_for("second spawn", || f.log().len() == 2);
        let log = f.log();
        assert!(log[1].contains("--resume s1"), "{}", log[1]);
        assert!(log[1].contains("--model haiku"), "{}", log[1]);
        assert_ne!(m.pid("s1").unwrap(), old_pid);
        std::fs::write(f.gate(), "").unwrap();
        wait_for("idle 3", || m.state("s1") == Some(RunState::Idle));
    }

    #[test]
    fn busy_until_result_event() {
        let f = fake(true);
        let mut s = ManagedSession::spawn(f.spec(Some("b1"), &Overlay::default())).unwrap();
        wait_for("idle after start", || s.state() == RunState::Idle);
        assert_eq!(s.session_id().as_deref(), Some("b1"));
        s.send("go").unwrap();
        assert_eq!(s.state(), RunState::Busy);
        // init + assistant + junk line arrive, still busy until result.
        wait_for("turn started", || f.stdin_lines().len() == 1);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(s.state(), RunState::Busy);
        std::fs::write(f.gate(), "").unwrap();
        wait_for("idle after result", || s.state() == RunState::Idle);
        s.stop();
    }

    #[test]
    fn interrupt_writes_control_request() {
        let f = fake(false);
        let mut s = ManagedSession::spawn(f.spec(Some("i1"), &Overlay::default())).unwrap();
        s.interrupt().unwrap();
        wait_for("line", || f.stdin_lines().len() == 1);
        let v: serde_json::Value = serde_json::from_str(&f.stdin_lines()[0]).unwrap();
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request"]["subtype"], "interrupt");
        assert!(v["request_id"].as_str().is_some_and(|s| !s.is_empty()));
        wait_for("idle", || s.state() == RunState::Idle);
    }

    #[test]
    fn exited_reports_stderr_tail() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("dying");
        // ~10 KiB of noise on stderr, then the real error, exit 3.
        std::fs::write(
            &program,
            "#!/bin/sh\ni=0\nwhile [ $i -lt 200 ]; do echo 'noise noise noise noise noise noise' >&2; i=$((i+1)); done\necho 'fatal: boom' >&2\nexit 3\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let s = ManagedSession::spawn(LaunchSpec {
            program,
            args: vec![],
            cwd: dir.path().to_path_buf(),
            settings_file: None,
        })
        .unwrap();
        wait_for("exited", || matches!(s.state(), RunState::Exited { .. }));
        let RunState::Exited { code, stderr_tail } = s.state() else {
            unreachable!()
        };
        assert_eq!(code, Some(3));
        assert!(
            stderr_tail.trim_end().ends_with("fatal: boom"),
            "{stderr_tail}"
        );
        assert!(stderr_tail.len() <= STDERR_TAIL);
        assert!(stderr_tail.len() > 1000);
    }

    #[test]
    fn stop_kills_child_and_removes_settings_file() {
        let f = fake(false);
        let o = Overlay {
            disabled_plugins: ["p@m".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let spec = f.spec(Some("k1"), &o);
        let settings = spec.settings_file.clone().unwrap();
        assert!(settings.exists());
        let s = ManagedSession::spawn(spec).unwrap();
        let pid = s.pid();
        s.stop();
        assert!(!settings.exists());
        // stop() kills and reaps synchronously.
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn manager_drop_kills_children_and_notifies() {
        let f = fake(false);
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let mut m = Manager::with_program(
            f.program.clone(),
            f.state_dir(),
            Box::new(move |sid, st| {
                if sid == "d1" && *st == RunState::Idle {
                    h.fetch_add(1, Ordering::SeqCst);
                }
            }),
        );
        m.send(
            Some("d1"),
            f.dir.path(),
            "x",
            &Overlay::default(),
            &HashMap::new(),
        )
        .unwrap();
        wait_for("notified idle", || hits.load(Ordering::SeqCst) > 0);
        let pid = m.pid("d1").unwrap();
        drop(m);
        wait_for("gone", || !Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn missing_program_is_claude_not_found() {
        let d = tempfile::tempdir().unwrap();
        let mut m = Manager::with_program(
            d.path().join("nope"),
            d.path().to_path_buf(),
            Box::new(|_, _| {}),
        );
        let r = m.send(
            Some("z"),
            d.path(),
            "x",
            &Overlay::default(),
            &HashMap::new(),
        );
        assert_eq!(r, Err(ControlError::ClaudeNotFound));
    }
}
