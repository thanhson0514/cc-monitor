import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { api, fmtDur, fmtTokens, HistoryProject, StatusInfo, ThreadView, TreeRow } from './api';
import { Chat } from './Chat';
import { Sidebar, Viewer } from './Sidebar';
import { ChangesPanel } from './Changes';
import { Setup } from './Setup';
import logo from './assets/logo.png';

const PAGE = 400;

const STATUS_DOT: Record<string, string> = { running: '●', done: '✓', error: '✗', idle: '○' };

function TreeRows({ rows, selected, onSelect }: { rows: TreeRow[]; selected: string | null; onSelect: (k: string) => void }) {
    return (
        <>
            {rows.map((r) => (
                <button
                    key={r.key}
                    type="button"
                    className={`tree-row ${r.key === selected ? 'selected' : ''} kind-${r.kind}`}
                    style={{ paddingLeft: 10 + r.depth * 16 }}
                    onClick={() => onSelect(r.key)}
                    title={r.tmux ? `${r.label}\ntmux ${r.tmux}` : r.label}
                >
                    <span className={`dot st-${r.status}`}>{STATUS_DOT[r.status]}</span>
                    {r.kind === 'workflow' && <span className="icon">⚙</span>}
                    {r.kind === 'main' && r.tool && <span className={`tool-tag tool-${r.tool}`}>{r.tool === 'codex' ? 'Codex' : 'Claude'}</span>}
                    {r.kind === 'agent' && r.agentType && <span className="agent-type">{r.agentType}</span>}
                    <span className="tree-label">{r.label}</span>
                    {r.kind === 'main' && r.live && (
                        <span className={`live-badge live-${r.live}`}>{r.live === 'busy' ? 'đang làm' : 'chờ'}</span>
                    )}
                    {r.progress && (
                        <span className="progress">
                            {r.progress[0]}/{r.progress[1]}
                        </span>
                    )}
                </button>
            ))}
        </>
    );
}

/** Split the flat tree into per-session blocks (a depth-0 row and its descendants). */
function sessionBlocks(rows: TreeRow[]): TreeRow[][] {
    const out: TreeRow[][] = [];
    for (const r of rows) {
        if (r.depth === 0 || out.length === 0) out.push([r]);
        else out[out.length - 1].push(r);
    }
    return out;
}

function ago(ts: number): string {
    const m = Math.round((Date.now() - ts) / 60000);
    if (m < 60) return `${m} phút trước`;
    const h = Math.round(m / 60);
    if (h < 48) return `${h} giờ trước`;
    return `${Math.round(h / 24)} ngày trước`;
}

function PanelIcon({ open }: { open: boolean }) {
    return (
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden="true">
            <rect x="1.75" y="2.75" width="12.5" height="10.5" rx="2" />
            <path d="M6 3v10" />
            <path d={open ? 'M11 6.5 9.5 8l1.5 1.5' : 'M9.5 6.5 11 8l-1.5 1.5'} strokeLinecap="round" strokeLinejoin="round" />
        </svg>
    );
}

/** Collapsed tree: logo, expand button and one dot per open session. */
function TreeRail({
    rows,
    selected,
    onSelect,
    onExpand
}: {
    rows: TreeRow[];
    selected: string | null;
    onSelect: (k: string) => void;
    onExpand: () => void;
}) {
    const sessions = rows.filter((r) => r.depth === 0 && r.open);
    return (
        <nav className="tree tree-rail">
            <img className="brand-logo" src={logo} alt="ccmon" />
            <button type="button" className="icon-btn" onClick={onExpand} title="Mở thanh bên (Ctrl+B)">
                <PanelIcon open={false} />
            </button>
            <div className="rail-sep" />
            {sessions.map((r) => (
                <button
                    key={r.key}
                    type="button"
                    className={`rail-item ${selected === r.key ? 'selected' : ''}`}
                    onClick={() => onSelect(r.key)}
                    title={`${r.label}\n${r.live === 'busy' ? 'Đang làm' : 'Đang chờ'}`}
                >
                    <span className={`rail-dot ${r.live === 'busy' ? 'busy' : ''}`} />
                    <span className="rail-initial">{r.label.slice(0, 1).toUpperCase()}</span>
                </button>
            ))}
        </nav>
    );
}

