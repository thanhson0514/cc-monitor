# Managed-session control: CLI flag spike (Claude Code 2.1.326)

Managed sessions run `claude -p --verbose --input-format stream-json --output-format stream-json [--resume <sid>] [flags]`.
Spike run in an empty scratch dir with `--model haiku`, evidence taken mainly from the `system/init` event
(`tools`, `mcp_servers`, `skills`, `agents`, `plugins`, `permissionMode`, `model`) and from `tool_result` denials.
`<tr>` below means the tool_result text. ~12 claude invocations.

## Verified mapping (overlay -> CLI)

| Overlay | Flag | Verdict | Evidence |
|---|---|---|---|
| model | `--model <id>` | OK | `--model sonnet` -> init `model: claude-sonnet-5-5` |
| effort | `--effort <level>` | OK (accepted; not shown in init) | no warning for `low`; bogus value prints `Warning: Unknown --effort value 'bogus' ... Valid values: low, medium, high, xhigh, max.` and falls back |
| permission mode | `--permission-mode <m>` | OK | `--permission-mode plan` -> init `permissionMode: plan` |
| main agent | `--agent <name>` | OK | `--agent Explore` -> init tool list switches to Explore's tool set (no Edit/Write/Task) |
| disable skill | `--disallowedTools "Skill(x)"` | OK, incl. colon ids | `Skill(superpowers:brainstorming)` -> `<tr>` `Skill execution blocked by permission rules`; `find-skills` (not listed) still `Launching skill`. Init still lists the skill (deny is at call time, not listing) |
| disable plugin | `--settings <file>` `{"enabledPlugins":{"id":false}}` | OK | `superpowers@claude-plugins-official`: init skills 64 -> 49, plugin gone from `plugins`, `superpowers:*` gone |
| disable MCP server | `--disallowedTools "mcp__<normalized>"` | OK (server-level prefix) | `mcp__codegraph mcp__claude_ai_Claude_Docs` -> init mcp tools 12 -> 3 (only docs-langchain left). Tools are removed from the list entirely. `<normalized>` = `catalog::mcp::normalize(id)`: chars outside `[A-Za-z0-9_-]` become `_`, hyphens are kept (`claude.ai Claude Docs` -> `claude_ai_Claude_Docs`, `docs-langchain` -> `docs-langchain`, `plugin:chrome-devtools-mcp:chrome-devtools` -> `plugin_chrome-devtools-mcp_chrome-devtools`). Fallback (`--mcp-config --strict-mcp-config`) NOT needed |
| disable agent | `--disallowedTools "Agent(a)"` | OK, incl. colon names | `Agent(Explore)`: `<tr>` `Agent type 'Explore' has been denied by permission rule 'Agent(Explore)' from cliArg.`; same for `Agent(impeccable:impeccable-documenter)`. Init still lists the agent |
| disable all workflows | `--disallowedTools "Workflow"` | OK | `Workflow` tool disappears from init tools |
| disable one workflow | `--disallowedTools "Workflow(name)"` | OK (per-workflow form exists) | `Workflow(deep-research)` -> `<tr>` `Workflow deep-research blocked by permission rules`; with `Workflow(other)` the same call is not blocked (`Review dynamic workflow before running`). Tool stays listed |
| resume + stream-json input | `--resume <sid>` + stdin JSON line | OK | session_id unchanged (`ef296c24...`, no fork); `{"type":"user","message":{"role":"user","content":[{"type":"text","text":"say ok"}]}}` -> `assistant` + `result success` |
| interrupt | stdin `{"type":"control_request","request_id":"..","request":{"subtype":"interrupt"}}` | OK | immediate `control_response {subtype:success, request_id, response:{still_queued:[]}}`, then `result error_during_execution`; process stays alive and next user message works (`result success`) |
| stop subagent | stdin `{"type":"control_request","request_id":"..","request":{"subtype":"stop_task","task_id":"<agentId>"}}` | OK | immediate `control_response {subtype:success, request_id, response:{}}`; `task_id` = the `task_id` of the `task_started` event = `agentId` in the Agent tool_result = `agent-<id>.jsonl` (see Subagent control) |
| mid-turn user line | stdin `user` JSON line written while a turn runs (Bash `sleep 20`) | OK (queued, not dropped) | line written at t=18.9s during the sleep; no error/event at write time; after the tool_result the model answered `first done\n\nMIDTURN-ACK` in the same turn and a single `result success` followed |
| new `~/.claude/agents/<n>.md` while running (`-p`) | create file after process start, then `Agent(subagent_type=<n>)` | NOT picked up | tool_result (is_error) `Agent type 'ccmon-spike' not found. Available agents: claude, Explore, general-purpose, ...` |
| new `~/.claude/agents/<n>.md` while running (interactive) | same, inside `claude` in tmux | OK (hot reload) | agent launched with no restart and returned `SPIKE`. Note `~/.claude/agents/` did not exist beforehand and was created after the session started; still picked up |
| stop foreground subagent | `stop_task` with the `task_id` of `task_started` (`is_backgrounded:false`), sent 6 s after the start (spike 2b, 2026-11-27) | OK | `task_updated {status:killed}`, `task_notification {status:stopped}`, then `control_response success`; the agent's own child Bash task is killed too. The main-thread `Agent` tool_result becomes `"[Request interrupted by user for tool use]"` (`is_error`, `non_execution_kind:"interrupted"`). Main's turn is not aborted: the model answered and the turn ended with `result success`; the next user line worked. So `stop_task` is used for foreground agents too, no `interrupt` needed |
| multi-line paste into an interactive `claude` (tmux) | `load-buffer` + `paste-buffer -p` (no `-r`) of a 3-line text, then `send-keys Enter` (Task 2 fix round) | OK | tmux turns LF into CR when pasting without `-r`; inside the bracketed paste Claude Code kept the lines in one prompt, which was submitted once by the Enter |
| SendMessage to stopped subagent | `SendMessage{to:<agentId>}` after `stop_task` | NOT resumed | tool_result `{"success":false,"message":"Agent \"<id>\" was stopped and was not resumed. Treat its work as cancelled; only start a new agent for it if the user explicitly asks."}` |

