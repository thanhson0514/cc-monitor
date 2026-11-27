//! Managed `claude` sessions: one long-lived
//! `claude -p --input-format stream-json --output-format stream-json` child per
//! session (std::process + threads, no async).
//!
//! Stdout is used for state (`system/init`, `result`, session id) and for the
//! live [`Draft`] of the message being streamed (`--include-partial-messages`);
//! the conversation itself is still rendered from the transcript JSONL.
//!
//! Change notification: `Manager` takes an [`OnChange`] callback
//! `Fn(sid, &RunState)`. It is invoked from the per-child stdout reader thread
//! (and from the thread calling `Manager` methods for `Busy`/`Restarting`).
//! The reader thread never touches the `Manager`, so the callback must not
//! lock the `Manager` either: just forward/emit the event. `sid` is `""` until
//! a new session's id is known.
use super::draft::{Change, Draft};
use super::{LaunchSpec, Overlay};
use crate::discovery::LiveInfo;
use serde::Serialize;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Bytes of child stderr kept for [`RunState::Exited`].
pub const STDERR_TAIL: usize = 4096;
/// How long `Manager::send` waits for a new session's id.
const SID_TIMEOUT: Duration = Duration::from_secs(30);

/// State-change observer: `(sid, new_state)`. See module docs.
pub type OnChange = dyn Fn(&str, &RunState) + Send + Sync;

/// Live-draft observer: `(sid, draft)`, called from the reader thread like
/// [`OnChange`], at most every [`DRAFT_EVERY`] for text deltas.
pub type OnDraft = dyn Fn(&str, &Draft) + Send + Sync;

/// Throttle for draft text deltas (block starts/stops are sent at once).
pub const DRAFT_EVERY: Duration = Duration::from_millis(60);

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
    on_draft: Option<Arc<OnDraft>>,
    /// Draft and when it was last sent (reader thread only, but `Inner` is shared).
    draft: Mutex<(Draft, Option<Instant>)>,
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.m.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Apply `f`; notify (outside the lock) if the state changed or the
    /// session id just became known.
    fn update(&self, f: impl FnOnce(&mut Shared)) {
        let note = {
            let mut g = self.lock();
            let before = (g.state.clone(), g.sid.is_some());
            f(&mut g);
            self.cv.notify_all();
            let changed = g.state != before.0 || g.sid.is_some() != before.1;
            (changed && !g.stopped).then(|| (g.sid.clone(), g.state.clone()))
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
    /// When the child was spawned (it loads agent files only at start).
    started: std::time::SystemTime,
}

impl ManagedSession {
    pub fn spawn(spec: LaunchSpec) -> io::Result<Self> {
        Self::spawn_observed(spec, None)
    }

    /// `spawn` with a state-change callback (called from the reader thread).
    pub fn spawn_observed(spec: LaunchSpec, on_change: Option<Arc<OnChange>>) -> io::Result<Self> {
        Self::spawn_with(spec, on_change, None)
    }

    /// `spawn_observed` plus a live-draft callback.
    pub fn spawn_with(
        spec: LaunchSpec,
        on_change: Option<Arc<OnChange>>,
        on_draft: Option<Arc<OnDraft>>,
    ) -> io::Result<Self> {
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
                // Resumed sessions know their id up front.
                sid: resume_sid(&spec.args),
                stopped: false,
            }),
            cv: Condvar::new(),
            tail: Mutex::new(Vec::new()),
            settings_file: spec.settings_file.clone(),
            on_change,
            on_draft,
            draft: Mutex::new((Draft::default(), None)),
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
            started: std::time::SystemTime::now(),
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

    /// Stop one background subagent task (stream-json control_request).
    pub fn stop_task(&mut self, task_id: &str) -> io::Result<()> {
        self.req_seq += 1;
        let id = format!("ccmon-stop-{}-{}", self.pid, self.req_seq);
        self.write_line(&serde_json::json!({
            "type": "control_request",
            "request_id": id,
            "request": {"subtype": "stop_task", "task_id": task_id}
        }))
    }

    /// Write a user line whatever the state; claude queues it when a turn is
    /// running and answers it in the same turn. `Busy` only if currently `Idle`.
    pub fn send_queued(&mut self, text: &str) -> io::Result<()> {
        let st = self.state();
        if matches!(st, RunState::Exited { .. }) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "session exited"));
        }
        if st == RunState::Idle {
            self.inner.update(|s| s.state = RunState::Busy);
        }
        self.write_line(&serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]}
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
    /// Spawn time of the child process.
    pub fn started(&self) -> std::time::SystemTime {
        self.started
    }

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
        // Closing stdin ends a stream-json session; give it a moment.
        drop(self.stdin.take());
        let deadline = std::time::Instant::now() + STOP_GRACE;
        loop {
            let done = {
                let mut c = self.child.lock().unwrap_or_else(|e| e.into_inner());
                !matches!(c.try_wait(), Ok(None))
            };
            if done || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        {
            let mut c = self.child.lock().unwrap_or_else(|e| e.into_inner());
            // Always signal the group: descendants may outlive the leader.
            kill_tree(&mut c, self.pid);
            let _ = c.wait();
        }
        if let Some(f) = &self.inner.settings_file {
            let _ = std::fs::remove_file(f);
        }
    }
}

