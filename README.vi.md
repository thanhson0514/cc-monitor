<div align="center">

<img src="website/assets/logo.png" width="96" alt="ccmon logo" />

# ccmon

**Xem AI coding agent đang làm gì — theo thời gian thực.**

Ứng dụng desktop miễn phí, mã nguồn mở, biến các phiên **Claude Code** thành một màn hình gọn
gàng, cập nhật trực tiếp: từng prompt, lệnh, file đã sửa, subagent và workflow, kèm diff hai cột
và hạn mức gói. Local; read-only trừ khi bạn chủ động chạy session từ ccmon.
Trang Agents ghi file agent bạn tạo hoặc sửa (sau khi bạn xác nhận) vào `~/.claude/agents` hoặc `.claude/agents` của project.

[**Tải về**](https://github.com/wik1ngst-1103/cc-monitor/releases/latest) ·
[Website](https://cc-monitor.online) ·
[Báo lỗi](https://github.com/wik1ngst-1103/cc-monitor/issues) ·
[English](README.md)

<img src="website/assets/screen-main.png" alt="Màn hình chính của ccmon" />

</div>

## Tính năng

- **Timeline trực tiếp:** prompt, câu trả lời dạng Markdown (code block có số dòng, nút Copy), mỗi
  lần gọi tool là một thẻ dễ đọc: Bash có mô tả và output, Edit hiện diff, Read, Grep, Write, danh sách việc.
- **Subagent & workflow:** cây phiên đang mở, subagent và agent của workflow với trạng thái trực tiếp;
  mở luồng bất kỳ để xem việc được giao và báo cáo gửi về.
- **Diff hai cột:** mọi file phiên đã sửa, so với *trước phiên* (từ bản sao lưu của chính Claude Code,
  không cần git) hoặc với git HEAD.
- **File, ảnh & log:** ảnh bạn dán vào, ảnh chụp agent tạo, file đã đọc/ghi, thư mục nháp, log lệnh ✓/✗;
  xem trước ảnh/văn bản và Markdown đã định dạng.
- **Hạn mức gói:** model, context đã dùng, hạn mức 5 giờ và tuần, effort, nhánh git, số dòng thay đổi.
- **Lịch sử phiên:** phiên đang mở (đang làm/chờ), phiên vừa đóng, và các phiên cũ theo project. Cây
  phiên thu gọn được thành một dải hẹp (`Ctrl+B`).
- **Tác vụ nền:** thẻ Yield liệt kê agent đang chờ gì — shell, monitor, subagent, workflow — với trạng
  thái trực tiếp và tác vụ nào đã đánh thức nó; Bash và Monitor chạy nền hiện trạng thái và phần cuối output.
- **Đang làm gì:** một dòng trực tiếp dưới timeline cho biết agent đang chạy tool nào, đang chờ agent con,
  đang suy nghĩ, đang trả lời hay đang chờ bạn, kèm thời gian đã trôi.
- **Thư viện:** skill, plugin và MCP server có trên đĩa hoặc được session công bố, kèm “Đã dùng ×n” /
  “Khả dụng” cho session đang chọn và công tắc theo session.
- **Agents & workflow:** agent và workflow có sẵn, của plugin, người dùng và dự án; tạo hoặc sửa agent
  của bạn, tắt trong ccmon mà không mất file hay lịch sử chạy, mở lại các lượt đã chạy.
- **Chạy session từ ccmon:** gõ prompt ngay dưới timeline để tiếp tục một session hoặc mở session mới,
  kèm chip model, effort, chế độ quyền, agent chính, nhắc `@agent` được tô sáng và công tắc bật/tắt
  skill, plugin, MCP, agent, workflow cho từng session.
- **Header gọn:** một thanh mảnh với nút chuyển Luồng / Thư viện / Agents (`Ctrl+1/2/3`).
- **Local; read-only trừ khi bạn chạy session từ ccmon hoặc lưu agent:** không tài khoản, ccmon không
  tự gọi mạng, không telemetry.

<table>
<tr>
<td><img src="website/assets/screen-library.png" alt="Thư viện: skill, phạm vi, mức dùng và công tắc theo session" /></td>
<td><img src="website/assets/screen-agents.png" alt="Trang Agents: chi tiết và lịch sử chạy" /></td>
</tr>
<tr>
<td><img src="website/assets/screen-composer.png" alt="Ô soạn với @Explore, các chip và bảng capability theo session" /></td>
<td><img src="website/assets/screen-diff.png" alt="Diff hai cột" /></td>
</tr>
</table>

## Đọc và ghi gì

| | |
|---|---|
| **Đọc** | `~/.claude/projects/**/*.jsonl` (session, subagent, agent của workflow, bản sao lưu file); skill, agent, workflow, plugin trong `~/.claude` và `.claude/` của project; MCP server từ `~/.claude.json` và `.mcp.json` |
| **Ghi (chỉ khi bạn thao tác)** | trạng thái riêng trong `~/.config/ccmon/` (Windows: `%APPDATA%\ccmon\`); file agent bạn lưu từ trang Agents, sau khi xác nhận; tuỳ chọn bọc status line trong `settings.json` (có sao lưu, gỡ được) |
| **Không bao giờ sửa** | transcript, file skill, plugin, MCP; `settings.json` cho công tắc theo session (dùng cờ khởi chạy) |

Lần mở đầu tiên, ccmon tự tìm dữ liệu Claude Code và bắt đầu theo dõi; tuỳ chọn nằm ở **⚙ Cài đặt**.

## Cài đặt

Tải bộ cài ở [bản phát hành mới nhất](https://github.com/wik1ngst-1103/cc-monitor/releases/latest).

- **Windows 10/11:** chạy `ccmon_<version>_x64-setup.exe` (hoặc `.msi`). Cài cho riêng tài khoản của
  bạn, không cần quyền admin; WebView2 tự cài nếu máy chưa có.
- **Linux:** AppImage chạy trên mọi bản phân phối, hoặc `.deb` cho Debian/Ubuntu:

```bash
chmod +x ccmon_*_amd64.AppImage && ./ccmon_*_amd64.AppImage
# hoặc
sudo apt install ./ccmon_*_amd64.deb
```

### Hạn mức gói của Claude Code (tuỳ chọn)

Claude Code chỉ đưa hạn mức 5 giờ/tuần và kích thước cửa sổ context cho lệnh **status line**. Trong
Cài đặt, ccmon có thể bọc status line hiện tại thành `<ccmon> tee -- <lệnh của bạn>`: lưu một bản JSON
cho ccmon rồi chuyển nguyên cho lệnh cũ, nên status line trông y như trước. `settings.json` được sao
lưu, nút **Gỡ** trả lại như cũ.

## Chạy session từ ccmon

Ô soạn (composer) dưới timeline gửi prompt cho Claude Code thay bạn. ccmon chạy chính lệnh `claude` đã
cài trên máy (`PATH`, `~/.local/bin` hoặc `~/.claude/local`) dưới dạng
`claude -p --input-format stream-json --output-format stream-json [--resume <session>]`; hội thoại vẫn
được đọc từ transcript nên mọi phần xem khác vẫn như cũ.

- **Tiếp tục hoặc mở mới:** gõ vào ô soạn của session đang xem, hoặc bấm **+ Session mới…** trên cây
  session rồi chọn thư mục project. `Enter` gửi, `Shift+Enter` xuống dòng, **Dừng** ngắt lượt đang chạy.
- **Chip:** model, effort và chế độ quyền áp dụng từ lượt sau (ccmon khởi động lại tiến trình giữa hai
  lượt khi chúng đổi; chip “Áp dụng từ lượt sau” hiện trong lúc chờ). Ở chế độ `-p` không có hộp thoại
  duyệt: tool cần duyệt sẽ bị từ chối, trừ khi bạn chọn chế độ thoáng hơn (`acceptEdits`, `plan`, hoặc
  `bypassPermissions` — bỏ qua mọi kiểm tra).
- **Capability cho từng session:** nút ⚙ trong ô soạn (và công tắc ở trang Thư viện) tắt skill, plugin,
  MCP server, agent, workflow chỉ cho session đó, qua cờ `--disallowedTools` / `--settings` lúc khởi
  chạy. `settings.json` của bạn không bị sửa; trạng thái riêng của ccmon nằm ở `~/.config/ccmon/` (Windows: `%APPDATA%\ccmon\`)
  (`overlays/<session>.json`, `defaults.json`, `registry.json`).
- **Session ở terminal (tmux):** session mở ở terminal không bao giờ bị tiếp quản, nhưng nếu chạy trong tmux
  bạn có thể gõ vào đó từ ccmon (prompt, `/model`, `/effort`, Dừng = Esc). Chế độ quyền, agent chính và
  capability (⚙) là cấu hình lúc khởi động, cố định với session khởi động ở terminal. ccmon từ chối gửi khi
  pane đang hiện hộp thoại duyệt quyền hay lựa chọn (phím Enter sẽ trả lời nó); chữ bạn đang gõ dở trong
  pane đó cùng lúc sẽ bị trộn với nội dung ccmon gửi. Ngoài tmux, hoặc bị treo bằng Ctrl+Z, ô soạn bị khoá
  kèm lý do.
- **Điều khiển subagent:** Dừng, Tiếp tục, Đổi agent hoặc Hủy subagent từ header hay thẻ của nó. Ở session
  terminal, Dừng và Hủy là nhờ Claude dừng subagent (subagent tiền cảnh bị ngắt bằng Esc), nên ccmon không
  xác nhận được; thẻ hiện “đang chờ dừng” tới khi nó dừng hẳn. Đổi sang agent vừa tạo hoặc vừa sửa ở session
  do ccmon chạy sẽ khởi động lại session, làm dừng các tác vụ nền khác của nó; hộp chọn báo trước.
- **Nhắc `@agent`:** gõ `@` trong ô soạn để chọn agent; tham chiếu được tô sáng và liệt kê dưới prompt.

### Trung tâm điều khiển: Thư viện và Agents

- **Thư viện** (`Ctrl+2`): skill, plugin, MCP server trên đĩa hoặc được session công bố, kèm phạm vi,
  trạng thái toàn cục, “Đã dùng ×n” / “Khả dụng” cho session đang chọn và công tắc theo session.
- **Agents** (`Ctrl+3`): agent và workflow có sẵn, của plugin, người dùng và dự án, kèm tools, model,
  prompt và lịch sử chạy. Tạo hoặc sửa agent của bạn; tắt **Bật trong ccmon** sẽ chặn agent/workflow đó
  ở mọi session chạy từ ccmon, file và lịch sử vẫn giữ nguyên.
- **Giới hạn, nói thẳng:** skill và agent bị tắt sẽ bị chặn khi gọi, nhưng Claude Code vẫn liệt kê
  chúng cho model; ở session chạy từ ccmon, tool cần duyệt bị từ chối (chế độ `-p` không có hộp thoại
  duyệt); session đang mở ở terminal chỉ điều khiển được khi chạy trong tmux.

## Build từ mã nguồn

```bash
git clone https://github.com/wik1ngst-1103/cc-monitor && cd cc-monitor
cargo test --workspace
./gui/build.sh                       # ứng dụng desktop → target/release/ccmon-gui
cargo run --release -p cc-monitor    # bản terminal
```

## Đóng góp

Báo lỗi hoặc góp ý: mở [issue](https://github.com/wik1ngst-1103/cc-monitor/issues). Sửa code: fork repo
và mở pull request — xem [CONTRIBUTING.md](CONTRIBUTING.md).

## Giấy phép

[MIT](LICENSE) © 2026 Lê Thanh Sơn — tự do sử dụng, sửa đổi, phân phối, kể cả thương mại.

Claude Code là thương hiệu của Anthropic. ccmon không liên kết với Anthropic.