## Facts Task 9 needs
- `system/init` is emitted **per turn, after the user message arrives** (not at process start). With no input only `system/hook_started|hook_response` events appear. For a new session (no `--resume`) take the sid from the `session_id` field of the first stdout event of any kind.
- Interrupt: send the control_request; expect `control_response` then a `result` with `subtype: error_during_execution` for the aborted turn; keep the process and send the next user line. Match on `request_id`.
- `--disallowedTools` is variadic: `build_args` emits it last, one rule per args element, no quoting.
- Deny rules block at call time; skills/agents remain in init lists. UI should not rely on init to show "disabled".
- Plugin disabling needs the settings file: `state_dir/launch/<sid>.settings.json` (`new-<millis>` for new sessions).
- Rules with `Skill(...)`/`Agent(...)` accept `:` in names; MCP prefixes must use the normalized name.

## Settings layering
`--settings <file>` is layered on top of the user's settings: in the spike only `superpowers` was turned off and every other plugin stayed enabled. The file therefore only needs the `enabledPlugins` entries to disable. Cleanup of leftover `launch/*.settings.json` files belongs to Task 9.

## Binary lookup
`find_claude()` searches PATH, `~/.local/bin`, `~/.claude/local` (on Windows `claude.exe` / `claude.cmd`; on unix the file must be executable). `build_args` returns `io::ErrorKind::NotFound` when nothing is found.

## Limitations
- None blocking. Per-workflow disable works (`Workflow(name)`), so no fallback is used. Master switch `workflows_off` emits bare `Workflow` and supersedes per-workflow rules.
- `--effort` effect is not observable in init; only the value validation was verified.

