# Contributing to ccmon

Thanks for helping! ccmon is MIT-licensed: you may use, modify, redistribute and sell it.
To keep one shared source of truth, **changes to the code go through this repository**:

- **Found a bug or have an idea?** Open an [issue](https://github.com/wik1ngst-1103/cc-monitor/issues).
  Search first; add your OS, ccmon version and Claude Code version.
- **Want to change code?** [Fork](https://github.com/wik1ngst-1103/cc-monitor/fork) the repo,
  create a branch, and open a pull request against `main`. For larger changes, open an issue
  first so we can agree on the approach.

## Development setup

Requirements: Rust (stable), Node.js 20+, Yarn 1.x, and the Tauri prerequisites for your OS
(<https://tauri.app/start/prerequisites/>). On Ubuntu:

```bash
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev patchelf
```

```bash
git clone https://github.com/<you>/cc-monitor && cd cc-monitor
cargo test --workspace            # core + GUI backend tests
./gui/build.sh                    # builds the frontend and target/release/ccmon-gui
cargo run --release -p cc-monitor # terminal UI
```

Try the app on fake data instead of your own sessions:

```bash
python3 demo/make_demo.py /tmp/ccmon-demo   # prints env vars to export
```

## Project layout

| Path | What |
|---|---|
| `src/` | Core library (`cc_monitor`): Claude Code transcript parser, live store, TUI binary `ccmon` |
| `gui/src-tauri/` | Desktop app backend (Tauri 2) |
| `gui/ui/` | Desktop app frontend (React + Vite) |
| `website/` | Landing page (GitHub Pages) |
| `demo/` | Demo data generator used for screenshots and CI smoke tests |

Architecture notes: [capability catalog and control layer](docs/architecture/capabilities.md)
(how to add a new capability kind) and [CLI flag spike](docs/architecture/control.md).

## Pull request checklist

- [ ] `cargo test --workspace` passes, `cd gui/ui && yarn typecheck` passes
- [ ] `cargo fmt --all` applied
- [ ] New parsing behaviour has a test with a small fixture (never paste real transcripts:
      they contain private code and prompts)
- [ ] UI changes include a screenshot in the PR description

## Principles

- **Read-only.** ccmon never writes into `~/.claude`, except the explicit,
  opt-in status-line wrapper (with backup and an uninstall button).
- **Local only.** No network calls, no telemetry.
- **Tolerant parsing.** The transcript format is internal to Claude Code and changes; unknown
  lines must be skipped, never crash the app.
