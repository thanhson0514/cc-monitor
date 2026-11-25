mod app;
mod ui;

use cc_monitor::{config, discovery, event, metrics, platform, setup, store, tail, tee, thread};

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::event::{
    self as cev, DisableMouseCapture, EnableMouseCapture, Event as CEvent, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Bảng theo dõi chỉ-đọc cho Claude Code: phiên, subagent, workflow và tiến độ từng bước.
#[derive(Parser)]
#[command(name = "ccmon", version)]
struct Cli {
    /// Chỉ theo dõi phiên có id bắt đầu bằng chuỗi này
    #[arg(long)]
    session: Option<String>,
    /// Hiện cả phiên không còn hoạt động trong 24 giờ qua
    #[arg(long)]
    all: bool,
    /// Thư mục projects của Claude Code (mặc định ~/.claude/projects)
    #[arg(long, hide = true)]
    root: Option<PathBuf>,
    /// Đo thời gian nạp một transcript rồi thoát
    #[arg(long, hide = true)]
    bench_load: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Dùng trong statusLine: lưu JSON status line cho ccmon rồi chuyển cho lệnh gốc
    Tee {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Bọc statusLine trong ~/.claude/settings.json bằng `ccmon tee --` (có hỏi trước)
    Setup {
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Gỡ lớp bọc `ccmon tee --` khỏi statusLine
    Uninstall,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Some(Cmd::Tee { command }) => {
            let cmd: Vec<String> = command.into_iter().skip_while(|a| a == "--").collect();
            std::process::exit(tee::run_tee(&cmd));
        }
        Some(Cmd::Setup { yes }) => return setup::setup(&setup::settings_path(), yes),
        Some(Cmd::Uninstall) => return setup::uninstall(&setup::settings_path()),
        None => {}
    }
    if let Some(p) = cli.bench_load {
        return bench_load(&p);
    }
    let opts = discovery::ScanOpts {
        session: cli.session,
        window_secs: if cli.all {
            86_400
        } else {
            (store::ACTIVE_MS / 1000 * 3) as u64
        },
    };
    run_tui(cli.root.unwrap_or_else(discovery::projects_root), opts)
}

fn bench_load(p: &std::path::Path) -> Result<()> {
    let t0 = Instant::now();
    let mut tail = tail::Tail::new(p.to_path_buf());
    let lines = match tail.read_new()? {
        tail::TailRead::Lines(l) | tail::TailRead::Reset(l) => l,
    };
    let t1 = t0.elapsed();
    let mut th = thread::Thread::new();
    let n = lines.len();
    for l in &lines {
        if let Some(e) = event::parse_line(l) {
            th.push(e);
        }
    }
    let t2 = t0.elapsed();
    let items = th.items().len();
    let t3 = t0.elapsed();
    println!(
        "{} dòng, {} mục · đọc {:?} · parse {:?} · dựng timeline {:?} · tổng {:?}",
        n,
        items,
        t1,
        t2 - t1,
        t3 - t2,
        t3
    );
    Ok(())
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
}

fn run_tui(root: PathBuf, opts: discovery::ScanOpts) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let cfg = config::load();
    let shared = config::SharedConfig::new(std::sync::Mutex::new(cfg));
    std::thread::spawn(move || {
        let mut worker = discovery::Worker::new(root, opts, tx).with_config(shared);
        if config::CODEX_ENABLED {
            worker = worker.with_codex(platform::codex_home());
        }
        worker.run()
    });

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let res = event_loop(&mut term, rx);
    restore_terminal();
    res
}

fn event_loop(
    term: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    rx: mpsc::Receiver<discovery::Msg>,
) -> Result<()> {
    let tz = platform::local_offset_ms();
    let plain = std::env::var_os("NO_POWERLINE").is_some();
    let tee_dir = tee::cache_dir();
    let mut app = app::App::new(store::Store::default());
    let mut dirty = true;
    let mut last_tick = Instant::now();
    let mut tree_scroll = 0usize;
    let mut tl_scroll = 0usize;
    let mut mem = None;
    loop {
        loop {
            match rx.try_recv() {
                Ok(m) => dirty |= app.store.apply(m),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        if last_tick.elapsed() >= Duration::from_secs(1) {
            last_tick = Instant::now();
            dirty = true;
        }
        if dirty {
            dirty = false;
            let now = discovery::now_ms();
            mem = platform::memory().or(mem);
            app.refresh_rows(now);
            let area = term.size()?;
            let area = ratatui::layout::Rect::new(0, 0, area.width, area.height);
            let lay = ui::layouts(area, &app);
            let view = ui::build_timeline(&mut app, lay.timeline.width, now, tz);
            app.clamp_cursor(view.item_line.len());
            let h = lay.timeline.height.saturating_sub(2) as usize;
            app.page = h.max(1);
            tl_scroll = ui::timeline_scroll(tl_scroll, &view, app.cursor, app.follow, h);
            let sel_idx = app
                .rows
                .iter()
                .position(|r| Some(&r.id) == app.selected.as_ref())
                .unwrap_or(0);
            tree_scroll = ui::tree_scroll(
                tree_scroll,
                sel_idx,
                lay.tree
                    .map(|t| t.height.saturating_sub(2) as usize)
                    .unwrap_or(1)
                    .max(1),
            );
            let metrics = match app.selected.as_ref() {
                Some(id) => {
                    let sid = id.session().to_string();
                    let tee = metrics::load_tee(&tee_dir, &sid, now);
                    let main = app.store.thread(&store::NodeId::Main(sid.clone()));
                    match main {
                        Some(t) => metrics::collect(t, tee.as_ref(), mem),
                        None => metrics::Metrics::default(),
                    }
                }
                None => metrics::Metrics {
                    mem,
                    ..Default::default()
                },
            };
            let frame = ui::Frame0 {
                timeline: &view,
                metrics: &metrics,
                running_agents: app.store.running_agents(now),
                ready: app.store.ready,
                plain,
                tree_scroll,
                timeline_scroll: tl_scroll,
            };
            term.draw(|f| ui::draw(f, &app, &frame))?;
        }
        if cev::poll(Duration::from_millis(100))? {
            let size = term.size()?;
            let lay = ui::layouts(
                ratatui::layout::Rect::new(0, 0, size.width, size.height),
                &app,
            );
            match cev::read()? {
                CEvent::Key(k) if k.kind == KeyEventKind::Press => {
                    app.on_key(k, lay.tree.is_some())
                }
                CEvent::Mouse(m) => app.on_mouse(m, lay.tree.map(|t| t.width).unwrap_or(0)),
                CEvent::Resize(..) => {}
                _ => continue,
            }
            dirty = true;
            if app.quit {
                return Ok(());
            }
        }
    }
}
