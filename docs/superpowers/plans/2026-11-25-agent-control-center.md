# Agent Control Center — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Biến ccmon từ trình xem transcript thành trung tâm điều khiển Claude Code: (1) hiển thị chi tiết tác vụ nền (Bash nền, Monitor, Yield, TaskOutput…), (2) quản lý skill/plugin/MCP đang có, đã dùng, và bật/tắt theo session, (3) nhập prompt + đổi model/effort ngay trong app, (4) trang quản lý agent/subagent/workflow — người dùng tự thêm, bật/tắt mà không mất lịch sử.

**Architecture:** Ba lớp tách bạch, phát triển tăng dần theo milestone, mỗi milestone ship được độc lập:
- **Observe** (read-only, đã có): `event` → `thread` → `store`. Mở rộng để hiểu attachment (`skill_listing`, `mcp_instructions_delta`, `agent_listing_delta`, `queued_command`/`<task-notification>`) và `toolUseResult.backgroundTaskId`.
- **Catalog** (mới, core crate `src/catalog/`): kiểm kê mọi *capability* (skill, plugin, MCP, agent, workflow) từ đĩa + mức dùng theo session từ transcript. Một trait `Provider` cho mỗi loại → thêm loại mới = thêm 1 file.
- **Control** (mới, core crate `src/control/`): *managed session* — ccmon tự chạy `claude -p --resume <sid> --input-format stream-json --output-format stream-json` với cờ sinh ra từ *overlay* của session (model, effort, tắt skill/plugin/MCP/agent). Session chạy trong terminal ngoài vẫn chỉ xem; có nút “Tiếp quản” khi session đó đã đóng.

**Tech Stack:** Rust 2021 (core crate `cc-monitor`, serde, serde_json), Tauri 2 (`gui/src-tauri`), React 18 + TypeScript 5.7 + Vite (`gui/ui`). Claude Code CLI 2.1.326.

**Spec:** Yêu cầu của người dùng (phiên 2026-11-25) + quyết định đã chốt: **mô hình Managed session** (ccmon tự chạy session; không nhúng PTY). Không có spec riêng — phần “Bối cảnh & quyết định” dưới đây đóng vai trò spec.

---

## Bối cảnh & quyết định (đọc trước khi làm)

### Dữ liệu đã xác minh trong transcript thật (Claude Code 2.1.326)
| Nguồn | Dạng | Dùng cho |
|---|---|---|
| `toolUseResult.backgroundTaskId` trên kết quả `Bash` có `run_in_background:true` | `"bqat3cqah"` | Đăng ký tác vụ nền |
| `attachment.type == "queued_command"`, `prompt` chứa `<task-notification>` | `<task-id>`, `<tool-use-id>`, `<output-file>`, `<status>completed\|failed\|killed</status>`, `<summary>` (HTML-escaped) | Kết thúc tác vụ nền, đường dẫn output |
| `Yield` tool_use | `input: {}` — **đây là lý do UI hiện “Yield {}”** | Cần suy ra “đang chờ gì” từ registry tác vụ nền |
| `Monitor` tool_use | `input.description` (bắt buộc), lệnh/điều kiện | Tác vụ nền loại monitor |
| `attachment.type == "skill_listing"` | `content`: danh sách `- name: description` | Skill *khả dụng* trong session |
| `attachment.type == "mcp_instructions_delta"` | `addedNames[]` | MCP server *khả dụng* |
| `attachment.type == "agent_listing_delta"` | `addedTypes[]` | Agent type *khả dụng* |
| `attachment.type == "deferred_tools_delta"` | `addedNames[]` | Tool hoãn (gồm `mcp__*`) |
| `attachment.type == "model"` | `identity.modelId` | Model session |
| tool_use `Skill{skill}`, `Agent{subagent_type}`, `Workflow{name\|script}`, `mcp__<server>__<tool>` | — | Capability *đã dùng* |

Hiện `event.rs` gom mọi attachment vào `Kind::Link` (bỏ nội dung) — Task 1 và Task 5 mở rộng chỗ này.

### Nguồn kiểm kê trên đĩa
- Skill: `~/.claude/skills/*/SKILL.md`, `<project>/.claude/skills/*/SKILL.md`, `<plugin installPath>/skills/*/SKILL.md` (frontmatter `name`, `description`).
- Plugin: `~/.claude/plugins/installed_plugins.json` (`plugins: {id: [{scope, installPath, version}]}`) + bật/tắt toàn cục ở `~/.claude/settings.json` → `enabledPlugins: {id: bool}` (và `<project>/.claude/settings*.json`).
- MCP: `~/.claude.json` → `mcpServers`, `projects[<cwd>].mcpServers`, `projects[<cwd>].disabledMcpjsonServers`; `<project>/.mcp.json`; `<plugin>/.mcp.json`.
- Agent: `~/.claude/agents/*.md`, `<project>/.claude/agents/*.md`, `<plugin>/agents/*.md` (frontmatter `name, description, tools, model`); built-in lấy từ `agent_listing_delta`.
- Workflow: `<project>/.claude/workflows/*.js`, `~/.claude/workflows/*.js`; lịch sử chạy ở `projects/<key>/<sid>/subagents/workflows/wf_*` (đã được `discovery` đọc).

### Cơ chế điều khiển (Managed session)
Mỗi lượt gửi prompt, ccmon giữ **một tiến trình dài** `claude -p --verbose --input-format stream-json --output-format stream-json --resume <sid> [flags]` cho session đó và ghi JSON `{"type":"user","message":{"role":"user","content":[{"type":"text","text":…}]}}` vào stdin. Transcript vẫn được ghi ra `~/.claude/projects/**.jsonl` nên toàn bộ phần xem hiện có hoạt động không đổi. Khi overlay thay đổi (model/effort/toggle), tiến trình được **khởi động lại giữa hai lượt** (không cắt ngang lượt đang chạy).

