import { useEffect, useState } from 'react';
import { api, SetupInfo } from './api';

/** First-run (and settings) screen: choose which coding agents to monitor. */
export function Setup({ onDone, canClose }: { onDone: () => void; canClose: boolean }) {
    const [info, setInfo] = useState<SetupInfo | null>(null);
    const [claude, setClaude] = useState(true);
    const [codex, setCodex] = useState(true);
    const [statusline, setStatusline] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [saving, setSaving] = useState(false);

    useEffect(() => {
        api.setup().then((i) => {
            setInfo(i);
            const first = !i.config.setupDone;
            setClaude(first ? i.claudeFound : i.config.claude);
            setCodex(first ? i.codexFound : i.config.codex);
            setStatusline(!i.statuslineWrapped);
        });
    }, []);

    const save = () => {
        setSaving(true);
        setError(null);
        api.saveSetup(claude, codex, claude && statusline && !info?.statuslineWrapped)
            .then(() => onDone())
            .catch((e) => setError(String(e)))
            .finally(() => setSaving(false));
    };

    const removeStatusline = () =>
        api.removeStatusline().then(setInfo).catch((e) => setError(String(e)));

    if (!info) return null;
    return (
        <div className="viewer-backdrop">
            <div className="setup">
                <h2>{info.config.setupDone ? 'Cài đặt' : 'Chào mừng đến với ccmon'}</h2>
                <p className="muted">
                    Chọn công cụ bạn muốn theo dõi. ccmon chỉ <b>đọc</b> lịch sử phiên mà các công cụ tự ghi trên máy, không gửi dữ
                    liệu đi đâu.
                </p>

                <label className={`tool-opt ${claude ? 'on' : ''}`}>
                    <input type="checkbox" checked={claude} onChange={(e) => setClaude(e.target.checked)} />
                    <div>
                        <div className="tool-opt-title">
                            Claude Code{' '}
                            {info.claudeFound ? <span className="chip chip-new">đã tìm thấy</span> : <span className="chip">chưa thấy</span>}
                        </div>
                        <div className="muted small">{info.claudeDir}</div>
                        {claude && (
                            <div className="tool-sub">
                                {info.statuslineWrapped ? (
                                    <span className="small">
                                        ✓ Đã lấy Session % / Weekly % qua status line.{' '}
                                        <button type="button" className="link-btn" onClick={removeStatusline}>
                                            Gỡ
                                        </button>
                                    </span>
                                ) : (
                                    <label className="small">
                                        <input type="checkbox" checked={statusline} onChange={(e) => setStatusline(e.target.checked)} /> Hiện
                                        Session % / Weekly % / cửa sổ context: bọc status line hiện tại
                                        {info.statusline ? <code> {info.statusline}</code> : ' (chưa có)'} bằng ccmon. Sửa{' '}
                                        <code>settings.json</code>, có sao lưu, gỡ được.
                                    </label>
                                )}
                            </div>
                        )}
                    </div>
                </label>

                <label className={`tool-opt ${codex ? 'on' : ''}`}>
                    <input type="checkbox" checked={codex} onChange={(e) => setCodex(e.target.checked)} />
                    <div>
                        <div className="tool-opt-title">
                            Codex{' '}
                            {info.codexFound ? <span className="chip chip-new">đã tìm thấy</span> : <span className="chip">chưa thấy</span>}
                        </div>
                        <div className="muted small">{info.codexDir}</div>
                        <div className="tool-sub small muted">Không cần cấu hình: hạn mức và token đọc thẳng từ phiên.</div>
                    </div>
                </label>

                {error && <div className="setup-error">{error}</div>}
                <div className="setup-actions">
                    {canClose && (
                        <button type="button" className="btn" onClick={onDone}>
                            Huỷ
                        </button>
                    )}
                    <button type="button" className="btn btn-primary" disabled={saving || (!claude && !codex)} onClick={save}>
                        {info.config.setupDone ? 'Lưu' : 'Bắt đầu'}
                    </button>
                </div>
            </div>
        </div>
    );
}
