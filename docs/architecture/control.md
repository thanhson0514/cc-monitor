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
| disable MCP server | `--disallowedTools "mcp__<normalized>"` | OK (server-level prefix) | `mcp__codegraph mcp__claude_ai_Claude_Docs` -> init mcp tools 12 -> 3 (only docs-langchain left). Tools are removed from the list entirely. `<normalized>` = `catalog::mcp::normalize(id)` (`claude.ai Claude Docs` -> `claude_ai_Claude_Docs`). Fallback (`--mcp-config --strict-mcp-config`) NOT needed |
| disable agent | `--disallowedTools "Agent(a)"` | OK, incl. colon names | `Agent(Explore)`: `<tr>` `Agent type 'Explore' has been denied by permission rule 'Agent(Explore)' from cliArg.`; same for `Agent(impeccable:impeccable-documenter)`. Init still lists the agent |
| disable all workflows | `--disallowedTools "Workflow"` | OK | `Workflow` tool disappears from init tools |
| disable one workflow | `--disallowedTools "Workflow(name)"` | OK (per-workflow form exists) | `Workflow(deep-research)` -> `<tr>` `Workflow deep-research blocked by permission rules`; with `Workflow(other)` the same call is not blocked (`Review dynamic workflow before running`). Tool stays listed |
| resume + stream-json input | `--resume <sid>` + stdin JSON line | OK | session_id unchanged (`ef296c24...`, no fork); `{"type":"user","message":{"role":"user","content":[{"type":"text","text":"say ok"}]}}` -> `assistant` + `result success` |
| interrupt | stdin `{"type":"control_request","request_id":"..","request":{"subtype":"interrupt"}}` | OK | immediate `control_response {subtype:success, request_id, response:{still_queued:[]}}`, then `result error_during_execution`; process stays alive and next user message works (`result success`) |

## Facts Task 9 needs
- `system/init` is emitted **per turn, after the user message arrives** (not at process start). With no input only `system/hook_started|hook_response` events appear. For a new session (no `--resume`) take the sid from the `session_id` field of the first stdout event of any kind.
- Interrupt: send the control_request; expect `control_response` then a `result` with `subtype: error_during_execution` for the aborted turn; keep the process and send the next user line. Match on `request_id`.
- `--disallowedTools` is variadic: `build_args` emits it last, one rule per args element, no quoting.
- Deny rules block at call time; skills/agents remain in init lists. UI should not rely on init to show "disabled".
- Plugin disabling needs the settings file: `state_dir/launch/<sid>.settings.json` (`new-<millis>` for new sessions).
- Rules with `Skill(...)`/`Agent(...)` accept `:` in names; MCP prefixes must use the normalized name.

## Limitations
- None blocking. Per-workflow disable works (`Workflow(name)`), so no fallback is used. Master switch `workflows_off` emits bare `Workflow` and supersedes per-workflow rules.
- `--effort` effect is not observable in init; only the value validation was verified.