Ánh xạ overlay → cờ CLI (Task 8 spike xác minh từng dòng; nếu dòng nào không chạy, ghi lại trong `docs/architecture/control.md` và dùng phương án dự phòng):
| Overlay | Cờ chính | Dự phòng |
|---|---|---|
| model | `--model <id>` | — |
| effort | `--effort <low\|medium\|high\|xhigh\|max>` | — |
| tắt skill `x` | `--disallowedTools "Skill(x)"` | — |
| tắt plugin `p@m` | `--settings <file>` với `{"enabledPlugins":{"p@m":false}}` | — |
| tắt MCP server `s` | `--disallowedTools "mcp__s"` | `--mcp-config <file> --strict-mcp-config` chỉ với server còn bật |
| tắt agent `a` | `--disallowedTools "Agent(a)"` | — |
| tắt workflow | `--disallowedTools "Workflow"` | — |
| permission mode | `--permission-mode <default\|acceptEdits\|plan\|bypassPermissions>` | — |

### Quyết định đã chốt
1. **Managed session**, không PTY. Session đang mở ở terminal khác (`discovery::read_live` thấy pid sống) → **không cho tiếp quản** (tránh `--resume` tạo bản sao/ghi chồng); hiển thị “Đang chạy ở terminal — chỉ xem”.
2. ccmon **không bao giờ xoá hay sửa** file của người dùng ngoài: (a) file agent/workflow người dùng tạo/sửa qua trang Agents (có xác nhận), (b) file trạng thái riêng của ccmon. Bật/tắt chỉ ghi vào trạng thái ccmon, không đụng `settings.json` (trừ khi người dùng bấm “Áp dụng toàn cục” — milestone sau, ngoài phạm vi plan này).
3. Tắt agent/workflow = cờ `disabled` trong registry ccmon; file và lịch sử giữ nguyên, lịch sử vẫn xem được.
4. Trạng thái ccmon: thư mục cha của `config::config_path()` (`~/.config/ccmon/`): `overlays/<sid>.json`, `defaults.json` (overlay mặc định cho session mới), `registry.json` (agent/workflow disabled + ghi chú).

## Global Constraints

- Không thêm dependency Rust ngoài: `serde`, `serde_json`, `dirs`, `chrono`, `anyhow` (đã có). Không thêm crate tiến trình/async — dùng `std::process::Command` + thread.
- Không thêm dependency npm runtime mới. Chuỗi UI tiếng Việt, cùng giọng văn với `Tools.tsx` hiện có.
- Parsing transcript không bao giờ panic; trường lạ bị bỏ qua (giữ nguyên quy ước đầu `src/event.rs`).
- Mọi lệnh Tauri đọc file chỉ đọc trong các gốc cho phép: `~/.claude`, thư mục `tasks/` của Claude (`/tmp/claude-<uid>/**/tasks/*.output`), cwd của session, thư mục trạng thái ccmon.
- `cargo test --workspace` và `yarn build` (trong `gui/ui`) phải xanh sau mỗi task — đây là CI (`.github/workflows/ci.yml`).
- README/README.vi: câu “Local & read-only” đổi thành “Local; read-only trừ khi bạn chủ động chạy session từ ccmon” ở Task 10.
- Commit cuối mỗi task, message dạng `feat(scope): …`, kết thúc bằng dòng `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Tác vụ nền mồ côi** — `Bash` nền không bao giờ có `<task-notification>` (session bị đóng giữa chừng): phải hiện “không rõ / session đã đóng”, không treo ở “đang chạy” mãi. Test ở Task 2 (`bg_task_without_notice_is_unknown_after_session_end`).
2. **Yield không có tác vụ nào đang chờ** (hoặc chỉ chờ subagent/workflow): phải hiện “chờ agent X” hoặc “không có tác vụ nền”, không phải `{}` rỗng. Test ở Task 2 (`yield_waits_on_agents_and_shells`).
3. **Tiếp quản session đang mở ở terminal** — phải bị chặn. Test ở Task 9 (`refuses_resume_when_session_live`).
4. **Tên chứa ký tự đặc biệt** (skill `plugin:skill`, MCP `claude.ai Claude Docs` có dấu cách, agent `impeccable:impeccable-documenter`) — cờ CLI phải đúng, mỗi rule là một đối số riêng, không ghép chuỗi shell. Test ở Task 8 (`args_handle_colons_and_spaces`).
5. **Output file của tác vụ nền rất lớn / đã bị xoá** — `tail_task_output` chỉ đọc 64 KiB cuối, file mất → trả trạng thái “không còn file”, không lỗi. Test ở Task 3 (`tail_missing_file_is_gone`, `tail_reads_last_64k`).

---

## File Structure

**Core crate (`src/`)**
- Modify `src/event.rs` — thêm `Kind::Attachment`-family mới: `Kind::TaskNotice`, `Kind::Inventory`; parse `backgroundTaskId`.
- Create `src/bgtask.rs` — registry tác vụ nền theo thread (thuần, không IO).
- Modify `src/thread.rs` — nuôi `bgtask::Registry`, gắn `waits`/`bg` vào `ToolItem`.
- Create `src/catalog/mod.rs` — kiểu chung `Capability`, `CapKind`, `Scope`, trait `Provider`, `Catalog::scan(ctx)`.
- Create `src/catalog/{frontmatter,skills,plugins,mcp,agents,workflows}.rs` — mỗi provider một file.
- Create `src/catalog/usage.rs` — mức khả dụng/đã dùng theo session từ `Thread`.
- Create `src/control/mod.rs` — `Overlay`, `LaunchSpec`, `build_args`.
- Create `src/control/state.rs` — đọc/ghi `overlays/`, `defaults.json`, `registry.json`.
- Create `src/control/runner.rs` — vòng đời tiến trình managed (`ManagedSession`).
- Modify `src/lib.rs` — `pub mod bgtask; pub mod catalog; pub mod control;`.

**GUI backend (`gui/src-tauri/src/`)**
- Create `gui/src-tauri/src/cmd_catalog.rs`, `cmd_control.rs`, `cmd_agents.rs` — lệnh Tauri theo nhóm (main.rs đã 852 dòng; không dồn thêm).
- Modify `gui/src-tauri/src/main.rs` — DTO `ToolItem` thêm `bg`/`waits`; đăng ký lệnh mới; giữ `ManagedSessions` trong `Shared`.

**GUI frontend (`gui/ui/src/`)**
- Modify `Tools.tsx` — card cho `Yield`, `Monitor`, `TaskOutput`, `TaskStop`, `BashOutput`, `KillShell`, Bash nền.
- Create `BgTask.tsx` — khối tác vụ nền (trạng thái, summary, tail output).
- Create `Library.tsx` — trang Skills/Plugins/MCP (tab), cột “session này”.
- Create `Agents.tsx` — trang Agents & Workflows, form tạo/sửa, lịch sử.
- Create `Composer.tsx` — ô nhập, chọn model/effort/permission, ngăn kéo toggle.
- Modify `App.tsx` — điều hướng trang (`view: 'thread' | 'library' | 'agents'`), gắn Composer dưới Chat.
- Modify `api.ts` — kiểu + wrapper `invoke` mới.

**Docs**
- Create `docs/architecture/capabilities.md` — chuẩn mở rộng (Provider, Overlay, cách thêm loại capability mới).
- Create `docs/architecture/control.md` — kết quả spike cờ CLI, vòng đời managed session.

---

## Milestone A — Tác vụ nền hiển thị chi tiết (yêu cầu 1)

### Task 1: Parse tín hiệu tác vụ nền trong `event.rs`

**Files:**
- Modify: `src/event.rs` (enum `Kind` ~dòng 44; `parse_line`; khối `toolUseResult` ~dòng 321)
- Test: `src/event.rs` (`#[cfg(test)]` có sẵn)

