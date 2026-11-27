# Terminal control, session agents, file focus, subagent stop/swap — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let ccmon drive terminal sessions as if the user typed, scope the Agents page to the session, focus files in the chat, and pause/resume/swap/cancel subagents.

**Architecture:** A new `control::tmux` channel pastes human-origin input into the session's tmux pane; ccmon-managed sessions keep the stream-json channel and gain `stop_task` and mid-turn sends. A `control::subagent` module finds the original Agent call in the main thread, builds the messages sent to main, and stores pause markers in ccmon state. The GUI routes every action through one channel choice made in `cmd_control.rs`.

**Tech Stack:** Rust (cc_monitor lib + Tauri 2 backend), React 18 + TypeScript (Vite), tmux CLI, Claude Code 2.1.326 stream-json.

**Spec:** `docs/superpowers/specs/2026-11-27-terminal-control-subagent-swap-design.md`

## Global Constraints

- Input sent by ccmon must be human-origin: stream-json `user` message (managed) or tmux paste (terminal). Never the peer messaging socket.
- Never edit Claude Code files/settings; ccmon writes only its own state through `state::write_atomic`; every path built from a sid passes `valid_sid`.
- No takeover or kill of a terminal process.
- UI copy is Vietnamese; labels exactly: `Dừng`, `Tiếp tục`, `Đổi agent`, `Hủy`, state badge `tạm dừng`, Agents scope `Session này`.
- Messages sent to main start with `[ccmon] `.
- Tauri commands that block run in `spawn_blocking` (existing `blocking` helper); register every command in `main.rs` and mirror DTOs in `gui/ui/src/api.ts`.
- Verification commands: `cargo test --workspace` and `cd gui/ui && yarn build`.
- Release: patch bump in `Cargo.toml`, `gui/src-tauri/Cargo.toml`, `gui/src-tauri/tauri.conf.json`, `Cargo.lock` (`cargo update -w --offline`), and a top `CHANGELOG.md` section.

## Review Focus

1. Two claude processes share one tmux pane (one suspended with Ctrl+Z, as pids 14331/196451 on `%5` today): only the foreground one may be driven → `tmux::foreground_ok` test with a stopped-process pgrp (Task 2).
2. The pane is in copy-mode (user scrolled up): keys would be swallowed → `send_cmds` emits `send-keys -X cancel` first when `pane_in_mode=1` (Task 2).
3. Main is mid-turn (waiting in `Yield`) when Dừng/Hủy is pressed on a managed session: the follow-up message must still be written, not rejected as `Busy` → `send_queued_while_busy` test (Task 3).
4. Prompt contains quotes, backticks, `$()` or newlines: must reach the pane verbatim, never through a shell → tmux text goes through `load-buffer -` on stdin; test `paste_cmds_no_shell` (Task 2) and `swap_msg_keeps_prompt_verbatim` (Task 4).
5. Subagent already finished (or was launched by another subagent / a workflow) when its controls are shown: actions must be refused with a clear message, not sent → `find_call` returns `None` for non-main calls, `subagent_action` rejects `done` agents for pause (Tasks 4, 5).

---

### Task 1: Spike — verify Claude Code behaviour the design depends on

**Files:**
- Modify: `docs/architecture/control.md` (append rows + a "Subagent control" section)

**Interfaces:**
- Produces: confirmed facts used by Tasks 3–5: (a) `stop_task` request shape `{"type":"control_request","request_id":..,"request":{"subtype":"stop_task","task_id":"<agentId>"}}` and that `task_id` for a subagent equals the agent id in the `a:<sid>:<agentId>` key / `agent-<agentId>.jsonl`; (b) what main sees after stop (notification text/status); (c) a stream-json `user` line written while a turn runs is accepted and delivered mid-turn (queued), not dropped; (d) whether a running session (both `-p` and interactive) picks up a new `~/.claude/agents/<name>.md` without restart; (e) `SendMessage` to a stopped agent id resumes it.

