# Terminal control, session-scoped agents, file focus, subagent stop/swap

Date: 2026-11-27. Status: approved.

## Intent

ccmon monitors and drives Claude Code itself. A session must behave the same whether it was
started in a terminal or by ccmon: what you do in one place is visible and honoured in the
other. Today terminal sessions are view-only, the Agents page mixes agents from other sessions,
files in the side panel can't be traced back to the chat, and a running subagent can't be
stopped or replaced from the GUI.

## Goals

1. Prompt a session that is live in a terminal, from ccmon.
2. Agents page: show the agents used by the current session.
3. File panel: a focus button that jumps to the chat tool calls that read/edited the file.
4. Subagent control: **Dừng** (pause), then **Tiếp tục** (resume) or **Đổi agent** (swap);
   **Hủy** (cancel, treated as failed).

## Non-goals / anti-goals

- No takeover of a terminal process; no killing it.
- Never edit Claude Code's files or settings to achieve control.
- Input that Claude treats as coming from another session (peer origin) instead of the user is a
  failure: ccmon input must be human-origin on both sides.

## Approaches considered (item 1)

| Approach | Verdict |
|---|---|
| Peer messaging socket (`messagingSocketPath` in `~/.claude/sessions/<pid>.json`) | Rejected: Claude Code classifies socket input as `kind:"peer"`, not `kind:"human"`; Claude may refuse instructions. |
| Remote Control bridge | Rejected: goes through the cloud. |
| **tmux paste into the session's pane** (`tmux` field, e.g. `cde:@1.%4`) | **Chosen**: identical to typing; shows in the terminal, human-origin, written to the transcript ccmon already tails. |

Item 4 approach chosen by the user: **A** — the main thread is told and relaunches itself
(not ccmon running the replacement agent out of band).

## Design

### 1. Terminal input via tmux

- Unlock the composer for a live terminal session when its `tmux` pane exists and the pane's
  foreground process is that session's pid (or its descendant); otherwise stay view-only and
  show why (not in tmux / pane gone / pane runs another process).
- Send: `tmux load-buffer` + `paste-buffer -p` (bracketed) into the pane, then `send-keys Enter`.
  Interrupt: `send-keys Escape`. Model/effort changes: type `/model <id>` / `/effort <level>`.
- Permission mode and main agent are launch flags: locked for terminal sessions.
- Known risk: text the user is typing in that pane at the same moment gets merged.

### 2. Agents scoped to the session

- New scope "Session này" next to the project scope; default when the Agents page is opened
  from a thread. Lists agents the session invoked (`usage::session_usage`, main + subagent
  threads) and run history filtered to that session id. Project scope unchanged.

### 3. File focus

- Each file row in the panel gets a focus button: scroll the chat to the first tool call that
  touched the file (Read/Edit/Write/MultiEdit/NotebookEdit, plus paths in Bash where the panel
  already attributes them), highlight it; repeated clicks cycle with an "i/n" counter.

### 4. Subagent stop / swap / cancel

Controls in the subagent view header and on the subagent card in the main thread.

- **Dừng**: stop the task, then send the main thread: "The user paused subagent X (id). Do not
  relaunch it or act on its result; wait for instructions." Card shows *tạm dừng* with:
  - **Tiếp tục**: ask main to resume that agent by id (SendMessage to the agent id), same context.
  - **Đổi agent**: pick an existing agent or create one (existing agent editor). Ask main to
    relaunch Agent with `subagent_type=Y`, the original prompt verbatim (ccmon reads it from the
    transcript's `tool_use` input), same `description` and `run_in_background`.
  - **Hủy**.
- **Hủy** (also while running): stop the task, then tell main: "The user cancelled subagent X —
  treat it as failed, handle it and report back."
- Stop mechanism: ccmon-managed session → stream-json `control_request {subtype:"stop_task"}`
  (field names verified by spike); terminal session → typed prompt asking main to call TaskStop
  on the task id. All messages to main are human-origin (stream-json user message / tmux).
- "Dừng" is not a freeze: Claude Code has no pause; the agent is stopped and later resumed from
  its transcript.
- New agents: ccmon-managed sessions restart the child (`--resume`) before the relaunch so the
  new agent file is loaded. For terminal sessions, verify whether a new agent file is picked up
  live; if not, only existing agents are offered there.

## Left to the builder

- Exact wording of messages to main (keep short, include task id, agent name, verbatim prompt).
- Where the pause state lives (derived from transcript + ccmon-side marker).
- `stop_task` request shape (spike against Claude Code 2.1.326, add a row to
  `docs/architecture/control.md`).
- Release: bump version (4 files) and add a CHANGELOG section.
