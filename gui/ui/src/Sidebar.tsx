import { useEffect, useState } from 'react';
import { api, Assets, FileEntry, FileHit, fmtDur, fmtTime, Preview } from './api';
import { highlight, langOf, PathLabel } from './Tools';
import { Markdown } from './Markdown';

type Tab = 'user' | 'images' | 'files' | 'log';

function fmtSize(n: number | null | undefined): string {
    if (n == null) return '';
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function Viewer({ path, base, onClose }: { path: string; base?: string | null; onClose: () => void }) {
    const [p, setP] = useState<Preview | null>(null);
    const isMd = /\.(md|markdown|mdx)$/i.test(path);
    const [formatted, setFormatted] = useState(true);
    const [openErr, setOpenErr] = useState<string | null>(null);
    useEffect(() => {
        setP(null);
        setOpenErr(null);
        api.preview(path, base).then(setP);
    }, [path, base]);
    useEffect(() => {
        const k = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
        window.addEventListener('keydown', k);
        return () => window.removeEventListener('keydown', k);
    }, [onClose]);
    return (
        <div className="viewer-backdrop" onClick={onClose}>
            <div className="viewer" onClick={(e) => e.stopPropagation()}>
                <div className="viewer-head">
                    <PathLabel path={p?.path ?? path} />
                    <span className="muted small">{p ? fmtSize(p.size) : ''}</span>
                    {isMd && p?.kind === 'text' && (
                        <div className="seg-toggle">
                            <button type="button" className={formatted ? 'on' : ''} onClick={() => setFormatted(true)}>
                                Xem định dạng
                            </button>
                            <button type="button" className={!formatted ? 'on' : ''} onClick={() => setFormatted(false)}>
                                Mã nguồn
                            </button>
                        </div>
                    )}
                    <button
                        type="button"
                        className="link-btn"
                        onClick={() => navigator.clipboard?.writeText(path)}
                        title="Sao chép đường dẫn"
                    >
                        ⧉ Đường dẫn
                    </button>
                    {p && p.kind !== 'missing' && (
                        <button
                            type="button"
                            className="link-btn"
                            onClick={() => api.openExternal(p.path, base).catch((e) => setOpenErr(String(e)))}
                            title="Mở bằng ứng dụng mặc định của hệ thống"
                        >
                            ↗ Mở ngoài
                        </button>
                    )}
                    {openErr && <span className="small err-text">{openErr}</span>}
                    <button type="button" className="viewer-close" onClick={onClose}>
                        ✕
                    </button>
                </div>
                <div className="viewer-body">
                    {!p && <div className="muted pad">Đang tải…</div>}
                    {p?.kind === 'image' && <img src={p.dataUrl!} alt={path} />}
                    {p?.kind === 'text' && isMd && formatted && (
                        <div className="md-doc">
                            <Markdown text={p.text!} />
                        </div>
                    )}
                    {p?.kind === 'text' && !(isMd && formatted) && (
                        <div className="codeblock-body viewer-code">
                            <pre className="gutter">
                                {p.text!.split('\n').map((_, i) => i + 1).join('\n')}
                            </pre>
                            <pre className="code">
                                <code dangerouslySetInnerHTML={{ __html: highlight(p.text!, langOf(path)) }} />
                            </pre>
                        </div>
                    )}
                    {p?.kind === 'text' && p.truncated && <div className="muted pad">… file dài, chỉ hiện 512 KB đầu.</div>}
                    {p?.kind === 'dir' && (
                        <ul className="dir-list">
                            {p.entries.map((e) => (
                                <li key={e}>{e.endsWith('/') ? '📁' : '📄'} {e}</li>
                            ))}
                        </ul>
                    )}
                    {p?.kind === 'binary' && <div className="muted pad">File nhị phân hoặc quá lớn để xem trước.</div>}
                    {p?.kind === 'missing' && <div className="muted pad">File không còn tồn tại.</div>}
                </div>
            </div>
        </div>
    );
}

function Thumb({ f, onOpen }: { f: FileEntry; onOpen: (p: string) => void }) {
    const [src, setSrc] = useState<string | null>(null);
    useEffect(() => {
        if (f.exists && (f.size ?? 0) < 8 * 1024 * 1024) api.preview(f.path).then((p) => p.dataUrl && setSrc(p.dataUrl));
    }, [f.path, f.exists, f.size, f.ts]);
    const name = f.path.split('/').pop();
    return (
        <button type="button" className="thumb" onClick={() => onOpen(f.path)} title={f.path}>
            {src ? <img src={src} alt={name} /> : <div className="thumb-ph">{f.exists ? '…' : '✕'}</div>}
            <span className="thumb-name">{name}</span>
            <span className="thumb-meta">{fmtTime(f.ts)}</span>
        </button>
    );
}

function FileRow({ f, onOpen }: { f: FileEntry; onOpen: (p: string) => void }) {
    const icon = f.kind === 'dir' ? '📁' : f.kind === 'image' ? '🖼' : '📄';
    return (
        <button type="button" className={`frow ${f.exists ? '' : 'missing'}`} onClick={() => onOpen(f.path)} title={f.path}>
            <span>{icon}</span>
            <PathLabel path={f.path} />
            <span className="frow-meta">
                {fmtSize(f.size)} {f.source ? `· ${f.source}` : ''}
            </span>
        </button>
    );
}

export function Sidebar({
    nodeKey,
    refreshTick,
    onOpenThread,
    onFocus
}: {
    nodeKey: string;
    refreshTick: number;
    onOpenThread: (key: string) => void;
    onFocus: (hit: FileHit) => void;
}) {
    // Per-file cursor into `hits`, counted from the newest.
    // Keyed by the last focused toolId so live-growing `hits` cannot shift it.
    const [cursor, setCursor] = useState<Record<string, string>>({});
    const posOf = (path: string, hits: FileHit[]) => {
        const id = cursor[path];
        const p = id == null ? -1 : hits.findIndex((h) => h.toolId === id);
        return p < 0 ? -1 : hits.length - 1 - p; // 0 = newest
    };
    const focusNext = (path: string, hits: FileHit[]) => {
        if (hits.length === 0) return;
        const cur = posOf(path, hits);
        const i = cur < 0 || cur + 1 >= hits.length ? 0 : cur + 1;
        const hit = hits[hits.length - 1 - i];
        setCursor((c) => ({ ...c, [path]: hit.toolId }));
        onFocus(hit);
    };
    const [tab, setTab] = useState<Tab>('user');
    const scope = 'thread' as const;
    const [assets, setAssets] = useState<Assets | null>(null);
    const [viewing, setViewing] = useState<string | null>(null);
    const [onlyErrors, setOnlyErrors] = useState(false);

    const [tick, setTick] = useState(0);
    useEffect(() => {
        // Scratchpad files are not in the transcript: re-list periodically too.
        const t = setInterval(() => setTick((x) => x + 1), 3000);
        return () => clearInterval(t);
    }, []);
    useEffect(() => {
        api.assets(nodeKey, scope).then(setAssets);
    }, [nodeKey, scope, refreshTick, tick]);

    const a = assets;
    const counts = {
        user: a?.userFiles.length ?? 0,
        images: a?.images.length ?? 0,
        files: (a?.files.length ?? 0) + (a?.scratch.length ?? 0),
        log: a?.log.length ?? 0
    };
    const errors = a?.log.filter((l) => l.state === 'error').length ?? 0;

    return (
        <aside className="sidebar">
            <div className="sb-head">
                <span className="pane-title">Quản lý</span>
                <span className="muted small">theo luồng đang chọn</span>
            </div>
            <div className="sb-tabs">
                {(
                    [
                        ['user', 'Người dùng'],
                        ['images', 'Ảnh'],
                        ['files', 'File'],
                        ['log', 'Log']
                    ] as [Tab, string][]
                ).map(([t, label]) => (
                    <button key={t} type="button" className={tab === t ? 'on' : ''} onClick={() => setTab(t)}>
                        {label} <span className="count">{counts[t]}</span>
                        {t === 'log' && errors > 0 && <span className="count err">{errors}✗</span>}
                    </button>
                ))}
            </div>
            <div className="sb-body">
                {!a && <div className="muted pad">Đang tải…</div>}
                {a && tab === 'user' && (
                    <>
                        <div className="sb-note">Ảnh dán vào và file/thư mục bạn nhắc bằng @.</div>
                        <div className="thumbs">
                            {a.userFiles.filter((f) => f.kind === 'image').map((f) => (
                                <Thumb key={f.path} f={f} onOpen={setViewing} />
                            ))}
                        </div>
                        {a.userFiles.filter((f) => f.kind !== 'image').map((f) => (
                            <FileRow key={f.path} f={f} onOpen={setViewing} />
                        ))}
                        {a.userFiles.length === 0 && <div className="muted pad">Chưa có file nào.</div>}
                    </>
                )}
                {a && tab === 'images' && (
                    <>
                        <div className="sb-note">Ảnh agent tạo ra hoặc đọc (chụp màn hình, ảnh trong thư mục nháp).</div>
                        <div className="thumbs">
                            {a.images.map((f) => (
                                <Thumb key={f.path} f={f} onOpen={setViewing} />
                            ))}
                        </div>
                        {a.images.length === 0 && <div className="muted pad">Chưa có ảnh.</div>}
                    </>
                )}
                {a && tab === 'files' && (
                    <>
                        <div className="sb-section">File đã thao tác</div>
                        {a.files.map((f) => (
                            <div key={f.path} className={`frow ${f.exists ? '' : 'missing'}`}>
                                <button
                                    type="button"
                                    className="frow-open"
                                    onClick={() => setViewing(f.path)}
                                    title={`${f.path}\n${f.threads.join(', ')}`}
                                >
                                    <span className={`op op-${f.lastOp}`}>{f.lastOp === 'edit' ? '✎' : f.lastOp === 'write' ? '＋' : '👁'}</span>
                                    <PathLabel path={f.path} />
                                    <span className="frow-meta">
                                        {f.edits > 0 && <span className="op-edit">✎{f.edits}</span>}
                                        {f.writes > 0 && <span className="op-write"> ＋{f.writes}</span>}
                                        {f.reads > 0 && <span className="op-read"> 👁{f.reads}</span>}
                                    </span>
                                </button>
                                {f.hits.length > 0 && (
                                    <>
                                        {posOf(f.path, f.hits) >= 0 && (
                                            <span className="frow-pos">
                                                {posOf(f.path, f.hits) + 1}/{f.hits.length}
                                            </span>
                                        )}
                                        <button
                                            type="button"
                                            className="frow-focus"
                                            title="Tìm trong chat"
                                            aria-label="Tìm trong chat"
                                            onClick={(e) => {
                                                e.stopPropagation();
                                                focusNext(f.path, f.hits);
                                            }}
                                        >
                                            ⌖
                                        </button>
                                    </>
                                )}
                            </div>
                        ))}
                        {a.files.length === 0 && <div className="muted pad">Chưa thao tác file nào.</div>}
                        <div className="sb-section">
                            Thư mục nháp {a.tmpDir && <span className="muted small">{a.tmpDir}/scratchpad</span>}
                        </div>
                        {a.scratch.map((f) => (
                            <FileRow key={f.path} f={{ ...f, source: fmtTime(f.ts) }} onOpen={setViewing} />
                        ))}
                        {a.scratch.length === 0 && <div className="muted pad">Trống.</div>}
                    </>
                )}
                {a && tab === 'log' && (
                    <>
                        <label className="sb-filter">
                            <input type="checkbox" checked={onlyErrors} onChange={(e) => setOnlyErrors(e.target.checked)} /> Chỉ lỗi
                        </label>
                        {a.log
                            .filter((l) => !onlyErrors || l.state === 'error')
                            .map((l) => (
                                <button
                                    key={l.toolId}
                                    type="button"
                                    className={`logrow log-${l.state}`}
                                    onClick={() => onOpenThread(l.threadKey)}
                                    title={l.command}
                                >
                                    <span className={`tdot st-${l.state}`}>●</span>
                                    <span className="log-main">
                                        <span className="log-title">{l.title}</span>
                                        <span className="log-cmd">{l.command.split('\n')[0]}</span>
                                    </span>
                                    <span className="log-meta">
                                        {fmtTime(l.ts)}
                                        <br />
                                        {l.durMs != null ? fmtDur(l.durMs) : ''} · {l.thread}
                                    </span>
                                </button>
                            ))}
                        {a.log.length === 0 && <div className="muted pad">Chưa chạy lệnh nào.</div>}
                    </>
                )}
            </div>
            {viewing && <Viewer path={viewing} onClose={() => setViewing(null)} />}
        </aside>
    );
}