function Tree({
    rows,
    selected,
    onSelect,
    onOpenOld,
    onSettings,
    onCollapse
}: {
    rows: TreeRow[];
    selected: string | null;
    onSelect: (k: string) => void;
    onOpenOld: (sid: string) => void;
    onSettings: () => void;
    onCollapse: () => void;
}) {
    const blocks = sessionBlocks(rows);
    const open = blocks.filter((b) => b[0].open);
    const recent = blocks.filter((b) => !b[0].open);
    const [showOld, setShowOld] = useState(false);
    const [history, setHistory] = useState<HistoryProject[] | null>(null);
    const [days, setDays] = useState(30);
    const [expanded, setExpanded] = useState<Set<string>>(new Set());
    const tracked = new Set(blocks.map((b) => b[0].key.slice(2)));

    useEffect(() => {
        if (showOld) api.history(days).then(setHistory);
    }, [showOld, days]);

    const toggle = (p: string) =>
        setExpanded((s) => {
            const n = new Set(s);
            if (n.has(p)) n.delete(p);
            else n.add(p);
            return n;
        });

    return (
        <nav className="tree">
            <div className="brand">
                <img className="brand-logo" src={logo} alt="" /> ccmon
                <button type="button" className="gear" onClick={onSettings} title="Cài đặt: chọn Claude Code / Codex">
                    ⚙
                </button>
                <button type="button" className="icon-btn" onClick={onCollapse} title="Thu gọn thanh bên (Ctrl+B)">
                    <PanelIcon open />
                </button>
            </div>
            <div className="pane-title">Đang mở · {open.length}</div>
            {open.length === 0 && <div className="muted pad small">Không có phiên nào đang chạy.</div>}
            {open.map((b) => (
                <TreeRows key={b[0].key} rows={b} selected={selected} onSelect={onSelect} />
            ))}
            {recent.length > 0 && <div className="pane-title">Đã đóng gần đây / đang xem</div>}
            {recent.map((b) => (
                <TreeRows key={b[0].key} rows={b} selected={selected} onSelect={onSelect} />
            ))}
            <button type="button" className="pane-title pane-toggle" onClick={() => setShowOld(!showOld)}>
                {showOld ? '▾' : '▸'} Phiên cũ ({days} ngày)
            </button>
            {showOld && !history && <div className="muted pad small">Đang tải…</div>}
            {showOld &&
                history?.map((p) => (
                    <div key={p.project} className="hist-project">
                        <button type="button" className="hist-head" onClick={() => toggle(p.project)} title={p.project}>
                            <span>{expanded.has(p.project) ? '▾' : '▸'}</span>
                            <span className={`tool-tag tool-${p.tool}`}>{p.tool === 'codex' ? 'Codex' : 'Claude'}</span>
                            <span className="tree-label">{p.label}</span>
                            <span className="progress">{p.sessions.length}</span>
                        </button>
                        {expanded.has(p.project) &&
                            p.sessions
                                .filter((s) => !tracked.has(s.sid))
                                .map((s) => (
                                    <button
                                        key={s.sid}
                                        type="button"
                                        className="hist-row"
                                        onClick={() => onOpenOld(s.sid)}
                                        title={s.sid}
                                    >
                                        <span className="tree-label">{s.title ?? s.sid.slice(0, 8)}</span>
                                        <span className="progress">{ago(s.mtime)}</span>
                                    </button>
                                ))}
                    </div>
                ))}
            {showOld && history && (
                <button type="button" className="more-btn" onClick={() => setDays((d) => d + 30)}>
                    Xem thêm 30 ngày
                </button>
            )}
        </nav>
    );
}

