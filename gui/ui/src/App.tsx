import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { api, Draft, FileHit, fmtDur, fmtTokens, HistoryProject, RunState, StatusInfo, ThreadView, TreeRow } from './api';
import { OpenPath } from './open';
import { Chat } from './Chat';
import { Sidebar, Viewer } from './Sidebar';
import { ChangesPanel } from './Changes';
import { Setup } from './Setup';
import { Library } from './Library';
import { Agents } from './Agents';
import { SubagentControl } from './SubagentControl';
import { Composer } from './Composer';
import { AppHeader, AssetsIcon, DiffIcon, HeadIconBtn, Page, PAGE_TITLE } from './Header';
import logo from './assets/logo.png';

const PAGE = 400;
/** How long a just-started session may take to show up in the tree before we say so. */
const START_TIMEOUT_MS = 15000;

const STATUS_DOT: Record<string, string> = { running: '●', done: '✓', error: '✗', idle: '○' };

/** Children above this count start folded (until the user toggles the node). */
const AUTO_FOLD = 6;

type Fold = Record<string, boolean>;

/** Per row: number of descendants and how many of them are running. */
function subtreeStats(rows: TreeRow[]): { kids: number; running: number; end: number }[] {
    return rows.map((r, i) => {
        let j = i + 1;
        let running = 0;
        while (j < rows.length && rows[j].depth > r.depth) {
            if (rows[j].status === 'running') running++;
            j++;
        }
        return { kids: j - i - 1, running, end: j };
    });
}

const isFolded = (fold: Fold, key: string, kids: number) => kids > 0 && (fold[key] ?? kids > AUTO_FOLD);

function TreeRows({
    rows,
    selected,
    onSelect,
    fold,
    onFold
}: {
    rows: TreeRow[];
    selected: string | null;
    onSelect: (k: string) => void;
    fold: Fold;
    onFold: (key: string, folded: boolean) => void;
}) {
    const stats = subtreeStats(rows);
    const sel = rows.findIndex((r) => r.key === selected);
    const shown: number[] = [];
    for (let i = 0; i < rows.length; ) {
        shown.push(i);
        const st = stats[i];
        // A folded node still opens when the selected row is inside it.
        const holdsSel = sel > i && sel < st.end;
        i = isFolded(fold, rows[i].key, st.kids) && !holdsSel ? st.end : i + 1;
    }
    return (
        <>
            {shown.map((i) => {
                const r = rows[i];
                const st = stats[i];
                const folded = isFolded(fold, r.key, st.kids) && !(sel > i && sel < st.end);
                return (
                    <button
                        key={r.key}
                        type="button"
                        className={`tree-row ${r.key === selected ? 'selected' : ''} kind-${r.kind}`}
                        style={{ paddingLeft: 4 + r.depth * 16 }}
                        onClick={() => onSelect(r.key)}
                        title={r.tmux ? `${r.label}\ntmux ${r.tmux}` : r.label}
                    >
                        {st.kids > 0 ? (
                            <span
                                className="twisty"
                                role="button"
                                tabIndex={-1}
                                aria-expanded={!folded}
                                title={folded ? `Mở ${st.kids} mục con` : 'Thu gọn mục con'}
                                onClick={(e) => {
                                    e.stopPropagation();
                                    onFold(r.key, !folded);
                                }}
                            >
                                {folded ? '▸' : '▾'}
                            </span>
                        ) : (
                            <span className="twisty" />
                        )}
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
                        {folded && (
                            <span className="fold-count" title={`${st.kids} mục con đang ẩn`}>
                                {st.running > 0 && <span className="fold-running">●{st.running}</span>}+{st.kids}
                            </span>
                        )}
                    </button>
                );
            })}
        </>
    );
}