- [ ] **Step 1:** In a scratch dir run `claude -p --verbose --input-format stream-json --output-format stream-json --model haiku`, ask it to launch a background `general-purpose` subagent that runs `sleep 60`, then write the `stop_task` control_request with the agent id from the `task_started` system event. Expected: `control_response` success, a `task_notification`/`task_updated` with stopped status.
- [ ] **Step 2:** In the same process, while a turn runs (`sleep 20` in Bash), write a second `user` line. Expected: no error; the text appears to the model in the same turn or the next.
- [ ] **Step 3:** With the process alive, create `~/.claude/agents/ccmon-spike.md`, then ask main to call `Agent(subagent_type="ccmon-spike")`. Record accepted / "agent type not found". Repeat in an interactive `claude` inside tmux. Delete the spike agent file afterwards.
- [ ] **Step 4:** Ask main to `SendMessage` to the stopped agent id. Record whether it resumes.
- [ ] **Step 5:** Write results as table rows in `docs/architecture/control.md`. If (c) fails, stop and report — Task 3's `send_queued` must then wait for `result` instead (escalate to the user). If (d) fails for interactive sessions, Task 7 hides "+ Agent mới" for the tmux channel; if it fails for `-p`, Task 5 restarts an idle managed child before a swap to a new agent.
- [ ] **Step 6:** Commit: `git add docs/architecture/control.md && git commit -m "docs: spike stop_task, mid-turn input, agent reload"`

### Task 2: `control::tmux` — drive a terminal session's pane

**Files:**
- Create: `src/control/tmux.rs`
- Modify: `src/control/mod.rs` (`pub mod tmux;`)

**Interfaces:**
- Produces:
  - `pub fn pane_id(tmux_field: &str) -> Option<String>` — `"cde:@1.%4"` → `"%4"`; `None` without `%<digits>`.
  - `pub fn foreground_ok(stat: &str) -> bool` — parses `/proc/<pid>/stat` text (comm may contain spaces/parens: split after the last `)`), true iff `pgrp == tpgid` and `tty_nr != 0`.
  - `pub enum Block { NotInTmux, PaneGone, NotForeground, Unsupported }` (serde `camelCase`)
  - `pub struct Pane { pub id: String, pub in_mode: bool }`
  - `pub fn check(live: &LiveInfo) -> Result<Pane, Block>` — Linux: `pane_id`, `tmux display-message -p -t <id> '#{pane_tty} #{pane_in_mode}'`, compare `pane_tty` with `readlink /proc/<pid>/fd/0`, then `foreground_ok`. Non-Linux → `Unsupported`.
  - `pub fn send_cmds(p: &Pane, buf: &str) -> Vec<Vec<String>>` — argv lists: optional `["send-keys","-t",id,"-X","cancel"]` when `in_mode`, then `["paste-buffer","-p","-d","-b",buf,"-t",id]`, then `["send-keys","-t",id,"Enter"]`.
  - `pub fn send_text(p: &Pane, text: &str) -> io::Result<()>` — `tmux load-buffer -b <buf> -` with `text` on stdin (buffer name `ccmon-<pid>-<millis>`), then runs `send_cmds`. No shell anywhere.
  - `pub fn send_escape(p: &Pane) -> io::Result<()>` — `send-keys -t <id> Escape`.

- [ ] **Step 1: Write failing tests** in `src/control/tmux.rs`:
  - `pane_id_parses`: `pane_id("cde:@1.%4") == Some("%4")`, `pane_id("main:@0.%12") == Some("%12")`, `pane_id("garbage") == None`.
  - `foreground_ok_matches_tpgid`: stat `"123 (claude) S 1 123 123 34817 123 ..."` → true; same with tpgid `999` → false; `tty_nr` `0` → false; comm `"(my (weird) name)"` parses.
  - `send_cmds_cancels_copy_mode`: `in_mode: true` → first argv is `send-keys -t %4 -X cancel`; `in_mode: false` → 2 argvs, last is `send-keys -t %4 Enter`.
  - `paste_cmds_no_shell`: no argv element equals `sh`/`bash`/`-c`, and the text never appears in any argv (it travels on stdin).
- [ ] **Step 2:** Run `cargo test -p cc-monitor tmux` → FAIL (module missing).
- [ ] **Step 3:** Implement the functions above.
- [ ] **Step 4:** Run `cargo test -p cc-monitor tmux` → PASS.
- [ ] **Step 5:** Commit `feat(control): tmux channel for terminal sessions`.

