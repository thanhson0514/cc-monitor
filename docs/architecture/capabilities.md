# Capability architecture and extension guide

How ccmon models "what Claude Code can use" (skills, plugins, MCP servers, agents,
workflows), how it controls a session, and how to add a new kind. This describes the code
as built; the verified CLI flag table lives in [control.md](control.md) and is not repeated here.

## Three layers

```
  ~/.claude (transcripts, skills, agents, ...)         ccmon state dir (~/.config/ccmon)
        | read-only                                        ^ atomic writes, ccmon-owned only
        v                                                  |
 +-------------- OBSERVE ---------------+    +------------ CONTROL --------------+
 | event.rs  parse_line -> Kind::*      |    | Overlay (per session) + Registry  |
 |   TaskNotice, Inventory, Link, ...   |    | build_args(overlay) -> LaunchSpec |
 | thread.rs / store.rs  Thread,        |    | runner.rs Manager / ManagedSession|
 |   inventory(), tool_uses()           |    |   long-lived `claude -p` child    |
 | bgtask.rs  background-task Registry  |    +-----------------^-----------------+
 +------------------+-------------------+                      | overlay ids
                    | threads                                  |
                    v                                          |
 +-------------------------------- CATALOG ---------------------+-----------+
 | Provider::scan(&ScanCtx) -> Vec<Capability>   (one per CapKind)           |
 | Catalog::scan = providers() + registry post-step (enabled_globally)       |
 | usage.rs   session_usage / inventory_only   (available vs. used)          |
 | history.rs agent_history / workflow_history / run_stats                   |
 +-------------------------------+-------------------------------------------+
                                 v
   gui/src-tauri: cmd_catalog.rs, cmd_control.rs, cmd_agents.rs  ->  gui/ui tabs
```

- **Observe** never writes. Transcript lines become `Kind::TaskNotice` (a `<task-notification>`
  from a background task) and `Kind::Inventory` (`skill_listing`, `mcp_instructions_delta`,
  `agent_listing_delta`, `deferred_tools_delta`: what the session announced as available).
  Unknown attachments stay `Kind::Link`; parsing never panics.
- **Catalog** answers "what exists" (disk) and "what was available / used" (session).
- **Control** answers "what should the next turn be allowed to use". It only produces CLI
  flags and ccmon-owned state; it never edits `settings.json`, skill, plugin or MCP files.

## Catalog contract (`src/catalog/mod.rs`)

`Capability { kind: CapKind, id, name, description, scope: Scope, path, enabled_globally, meta }`.
`CapKind` = `Skill | Plugin | Mcp | Agent | Workflow`. `Scope` = `User | Project | Plugin(id) | BuiltIn | Managed`.

A `Provider` (`fn kind()`, `fn scan(&self, &ScanCtx) -> Vec<Capability>`) must:

1. **Stable ids.** `id` is the key used by overlays, usage matching and the UI: skill `name` or
   `plugin:name`, plugin `name@marketplace`, MCP server name, agent name, workflow file stem.
   Never derive it from a path or index.
2. **No IO outside `ScanCtx`.** Read only `claude_home`, `claude_json`, `cwd` (project files)
   and plugin install paths found through them. ccmon state arrives as `ctx.registry`; do not
   call `state::load_*` from a provider. Read-only, no network.
3. **Never panic.** Missing or malformed files yield fewer items, not errors (see `frontmatter.rs`).
4. Put kind-specific data in `meta` (tools, model, version, transport...), not new struct fields.

`Catalog::scan` flat-maps `providers()` and then applies the registry: for agents and workflows
`enabled_globally = !registry.disabled_*.contains(id)`. Disabling keeps the file and history.

## Usage (`src/catalog/usage.rs`)

`session_usage(threads, catalog)` yields `Used{count,last} | Available | Unavailable` per
`(CapKind, id)`. `Used` comes from `tool_use` blocks of the main thread and every subagent or
workflow thread (`target()`: `Skill{skill}`, `Agent`/`Task{subagent_type}`, `Workflow{scriptPath|script}`,
`mcp__<server>__*`). `Available` means the session announced it (`Inventory`), or it is on disk and
the session announced nothing of that kind. `inventory_only` returns announced or used items with
no file: built-in agents/skills (`Scope::BuiltIn`), claude.ai MCP and ad-hoc workflows (`Scope::Managed`).