**Interfaces:**
- Produces:
  - `Kind::TaskNotice(TaskNotice)` với `pub struct TaskNotice { pub task_id: String, pub tool_use_id: Option<String>, pub status: String, pub summary: String, pub output_file: Option<String> }`
  - `Event.bg_task_id: Option<String>` (từ `toolUseResult.backgroundTaskId`)
  - `pub fn parse_task_notification(s: &str) -> Option<TaskNotice>` — tách thẻ XML đơn giản bằng tìm chuỗi, HTML-unescape `&amp; &lt; &gt; &quot; &#39;`.

- [ ] **Step 1: Viết test thất bại**

```rust
#[test]
fn parses_task_notification_attachment() {
    let l = r#"{"type":"attachment","uuid":"u1","parentUuid":"p","attachment":{"type":"queued_command","prompt":"<task-notification>\n<task-id>b9b5g4q3w</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n<output-file>/tmp/claude-1000/x/tasks/b9b5g4q3w.output</output-file>\n<status>failed</status>\n<summary>Background command \"a &amp;&amp; b\" failed (exit code 1)</summary>\n</task-notification>"}}"#;
    let e = parse_line(l).unwrap();
    assert_eq!(e.kind, Kind::TaskNotice(TaskNotice {
        task_id: "b9b5g4q3w".into(), tool_use_id: Some("toolu_1".into()), status: "failed".into(),
        summary: "Background command \"a && b\" failed (exit code 1)".into(),
        output_file: Some("/tmp/claude-1000/x/tasks/b9b5g4q3w.output".into()) }));
    assert_eq!(e.uuid.as_deref(), Some("u1")); // vẫn nằm trong chuỗi parentUuid
}

#[test]
fn reads_background_task_id_from_tool_use_result() {
    let l = r#"{"type":"user","uuid":"u2","toolUseResult":{"backgroundTaskId":"bqat3cqah"},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Command running in background with ID: bqat3cqah."}]}}"#;
    assert_eq!(parse_line(l).unwrap().bg_task_id.as_deref(), Some("bqat3cqah"));
}

#[test]
fn notification_also_found_in_user_text() { /* cùng payload nhưng nằm trong message.content dạng chuỗi của dòng type=user → Kind::TaskNotice */ }
```

- [ ] **Step 2:** `cargo test -p cc-monitor event::` → FAIL (`TaskNotice` chưa có).
- [ ] **Step 3:** Thêm kiểu + parse. `queued_command` không chứa `<task-notification>` giữ nguyên hành vi cũ (`Kind::Link`). `Thread` hiện match `Kind` — thêm nhánh `TaskNotice` vào chỗ đang xử lý `Link` để chuỗi parent không đứt.
- [ ] **Step 4:** `cargo test --workspace` → PASS.
- [ ] **Step 5:** Commit `feat(event): parse background task notifications`.

### Task 2: Registry tác vụ nền + liên kết vào `ToolItem`

**Files:**
- Create: `src/bgtask.rs`
- Modify: `src/thread.rs` (`ToolItem` ~dòng 56, `Thread::push`, `Thread::items`), `src/lib.rs`
- Test: `src/bgtask.rs`, `src/thread.rs`