### Task 3: Runner — `stop_task` and mid-turn sends

**Files:**
- Modify: `src/control/runner.rs` (next to `ManagedSession::interrupt` ~line 235 and `Manager::interrupt` ~line 654; tests near `interrupt_writes_control_request` ~line 1044)

**Interfaces:**
- Produces:
  - `ManagedSession::stop_task(&mut self, task_id: &str) -> io::Result<()>` — request id `ccmon-stop-<pid>-<seq>`.
  - `ManagedSession::send_queued(&mut self, text: &str) -> io::Result<()>` — writes the `user` line whatever the state (Err only if `Exited`); sets `Busy` only if currently `Idle`.
  - `Manager::stop_task(&mut self, sid: &str, task_id: &str) -> Result<(), ControlError>`
  - `Manager::send_queued(&mut self, sid: &str, text: &str) -> Result<(), ControlError>` — managed session only (else `Io("session not managed")`).

- [ ] **Step 1: Failing tests** (reuse the fake-child harness used by `interrupt_writes_control_request`):
  - `stop_task_writes_control_request`: written JSON has `type=control_request`, `request.subtype=stop_task`, `request.task_id="aa18bd04354ab1013"`, request_id starts with `ccmon-stop-`.
  - `send_queued_while_busy`: after `send("a")` (state Busy) `send_queued("b")` is `Ok` and the second user line is written.
- [ ] **Step 2:** `cargo test -p cc-monitor runner` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p cc-monitor runner` → PASS.
- [ ] **Step 5:** Commit `feat(control): stop_task and queued sends for managed sessions`.

### Task 4: `control::subagent` — call lookup, messages, pause markers

**Files:**
- Create: `src/control/subagent.rs`
- Modify: `src/control/mod.rs` (`pub mod subagent;`), `src/control/state.rs` (paused markers)

**Interfaces:**
- Consumes: `Thread::items()`, `Item::Tool(t)` with `t.id`, `t.name`, `t.input`, `t.link: Option<ThreadLink>` (`src/thread.rs`).
- Produces:
  - `#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)] #[serde(rename_all="camelCase")] pub struct SubagentCall { pub agent_id: String, pub tool_use_id: String, pub agent_type: String, pub description: String, pub prompt: String, pub background: bool }`
  - `pub fn find_call(main: &Thread, agent_id: &str) -> Option<SubagentCall>` — the main thread's `Agent`/`Task` tool whose `link == ThreadLink::Agent(agent_id)`; `agent_type` from `subagent_type` (default `"general-purpose"`), `background` from `run_in_background` (default false).
  - `pub enum Channel { Managed, Terminal }`
  - `pub fn pause_msg(c: &SubagentCall, ch: Channel) -> String`, `resume_msg(c) -> String`, `swap_msg(c, new_type: &str) -> String`, `cancel_msg(c, ch) -> String`. Exact copy:
    - pause, Managed: `[ccmon] I paused subagent {agent_type} ({agent_id}). Do not relaunch it or act on its result; wait for my next instruction.`
    - pause, Terminal: `[ccmon] Stop subagent {agent_type} now with TaskStop (task_id: {agent_id}). This is a pause: do not relaunch it or act on its result; wait for my next instruction.`
    - resume: `[ccmon] Resume the paused subagent {agent_type}: use SendMessage to {agent_id} and tell it to continue where it stopped.`
    - swap: `[ccmon] Replace the paused subagent {agent_type} ({agent_id}) with a new Agent call: subagent_type "{new}", description "{description}", run_in_background: {background}, and exactly this prompt, verbatim:\n<prompt>\n{prompt}\n</prompt>\nWhen it finishes, treat its result as the result of the original task.`
    - cancel, Managed: `[ccmon] I cancelled subagent {agent_type} ({agent_id}). Treat that task as failed: handle it and tell me what you will do next.`
    - cancel, Terminal: `[ccmon] Stop subagent {agent_type} now with TaskStop (task_id: {agent_id}). I am cancelling it: treat that task as failed, handle it and tell me what you will do next.`
  - In `state.rs`: `pub struct Paused { pub call: SubagentCall, pub at: i64 }`; `load_paused_in(dir, sid) -> Vec<Paused>`, `save_paused_in(dir, sid, &[Paused]) -> io::Result<()>` at `paused/<sid>.json` (empty list removes the file), plus `load_paused(sid)` / `save_paused(sid, ..)` wrappers on `state_dir()`.

