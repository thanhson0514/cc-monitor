# Changelog

All notable changes to ccmon are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

The section for the version in `Cargo.toml` becomes the GitHub release notes
(see `.github/workflows/release.yml`), so write it for users.

## [0.3.2] - 2026-11-27

### Added
- **Type into sessions running in a terminal (tmux).** If Claude Code runs in a
  tmux pane, you can now send prompts from ccmon as if typing in that terminal.
  Model and effort are sent as `/model` and `/effort`, and Stop sends Esc.
  Sessions outside tmux, or suspended with Ctrl+Z, stay view-only and ccmon
  shows the reason. Permission mode, main agent and ⚙ capabilities are fixed
  at launch, so they stay locked for terminal sessions. Two things to know:
  text you are typing in that terminal at the same moment gets mixed with what
  ccmon sends, and the Enter ccmon presses would answer an open permission or
  selection dialog, so ccmon refuses to send while one is on screen.
- **Subagent controls.** The subagent header and its card have **Dừng** (pause),
  **Tiếp tục** (continue), **Đổi agent** (swap to another existing agent or a
  new one, keeping the original prompt verbatim) and **Hủy** (cancel, which the
  main agent treats as failed). Continue relaunches the same agent and points it
  at the stopped transcript, because Claude Code cannot resume a stopped
  subagent. In a terminal session, Dừng and Hủy ask Claude to stop the
  subagent, so ccmon cannot confirm it: the card shows **đang chờ dừng** until
  it has stopped, and Tiếp tục / Đổi agent wait until then. A foreground
  subagent is interrupted with Esc. Swapping to a newly created or edited
  agent in a session run from ccmon restarts that session, which also stops
  its other background tasks; the picker warns you first.
- **Agents page "Session này" scope.** It now shows only the agents and run
  history of the current session.
- **Focus files in chat.** Each file in Quản lý → File has a ⌖ button that jumps
  to the chat tool calls that read or edited it, cycling between them.

### Fixed

- An image the agent reads (for example a screenshot) no longer shows "ảnh không còn" for good when the Read appears before the file is written. ccmon now keeps looking for the file for over a minute and reloads it once the tool finishes, and a half-written image is retried instead of staying broken.

## [0.3.1] - 2026-11-27

### Added
- **Live reply for sessions started from ccmon.** The reply appears word by
  word as Claude writes it, including thinking and a tool's input (for example
  a long command) while it is being composed. Before, nothing showed until
  Claude Code wrote the whole message to the transcript, sometimes tens of
  seconds later. Sessions running in a separate terminal still update from the
  transcript.
- **Images shown inline.** Pasted images appear under the prompt, `[Image #N]`
  opens the image, and reading an image file shows the image instead of
  `[image]`.
- **Clickable paths.** File paths in messages, inline code, tool titles and
  tool output open in the file viewer. Relative paths resolve against the
  session folder. The viewer has an "Open externally" button, and web links
  open in your browser.
- **Fold / unfold in the session tree.** Sessions and agents with sub-items get
  a ▸/▾ toggle. A folded row shows how many items are hidden and how many are
  running. Rows with many sub-items start folded, each section has a fold-all
  button, and your choices are remembered.
- **Website documentation.** New pages: user guide, contributing guide, privacy,
  security policy, code of conduct and license.

### Fixed
- Messages you type while Claude is still working now appear in the thread.
  Before, they were missing because Claude Code stores them differently.

[0.3.2]: https://github.com/wik1ngst-1103/cc-monitor/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/wik1ngst-1103/cc-monitor/compare/v0.3.0...v0.3.1