## Subagent control (spike 2, Claude Code 2.1.326, `-p` stream-json, `--model haiku`)
Setup: ask main to launch `Agent(subagent_type=general-purpose, run_in_background=true)` running `sleep 60`, then send `stop_task`.

Events in order after the Agent tool_use (all on stdout, session_id omitted below):
```
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a8b144350a2b52866","run_id":"0mwymlxeh-4c324838","task_type":"local_agent","subagent_type":"general-purpose","description":"Sleep 60 then report done"}]}
{"type":"system","subtype":"task_started","task_id":"a8b144350a2b52866","run_id":"..","tool_use_id":"toolu_01MA..","description":"..","subagent_type":"general-purpose","is_backgrounded":true,"spawn_depth":1,"task_type":"local_agent","prompt":".."}
>> {"type":"control_request","request_id":"stop1","request":{"subtype":"stop_task","task_id":"a8b144350a2b52866"}}
{"type":"system","subtype":"task_updated","task_id":"a8b..","run_id":"..","patch":{"status":"killed","end_time":1795762469505}}
{"type":"system","subtype":"task_notification","task_id":"a8b..","run_id":"..","tool_use_id":"toolu_01MA..","status":"stopped","output_file":"/tmp/claude-1000/<mangled-cwd>/<sid>/tasks/a8b144350a2b52866.output","summary":"Sleep 60 then report done"}
{"type":"system","subtype":"background_tasks_changed","tasks":[]}
{"type":"control_response","response":{"subtype":"success","request_id":"stop1","response":{}}}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]},"parent_tool_use_id":"toolu_01MA..","agent_id":"a8b144350a2b52866","subagent_type":"general-purpose",...}
```
- Task status vocabulary seen: `task_updated.patch.status = "killed"`, `task_notification.status = "stopped"` (a completed Bash task gives `"completed"`). Bash commands also get `task_started` with `task_type:"local_bash"` and a short id (`bz28aqcxt`): filter on `task_type == "local_agent"` (or presence of `subagent_type`).
- `task_id` for a subagent equals the `agentId` in the Agent tool_result (`agentId: a8b144350a2b52866 ... Use SendMessage with to: '<id>'`) and the `agent_id` on that subagent's own stream lines. Match `tool_use_id` back to the main-thread Agent tool_use.
- Subagent transcript: `~/.claude/projects/<mangled-cwd>/<sid>/subagents/agent-<agentId>.jsonl` (verified: `.../34a6a8ef-.../subagents/agent-a8b144350a2b52866.jsonl`). So `task_id == <agentId>` of that filename.
- What main sees after the stop: the `Agent` tool_result stays the launch text ("Async agent launched successfully ... agentId: <id> ..."), plus an injected status notification for `stopped`. On the next turn haiku said "a status notification says this agent was stopped, not completed" / "reported as stopped ... with a note that the user stopped it". The stop does not abort the main turn: the turn that launched the agent ended normally with `result success` (no `error_during_execution`).
- `stop_task` with a valid id only succeeds while the task is running; the stopped agent cannot be resumed with `SendMessage` (see table), so a "swap" must start a new agent, not message the old one.
- Caveat: the stop was issued ~0.1s after `task_started`, so the subagent had barely begun. Behaviour for a long-running subagent was not separately measured.
- Foreground agent (spike 2b, same setup with `run_in_background:false`, `--allowedTools=Bash`): `task_started` is emitted with `is_backgrounded:false` and the same id rules. `stop_task` stopped it (see table): the parent `Agent` tool_result is the interrupt error, so the tree row turns `error` at once. In a tmux session there is no `stop_task`; ccmon sends Esc first, which interrupts main's turn and the foreground agent inside it, then pastes the message.
- Risk on the tmux channel: the Enter after a paste answers any open permission or selection dialog. `tmux::send_text` (and the Esc step of a subagent action) first runs `tmux capture-pane -p` and refuses while the last 15 lines show a dialog marker (`Do you want to proceed?`, `❯ 1. Yes`, `Enter to select`, ...). Text the user is typing in the pane at the same moment still gets mixed with ccmon's.