- [ ] **Step 1: Failing tests:**
  - `find_call_from_main_link`: a thread built from fixture lines with an `Agent` tool_use (`subagent_type:"Explore"`, `run_in_background:true`, prompt `"look"`) linked to `aa18`, → `SubagentCall{agent_type:"Explore", background:true, prompt:"look", ..}`; unknown id → `None`.
  - `swap_msg_keeps_prompt_verbatim`: prompt `"a \"b\" `c` $(d)\nline2"` appears byte-identical between `<prompt>\n` and `\n</prompt>`.
  - `messages_have_prefix_and_ids`: every builder output starts with `[ccmon] ` and contains `agent_id`; Terminal pause/cancel contain `TaskStop (task_id: aa18)`.
  - `paused_roundtrip`: save two, load two; save empty → file gone; invalid sid → load empty, save Err.
- [ ] **Step 2:** `cargo test -p cc-monitor subagent paused` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `feat(control): subagent call lookup, control messages, pause markers`.

### Task 5: GUI backend — channel routing and subagent commands

**Files:**
- Modify: `gui/src-tauri/src/cmd_control.rs`, `gui/src-tauri/src/main.rs` (register), `gui/ui/src/api.ts`

**Interfaces:**
- Consumes: Tasks 2–4.
- Produces:
  - `ControlDto` gains `channel: Option<&'static str>` (`"managed"` | `"tmux"`; `None` when not controllable) and `reason` gains `"notInTmux" | "paneGone" | "notForeground" | "unsupported"` (from `tmux::Block`). `control_dto` input gains `tmux: Option<Result<Pane, Block>>` (computed only when `live` exists and not ours). `can_control` true for tmux.
  - `Ctx::send`: when the session is live elsewhere and `tmux::check` is Ok → `tmux::send_text`, return sid. `interrupt` command: managed → existing; tmux → `send_escape`.
  - `#[tauri::command] get_subagent(key: String) -> Result<Option<SubagentDto>, String>` with `SubagentDto { call: SubagentCall, status: String /* running|done|error */, paused: bool, channel: Option<&'static str>, reason: Option<&'static str> }` — `None` when `find_call` fails (nested/workflow agent).
  - `#[tauri::command] subagent_action(key: String, action: String, agent: Option<String>) -> Result<(), String>`; actions:
    - `"pause"`: refuse unless status `running`; managed → `stop_task` then `send_queued(pause_msg(Managed))`; tmux → `send_text(pause_msg(Terminal))`; add `Paused` marker.
    - `"resume"`: requires marker; send `resume_msg`; remove marker.
    - `"swap"`: requires marker and `agent` (valid agent name via `cmd_agents::valid_agent_name` or a catalog id); send `swap_msg`; remove marker. Apply Task 1 (d) outcome for new agents on managed sessions.
    - `"cancel"`: running → stop as in pause with `cancel_msg`; paused → send `cancel_msg(Managed)` (agent already stopped) and remove marker.
    - Managed send uses `Manager::send_queued` if the session is managed, else `Manager::send` (resumes a non-live session as today).
  - `api.ts`: `channel`, new `reason` values, `SubagentCall`, `SubagentDto`, `api.subagent(key)`, `api.subagentAction(key, action, agent?)`.

- [ ] **Step 1: Failing tests** in `cmd_control.rs` tests module:
  - `dto_tmux_channel`: live elsewhere + `tmux: Some(Ok(pane))` → `can_control`, `channel == Some("tmux")`, `reason == None`.
  - `dto_tmux_blocked`: `Some(Err(Block::NotForeground))` → `!can_control`, `reason == Some("notForeground")`.
  - `dto_managed_channel`: own running child → `channel == Some("managed")`.
  - `action_plan_pause_requires_running`: a pure `fn plan_action(action, status, paused, channel) -> Result<Vec<Step>, String>` (`Step::{StopTask, Send(String), MarkPaused, Unmark}`) returns Err for pause on `done`, `[StopTask, Send(..), MarkPaused]` for managed running, `[Send(..), MarkPaused]` for tmux; swap without marker → Err.