**Interfaces:**
- Consumes: `Kind::TaskNotice`, `Event.bg_task_id` (Task 1); `ThreadLink` hiện có.
- Produces:
  ```rust
  pub enum BgKind { Shell, Monitor, Agent, Workflow }
  pub enum BgState { Running, Completed, Failed, Killed, Unknown }
  pub struct BgTask { pub id: String, pub kind: BgKind, pub label: String, pub tool_use_id: String,
                      pub output_file: Option<String>, pub state: BgState, pub started: Option<i64>,
                      pub ended: Option<i64>, pub summary: Option<String> }
  pub struct Registry { /* id -> BgTask, giữ thứ tự */ }
  impl Registry {
      pub fn launch(&mut self, t: BgTask);
      pub fn notice(&mut self, n: &TaskNotice, ts: Option<i64>);
      pub fn pending_at(&self, ts: i64) -> Vec<&BgTask>;   // started <= ts và (ended None hoặc > ts)
      pub fn by_tool_use(&self, tool_use_id: &str) -> Option<&BgTask>;
      pub fn get(&self, id: &str) -> Option<&BgTask>;
      pub fn finalize_unknown(&mut self);                   // gọi khi session đã đóng
  }
  ```
  `ToolItem` thêm: `pub bg: Option<BgTask>` (tác vụ do tool này khởi chạy, hoặc tác vụ mà `TaskOutput/TaskStop/BashOutput/KillShell` nhắm tới qua `input.task_id|shell_id|bash_id`), `pub waits: Vec<BgTask>` (chỉ cho `Yield`: `pending_at(started)`), `pub woke_by: Option<String>` (Yield: task-id của notice đầu tiên sau `started`).
- Nguồn `launch`: Bash có `run_in_background` + `bg_task_id` → `Shell`, label = `description` hoặc dòng đầu lệnh; `Monitor` → `Monitor`, label = `input.description`, id = `bg_task_id` hoặc tool_use_id; Agent `async_launched` / Workflow `runId` (link sẵn có) → `Agent`/`Workflow`, label = `description` / `workflowName`, kết thúc khi notice có `<tool-use-id>` trùng.

- [ ] **Step 1: Test thất bại**

```rust
#[test] fn yield_waits_on_agents_and_shells() {
    // Bash nền (bgA, ts=100) + Agent async (t2, ts=110) + Yield (ts=120) + notice bgA completed (ts=130)
    // => yield.waits ids == ["bgA", <agent id>], yield.woke_by == Some("bgA"),
    //    bash.bg.state == Completed, bash.bg.summary chứa "completed"
}
#[test] fn bg_task_without_notice_is_unknown_after_session_end() {
    // Bash nền không có notice; registry.finalize_unknown() => state == Unknown
}
#[test] fn task_output_links_to_target_task() {
    // TaskOutput {task_id:"bgA"} => item.bg.id == "bgA"
}
```

- [ ] **Step 2:** `cargo test -p cc-monitor bgtask:: thread::` → FAIL.
- [ ] **Step 3:** Implement. `store` gọi `finalize_unknown()` khi `session_status` chuyển sang đã đóng (xem `store.rs:235`).
- [ ] **Step 4:** `cargo test --workspace` → PASS.
- [ ] **Step 5:** Commit `feat(thread): track background tasks and what Yield waits on`.

### Task 3: DTO + lệnh `tail_task_output` + card UI