fn resume_sid(args: &[String]) -> Option<String> {
    args.iter()
        .position(|a| a == "--resume")
        .and_then(|i| args.get(i + 1))
        .filter(|s| super::state::valid_sid(s))
        .cloned()
}

/// Grace period for the child to exit after stdin closes, before SIGKILL.
const STOP_GRACE: Duration = Duration::from_millis(1500);

/// Kill the child and, on unix, its whole process group (it was spawned as
/// a group leader), so tools it started (Bash, dev servers) cannot keep the
/// stdout/stderr pipes open. No libc: shell out to `kill`.
fn kill_tree(c: &mut Child, pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    let _ = pid; // Windows: only the direct child is killed (no job object without new crates).
    let _ = c.kill();
}

/// Spawn, retrying briefly on ETXTBSY (binary being replaced, e.g. by
/// claude's auto-updater, or a script still open for writing in a forked
/// sibling).
fn spawn_retrying(spec: &LaunchSpec) -> io::Result<Child> {
    let mut tries = 0;
    loop {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0); // own group, killed as a whole by kill_tree
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: the GUI has no console, so without this every
            // claude.exe / claude.cmd child would open a visible console window.
            cmd.creation_flags(0x0800_0000);
        }
        let r = cmd.spawn();
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
    if ty == "stream_event" {
        on_stream_event(v, inner);
    }
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

fn on_stream_event(v: &serde_json::Value, inner: &Inner) {
    let Some(cb) = &inner.on_draft else { return };
    let out = {
        let mut g = inner.draft.lock().unwrap_or_else(|e| e.into_inner());
        let (draft, last) = &mut *g;
        let Some(change) = draft.apply(v) else { return };
        let due = change == Change::Now || last.is_none_or(|t| t.elapsed() >= DRAFT_EVERY);
        if !due {
            return;
        }
        *last = Some(Instant::now());
        draft.clone()
    };
    let sid = {
        let g = inner.lock();
        if g.stopped {
            return;
        }
        g.sid.clone().unwrap_or_default()
    };
    cb(&sid, &out);
}

struct Entry {
    s: ManagedSession,
    overlay: Overlay,
}