- [ ] **Step 2:** `cargo test -p ccmon-gui` (crate name from `gui/src-tauri/Cargo.toml`) → FAIL.
- [ ] **Step 3:** Implement; register `get_subagent`, `subagent_action` in `generate_handler!`; update `api.ts`.
- [ ] **Step 4:** `cargo test --workspace` and `cd gui/ui && yarn build` → PASS.
- [ ] **Step 5:** Commit `feat(gui): terminal channel routing and subagent control commands`.

### Task 6: Composer for terminal sessions

**Files:**
- Modify: `gui/ui/src/Composer.tsx` (`LOCK_REASON` ~line 16, `locked` ~line 252, selects)

**Interfaces:**
- Consumes: `ControlDto.channel`, new reasons (Task 5).

- [ ] **Step 1:** `LOCK_REASON` adds: `notInTmux: 'Session terminal không chạy trong tmux — chỉ xem'`, `paneGone: 'Không tìm thấy pane tmux của session — chỉ xem'`, `notForeground: 'Session đang bị tạm ngưng trong terminal (Ctrl+Z) — chỉ xem'`, `unsupported: 'Hệ điều hành này chưa hỗ trợ gửi vào terminal — chỉ xem'`.
- [ ] **Step 2:** When `channel === 'tmux'`: show a hint line `Gửi vào terminal (tmux <pane>) như bạn gõ`; model/effort selects send `/model <id>` / `/effort <level>` via `api.sendPrompt` instead of `setOverlay`; permission, agent and capability toggles are disabled with title `Chỉ đổi được khi khởi động session`; the stop button calls `api.interrupt` (Escape).
- [ ] **Step 3:** `cd gui/ui && yarn build` → PASS. Manual check with the run skill: a tmux session's composer is unlocked and a prompt appears in its pane.
- [ ] **Step 4:** Commit `feat(ui): prompt terminal sessions through tmux`.

### Task 7: Subagent controls UI

**Files:**
- Create: `gui/ui/src/SubagentControl.tsx`
- Modify: `gui/ui/src/Chat.tsx` (`AgentCard` ~line 145), `gui/ui/src/App.tsx` (thread header when `view.kind === 'agent'`), `gui/ui/src/Agents.tsx` (`export` `AgentForm`), `gui/ui/src/styles.css`

**Interfaces:**
- Consumes: `api.subagent`, `api.subagentAction`, `api.listAgents(sid, cwd, true)`, `AgentForm`.
- Produces: `export function SubagentControl({ agentKey, compact }: { agentKey: string; compact?: boolean })`.

- [ ] **Step 1:** Component loads `api.subagent(agentKey)` (refresh on `store-changed`); renders nothing when `null` or `channel === null` (show `reason` text as title on a disabled group instead).
  - running: buttons `Dừng`, `Hủy`.
  - paused: badge `tạm dừng`, buttons `Tiếp tục`, `Đổi agent`, `Hủy`.
  - done/error: nothing.
  - `Hủy` asks confirm: `Hủy subagent này? Main thread sẽ coi nó là thất bại.`
- [ ] **Step 2:** `Đổi agent` opens a picker: agents from `listAgents` excluding disabled and the current `agentType`, filter box, plus `+ Agent mới` opening `AgentForm`; after save the new name is preselected. Hide `+ Agent mới` when Task 1 found tmux sessions don't reload agents and `channel === 'tmux'`. Confirm button `Chạy lại bằng <name>`.
- [ ] **Step 3:** Mount `<SubagentControl agentKey={link.key} compact />` in `AgentCard` head (when `link.kind === 'agent' && link.key`), and full-size in the App thread header for agent threads.
- [ ] **Step 4:** `yarn build` → PASS. Manual: in a managed session start a background subagent, press Dừng → badge `tạm dừng` and a `[ccmon]` prompt appears in main; Đổi agent → main relaunches with the chosen type.
- [ ] **Step 5:** Commit `feat(ui): pause, resume, swap and cancel subagents`.

### Task 8: Agents page — `Session này` scope

**Files:**
- Modify: `gui/ui/src/Agents.tsx` (`Agents` ~line 341, `History` ~line 57)