Normalize rules: MCP ids are compared as tool prefixes via `catalog::mcp::normalize` (chars outside
`[A-Za-z0-9_-]` become `_`, hyphens kept); workflow ids are the `scriptPath` stem, else `meta.name`
scanned from an inline `script`, else `"workflow"`. Everything else compares verbatim.

History (`history.rs`) reads subagent and workflow run directories under `projects/` by `stat`
and tail reads, filtered by `since_days`; it is returned regardless of registry state.

## Control (`src/control/`)

- `Overlay` (per session): `model`, `effort`, `permission_mode`, `main_agent`, `disabled_skills`,
  `disabled_plugins`, `disabled_mcp`, `disabled_agents`, `disabled_workflows`, and the user-set master
  switch `workflows_off` (never derived; emits bare `Workflow`, superseding per-workflow rules).
- `Registry` (ccmon-wide): `disabled_agents`, `disabled_workflows`, `notes`. `state::effective(overlay,
  registry)` merges it into the overlay before `build_args`.
- `build_args(sid, cwd, overlay, state_dir) -> LaunchSpec` is the single overlay-to-flags mapping.
  `--disallowedTools` is variadic, so it is emitted last, one rule per element. Disabled plugins go
  through `--settings <state_dir>/launch/<sid|new-millis>.settings.json`.
- `runner.rs`: `ManagedSession` is one long-lived `claude -p` child; `Manager` holds them by sid.
  A changed overlay restarts the child between turns (`--resume`); during a turn it is `Err(Busy)`.
  `stop_task(sid, task_id)` and `send_queued(sid, text)` forward to the child (stop a background/subagent
  task, queue a message). A session live in another terminal (`discovery::read_live`, pid not ours) is
  `SessionLiveElsewhere`: ccmon never takes it over, but if it runs in tmux it can be driven by paste
  through `control::tmux`; otherwise it is view only.
- `tmux.rs` (`control::tmux`): the terminal channel. `check` finds the tmux pane of the live pid and
  refuses (with a user-facing reason) when there is no pane or the process is suspended (Ctrl+Z). Prompts
  are pasted into the pane, `/model` and `/effort` are sent for chips, and interrupt sends Esc. Before a
  paste, `ensure_no_dialog` captures the pane and refuses while a permission/selection dialog is open
  (pure `dialog_pending`), since the Enter would answer it. Permission mode, main agent and capabilities
  are launch settings: fixed for terminal sessions (`set_overlay` refuses while the session is live elsewhere).
- `subagent.rs` (`control::subagent`): subagent control. Pause, resume, swap and cancel are turned into
  steps (stop the task, send a follow-up to main). Resume relaunches the same agent against the stopped
  transcript, since Claude Code cannot resume a stopped subagent. Paused state is a marker
  `paused/<sid>.json` in the ccmon state dir (`state::{load,save}_paused`); `stopped` records that ccmon
  stopped the task itself (`stop_task`). On a terminal session Dừng/Hủy only ask Claude to stop the
  subagent (TaskStop) and ccmon cannot confirm it; a foreground subagent is interrupted with Esc first.
  Resume and swap are refused while the agent still runs (UI: "đang chờ dừng").
- **Lock-free start of a new session:** `Manager::start_new(&self, ...)` spawns and sends the first
  message; then, with the Manager lock released, `ManagedSession::wait_session_id(timeout)`; then
  `Manager::adopt(sid, session, overlay)`. Avoid `Manager::send(None, ...)` under a shared lock
  (it blocks up to 30 s).
- **Callback rule:** the `OnChange(sid, &RunState)` callback runs on the child's stdout reader thread
  (and on the caller for `Busy`/`Restarting`). It must not lock the `Manager`; only forward or emit.
  `sid` is `""` until a new session's id is known.

### ccmon state conventions (`src/control/state.rs`)

- Location: `state_dir()` = parent of `config::config_path()` (`~/.config/ccmon/`). Files:
  `overlays/<sid>.json`, `defaults.json` (overlay for new sessions), `registry.json`, `paused/<sid>.json`, `launch/*.settings.json`.
- All writes go through `write_atomic` (temp file in the same dir, then rename). Reads are tolerant:
  missing or corrupt files fall back to defaults. `valid_sid` (`[A-Za-z0-9-]`) guards every path built from a sid.
- ccmon writes only its own state, plus user agent/workflow files the user explicitly saves from the
  Agents page (`save_agent`, which refuses to overwrite without confirmation). It never deletes or edits
  anything else under `~/.claude`.

## Background tasks