**Files:**
- Modify: `gui/src-tauri/src/main.rs` (DTO tool trong `get_thread` ~dòng 297; `invoke_handler` ~dòng 833)
- Create: `gui/ui/src/BgTask.tsx`
- Modify: `gui/ui/src/Tools.tsx` (`switch (tool.name)` ~dòng 179), `gui/ui/src/api.ts`, `gui/ui/src/App.tsx` (`activityOf` ~dòng 284), `gui/ui/src/styles.css`
- Test: `gui/src-tauri/src/main.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `ToolItem.bg / waits / woke_by` (Task 2).
- Produces:
  - DTO `BgTaskDto { id, kind: "shell"|"monitor"|"agent"|"workflow", label, state: "running"|"completed"|"failed"|"killed"|"unknown", started, ended, summary, outputFile }` (camelCase); `ToolItemDto` thêm `bg?: BgTaskDto`, `waits: BgTaskDto[]`, `wokeBy?: string`.
  - Lệnh `tail_task_output(path: String, max: Option<u64>) -> TailDto { text: String, truncated: bool, gone: bool, size: u64 }` — mặc định & trần 64 KiB cuối; chỉ chấp nhận path khớp `*/tasks/*.output` dưới `std::env::temp_dir()` hoặc `/tmp/claude-*`, còn lại trả `gone: true`.
  - `api.ts`: `tailTaskOutput(path: string): Promise<TailDto>`.
- UI:
  - `Yield`: title = `Chờ N tác vụ nền` (hoặc `Chờ: <label>` khi N=1; `Không có tác vụ nền đang chạy` khi 0); body = danh sách `BgTask` (icon theo kind, trạng thái, thời gian), dòng “Đánh thức bởi: <label>” khi có `wokeBy`.
  - `Monitor`: title = `input.description`; badge `monitor`; body = lệnh/điều kiện (KeyValues trừ `description`) + `BgTask`.
  - Bash nền: giữ card Bash, thêm `BgTask` (trạng thái thật thay cho kết quả “Command running in background…”).
  - `TaskOutput`/`BashOutput`/`TaskStop`/`KillShell`: title = `Đọc output: <label>` / `Dừng: <label>`.
  - `BgTask` có nút “Xem output” → gọi `tailTaskOutput`, poll 1 s khi `state=running` và khối đang mở, render `<Code>` `maxLines={20}`.
  - `ActivityBar`: khi tool đang chạy là `Yield` → “Đang chờ: label1, label2 (+n)”.

- [ ] **Step 1: Test thất bại** — `tail_missing_file_is_gone`, `tail_reads_last_64k` (ghi 100 KiB vào tempdir `…/tasks/x.output`, kỳ vọng `text.len() == 65536`, `truncated`), `tail_rejects_outside_path` (`/etc/passwd` → `gone: true`, không đọc).
- [ ] **Step 2:** `cargo test -p ccmon-gui tail_` → FAIL.
- [ ] **Step 3:** Implement lệnh + DTO; UI theo mô tả trên.
- [ ] **Step 4:** `cargo test --workspace && (cd gui/ui && yarn build)` → PASS. Kiểm tra thủ công bằng skill `run` trên `demo-data` (bổ sung vào `demo/make_demo.py` một Bash nền + Yield + notice để demo có dữ liệu).
- [ ] **Step 5:** Commit `feat(gui): detailed cards for Yield, Monitor and background tasks`.

---

## Milestone B — Catalog: skill / plugin / MCP / agent / workflow (yêu cầu 2, phần xem)

### Task 4: Khung `catalog` + provider skill & plugin

**Files:**
- Create: `src/catalog/mod.rs`, `src/catalog/frontmatter.rs`, `src/catalog/skills.rs`, `src/catalog/plugins.rs`
- Modify: `src/lib.rs`
- Test: trong từng file, dùng `tempfile` dựng cây `~/.claude` giả

**Interfaces:**
- Produces:
  ```rust
  pub enum CapKind { Skill, Plugin, Mcp, Agent, Workflow }
  pub enum Scope { User, Project, Plugin(String /*plugin id*/), BuiltIn, Managed }
  pub struct Capability {
      pub kind: CapKind, pub id: String,        // id ổn định: skill "name" hoặc "plugin:name"; mcp server name; agent name; plugin "p@m"
      pub name: String, pub description: String, pub scope: Scope,
      pub path: Option<PathBuf>, pub enabled_globally: bool,
      pub meta: serde_json::Map<String, Value>, // trường riêng loại (tools, model, version, transport…)
  }
  pub struct ScanCtx { pub claude_home: PathBuf, pub claude_json: PathBuf, pub cwd: Option<PathBuf> }
  pub trait Provider { fn kind(&self) -> CapKind; fn scan(&self, ctx: &ScanCtx) -> Vec<Capability>; }
  pub struct Catalog { pub items: Vec<Capability> }
  impl Catalog { pub fn scan(ctx: &ScanCtx) -> Catalog; pub fn of(&self, k: CapKind) -> impl Iterator<Item=&Capability>; }
  pub fn providers() -> Vec<Box<dyn Provider>>; // danh sách đăng ký duy nhất — thêm loại mới chỉ sửa ở đây
  // frontmatter.rs
  pub fn parse(md: &str) -> (serde_json::Map<String, Value>, &str); // YAML phẳng key: value, list "[a, b]" hoặc "- a"; không thêm crate yaml
  ```
- Plugin provider đọc `installed_plugins.json` + `enabledPlugins` (user settings, rồi project `.claude/settings.json`, `.claude/settings.local.json` ghi đè). Mỗi plugin `meta.components = {skills:n, agents:n, commands:n, mcp:[names]}`. Skill provider quét cả `<installPath>/skills` của plugin **đang bật**, id `"<plugin-name>:<skill>"`.

- [ ] **Step 1:** Test: `skills_from_user_project_and_plugin` (3 nguồn, id đúng dạng, scope đúng), `plugin_enabled_flag_project_overrides_user`, `frontmatter_handles_lists_and_quotes`, `malformed_skill_md_is_skipped_not_panic`.
- [ ] **Step 2:** `cargo test -p cc-monitor catalog::` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(catalog): capability model with skill and plugin providers`.

### Task 5: Provider MCP, agent, workflow + mức dùng theo session

**Files:**
- Create: `src/catalog/mcp.rs`, `src/catalog/agents.rs`, `src/catalog/workflows.rs`, `src/catalog/usage.rs`
- Modify: `src/event.rs` (attachment inventory), `src/thread.rs` (lưu inventory)
- Test: trong từng file

**Interfaces:**
- Consumes: `Capability`, `Provider`, `ScanCtx` (Task 4).
- Produces:
  - `event.rs`: `Kind::Inventory(InventoryDelta)` với `pub struct InventoryDelta { pub skills: Vec<String>, pub mcp: Vec<String>, pub agents: Vec<String>, pub deferred_tools: Vec<String> }` từ các attachment `skill_listing` (tách `- name:`), `mcp_instructions_delta.addedNames`, `agent_listing_delta.addedTypes`, `deferred_tools_delta.addedNames`; `removedNames` nếu có thì trừ.
  - `Thread::inventory(&self) -> &InventoryDelta` (gộp dồn).
  - `usage.rs`:
    ```rust
    pub enum UseState { Unavailable, Available, Used { count: u32, last: Option<i64> } }
    pub struct SessionUsage { pub entries: Vec<(CapKind, String /*id*/, UseState)> }
    pub fn session_usage(threads: &[&Thread], catalog: &Catalog) -> SessionUsage;
    ```
    “Used” đếm tool_use trên main + mọi subagent/workflow của session: `Skill.input.skill`, `Agent.input.subagent_type` (mặc định `general-purpose`), `Workflow`, tên tool `mcp__<server>__…` → server (server chứa dấu cách được Claude Code chuẩn hoá thành `_`; khớp theo dạng chuẩn hoá của cả hai bên). Capability có trong inventory nhưng không có trên đĩa (built-in agent, MCP claude.ai) vẫn được thêm với `Scope::BuiltIn`/`Managed`.
- Agent provider: `meta = {tools, model, color}`; built-in đến từ inventory. Workflow provider: `meta.runs` lấy ở Task 11, ở đây chỉ file.

- [ ] **Step 1:** Test: `mcp_merges_user_project_and_mcp_json_with_disabled_list`, `agents_from_md_frontmatter`, `inventory_delta_accumulates_and_removes`, `usage_counts_skill_agent_and_mcp_across_subagents`, `mcp_name_with_space_matches_normalized_tool_prefix`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** `cargo test --workspace` PASS.
- [ ] **Step 5:** Commit `feat(catalog): MCP, agent, workflow providers and per-session usage`.

