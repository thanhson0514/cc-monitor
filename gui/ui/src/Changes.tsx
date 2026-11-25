import { useEffect, useState } from 'react';
import { api, ChangedFile, DiffView, fmtTime } from './api';
import { Viewer } from './Sidebar';
import { highlight, langOf, PathLabel } from './Tools';

const GIT_LABEL: Record<string, string> = { M: 'sửa', A: 'thêm', D: 'xoá', '??': 'chưa track', R: 'đổi tên', AM: 'thêm' };

function DiffModal({ nodeKey, path, onClose }: { nodeKey: string; path: string; onClose: () => void }) {
    const [baseline, setBaseline] = useState<string | null>(null);
    const [view, setView] = useState<DiffView | null>(null);
    const [viewing, setViewing] = useState(false);
    useEffect(() => {
        api.diff(nodeKey, path, baseline).then(setView);
    }, [nodeKey, path, baseline]);
    useEffect(() => {
        const k = (e: KeyboardEvent) => e.key === 'Escape' && !viewing && onClose();
        window.addEventListener('keydown', k);
        return () => window.removeEventListener('keydown', k);
    }, [onClose, viewing]);
    const lang = langOf(path);
    const cell = (text: string | undefined) => ({ __html: text === undefined ? '' : highlight(text, lang) || ' ' });
    return (
        <div className="viewer-backdrop" onClick={onClose}>
            <div className="viewer diff-modal" onClick={(e) => e.stopPropagation()}>
                <div className="viewer-head">
                    <PathLabel path={path} />
                    {view && (
                        <span className="tbadge">
                            <span className="add">+{view.added}</span> <span className="del">−{view.removed}</span>
                        </span>
                    )}
                    {view && view.available.length > 1 && (
                        <div className="seg-toggle">
                            {view.available.map((b) => (
                                <button key={b} type="button" className={view.baseline === b ? 'on' : ''} onClick={() => setBaseline(b)}>
                                    So với {b === 'session' ? 'trước phiên' : 'git HEAD'}
                                </button>
                            ))}
                        </div>
                    )}
                    <button type="button" className="link-btn" onClick={() => setViewing(true)}>
                        Mở file
                    </button>
                    <button type="button" className="viewer-close" onClick={onClose}>
                        ✕
                    </button>
                </div>
                <div className="diff2-head">
                    <div>◀ {view?.baselineLabel ?? '…'}</div>
                    <div>Hiện tại ▶</div>
                </div>
                <div className="viewer-body">
                    {!view && <div className="muted pad">Đang tính diff…</div>}
                    {view?.note && <div className="muted pad">{view.note}</div>}
                    {view && view.rows.length === 0 && !view.note && <div className="muted pad">Không có khác biệt.</div>}
                    {view && view.rows.length > 0 && (
                        <table className="diff2">
                            <tbody>
                                {view.rows.map((r, i) =>
                                    r.kind === 'fold' ? (
                                        <tr key={i} className="d2-fold">
                                            <td colSpan={4}>⋯ {r.hidden} dòng không đổi</td>
                                        </tr>
                                    ) : (
                                        <tr key={i} className={`d2-${r.kind}`}>
                                            <td className="d2-no">{r.left?.no ?? ''}</td>
                                            <td className={`d2-code ${r.left ? 'd2-left' : 'd2-empty'}`}>
                                                <code dangerouslySetInnerHTML={cell(r.left?.text)} />
                                            </td>
                                            <td className="d2-no">{r.right?.no ?? ''}</td>
                                            <td className={`d2-code ${r.right ? 'd2-right' : 'd2-empty'}`}>
                                                <code dangerouslySetInnerHTML={cell(r.right?.text)} />
                                            </td>
                                        </tr>
                                    )
                                )}
                            </tbody>
                        </table>
                    )}
                </div>
            </div>
            {viewing && <Viewer path={path} onClose={() => setViewing(false)} />}
        </div>
    );
}

export function ChangesPanel({ nodeKey, refreshTick, initial }: { nodeKey: string; refreshTick: number; initial?: string | null }) {
    const [files, setFiles] = useState<ChangedFile[] | null>(null);
    const [open, setOpen] = useState<string | null>(initial ?? null);
    useEffect(() => {
        api.changes(nodeKey).then(setFiles);
    }, [nodeKey, refreshTick]);
    const total = files?.reduce((a, f) => [a[0] + f.added, a[1] + f.removed], [0, 0]) ?? [0, 0];
    return (
        <aside className="sidebar">
            <div className="sb-head">
                <span className="pane-title">Thay đổi trong phiên</span>
                {files && files.length > 0 && (
                    <span className="tbadge">
                        {files.length} file · <span className="add">+{total[0]}</span> <span className="del">−{total[1]}</span>
                    </span>
                )}
            </div>
            <div className="sb-body">
                <div className="sb-note">File agent đã sửa hoặc tạo. Bấm để xem diff hai cột (trước phiên ↔ hiện tại, hoặc so với git HEAD).</div>
                {!files && <div className="muted pad">Đang tải…</div>}
                {files?.length === 0 && <div className="muted pad">Phiên này chưa sửa file nào.</div>}
                {files?.map((f) => (
                    <button
                        key={f.path}
                        type="button"
                        className={`chg-row ${f.exists ? '' : 'missing'}`}
                        onClick={() => setOpen(f.path)}
                        title={`${f.path}${f.repo ? `\nrepo: ${f.repo}` : ''}`}
                    >
                        <div className="chg-top">
                            <PathLabel path={f.path} />
                        </div>
                        <div className="chg-meta">
                            <span className="add">+{f.added}</span> <span className="del">−{f.removed}</span>
                            {f.createdInSession && <span className="chip chip-new">mới</span>}
                            {!f.exists && <span className="chip chip-del">đã xoá</span>}
                            {f.gitStatus && (
                                <span className="chip" title="git status">
                                    git {GIT_LABEL[f.gitStatus] ?? f.gitStatus}
                                </span>
                            )}
                            <span className="muted"> · {f.edits} lần sửa · {fmtTime(f.lastTs)}</span>
                        </div>
                    </button>
                ))}
            </div>
            {open && <DiffModal nodeKey={nodeKey} path={open} onClose={() => setOpen(null)} />}
        </aside>
    );
}