**Interfaces:**
- Consumes: `api.sessionUsage(sid)` (`Usage{kind:'agent', state:'used', count, last}`), `RunRecord.sessionId`.

- [ ] **Step 1:** Scope toggle `Session này | Dự án` in the bar; default `Session này` when `sid` is set, hidden when `sid` is null.
- [ ] **Step 2:** In session scope, list only agents whose usage is `used` for this session (sorted by `last` desc, count shown as `×n`), include `inventory_only` items already returned by `listAgents`; `History` receives `sid` and filters `r.sessionId === sid`. Empty state: `Session này chưa dùng agent nào.` Workflows tab filtered the same way.
- [ ] **Step 3:** `yarn build` → PASS; manual: plan-atlas session shows only its agents.
- [ ] **Step 4:** Commit `feat(ui): agents scoped to the current session`.

### Task 9: File focus in chat

**Files:**
- Modify: `gui/src-tauri/src/assets.rs` (`TouchedFile` ~line 199, `collect` ~line 302), `gui/ui/src/api.ts`, `gui/ui/src/Sidebar.tsx`, `gui/ui/src/App.tsx`, `gui/ui/src/Tools.tsx` (`ToolCard` root), `gui/ui/src/Chat.tsx` (`AgentCard` root), `gui/ui/src/styles.css`

**Interfaces:**
- Produces:
  - Rust `#[derive(Serialize, Clone)] #[serde(rename_all="camelCase")] pub struct FileHit { pub thread_key: String, pub tool_id: String, pub idx: usize, pub ts: Option<i64>, pub op: String }`; `TouchedFile.hits: Vec<FileHit>` sorted by `ts` ascending (`idx` = index in `thread.items()`, matching `ItemDto.idx`). Patch tool hits included.
  - TS `FileHit`, `TouchedFile.hits`; `Sidebar` prop `onFocus: (hit: FileHit) => void`; App `focusTool(hit)`.
  - DOM: tool cards carry `data-tool-id={tool.id}`.

- [ ] **Step 1: Failing test** in `assets.rs`: `touched_file_hits_in_order` — two Reads and one Edit of `/a.rs` across main + one agent thread → 3 hits, ascending `ts`, correct `thread_key`, `tool_id`, `idx`, `op`.
- [ ] **Step 2:** `cargo test -p ccmon-gui touched_file_hits` → FAIL; implement; → PASS.
- [ ] **Step 3:** Sidebar file row gets a `⌖` button (`title="Tìm trong chat"`, stops propagation) with a per-file cursor; clicking cycles hits newest-first and shows `i/n` next to the button.
- [ ] **Step 4:** App `focusTool(hit)`: select `hit.threadKey` if different; set `tail` to `max(tail, total - hit.idx + 20)`; after the view containing `hit.idx` renders, turn off follow (`stickBottom.current = false`), `scrollIntoView({block:'center'})` the `[data-tool-id]` element, add class `flash` for 1.5 s.
- [ ] **Step 5:** `yarn build` → PASS; manual: focus an old Read in a long thread scrolls to it.
- [ ] **Step 6:** Commit `feat(ui): focus files in chat`.

### Task 10: Docs and release

**Files:**
- Modify: `docs/architecture/capabilities.md` (GUI wiring + control sections: tmux channel, subagent commands), `README.md`/`README.vi.md` feature list if they list composer limits, `website/docs.html` user-guide section on the composer, `Cargo.toml`, `gui/src-tauri/Cargo.toml`, `gui/src-tauri/tauri.conf.json`, `Cargo.lock`, `CHANGELOG.md`

- [ ] **Step 1:** `git fetch && git rebase origin/main`; bump patch version in the 3 manifests, `cargo update -w --offline`.
- [ ] **Step 2:** `CHANGELOG.md` top section `## [x.y.z] - 2026-11-27` with Added: prompt terminal sessions via tmux; Dừng/Tiếp tục/Đổi agent/Hủy for subagents; Agents `Session này` scope; file focus in chat. Add compare link.
- [ ] **Step 3:** Update docs listed above.
- [ ] **Step 4:** `cargo test --workspace && (cd gui/ui && yarn build)` → PASS.
- [ ] **Step 5:** Commit `release x.y.z: terminal control, subagent swap, session agents, file focus`.