### Task 6: Trang Library (Skills / Plugins / MCP)

**Files:**
- Create: `gui/src-tauri/src/cmd_catalog.rs`, `gui/ui/src/Library.tsx`
- Modify: `gui/src-tauri/src/main.rs`, `gui/ui/src/App.tsx`, `gui/ui/src/api.ts`, `gui/ui/src/styles.css`

**Interfaces:**
- Consumes: `Catalog::scan`, `session_usage` (Task 4–5).
- Produces:
  - Lệnh `get_catalog(cwd: Option<String>) -> Vec<CapabilityDto>`; `get_session_usage(sid: String) -> Vec<UsageDto>` (`UsageDto { kind, id, state: "unavailable"|"available"|"used", count, last }`). Cache catalog 5 s theo cwd.
  - `App.tsx`: state `page: 'thread' | 'library' | 'agents'` + nút trên thanh tiêu đề; phím tắt `Ctrl+1/2/3`.
  - `Library.tsx`: tab Skills | Plugins | MCP; ô lọc; mỗi dòng: tên, mô tả (1 dòng), scope chip, trạng thái toàn cục; khi có session đang chọn: cột “Session này” = Đã dùng ×n / Khả dụng / Không có, sắp “Đã dùng” lên đầu; công tắc bật/tắt cho session (Task 9 nối dây — ở task này hiển thị disabled với tooltip “Chỉ áp dụng cho session chạy từ ccmon”).
- [ ] **Step 1:** Test Rust `catalog_dto_serializes_scope_and_kind` (snapshot JSON camelCase).
- [ ] **Step 2–4:** FAIL → implement → `cargo test --workspace && (cd gui/ui && yarn build)` PASS; kiểm thủ công bằng skill `run`.
- [ ] **Step 5:** Commit `feat(gui): library page for skills, plugins and MCP`.

---

## Milestone C — Managed session: nhập liệu, model, effort, bật/tắt (yêu cầu 2 phần toggle + yêu cầu 3)

### Task 7: Overlay + trạng thái ccmon

**Files:**
- Create: `src/control/mod.rs`, `src/control/state.rs`
- Modify: `src/lib.rs`
- Test: trong file (tempdir qua biến môi trường `CCMON_CONFIG`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Serialize, Deserialize, Default, Clone, PartialEq)] #[serde(rename_all="camelCase", default)]
  pub struct Overlay {
      pub model: Option<String>, pub effort: Option<String>, pub permission_mode: Option<String>,
      pub disabled_skills: BTreeSet<String>, pub disabled_plugins: BTreeSet<String>,
      pub disabled_mcp: BTreeSet<String>, pub disabled_agents: BTreeSet<String>,
      pub workflows_off: bool, pub main_agent: Option<String>, // --agent
  }
  pub struct Registry { pub disabled_agents: BTreeSet<String>, pub disabled_workflows: BTreeSet<String>, pub notes: BTreeMap<String,String> }
  // state.rs
  pub fn state_dir() -> PathBuf;                       // config_path().parent()
  pub fn load_overlay(sid: &str) -> Overlay;           // overlays/<sid>.json, không có → defaults.json, không có → Default
  pub fn save_overlay(sid: &str, o: &Overlay) -> io::Result<()>; // ghi tmp + rename
  pub fn load_registry() -> Registry; pub fn save_registry(r: &Registry) -> io::Result<()>;
  pub fn effective(o: &Overlay, r: &Registry) -> Overlay; // hợp registry (tắt toàn ccmon) vào overlay
  ```
  `sid` được kiểm tra `[A-Za-z0-9-]+` trước khi ghép path.
- [ ] **Step 1:** Test `overlay_falls_back_to_defaults`, `save_is_atomic_and_roundtrips`, `rejects_path_traversal_sid`, `registry_disabled_agents_merge_into_overlay`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(control): per-session overlay and ccmon registry`.

### Task 8: Spike cờ CLI + `build_args`

**Files:**
- Create: `docs/architecture/control.md`
- Modify: `src/control/mod.rs`
- Test: `src/control/mod.rs`

**Interfaces:**
- Consumes: `Overlay` (Task 7).
- Produces:
  ```rust
  pub struct LaunchSpec { pub program: PathBuf, pub args: Vec<String>, pub cwd: PathBuf, pub settings_file: Option<PathBuf> }
  pub fn build_args(sid: Option<&str>, cwd: &Path, o: &Overlay, state_dir: &Path) -> io::Result<LaunchSpec>;
  pub fn find_claude() -> Option<PathBuf>; // PATH, ~/.local/bin/claude, ~/.claude/local/claude
  ```
  Cờ cố định: `-p --verbose --input-format stream-json --output-format stream-json`; `--resume <sid>` khi có sid (không có → session mới, sid đọc từ sự kiện `system/init` đầu tiên của stdout). `--settings` chỉ ghi khi có `disabled_plugins` (file `state_dir/launch/<sid>.settings.json`). Mỗi rule `--disallowedTools` là **một phần tử args riêng** sau cờ.
- [ ] **Step 1 (spike, thủ công, ghi kết quả vào `control.md`):** trong thư mục tạm, với Claude Code 2.1.326 xác minh từng dòng bảng ánh xạ ở phần Bối cảnh: chạy `claude -p --output-format stream-json --verbose --disallowedTools "Skill(find-skills)" "dùng skill find-skills"` và kiểm tra tool_result là lỗi từ chối; tương tự `mcp__codegraph`, `Agent(Explore)`, `--settings` tắt plugin (kiểm tra `skill_listing` không còn skill của plugin đó), `--effort`, `--model`. Ghi lệnh + kết quả. Dòng nào thất bại → chọn cột dự phòng và cập nhật Task 8–9 trước khi đi tiếp.
- [ ] **Step 2: Test thất bại** — `args_basic_resume`, `args_handle_colons_and_spaces` (skill `superpowers:brainstorming`, MCP `claude.ai Claude Docs` → đúng 1 phần tử args mỗi rule, không có dấu nháy thừa), `settings_file_only_when_plugins_disabled`, `new_session_has_no_resume`.
- [ ] **Step 3–4:** Implement → `cargo test --workspace` PASS.
- [ ] **Step 5:** Commit `feat(control): translate overlay into claude CLI flags`.