/// All sessions ccmon runs, keyed by session id. Dropping it kills every child.
pub struct Manager {
    sessions: HashMap<String, Entry>,
    on_change: Arc<OnChange>,
    on_draft: Option<Arc<OnDraft>>,
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
            on_draft: None,
            state_dir: super::state::state_dir(),
            program: None,
        }
    }

    /// Explicit program and state dir (tests, custom binary).
    pub fn with_program(program: PathBuf, state_dir: PathBuf, on_change: Box<OnChange>) -> Self {
        Self {
            sessions: HashMap::new(),
            on_change: Arc::from(on_change),
            on_draft: None,
            state_dir,
            program: Some(program),
        }
    }

    /// Receive the live draft of each session's streaming message.
    pub fn with_draft_observer(mut self, f: Box<OnDraft>) -> Self {
        self.on_draft = Some(Arc::from(f));
        self
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
        Ok(ManagedSession::spawn_with(
            spec,
            Some(self.on_change.clone()),
            self.on_draft.clone(),
        )?)
    }

    /// Report a terminal state after a failed launch/send so the UI never
    /// stays on `Restarting`/`Busy`.
    fn fail(&self, sid: &str, e: ControlError) -> ControlError {
        self.notify(
            sid,
            &RunState::Exited {
                code: None,
                stderr_tail: e.to_string(),
            },
        );
        e
    }

    /// Lock-free new-session start, step 1 (needs only `&self`): spawn
    /// without `--resume` and send the first message. Step 2, with the
    /// Manager lock RELEASED: `session.wait_session_id(timeout)`. Step 3:
    /// `adopt(sid, session, overlay)`.
    pub fn start_new(
        &self,
        cwd: &Path,
        text: &str,
        o: &Overlay,
    ) -> Result<ManagedSession, ControlError> {
        let mut s = self.launch(None, cwd, o)?;
        if let Err(e) = s.send(text) {
            return Err(self.fail("", e.into()));
        }
        Ok(s)
    }

    /// Register a session started with `start_new` under its id (replacing,
    /// i.e. killing, any previous entry for that id).
    pub fn adopt(&mut self, sid: String, s: ManagedSession, overlay: Overlay) {
        self.sessions.insert(sid, Entry { s, overlay });
    }

    /// Send one user message. `sid == None` starts a new session and returns
    /// its id, BLOCKING up to 30 s for it under `&mut self`; under a shared
    /// lock prefer `start_new` + `wait_session_id` + `adopt`. A changed overlay restarts the process between turns
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
            let s = self.start_new(cwd, text, o)?;
            let Some(new) = s.wait_session_id(SID_TIMEOUT) else {
                let msg = match s.state() {
                    RunState::Exited { stderr_tail, .. } => {
                        format!("claude exited: {}", stderr_tail.trim())
                    }
                    _ => "no session id from claude".to_string(),
                };
                return Err(ControlError::Io(msg));
            };
            self.adopt(new.clone(), s, o.clone());
            return Ok(new);
        };
        if let Some(info) = live.get(sid) {
            if !self.owns_live(sid, info.pid) {
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
                    if let Err(err) = e.s.send(text) {
                        self.sessions.remove(sid);
                        return Err(self.fail(sid, err.into()));
                    }
                    return Ok(sid.to_string());
                }
            }
        }
        let mut s = self
            .launch(Some(sid), cwd, o)
            .map_err(|e| self.fail(sid, e))?;
        if let Err(e) = s.send(text) {
            drop(s);
            return Err(self.fail(sid, e.into()));
        }
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

    pub fn stop_task(&mut self, sid: &str, task_id: &str) -> Result<(), ControlError> {
        match self.sessions.get_mut(sid) {
            Some(e) => Ok(e.s.stop_task(task_id)?),
            None => Err(ControlError::Io("session not managed".into())),
        }
    }

    pub fn send_queued(&mut self, sid: &str, text: &str) -> Result<(), ControlError> {
        match self.sessions.get_mut(sid) {
            Some(e) => Ok(e.s.send_queued(text)?),
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

    /// A live registry entry (pid `live_pid`) for `sid` is ccmon's own when we
    /// hold a non-exited process for it — whatever its pid: with a wrapper
    /// launcher (`claude.cmd` -> cmd.exe -> node) our child's pid never
    /// matches the one claude records. An exited entry still counts on a
    /// pid match (stale registry file).
    pub fn owns_live(&self, sid: &str, live_pid: u32) -> bool {
        self.sessions.get(sid).is_some_and(|e| {
            !matches!(e.s.state(), RunState::Exited { .. }) || e.s.pid() == live_pid
        })
    }

    /// Remove every managed session without stopping it (see `stop_parallel`).
    pub fn take_all(&mut self) -> Vec<(String, ManagedSession)> {
        self.sessions.drain().map(|(k, e)| (k, e.s)).collect()
    }

    pub fn pid(&self, sid: &str) -> Option<u32> {
        self.sessions.get(sid).map(|e| e.s.pid())
    }

    /// Spawn time of the session's current process.
    pub fn started(&self, sid: &str) -> Option<std::time::SystemTime> {
        self.sessions.get(sid).map(|e| e.s.started())
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

/// Stop sessions concurrently (each `Drop` waits up to `STOP_GRACE`), waiting
/// at most `cap` overall; stragglers keep stopping on their own threads.
pub fn stop_parallel(sessions: Vec<ManagedSession>, cap: Duration) {
    let (tx, rx) = std::sync::mpsc::channel();
    let n = sessions.len();
    for s in sessions {
        let tx = tx.clone();
        std::thread::spawn(move || {
            s.stop();
            let _ = tx.send(());
        });
    }
    drop(tx);
    let deadline = std::time::Instant::now() + cap;
    for _ in 0..n {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if rx.recv_timeout(left).is_err() {
            break;
        }
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
      echo '{{"type":"stream_event","session_id":"'$SID'","parent_tool_use_id":null,"event":{{"type":"message_start","message":{{"id":"msg_f"}}}}}}'
      echo '{{"type":"stream_event","session_id":"'$SID'","parent_tool_use_id":null,"event":{{"type":"content_block_start","index":0,"content_block":{{"type":"text","text":""}}}}}}'
      echo '{{"type":"stream_event","session_id":"'$SID'","parent_tool_use_id":null,"event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"hi"}}}}}}'
      echo '{{"type":"stream_event","session_id":"'$SID'","parent_tool_use_id":null,"event":{{"type":"content_block_stop","index":0}}}}'
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
    fn own_running_session_is_never_live_elsewhere() {
        let f = fake(false);
        let mut m = manager(&f);
        let o = Overlay::default();
        m.send(Some("w1"), f.dir.path(), "hi", &o, &HashMap::new())
            .unwrap();
        wait_for("idle", || m.state("w1") == Some(RunState::Idle));
        // Wrapper launcher: the registry pid is not our child's pid.
        let other = m.pid("w1").unwrap() + 100_000;
        assert!(m.owns_live("w1", other));
        m.send(Some("w1"), f.dir.path(), "again", &o, &live("w1", other))
            .unwrap();
        wait_for("second turn", || f.stdin_lines().len() == 2);
        assert!(!m.owns_live("nope", other));
    }

    #[test]
    fn stop_parallel_is_bounded() {
        let f = fake(true); // turns never finish: each Drop waits the full grace
        let mut m = manager(&f);
        for sid in ["p1", "p2", "p3"] {
            m.send(
                Some(sid),
                f.dir.path(),
                "x",
                &Overlay::default(),
                &HashMap::new(),
            )
            .unwrap();
        }
        wait_for("busy", || {
            ["p1", "p2", "p3"]
                .iter()
                .all(|s| m.state(s) == Some(RunState::Busy))
        });
        let pids: Vec<u32> = ["p1", "p2", "p3"]
            .iter()
            .map(|s| m.pid(s).unwrap())
            .collect();
        let all = m.take_all();
        assert_eq!(all.len(), 3);
        assert!(m.sessions().is_empty());
        let t = Instant::now();
        stop_parallel(
            all.into_iter().map(|(_, s)| s).collect(),
            Duration::from_secs(2),
        );
        assert!(
            t.elapsed() < Duration::from_millis(2500),
            "{:?}",
            t.elapsed()
        );
        for pid in pids {
            wait_for("gone", || !Path::new(&format!("/proc/{pid}")).exists());
        }
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
    fn streams_draft_to_observer() {
        let f = fake(false);
        let seen: Arc<Mutex<Vec<(String, Draft)>>> = Arc::default();
        let sink = seen.clone();
        let cb: Arc<OnDraft> = Arc::new(move |sid: &str, d: &Draft| {
            sink.lock().unwrap().push((sid.to_string(), d.clone()))
        });
        let mut s =
            ManagedSession::spawn_with(f.spec(Some("d1"), &Overlay::default()), None, Some(cb))
                .unwrap();
        s.send("go").unwrap();
        wait_for("block stop", || {
            seen.lock()
                .unwrap()
                .last()
                .is_some_and(|(_, d)| d.blocks.first().is_some_and(|b| b.done))
        });
        let (sid, d) = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(sid, "d1");
        assert_eq!(d.msg_id, "msg_f");
        assert_eq!(d.blocks[0].text, "hi");
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
    fn stop_task_writes_control_request() {
        let f = fake(false);
        let mut s = ManagedSession::spawn(f.spec(Some("t1"), &Overlay::default())).unwrap();
        s.stop_task("aa18bd04354ab1013").unwrap();
        wait_for("line", || f.stdin_lines().len() == 1);
        let v: serde_json::Value = serde_json::from_str(&f.stdin_lines()[0]).unwrap();
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request"]["subtype"], "stop_task");
        assert_eq!(v["request"]["task_id"], "aa18bd04354ab1013");
        assert!(v["request_id"].as_str().unwrap().starts_with("ccmon-stop-"));
    }

    #[test]
    fn send_queued_while_busy() {
        let f = fake(false);
        let mut s = ManagedSession::spawn(f.spec(Some("q1"), &Overlay::default())).unwrap();
        s.send("a").unwrap();
        assert_eq!(s.state(), RunState::Busy);
        s.send_queued("b").unwrap();
        wait_for("lines", || f.stdin_lines().len() == 2);
        let v: serde_json::Value = serde_json::from_str(&f.stdin_lines()[1]).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["content"][0]["text"], "b");
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

    type Events = Arc<Mutex<Vec<(String, RunState)>>>;

    fn recording(f: &Fake) -> (Manager, Events) {
        let ev: Events = Arc::default();
        let e = ev.clone();
        let m = Manager::with_program(
            f.program.clone(),
            f.state_dir(),
            Box::new(move |sid, st| e.lock().unwrap().push((sid.to_string(), st.clone()))),
        );
        (m, ev)
    }

    #[test]
    fn resumed_session_notifications_carry_sid() {
        let f = fake(false);
        let (mut m, ev) = recording(&f);
        m.send(
            Some("r1"),
            f.dir.path(),
            "x",
            &Overlay::default(),
            &HashMap::new(),
        )
        .unwrap();
        wait_for("idle", || m.state("r1") == Some(RunState::Idle));
        wait_for("idle event", || {
            ev.lock()
                .unwrap()
                .iter()
                .any(|(_, st)| *st == RunState::Idle)
        });
        let ev = ev.lock().unwrap().clone();
        assert!(ev.contains(&("r1".to_string(), RunState::Busy)), "{ev:?}");
        assert!(ev.iter().all(|(sid, _)| sid == "r1"), "{ev:?}");
    }

    #[test]
    fn lock_free_new_session_start() {
        let f = fake(false);
        let (mut m, ev) = recording(&f);
        let s = m
            .start_new(f.dir.path(), "hi", &Overlay::default())
            .unwrap();
        // (Task 10: Manager lock released here.)
        let sid = s.wait_session_id(Duration::from_secs(5)).unwrap();
        assert_eq!(sid, NEW_SID);
        m.adopt(sid.clone(), s, Overlay::default());
        wait_for("idle", || m.state(NEW_SID) == Some(RunState::Idle));
        // The moment the id became known was reported with the id.
        wait_for("sid event", || {
            ev.lock().unwrap().iter().any(|(s, _)| s == NEW_SID)
        });
    }

    #[test]
    fn failed_restart_reports_exited() {
        let f = fake(false);
        let (mut m, ev) = recording(&f);
        m.send(
            Some("f1"),
            f.dir.path(),
            "x",
            &Overlay::default(),
            &HashMap::new(),
        )
        .unwrap();
        wait_for("idle", || m.state("f1") == Some(RunState::Idle));
        std::fs::remove_file(&f.program).unwrap();
        let b = Overlay {
            model: Some("haiku".into()),
            ..Default::default()
        };
        let r = m.send(Some("f1"), f.dir.path(), "y", &b, &HashMap::new());
        assert_eq!(r, Err(ControlError::ClaudeNotFound));
        assert_eq!(m.state("f1"), None);
        let ev = ev.lock().unwrap().clone();
        let n = ev.len();
        assert_eq!(ev[n - 2], ("f1".to_string(), RunState::Restarting));
        assert!(
            matches!(&ev[n - 1], (sid, RunState::Exited { code: None, stderr_tail })
                if sid == "f1" && !stderr_tail.is_empty()),
            "{ev:?}"
        );
    }

    #[test]
    fn stop_kills_process_group() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().display();
        let program = dir.path().join("forky");
        // A descendant keeps stdout/stderr open; the leader ignores stdin EOF.
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nsleep 30 &\necho $! > \"{d}/bg.pid\"\necho '{{\"type\":\"system\",\"session_id\":\"g1\"}}'\nwhile :; do sleep 1; done\n"
            ),
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
        wait_for("idle", || s.state() == RunState::Idle);
        let bg: u32 = std::fs::read_to_string(dir.path().join("bg.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let inner = s.inner.clone();
        s.stop();
        wait_for("reader saw exit", || {
            matches!(inner.lock().state, RunState::Exited { .. })
        });
        wait_for("background child killed", || {
            // Gone, or a zombie awaiting its new parent.
            std::fs::read_to_string(format!("/proc/{bg}/stat"))
                .map(|st| st.split(") ").nth(1).is_some_and(|r| r.starts_with('Z')))
                .unwrap_or(true)
        });
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