`bgtask::Registry` is filled per `Thread`: `launch` from a `Bash` result carrying `backgroundTaskId`
(and Monitor/Agent/Workflow equivalents), `notice` from `Kind::TaskNotice` (match by task id, then
launching `tool_use` id), `finalize_unknown` for tasks that never reported. `pending_at(ts)` gives the UI
what a `Yield {}` is waiting on.

## GUI wiring (`gui/src-tauri/src/`)

- `cmd_catalog.rs`: `scan_ctx` (loads the registry), `catalog_for(cwd)` (5 s cache; call
  `invalidate_catalog()` after ccmon changes a file or the registry), `listing`, `get_catalog`, `get_session_usage`.
  The session cwd comes from the store, never from the webview.
- `cmd_control.rs`: `get_control`, `set_overlay`, `send_prompt`, `interrupt`, `stop_managed`, `list_projects`.
  `Ctx::route(sid)` picks the channel: ccmon's own child (`managed`), not live (`Manager::send` resumes
  it), or live in a terminal (`tmux::check` right before each send/Escape; a failed check is the `reason`).
- `cmd_subagent.rs`: `get_subagent(key)`, `subagent_action(key, action, agent)` (pause/resume/swap/cancel).
  Pure `plan_action` returns the steps; markers in `state::{load,save}_paused`. Swap targets come from the
  session's agent listing (`cmd_agents::session_agents`, matched by id: plugin agents are `plugin:name`);
  a user/project agent file newer than ccmon's idle `-p` child stops it so the next send relaunches with
  `--resume`, which also stops the session's other background tasks. `subagent_swap_check(key, agent)` is
  the dry run behind the picker's restart warning. One action per session at a time (`InFlight`).
  `get_subagent` reads the status with `Store::agent_status_of` (no tree build) and skips the tmux check
  for finished, unpaused agents; thread links carry `paused` so cards mount controls only when needed.
- `cmd_agents.rs`: `list_agents/workflows`, `*_history`, `set_agent_disabled`, `set_workflow_disabled`, `save_agent`.
- Every command is registered in `main.rs` (`generate_handler!`); mirror DTOs in `gui/ui/src/api.ts`.

## Checklist: add a new capability kind (example: "hooks")

1. `src/catalog/hooks.rs`: `pub struct HookProvider; impl Provider` honoring the contract above;
   `pub mod hooks;` and `Box::new(hooks::HookProvider)` in `providers()` (`src/catalog/mod.rs`).
2. `CapKind::Hook` in `src/catalog/mod.rs` (serializes as `"hook"`). Fix any exhaustive `match` it breaks.
3. Usage (`src/catalog/usage.rs`): map the observable call in `target()`; add the kind to the loop in
   `session_usage` and, if the session announces it, to `inventory_names` plus `Kind::Inventory` /
   `InventoryDelta` in `src/event.rs` and `Thread::merge_inventory`. Add the kind to `inventory_only`
   if it can be announced without a file. Add `key()` normalization if names are mangled.
4. Overlay mapping: a `disabled_hooks: BTreeSet<String>` in `Overlay` (`src/control/mod.rs`, serde default
   keeps old files loadable). If it should be ccmon-wide, also in `Registry`, `state::effective`, and the
   post-scan step in `Catalog::scan`.
5. CLI flag: emit the rule or settings entry in `build_args_with` (`src/control/mod.rs`). Verify the flag
   with a spike first and add a row to [control.md](control.md). Add a `build_args` unit test.
6. GUI backend: DTO fields in `cmd_catalog.rs` (`CapabilityDto`, `usage_dto`); a `set_*_disabled`
   command in `cmd_agents.rs` if registry-backed; register it in `main.rs`.
7. UI (`gui/ui/src/`): `CapKind` union, `DISABLED_FIELD` and the overlay type in `api.ts`; tab lists in
   `Library.tsx` (`TABS`) and `Composer.tsx` (`CAP_TABS`).
8. Tests: provider fixture in a tempdir (use `catalog::write`), usage case, `build_args` case, registry case.

## Future work (not implemented)

- **Apply globally:** a button that writes a disabled item into `settings.json` (today toggles live only in
  ccmon state and launch flags).
- **Plugin marketplace browser:** discover and install plugins from marketplaces.
- **Workflow script editing:** create and edit `workflows/*.js` from the Agents page (agents are editable today).
- **Permission approval via `--permission-prompt-tool`:** answer tool-permission prompts from the GUI
  instead of relying on `--permission-mode`.