### Task 9: Runner managed session

**Files:**
- Create: `src/control/runner.rs`
- Test: `src/control/runner.rs` với một chương trình giả (script shell in sẵn stream-json, đặt qua `LaunchSpec.program`)

**Interfaces:**
- Consumes: `LaunchSpec`, `build_args` (Task 8); `discovery::read_live` (session đang mở ở terminal).
- Produces:
  ```rust
  pub enum RunState { Idle, Starting, Busy, Restarting, Exited { code: Option<i32>, stderr_tail: String } }
  pub struct ManagedSession { /* child, stdin, thread đọc stdout */ }
  impl ManagedSession {
      pub fn spawn(spec: LaunchSpec) -> io::Result<Self>;
      pub fn send(&mut self, text: &str) -> io::Result<()>;      // ghi 1 dòng JSON user message
      pub fn interrupt(&mut self) -> io::Result<()>;             // ghi {"type":"control_request","request":{"subtype":"interrupt"}} (xác minh ở spike; dự phòng: SIGINT/kill)
      pub fn state(&self) -> RunState;                           // Busy từ lúc send tới sự kiện {"type":"result"}
      pub fn session_id(&self) -> Option<String>;                // từ {"type":"system","subtype":"init","session_id"}
      pub fn stop(self);
  }
  pub struct Manager { /* sid -> ManagedSession, overlay đang áp dụng */ }
  impl Manager {
      pub fn send(&mut self, sid: Option<&str>, cwd: &Path, text: &str, o: &Overlay, live: &HashMap<String, LiveInfo>) -> Result<String /*sid*/, ControlError>;
      // overlay khác overlay đang chạy và state Idle → stop + spawn lại trước khi gửi
  }
  pub enum ControlError { SessionLiveElsewhere, ClaudeNotFound, Busy, Io(String) }
  ```
  Stdout của tiến trình chỉ dùng cho trạng thái (init, result, lỗi); hiển thị hội thoại vẫn đi qua transcript JSONL như cũ — không vẽ trùng.