/** Small labelled usage bar; colour shifts as it fills. */
function Meter({ label, value, detail, title }: { label: string; value: number | null; detail?: string; title: string }) {
    const level = value == null ? 'none' : value >= 85 ? 'high' : value >= 60 ? 'mid' : 'low';
    return (
        <span className={`meter meter-${level}`} title={title}>
            <span className="sb-label">{label}</span>
            <span className="meter-track">
                <span className="meter-fill" style={{ width: `${Math.min(100, Math.max(0, value ?? 0))}%` }} />
            </span>
            <span className="sb-val">{value == null ? '—' : `${Math.round(value)}%`}</span>
            {detail && <span className="sb-label">{detail}</span>}
        </span>
    );
}

function StatusBar({ s }: { s: StatusInfo | null }) {
    if (!s) return <footer className="statusbar" />;
    const gb = (b: number) => `${(b / 1024 ** 3).toFixed(1)}G`;
    const tok = (n: number | null) => (n == null ? '—' : fmtTokens(n).replace('K', 'k'));
    const ctxDetail = s.ctx != null || s.win != null ? `${tok(s.ctx)}/${tok(s.win)}${s.winEstimated ? '?' : ''}` : undefined;
    return (
        <footer className="statusbar">
            <div className="sb-group">
                <span className="sb-chip sb-model" title="Model">
                    {s.model ?? '—'}
                </span>
                {s.effort && (
                    <span className="sb-item" title="Mức suy nghĩ (effort)">
                        <span className="sb-label">effort</span> <span className="sb-val">{s.effort}</span>
                    </span>
                )}
                {s.branch && (
                    <span className="sb-item" title="Nhánh git">
                        <span className="sb-label">⎇</span> <span className="sb-val">{s.branch}</span>
                        {s.lines && (
                            <span className="sb-lines">
                                <span className="add">+{s.lines[0]}</span> <span className="del">−{s.lines[1]}</span>
                            </span>
                        )}
                    </span>
                )}
            </div>
            <div className="sb-group">
                <Meter
                    label="Ctx"
                    value={s.ctxPct}
                    detail={ctxDetail}
                    title={`Ngữ cảnh đã dùng${s.winEstimated ? ' (cửa sổ ước tính)' : ''}`}
                />
                <Meter label="5h" value={s.fiveH} title="Hạn mức phiên 5 giờ đã dùng" />
                <Meter label="Tuần" value={s.sevenD} title="Hạn mức tuần đã dùng" />
            </div>
            <div className="sb-group sb-right">
                {s.compactions > 0 && (
                    <span className="sb-item" title="Số lần compact">
                        <span className="sb-label">↻</span> <span className="sb-val">{s.compactions}</span>
                    </span>
                )}
                {s.mem && (
                    <span className="sb-item" title="Bộ nhớ máy">
                        <span className="sb-label">RAM</span>{' '}
                        <span className="sb-val">
                            {gb(s.mem[0])}/{gb(s.mem[1])}
                        </span>
                    </span>
                )}
                <span className="sb-item" title="Phiên hoạt động · agent đang chạy">
                    <span className="sb-val">{s.sessions}</span> <span className="sb-label">phiên</span>
                    <span className="sb-label"> · </span>
                    <span className={`sb-val ${s.runningAgents > 0 ? 'st-running' : ''}`}>{s.runningAgents}</span>{' '}
                    <span className="sb-label">agent</span>
                </span>
            </div>
        </footer>
    );
}

type Activity = { state: 'busy' | 'waiting' | 'stopped'; text: string; detail?: string; since: number | null };