function useFold(): [Fold, (key: string, folded: boolean) => void, (keys: string[], folded: boolean) => void] {
    const [fold, setFold] = useState<Fold>(() => {
        try {
            return JSON.parse(localStorage.getItem('ccmon.treeFold') ?? '{}') as Fold;
        } catch {
            return {};
        }
    });
    useEffect(() => {
        try {
            // Keep the map small: only the most recent choices.
            const entries = Object.entries(fold).slice(-300);
            localStorage.setItem('ccmon.treeFold', JSON.stringify(Object.fromEntries(entries)));
        } catch {
            /* storage unavailable: keep in memory only */
        }
    }, [fold]);
    const one = useCallback((key: string, folded: boolean) => setFold((f) => ({ ...f, [key]: folded })), []);
    const all = useCallback(
        (keys: string[], folded: boolean) => setFold((f) => ({ ...f, ...Object.fromEntries(keys.map((k) => [k, folded])) })),
        []
    );
    return [fold, one, all];
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
    onCollapse,
    onNewSession
}: {
    rows: TreeRow[];
    selected: string | null;
    onSelect: (k: string) => void;
    onOpenOld: (sid: string) => void;
    onSettings: () => void;
    onCollapse: () => void;
    onNewSession: () => void;
}) {
    const blocks = sessionBlocks(rows);
    const open = blocks.filter((b) => b[0].open);
    const recent = blocks.filter((b) => !b[0].open);
    const [fold, setFold, setFoldAll] = useFold();
    const parents = (bs: TreeRow[][]) =>
        bs.flatMap((b) => {
            const st = subtreeStats(b);
            return b.flatMap((r, i) => (st[i].kids > 0 ? [{ key: r.key, kids: st[i].kids }] : []));
        });
    const FoldAll = ({ bs }: { bs: TreeRow[][] }) => {
        const ps = parents(bs);
        if (ps.length === 0) return null;
        const anyOpen = ps.some((p) => !isFolded(fold, p.key, p.kids));
        return (
            <button
                type="button"
                className="fold-all"
                onClick={() => setFoldAll(ps.map((p) => p.key), anyOpen)}
                title={anyOpen ? 'Thu gọn tất cả subagent/workflow' : 'Mở tất cả subagent/workflow'}
                aria-label={anyOpen ? 'Thu gọn tất cả' : 'Mở tất cả'}
            >
                {anyOpen ? '⊟' : '⊞'}
            </button>
        );
    };
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
            <div className="pane-title pane-title-row">
                <span>Đang mở · {open.length}</span>
                <FoldAll bs={open} />
                <button type="button" className="new-sess-btn" onClick={onNewSession} title="Chạy một session Claude Code mới từ ccmon">
                    + Session mới…
                </button>
            </div>
            {open.length === 0 && <div className="muted pad small">Không có phiên nào đang chạy.</div>}
            {open.map((b) => (
                <TreeRows key={b[0].key} rows={b} selected={selected} onSelect={onSelect} fold={fold} onFold={setFold} />
            ))}
            {recent.length > 0 && (
                <div className="pane-title pane-title-row">
                    <span>Đã đóng gần đây / đang xem</span>
                    <FoldAll bs={recent} />
                </div>
            )}
            {recent.map((b) => (
                <TreeRows key={b[0].key} rows={b} selected={selected} onSelect={onSelect} fold={fold} onFold={setFold} />
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

/** Pick the working directory of a new managed session (projects ccmon already knows). */
function NewSessionPicker({ onPick, onClose }: { onPick: (cwd: string) => void; onClose: () => void }) {
    const [list, setList] = useState<string[] | null>(null);
    const [filter, setFilter] = useState('');
    useEffect(() => {
        api.projects().then(setList).catch(() => setList([]));
    }, []);
    const q = filter.trim().toLowerCase();
    const shown = (list ?? []).filter((p) => !q || p.toLowerCase().includes(q));
    return (
        <div className="viewer-backdrop" onClick={onClose}>
            <div className="setup new-sess" onClick={(e) => e.stopPropagation()}>
                <h2>Session mới</h2>
                <p className="muted small">Chọn thư mục làm việc (các project đã có session trong 30 ngày qua).</p>
                <input className="lib-filter new-sess-filter" autoFocus placeholder="Lọc theo đường dẫn…" value={filter} onChange={(e) => setFilter(e.target.value)} />
                <div className="new-sess-list">
                    {list === null && <div className="muted small">Đang tải…</div>}
                    {list !== null && shown.length === 0 && <div className="muted small">Không có project nào.</div>}
                    {shown.map((p) => (
                        <button key={p} type="button" className="hist-row new-sess-row" onClick={() => onPick(p)} title={p}>
                            <span className="tree-label">{p}</span>
                        </button>
                    ))}
                </div>
                <div className="setup-actions">
                    <button type="button" className="btn" onClick={onClose}>
                        Huỷ
                    </button>
                </div>
            </div>
        </div>
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
                <span className="sb-chip sb-model" title={s.nodeSub ? 'Model của subagent đang chọn' : 'Model'}>
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
        if (running.name === 'Yield') {
            const names = running.waits.map((w) => w.label);
            const shown = names.slice(0, 2).join(', ');
            const more = names.length > 2 ? ` (+${names.length - 2})` : '';
            return {
                state: 'busy',
                text: names.length ? `Đang chờ: ${shown}${more}` : 'Đang chờ tác vụ nền',
                since: running.started ?? turnStart
            };
        }
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
    const [page, setPage] = useState<Page>('thread');
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
    const [picking, setPicking] = useState(false);
    const [newCwd, setNewCwd] = useState<string | null>(null);
    /** A session ccmon just started whose tree row has not appeared yet. While set,
     *  the header, chat and Composer belong to it (never to the previous selection). */
    const [starting, setStarting] = useState<{ sid: string; cwd: string } | null>(null);
    const [startSlow, setStartSlow] = useState(false);
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
            if (!(e.ctrlKey || e.metaKey) || e.shiftKey || e.altKey) return;
            if (e.key.toLowerCase() === 'b') {
                e.preventDefault();
                setTreeOpen((o) => !o);
            } else if (e.key === '1' || e.key === '2' || e.key === '3') {
                e.preventDefault();
                setPage((['thread', 'library', 'agents'] as const)[Number(e.key) - 1]);
            }
        };
        window.addEventListener('keydown', k);
        return () => window.removeEventListener('keydown', k);
    }, []);
    const [viewPath, setViewPath] = useState<string | null>(null);
    /** Live drafts of managed sessions' streaming messages, by session id. */
    const [drafts, setDrafts] = useState<Record<string, Draft>>({});
    useEffect(() => {
        const timers = new Map<string, number>();
        const un1 = listen<{ sid: string; draft: Draft }>('draft', (e) => {
            const { sid, draft } = e.payload;
            clearTimeout(timers.get(sid));
            setDrafts((d) => ({ ...d, [sid]: draft }));
        });
        // A finished or dead turn: drop its draft once the transcript had time to catch up.
        const un2 = listen<{ sid: string; runState: RunState }>('control-changed', (e) => {
            const { sid, runState } = e.payload;
            if (runState.kind !== 'idle' && runState.kind !== 'exited') return;
            clearTimeout(timers.get(sid));
            timers.set(
                sid,
                window.setTimeout(() => {
                    setDrafts((d) => {
                        const { [sid]: _gone, ...rest } = d;
                        return rest;
                    });
                }, 4000)
            );
        });
        return () => {
            un1.then((f) => f());
            un2.then((f) => f());
            timers.forEach((t) => clearTimeout(t));
        };
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
        if (starting) {
            // A started session's row is pending: only its arrival changes the selection.
            const k = `m:${starting.sid}`;
            if (rows.some((r) => r.key === k)) {
                pendingOpen.current = null;
                setStarting(null);
                setTail(PAGE);
                setView(null);
                setSelected(k);
            }
            return;
        }
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
    }, [rows, selected, userPicked, starting]);

    // The new row normally shows up within seconds. If it does not, stay on the new
    // session's placeholder (its Composer still works) and say so; ask the worker again.
    useEffect(() => {
        setStartSlow(false);
        if (!starting) return;
        const t = setTimeout(() => {
            setStartSlow(true);
            api.openSession(starting.sid);
        }, START_TIMEOUT_MS);
        return () => clearTimeout(t);
    }, [starting]);

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

    const clearStarting = () => {
        if (starting) pendingOpen.current = null;
        setStarting(null);
    };

    const select = (key: string) => {
        setNewCwd(null);
        clearStarting();
        if (key === selected) return;
        if (selected) setHistory((h) => [...h.slice(-50), selected]);
        setUserPicked(true);
        setTail(PAGE);
        setView(null);
        setSelected(key);
    };

    // Jump to a tool card (file hit): load enough history to include it, then scroll + flash.
    const pendingFocus = useRef<FileHit | null>(null);
    const [focusTick, setFocusTick] = useState(0);
    const flashTimer = useRef<ReturnType<typeof setTimeout>>();
    useEffect(() => {
        if (pendingFocus.current && pendingFocus.current.threadKey !== selected) pendingFocus.current = null;
    }, [selected]);
    const focusTool = (hit: FileHit) => {
        pendingFocus.current = hit;
        if (hit.threadKey !== selected) select(hit.threadKey);
        else setFocusTick((n) => n + 1);
    };
    useEffect(() => {
        const hit = pendingFocus.current;
        if (!hit || !view || view.key !== hit.threadKey || selected !== hit.threadKey) return;
        if (view.total - view.items.length > hit.idx) {
            const need = view.total - hit.idx + 20;
            if (need > tail) setTail(need);
            return;
        }
        pendingFocus.current = null;
        stickBottom.current = false;
        setFollowing(false);
        const el = content.current?.querySelector<HTMLElement>(`[data-tool-id="${CSS.escape(hit.toolId)}"]`);
        if (!el) return;
        el.scrollIntoView({ block: 'center' });
        clearTimeout(flashTimer.current);
        document.querySelectorAll('.flash').forEach((n) => n.classList.remove('flash'));
        void el.offsetWidth; // force reflow so the animation restarts
        el.classList.add('flash');
        flashTimer.current = setTimeout(() => el.classList.remove('flash'), 1500);
    }, [view, focusTick, selected, tail]);

    const back = () => {
        const prev = history[history.length - 1];
        if (!prev) return;
        setHistory((h) => h.slice(0, -1));
        clearStarting();
        if (prev === selected) return; // leaving a starting placeholder: the view is intact
        setTail(PAGE);
        setView(null);
        setSelected(prev);
    };

    const row = rows.find((r) => r.key === selected);

    const started = (sid: string) => {
        if (newCwd) setStarting({ sid, cwd: newCwd });
        setNewCwd(null);
        setUserPicked(true);
        // `selected` (and its view) stay as they are underneath: going back or
        // re-picking that row must not land on an empty view.
        if (selected) setHistory((h) => [...h.slice(-50), selected]);
        pendingOpen.current = `m:${sid}`;
        api.openSession(sid);
    };

    // The Composer stays under a session's subagent threads too (it always talks to
    // the main session); its model/effort chips then show the selected subagent's.
    const rowSid = row && row.kind !== 'workflow' ? row.key.split(':')[1] ?? null : null;
    const mainRow = rowSid ? rows.find((r) => r.key === `m:${rowSid}`) : undefined;
    const composerSid = starting ? starting.sid : mainRow && mainRow.tool !== 'codex' ? rowSid : null;
    /** Session id the Library/Agents pages and side panels should follow. */
    const pageSid = starting ? starting.sid : selected ? selected.split(':')[1] ?? null : null;
    const panelKey = starting ? null : selected;

    const openOld = (sid: string) => {
        clearStarting();
        setUserPicked(true);
        pendingOpen.current = `m:${sid}`;
        api.openSession(sid);
    };

    /** Open a run from the Agents page in the thread view (loading an old session if needed). */
    const openRun = (key: string, sid: string) => {
        setPage('thread');
        setNewCwd(null);
        clearStarting();
        if (rows.some((r) => r.key === key)) {
            select(key);
            return;
        }
        // Not in the tree yet: keep the current view until the worker loads it.
        if (selected) setHistory((h) => [...h.slice(-50), selected]);
        setUserPicked(true);
        pendingOpen.current = key;
        api.openSession(sid);
    };

    const opener = useMemo(() => ({ open: setViewPath, base: view?.cwd ?? null }), [view?.cwd]);
    const viewDraft = view && view.key.startsWith('m:') ? drafts[view.key.slice(2)] : undefined;

    return (
        <OpenPath.Provider value={opener}>
        <div className={`app ${showSidebar && page === 'thread' && panelKey ? 'with-sidebar' : ''} ${treeOpen ? '' : 'tree-collapsed'}`}>
            {treeOpen ? (
                <Tree
                    rows={rows}
                    selected={panelKey}
                    onSelect={select}
                    onOpenOld={openOld}
                    onSettings={() => setSetup('settings')}
                    onCollapse={() => setTreeOpen(false)}
                    onNewSession={() => setPicking(true)}
                />
            ) : (
                <TreeRail rows={rows} selected={panelKey} onSelect={select} onExpand={() => setTreeOpen(true)} />
            )}
            {page !== 'thread' && (
                <main className="main">
                    <AppHeader page={page} onPage={setPage} lead={<span className="thread-title">{PAGE_TITLE[page]}</span>} />
                    {page === 'library' ? (
                        <Library sid={pageSid} sessionLabel={starting ? 'Session mới' : row?.label ?? null} />
                    ) : (
                        <Agents sid={pageSid} onOpenRun={openRun} />
                    )}
                </main>
            )}
            <main className="main" style={page !== 'thread' ? { display: 'none' } : undefined}>
                <AppHeader
                    page={page}
                    onPage={setPage}
                    lead={
                        <>
                            {history.length > 0 && (
                                <button className="back-btn" type="button" onClick={back} title="Quay lại luồng trước" aria-label="Quay lại luồng trước">
                                    ←
                                </button>
                            )}
                            <span className="thread-title">
                                {newCwd
                                    ? `Session mới · ${newCwd}`
                                    : starting
                                      ? `Session mới · ${startSlow ? 'chưa thấy trong danh sách' : 'đang mở…'}`
                                      : row?.label ?? view?.title ?? 'ccmon'}
                            </span>
                            {row && !newCwd && !starting && <span className={`badge st-${row.status}`}>{row.status}</span>}
                            {view?.kind === 'agent' && view.key === selected && !starting && !newCwd && <SubagentControl key={view.key} agentKey={view.key} />}
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
                        </>
                    }
                    actions={
                        <>
                            <HeadIconBtn label="Quản lý: ảnh, file, log của luồng" on={panel === 'assets'} onClick={() => setPanel(panel === 'assets' ? null : 'assets')}>
                                <AssetsIcon />
                            </HeadIconBtn>
                            <HeadIconBtn label="Thay đổi: file đã sửa trong phiên, diff hai cột" on={panel === 'changes'} onClick={() => setPanel(panel === 'changes' ? null : 'changes')}>
                                <DiffIcon />
                            </HeadIconBtn>
                        </>
                    }
                />
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
                    {!starting && view && view.total > view.items.length && (
                        <button className="more-btn" type="button" onClick={() => setTail((t) => t + PAGE)}>
                            Hiện thêm {Math.min(PAGE, view.total - view.items.length)} mục cũ hơn ({view.total - view.items.length} còn lại)
                        </button>
                    )}
                    {newCwd ? (
                        <div className="muted pad">
                            Session mới trong <code>{newCwd}</code>. Chọn model, effort, quyền và capability bên dưới rồi gửi prompt đầu tiên.
                        </div>
                    ) : starting ? (
                        <div className={`muted pad ${startSlow ? 'start-slow' : ''}`} role="status">
                            {startSlow ? (
                                <>
                                    Session <code>{starting.sid}</code> đã chạy trong <code>{starting.cwd}</code> nhưng chưa xuất hiện trong danh sách sau{' '}
                                    {START_TIMEOUT_MS / 1000} giây. Bạn vẫn đang ở session mới này — prompt gửi bên dưới đi vào chính nó; luồng sẽ hiện khi transcript được ghi.
                                </>
                            ) : (
                                <>
                                    Đang mở session mới <code>{starting.sid}</code> trong <code>{starting.cwd}</code>… Prompt gửi bên dưới đi vào session này.
                                </>
                            )}
                        </div>
                    ) : selected?.startsWith('w:') ? (
                        <WorkflowView rows={rows} wfKey={selected} onSelect={select} />
                    ) : view ? (
                        <Chat view={view} now={now} onOpen={select} draft={viewDraft} />
                    ) : (
                        <div className="muted pad">Đang tải…</div>
                    )}
                    </div>
                </div>
                {!newCwd && !starting && <ActivityBar a={activityOf(view && view.key === selected ? view : null, row, rows)} now={now} />}
                {newCwd ? (
                    <Composer key={`new:${newCwd}`} sid={null} cwd={newCwd} onStarted={started} />
                ) : (
                    composerSid && <Composer key={composerSid} sid={composerSid} cwd={starting ? starting.cwd : null} current={status} tmuxPane={mainRow?.tmux ?? null} running={mainRow?.status === 'running'} />
                )}
                <StatusBar s={status} />
            </main>
            {page === 'thread' && panel === 'assets' && panelKey && (
                <Sidebar nodeKey={panelKey} refreshTick={Math.floor(assetTick / 8)} onOpenThread={select} onFocus={focusTool} />
            )}
            {page === 'thread' && panel === 'changes' && panelKey && <ChangesPanel nodeKey={panelKey} refreshTick={Math.floor(assetTick / 8)} initial={startup.diff} />}
            {setup && (
                <Setup
                    canClose={setup === 'settings'}
                    onDone={() => {
                        setSetup(null);
                        loadTree();
                    }}
                />
            )}
            {picking && (
                <NewSessionPicker
                    onClose={() => setPicking(false)}
                    onPick={(cwd) => {
                        setPicking(false);
                        setPage('thread');
                        clearStarting();
                        setNewCwd(cwd);
                    }}
                />
            )}
            {startup.view && <Viewer path={startup.view} onClose={() => setStartup((s) => ({ ...s, view: null }))} />}
            {viewPath && <Viewer path={viewPath} base={opener.base} onClose={() => setViewPath(null)} />}
        </div>
        </OpenPath.Provider>
    );
}
