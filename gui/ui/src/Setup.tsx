import { useEffect, useState } from 'react';
import { api, SetupInfo } from './api';

/** First-run (and settings) screen. Only Claude Code is monitored for now (Codex is disabled). */
export function Setup({ onDone, canClose }: { onDone: () => void; canClose: boolean }) {
    const [info, setInfo] = useState<SetupInfo | null>(null);
    const [statusline, setStatusline] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [saving, setSaving] = useState(false);

    useEffect(() => {
        api.setup().then((i) => {
            setInfo(i);
            setStatusline(!i.statuslineWrapped);
        });
    }, []);

    const save = () => {
        setSaving(true);
        setError(null);
        api.saveSetup(true, false, statusline && !info?.statuslineWrapped)
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
                    ccmon theo dõi các phiên Claude Code. ccmon chỉ <b>đọc</b> lịch sử phiên mà Claude Code tự ghi trên máy, không
                    gửi dữ liệu đi đâu.
                </p>

                <div className="tool-opt on">
                    <div>
                        <div className="tool-opt-title">
                            Claude Code{' '}
                            {info.claudeFound ? <span className="chip chip-new">đã tìm thấy</span> : <span className="chip">chưa thấy</span>}
                        </div>
                        <div className="muted small">{info.claudeDir}</div>
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
                    </div>
                </div>

                {error && <div className="setup-error">{error}</div>}
                <div className="setup-actions">
                    {canClose && (
                        <button type="button" className="btn" onClick={onDone}>
                            Huỷ
                        </button>
                    )}
                    <button type="button" className="btn btn-primary" disabled={saving} onClick={save}>
                        {info.config.setupDone ? 'Lưu' : 'Bắt đầu'}
                    </button>
                </div>
            </div>
        </div>
    );
}