/** What the selected thread is doing right now, derived from its last items. */
function activityOf(view: ThreadView | null, row: TreeRow | undefined, rows: TreeRow[]): Activity | null {
    if (!view || !row) return null;
    const busy = row.kind === 'main' ? row.live === 'busy' || (row.live == null && row.status === 'running') : row.status === 'running';
    if (!busy) {
        if (row.kind === 'main' && row.live) return { state: 'waiting', text: 'Đang chờ bạn nhập', since: null };
        return { state: 'stopped', text: row.status === 'error' ? 'Đã dừng do lỗi' : 'Không hoạt động', since: null };
    }
    // Sub-agents still running under this thread.
    const i = rows.findIndex((r) => r.key === row.key);
    let subs = 0;
    if (i >= 0)
        for (const r of rows.slice(i + 1)) {
            if (r.depth <= row.depth) break;
            if (r.status === 'running' && r.kind !== 'workflow') subs++;
        }
    const subText = subs > 0 ? `${subs} agent con đang chạy` : undefined;

    const items = view.items;
    let turnStart: number | null = null;
    for (let k = items.length - 1; k >= 0; k--) {
        const it = items[k];
        if (it.type === 'prompt') {
            turnStart = it.ts;
            break;
        }
    }
    const running = [...items].reverse().find((it): it is Extract<typeof it, { type: 'tool' }> => it.type === 'tool' && it.state === 'running');
    if (running) {
        if (running.link)
            return {
                state: 'busy',
                text: `Đang chờ ${running.link.kind === 'workflow' ? 'workflow' : 'agent'}`,
                detail: running.link.label ?? running.summary,
                since: running.started ?? turnStart
            };
        return { state: 'busy', text: `Đang chạy ${running.name}`, detail: running.summary || subText, since: running.started ?? turnStart };
    }
    const last = items[items.length - 1];
    const text =
        !last || last.type === 'prompt'
            ? 'Đang xử lý yêu cầu'
            : last.type === 'text'
              ? 'Đang trả lời'
              : last.type === 'compact'
                ? 'Đang compact ngữ cảnh'
                : 'Đang suy nghĩ';
    return { state: 'busy', text, detail: subText, since: turnStart };
}

function ActivityBar({ a, now }: { a: Activity | null; now: number }) {
    if (!a) return null;
    return (
        <div className={`activity act-${a.state}`} role="status" aria-live="polite">
            <span className="act-dot" />
            <span className="act-text">{a.text}</span>
            {a.detail && <span className="act-detail">{a.detail}</span>}
            {a.state === 'busy' && a.since != null && <span className="act-time">{fmtDur(Math.max(0, now - a.since))}</span>}
        </div>
    );
}

function WorkflowView({ rows, wfKey, onSelect }: { rows: TreeRow[]; wfKey: string; onSelect: (k: string) => void }) {
    const i = rows.findIndex((r) => r.key === wfKey);
    if (i < 0) return null;
    const depth = rows[i].depth;
    const members: TreeRow[] = [];
    for (const r of rows.slice(i + 1)) {
        if (r.depth <= depth) break;
        members.push(r);
    }
    return (
        <div className="wf-view">
            {members.map((m) => (
                <button key={m.key} type="button" className="wf-member" onClick={() => onSelect(m.key)}>
                    <span className={`dot st-${m.status}`}>{STATUS_DOT[m.status]}</span>
                    <span>{m.label}</span>
                    <span className="link-btn">Mở luồng →</span>
                </button>
            ))}
        </div>
    );
}