- [ ] **Step 1:** Test `refuses_resume_when_session_live`, `restarts_between_turns_when_overlay_changes` (giả lập: 2 lần spawn), `busy_until_result_event`, `exited_reports_stderr_tail`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(control): managed session runner`.

### Task 10: Composer + toggle theo session trong GUI

**Files:**
- Create: `gui/src-tauri/src/cmd_control.rs`, `gui/ui/src/Composer.tsx`
- Modify: `gui/src-tauri/src/main.rs` (`Shared` thêm `manager: Arc<Mutex<Manager>>`), `gui/ui/src/App.tsx`, `gui/ui/src/Library.tsx`, `gui/ui/src/api.ts`, `gui/ui/src/styles.css`, `README.md`, `README.vi.md`

**Interfaces:**
- Consumes: `Manager`, `Overlay`, `load_overlay/save_overlay` (Task 7–9); `get_session_usage` (Task 6).
- Produces lệnh Tauri:
  - `get_control(sid: Option<String>) -> ControlDto { overlay, runState, canControl: bool, reason?: "liveElsewhere"|"claudeNotFound", models: string[], efforts: string[] }` — `models` lấy từ model đã thấy trong transcript + alias `opus`, `sonnet`, `haiku`, `fable`; `efforts = ["low","medium","high","xhigh","max"]`.
  - `set_overlay(sid: String, overlay: Overlay) -> ControlDto`
  - `send_prompt(sid: Option<String>, cwd: String, text: String) -> Result<String /*sid*/, String>` (sid None = session mới trong `cwd`; frontend chọn session mới khi `store-changed` có sid đó)
  - `interrupt(sid: String)`, `stop_managed(sid: String)`
  - Sự kiện `control-changed { sid, runState }`.
- UI:
  - `Composer` dưới Chat: textarea (Enter gửi, Shift+Enter xuống dòng), chip model, chip effort, chip permission mode, nút “Dừng” khi Busy, nút ⚙ mở ngăn kéo “Capability cho session” (danh sách từ Library, công tắc ghi `set_overlay`). Khi `canControl=false` → ô nhập bị khoá với lý do (“Session đang chạy ở terminal — chỉ xem”).
  - Thay đổi model/effort/toggle khi đang Busy: hiện chip “Áp dụng từ lượt sau”.
  - Nút “Session mới…” trên cây session: chọn cwd (từ danh sách project đã biết) → mở Composer trống.
  - Library: công tắc nối với overlay của session đang chọn.
  - README: cập nhật câu “Local & read-only” (xem Global Constraints) + mục “Chạy session từ ccmon”.
- [ ] **Step 1:** Test Rust `control_dto_reports_live_elsewhere`, `set_overlay_persists`.
- [ ] **Step 2–4:** FAIL → implement → `cargo test --workspace && (cd gui/ui && yarn build)` PASS. Kiểm thủ công bằng skill `run`: tạo session mới, gửi “liệt kê file”, đổi effort, tắt 1 skill, gửi lượt 2, xác nhận transcript lượt 2 không còn skill đó trong `skill_listing`.
- [ ] **Step 5:** Commit `feat(gui): composer with model, effort and per-session capability toggles`.

---

## Milestone D — Agents & Workflows (hướng phát triển chính)

### Task 11: Lịch sử agent/workflow + registry tắt/bật

**Files:**
- Create: `src/catalog/history.rs`
- Modify: `src/catalog/workflows.rs`, `src/catalog/agents.rs`
- Test: `src/catalog/history.rs` (cây `projects/` giả, dùng `demo-data` làm mẫu cấu trúc)

**Interfaces:**
- Consumes: `discovery::AgentMeta` (đọc `*.meta.json` — đã có), `control::Registry` (Task 7).
- Produces:
  ```rust
  pub struct RunRecord { pub session_id: String, pub project: String, pub node_key: String /* "a:<sid>:<agentId>" dùng được với get_thread */,
                         pub started: Option<i64>, pub ended: Option<i64>, pub status: String, pub description: String }
  pub fn agent_history(projects_root: &Path, agent: &str, since_days: u32) -> Vec<RunRecord>;
  pub fn workflow_history(projects_root: &Path, workflow: &str, since_days: u32) -> Vec<RunRecord>;
  ```
  `Capability.enabled_globally` của agent/workflow = không nằm trong `Registry.disabled_*`. Tắt không xoá gì; lịch sử luôn trả về bất kể trạng thái.
  `node_key` phải khớp định dạng `key_of` trong `gui/src-tauri/src/main.rs` (~dòng 58) để mở thẳng thread bằng viewer hiện có.
- [ ] **Step 1:** Test `history_lists_runs_of_agent_type_across_sessions`, `disabled_agent_still_has_history`, `workflow_runs_grouped_by_run_id`.
- [ ] **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Commit `feat(catalog): agent and workflow run history`.

### Task 12: Trang Agents (xem, tạo/sửa, bật/tắt, tham chiếu từ session)

**Files:**
- Create: `gui/src-tauri/src/cmd_agents.rs`, `gui/ui/src/Agents.tsx`
- Modify: `gui/src-tauri/src/main.rs`, `gui/ui/src/App.tsx`, `gui/ui/src/Composer.tsx`, `gui/ui/src/api.ts`, `gui/ui/src/styles.css`
- Test: `gui/src-tauri/src/cmd_agents.rs`

**Interfaces:**
- Consumes: `Catalog` (agents, workflows), `agent_history`, `workflow_history`, `Registry` (Task 7, 11).
- Produces lệnh:
  - `list_agents(cwd: Option<String>) -> Vec<AgentDto>` (`AgentDto = CapabilityDto + { disabled, runs: number, lastRun? }`), `list_workflows(cwd) -> Vec<WorkflowDto>`.
  - `agent_history(name, days) -> Vec<RunRecordDto>`, `workflow_history(name, days)`.
  - `set_agent_disabled(name, disabled: bool)`, `set_workflow_disabled(name, disabled: bool)` → ghi `registry.json`.
  - `save_agent(scope: "user"|"project", cwd: Option<String>, def: AgentDef) -> Result<String /*path*/, String>` với `AgentDef { name, description, tools: Option<Vec<String>>, model: Option<String>, prompt: String }`; tên phải khớp `^[a-z0-9][a-z0-9-]{0,63}$`; ghi `~/.claude/agents/<name>.md` hoặc `<cwd>/.claude/agents/<name>.md`; **từ chối ghi đè** file không do ccmon tạo trừ khi `overwrite: true` (UI hỏi xác nhận). Agent built-in/plugin: chỉ đọc.
- UI `Agents.tsx`: hai tab Agents | Workflows. Danh sách trái (nhóm theo scope: Built-in / Plugin / User / Project, chip “Đã tắt”), chi tiết phải: mô tả, tools, model, prompt (Markdown), công tắc “Bật trong ccmon”, bảng lịch sử chạy (click → mở thread với viewer hiện có, `page='thread'`). Nút “+ Agent mới” → form (tên, mô tả, tools đa chọn từ danh sách tool đã thấy, model, prompt).
- Tham chiếu từ session: trong Composer gõ `@` → popup agent đang bật → chèn `Dùng agent <name> để …` vào prompt (thủ công); chọn “Chạy session như agent” → `overlay.main_agent` (`--agent`). Agent tắt trong registry tự vào `disabled_agents` của mọi managed session (qua `effective`, Task 7).
- [ ] **Step 1:** Test `save_agent_writes_frontmatter`, `save_agent_refuses_overwrite_without_flag`, `save_agent_rejects_bad_name`.
- [ ] **Step 2–4:** FAIL → implement → `cargo test --workspace && (cd gui/ui && yarn build)` PASS; kiểm thủ công bằng skill `run`.
- [ ] **Step 5:** Commit `feat(gui): agents and workflows management page`.

### Task 13: Chuẩn mở rộng (tài liệu kiến trúc)

**Files:**
- Create: `docs/architecture/capabilities.md`
- Modify: `CONTRIBUTING.md` (liên kết)

Nội dung bắt buộc: sơ đồ 3 lớp Observe/Catalog/Control; hợp đồng `Provider` (id ổn định, không IO ngoài `ScanCtx`, không panic); quy trình thêm loại capability mới (file provider → `providers()` → nhánh usage → ánh xạ overlay → cờ CLI trong `build_args` → tab UI); quy ước trạng thái ccmon (`state_dir`, ghi atomic, không đụng file người dùng); danh sách việc tương lai rõ ràng: “Áp dụng toàn cục” vào `settings.json`, trình duyệt marketplace plugin, sửa workflow script, phê duyệt quyền qua `--permission-prompt-tool`.
- [ ] **Step 1:** Viết tài liệu. **Step 2:** Commit `docs: capability architecture and extension guide`.

---

## Thứ tự & PR

Một PR nháp cho nhánh `feat/agent-control-center` chứa plan này. Mỗi milestone (A→D) có thể merge riêng: A không phụ thuộc gì; B độc lập với A; C cần B (Task 6) cho ngăn kéo toggle; D cần B và C (Task 7).