export default function App() {
    const [rows, setRows] = useState<TreeRow[]>([]);
    const [selected, setSelected] = useState<string | null>(null);
    const [userPicked, setUserPicked] = useState(false);
    const [view, setView] = useState<ThreadView | null>(null);
    const [status, setStatus] = useState<StatusInfo | null>(null);
    const [tail, setTail] = useState(PAGE);
    const [now, setNow] = useState(Date.now());
    const [history, setHistory] = useState<string[]>([]);
    const [panel, setPanel] = useState<'assets' | 'changes' | null>('assets');
    const [setup, setSetup] = useState<'first' | 'settings' | null>(null);
    useEffect(() => {
        api.setup().then((i) => !i.config.setupDone && setSetup('first'));
    }, []);
    const showSidebar = panel !== null;
    const [treeOpen, setTreeOpen] = useState(() => {
        try {
            return localStorage.getItem('ccmon.treeOpen') !== '0';
        } catch {
            return true;
        }
    });
    useEffect(() => {
        try {
            localStorage.setItem('ccmon.treeOpen', treeOpen ? '1' : '0');
        } catch {
            /* storage unavailable: keep in memory only */
        }
    }, [treeOpen]);
    useEffect(() => {
        const k = (e: KeyboardEvent) => {
            if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'b') {
                e.preventDefault();
                setTreeOpen((o) => !o);
            }
        };
        window.addEventListener('keydown', k);
        return () => window.removeEventListener('keydown', k);
    }, []);
    const [assetTick, setAssetTick] = useState(0);
    const pendingOpen = useRef<string | null>(null);
    const scroller = useRef<HTMLDivElement>(null);
    const content = useRef<HTMLDivElement>(null);
    const stickBottom = useRef(true);
    const [following, setFollowing] = useState(true);
    const selectedRef = useRef<string | null>(null);
    selectedRef.current = selected;

    const loadTree = useCallback(() => api.tree().then(setRows), []);
    const loadThread = useCallback((key: string, t: number) => {
        api.thread(key, t).then((v) => {
            if (selectedRef.current === key) setView(v);
        });
    }, []);

    // Auto-select the most recently active session until the user picks a node.
    useEffect(() => {
        if (pendingOpen.current && rows.some((r) => r.key === pendingOpen.current)) {
            const k = pendingOpen.current;
            pendingOpen.current = null;
            setSelected(k);
            return;
        }
        if (rows.length === 0) return;
        const valid = selected && rows.some((r) => r.key === selected);
        const first = (rows.find((r) => r.depth === 0 && r.live === 'busy') ?? rows.find((r) => r.depth === 0 && r.live) ?? rows[0]).key;
        if (!valid || (!userPicked && first !== selected)) setSelected(first);
    }, [rows, selected, userPicked]);

    useEffect(() => {
        if (!selected) return;
        stickBottom.current = true;
        setFollowing(true);
        loadThread(selected, tail);
    }, [selected, tail, loadThread]);

    const [startup, setStartup] = useState<{ diff: string | null; view: string | null }>({ diff: null, view: null });
    useEffect(() => {
        api.startup().then((s) => {
            setStartup(s);
            if (s.diff) setPanel('changes');
        });
        api.initialSelect().then((k) => {
            if (k) {
                setUserPicked(true);
                pendingOpen.current = k;
                setSelected(k);
                // An old session is not tracked yet: ask the worker to load it.
                const sid = k.split(':')[1];
                if (sid) api.openSession(sid);
            }
        });
    }, []);

    useEffect(() => {
        loadTree();
        const un = listen<{ keys: string[]; tree: boolean }>('store-changed', (e) => {
            if (e.payload.tree) loadTree();
            setAssetTick((t) => t + 1);
            const sel = selectedRef.current;
            if (sel && (e.payload.keys.includes(sel) || sel.startsWith('w:'))) loadThread(sel, tailRef.current);
        });
        const timer = setInterval(() => {
            setNow(Date.now());
            api.status(selectedRef.current).then(setStatus);
        }, 1000);
        return () => {
            un.then((f) => f());
            clearInterval(timer);
        };
    }, [loadTree, loadThread]);

    const tailRef = useRef(tail);
    tailRef.current = tail;

    useLayoutEffect(() => {
        const el = scroller.current;
        if (el && stickBottom.current) {
            el.scrollTop = el.scrollHeight;
            requestAnimationFrame(() => {
                if (stickBottom.current) el.scrollTop = el.scrollHeight;
            });
        }
    }, [view]);

    // Content can grow after commit (markdown, fonts, expanded cards): keep pinned.
    useEffect(() => {
        const el = scroller.current;
        const inner = content.current;
        if (!el || !inner) return;
        const ro = new ResizeObserver(() => {
            if (stickBottom.current) el.scrollTop = el.scrollHeight;
        });
        ro.observe(inner);
        return () => ro.disconnect();
    }, []);

    // Only user input may turn following off; layout-driven scroll events are ignored.
    const userInput = useRef(0);
    const markUser = () => (userInput.current = Date.now());
    const onScroll = () => {
        const el = scroller.current;
        if (!el || Date.now() - userInput.current > 400) {
            if (el && stickBottom.current) el.scrollTop = el.scrollHeight;
            return;
        }
        const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
        stickBottom.current = atBottom;
        setFollowing(atBottom);
    };

    const select = (key: string) => {
        if (key === selected) return;
        if (selected) setHistory((h) => [...h.slice(-50), selected]);
        setUserPicked(true);
        setTail(PAGE);
        setView(null);
        setSelected(key);
    };

    const back = () => {
        const prev = history[history.length - 1];
        if (!prev) return;
        setHistory((h) => h.slice(0, -1));
        setTail(PAGE);
        setView(null);
        setSelected(prev);
    };

    const row = rows.find((r) => r.key === selected);

    const openOld = (sid: string) => {
        setUserPicked(true);
        pendingOpen.current = `m:${sid}`;
        api.openSession(sid);
    };

    return (
        <div className={`app ${showSidebar ? 'with-sidebar' : ''} ${treeOpen ? '' : 'tree-collapsed'}`}>
            {treeOpen ? (
                <Tree
                    rows={rows}
                    selected={selected}
                    onSelect={select}
                    onOpenOld={openOld}
                    onSettings={() => setSetup('settings')}
                    onCollapse={() => setTreeOpen(false)}
                />
            ) : (
                <TreeRail rows={rows} selected={selected} onSelect={select} onExpand={() => setTreeOpen(true)} />
            )}
            <main className="main">
                <header className="thread-head">
                    {history.length > 0 && (
                        <button className="back-btn" type="button" onClick={back} title="Quay lại luồng trước">
                            ←
                        </button>
                    )}
                    <span className="thread-title">{row?.label ?? view?.title ?? 'ccmon'}</span>
                    {row && <span className={`badge st-${row.status}`}>{row.status}</span>}
                    <div className="panel-btns">
                        <button
                            type="button"
                            className={`sb-toggle ${panel === 'assets' ? 'on' : ''}`}
                            onClick={() => setPanel(panel === 'assets' ? null : 'assets')}
                            title="Ảnh, file, log của luồng"
                        >
                            🗂 Quản lý
                        </button>
                        <button
                            type="button"
                            className={`sb-toggle ${panel === 'changes' ? 'on' : ''}`}
                            onClick={() => setPanel(panel === 'changes' ? null : 'changes')}
                            title="File đã sửa trong phiên, xem diff hai cột"
                        >
                            ± Thay đổi
                        </button>
                    </div>
                    {!following && (
                        <button
                            className="follow-btn"
                            type="button"
                            onClick={() => {
                                stickBottom.current = true;
                                setFollowing(true);
                                const el = scroller.current;
                                if (el) el.scrollTop = el.scrollHeight;
                            }}
                        >
                            ↓ Mới nhất
                        </button>
                    )}
                </header>
                <div
                    className="chat"
                    ref={scroller}
                    onScroll={onScroll}
                    onWheel={markUser}
                    onMouseDown={markUser}
                    onKeyDown={markUser}
                    onTouchMove={markUser}
                    tabIndex={0}
                >
                    <div ref={content}>
                    {view && view.total > view.items.length && (
                        <button className="more-btn" type="button" onClick={() => setTail((t) => t + PAGE)}>
                            Hiện thêm {Math.min(PAGE, view.total - view.items.length)} mục cũ hơn ({view.total - view.items.length} còn lại)
                        </button>
                    )}
                    {selected?.startsWith('w:') ? (
                        <WorkflowView rows={rows} wfKey={selected} onSelect={select} />
                    ) : view ? (
                        <Chat view={view} now={now} onOpen={select} />
                    ) : (
                        <div className="muted pad">Đang tải…</div>
                    )}
                    </div>
                </div>
                <ActivityBar a={activityOf(view && view.key === selected ? view : null, row, rows)} now={now} />
                <StatusBar s={status} />
            </main>
            {panel === 'assets' && selected && (
                <Sidebar nodeKey={selected} refreshTick={Math.floor(assetTick / 8)} onOpenThread={select} />
            )}
            {panel === 'changes' && selected && <ChangesPanel nodeKey={selected} refreshTick={Math.floor(assetTick / 8)} initial={startup.diff} />}
            {setup && (
                <Setup
                    canClose={setup === 'settings'}
                    onDone={() => {
                        setSetup(null);
                        loadTree();
                    }}
                />
            )}
            {startup.view && <Viewer path={startup.view} onClose={() => setStartup((s) => ({ ...s, view: null }))} />}
        </div>
    );
}
