import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { api, CapKind, Capability, capDisabled, ControlDto, EMPTY_OVERLAY, isBusy, Overlay, RunState, toggleCap } from './api';

const PERMISSION_MODES: [string, string][] = [
    ['default', 'default'],
    ['acceptEdits', 'acceptEdits'],
    ['plan', 'plan'],
    ['bypassPermissions', 'bypassPermissions']
];

const PERMISSION_HINT = 'Chạy ở chế độ -p: tool cần duyệt sẽ tự bị từ chối, không có hộp thoại hỏi lại.';

export const LOCK_REASON: Record<string, string> = {
    liveElsewhere: 'Session đang chạy ở terminal — chỉ xem',
    claudeNotFound: 'Không tìm thấy lệnh claude — cài Claude Code hoặc thêm vào PATH'
};

const CAP_TABS: [CapKind, string][] = [
    ['skill', 'Skills'],
    ['plugin', 'Plugins'],
    ['mcp', 'MCP'],
    ['agent', 'Agents'],
    ['workflow', 'Workflows']
];

/** Switch state of one capability for a session: overlay first, then ccmon/global locks. */
export function capSwitch(c: Capability, o: Overlay, dto: ControlDto | null): { on: boolean; locked: string | null } {
    if (!c.enabledGlobally) return { on: false, locked: 'Đang tắt trong cấu hình Claude Code' };
    if (c.kind === 'agent' && dto?.registryAgents.includes(c.id)) return { on: false, locked: 'Đã tắt cho toàn ccmon (trang Agents)' };
    if (c.kind === 'workflow') {
        if (dto?.registryWorkflows.includes(c.id)) return { on: false, locked: 'Đã tắt cho toàn ccmon (trang Agents)' };
        if (o.workflowsOff) return { on: false, locked: 'Đang tắt mọi workflow cho session này' };
    }
    return { on: !capDisabled(o, c.kind, c.id), locked: null };
}

function runLabel(r: RunState | null): string | null {
    if (!r) return null;
    switch (r.kind) {
        case 'starting':
            return 'Đang khởi động';
        case 'busy':
            return 'Đang chạy lượt';
        case 'restarting':
            return 'Đang khởi động lại';
        case 'idle':
            return 'Sẵn sàng';
        case 'exited':
            return 'Tiến trình đã dừng';
    }
}

function CapDrawer({
    sid,
    overlay,
    dto,
    onChange,
    onClose
}: {
    sid: string | null;
    overlay: Overlay;
    dto: ControlDto | null;
    onChange: (o: Overlay) => void;
    onClose: () => void;
}) {
    const [tab, setTab] = useState<CapKind>('skill');
    const [filter, setFilter] = useState('');
    const [caps, setCaps] = useState<Capability[] | null>(null);
    useEffect(() => {
        let ignore = false;
        api.catalog(sid).then((c) => !ignore && setCaps(c));
        return () => {
            ignore = true;
        };
    }, [sid]);
    const rows = useMemo(() => {
        const q = filter.trim().toLowerCase();
        return (caps ?? []).filter((c) => c.kind === tab && (!q || c.name.toLowerCase().includes(q) || c.description.toLowerCase().includes(q)));
    }, [caps, tab, filter]);
    const off = (k: CapKind) => (caps ?? []).filter((c) => c.kind === k && !capSwitch(c, overlay, dto).on).length;

    return (
        <div className="cmp-drawer">
            <div className="cmp-drawer-head">
                <span className="cmp-drawer-title">Capability cho session</span>
                <span className="muted small">Chỉ áp dụng cho session chạy từ ccmon; không sửa settings.json.</span>
                <button type="button" className="viewer-close" onClick={onClose} title="Đóng">
                    ✕
                </button>
            </div>
            <div className="lib-bar cmp-drawer-bar">
                <div className="lib-tabs">
                    {CAP_TABS.map(([k, label]) => (
                        <button key={k} type="button" className={`sb-toggle ${tab === k ? 'on' : ''}`} onClick={() => setTab(k)}>
                            {label}
                            {off(k) > 0 && <span className="cmp-off-count"> · tắt {off(k)}</span>}
                        </button>
                    ))}
                </div>
                <input className="lib-filter" placeholder="Lọc…" value={filter} onChange={(e) => setFilter(e.target.value)} />
            </div>
            <div className="cmp-drawer-list">
                {tab === 'workflow' && (
                    <label className="cmp-cap cmp-cap-master">
                        <span className="lib-name">Tắt mọi workflow</span>
                        <span className="lib-desc">Chặn hẳn tool Workflow trong session này</span>
                        <button
                            type="button"
                            className={`switch live ${overlay.workflowsOff ? 'on' : ''}`}
                            role="switch"
                            aria-checked={overlay.workflowsOff}
                            onClick={() => onChange({ ...overlay, workflowsOff: !overlay.workflowsOff })}
                        />
                    </label>
                )}
                {caps === null && <div className="muted pad small">Đang tải…</div>}
                {caps !== null && rows.length === 0 && <div className="muted pad small">Không có mục nào.</div>}
                {rows.map((c) => {
                    const s = capSwitch(c, overlay, dto);
                    return (
                        <div key={`${c.kind}:${c.id}:${c.path ?? ''}`} className={`cmp-cap ${s.on ? '' : 'off'}`} title={c.path ?? c.id}>
                            <span className="lib-name">{c.name}</span>
                            <span className="lib-desc" title={c.description}>
                                {s.locked ?? c.description.split('\n')[0]}
                            </span>
                            <button
                                type="button"
                                className={`switch ${s.locked ? '' : 'live'} ${s.on ? 'on' : ''}`}
                                role="switch"
                                aria-checked={s.on}
                                disabled={!!s.locked}
                                title={s.locked ?? (s.on ? 'Tắt cho session này' : 'Bật lại cho session này')}
                                onClick={() => onChange(toggleCap(overlay, c.kind, c.id))}
                            />
                        </div>
                    );
                })}
            </div>
        </div>
    );
}

/**
 * Prompt input under the chat. `sid = null`: new session in `cwd` (overlay kept
 * locally until the first send, then stored for the new session).
 */
export function Composer({ sid, cwd, onStarted }: { sid: string | null; cwd: string | null; onStarted?: (sid: string) => void }) {
    const [dto, setDto] = useState<ControlDto | null>(null);
    const [draft, setDraft] = useState<Overlay | null>(null);
    const [text, setText] = useState('');
    const [sending, setSending] = useState(false);
    const [err, setErr] = useState<string | null>(null);
    const [drawer, setDrawer] = useState(false);
    const sidRef = useRef(sid);
    sidRef.current = sid;

    const load = useCallback(() => {
        const want = sidRef.current;
        api.control(want)
            .then((d) => {
                if (sidRef.current !== want) return;
                setDto(d);
                if (want === null) setDraft((cur) => cur ?? d.overlay);
            })
            .catch((e) => setErr(String(e)));
    }, []);

    useEffect(() => {
        setDto(null);
        setDraft(null);
        setErr(null);
        setText('');
        load();
        const t = setInterval(load, 4000);
        const un = listen<{ sid: string; runState: RunState }>('control-changed', (e) => {
            const cur = sidRef.current;
            if (e.payload.sid === cur || (cur === null && e.payload.sid === '')) load();
        });
        return () => {
            clearInterval(t);
            un.then((f) => f());
        };
    }, [sid, cwd, load]);

    const overlay = (sid ? dto?.overlay : draft) ?? EMPTY_OVERLAY;
    const runState = dto?.runState ?? null;
    const busy = isBusy(runState);
    const locked = dto !== null && !dto.canControl;
    const managed = runState !== null && runState.kind !== 'exited';

    const update = (o: Overlay) => {
        setErr(null);
        if (!sid) {
            setDraft(o);
            return;
        }
        setDto((d) => (d ? { ...d, overlay: o } : d));
        api.setOverlay(sid, o)
            .then(setDto)
            .catch((e) => {
                setErr(String(e));
                load();
            });
    };

    const send = () => {
        const t = text.trim();
        if (!t || sending || locked || busy || !dto) return;
        if (!sid && !cwd) return;
        setSending(true);
        setErr(null);
        api.sendPrompt(sid, cwd ?? '', t, sid ? null : overlay)
            .then((newSid) => {
                setText('');
                if (!sid) onStarted?.(newSid);
                load();
            })
            .catch((e) => setErr(String(e)))
            .finally(() => setSending(false));
    };

    const interrupt = () => {
        if (sid) api.interrupt(sid).catch((e) => setErr(String(e)));
    };
    const stop = () => {
        if (sid) api.stopManaged(sid).then(load).catch((e) => setErr(String(e)));
    };

    const pm = overlay.permissionMode ?? 'default';
    const exited = runState?.kind === 'exited' && (runState.code ?? 0) !== 0 && runState.stderrTail.trim() ? runState : null;
    const lockText = locked ? LOCK_REASON[dto?.reason ?? ''] ?? 'Không điều khiển được session này' : null;
    const placeholder = lockText ?? (sid ? 'Nhắn cho Claude… (Enter gửi, Shift+Enter xuống dòng)' : `Prompt đầu tiên cho session mới trong ${cwd ?? '…'}`);

    return (
        <div className={`composer ${locked ? 'locked' : ''}`}>
            {drawer && !locked && <CapDrawer sid={sid} overlay={overlay} dto={dto} onChange={update} onClose={() => setDrawer(false)} />}
            {lockText && (
                <div className="cmp-lock" role="note">
                    🔒 {lockText}
                </div>
            )}
            {err && <div className="cmp-err">{err}</div>}
            {exited && (
                <div className="cmp-err" title={exited.stderrTail}>
                    claude đã thoát (mã {exited.code}): {exited.stderrTail.trim().split('\n').slice(-1)[0]}
                </div>
            )}
            <div className="cmp-row">
                <textarea
                    className="cmp-input"
                    rows={2}
                    value={text}
                    disabled={locked || sending}
                    placeholder={placeholder}
                    onChange={(e) => setText(e.target.value)}
                    onKeyDown={(e) => {
                        if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                            e.preventDefault();
                            send();
                        }
                    }}
                />
                {busy && sid ? (
                    <button type="button" className="btn cmp-stop" onClick={interrupt} title="Dừng lượt đang chạy">
                        Dừng
                    </button>
                ) : (
                    <button type="button" className="btn btn-primary" onClick={send} disabled={locked || sending || !text.trim() || !dto}>
                        {sending ? 'Đang gửi…' : 'Gửi'}
                    </button>
                )}
            </div>
            <div className="cmp-chips">
                <select
                    className="cmp-chip"
                    value={overlay.model ?? ''}
                    disabled={locked}
                    title="Model cho các lượt sau"
                    onChange={(e) => update({ ...overlay, model: e.target.value || null })}
                >
                    <option value="">model: mặc định</option>
                    {(dto?.models ?? []).map((m) => (
                        <option key={m} value={m}>
                            model: {m}
                        </option>
                    ))}
                    {overlay.model && !(dto?.models ?? []).includes(overlay.model) && <option value={overlay.model}>model: {overlay.model}</option>}
                </select>
                <select
                    className="cmp-chip"
                    value={overlay.effort ?? ''}
                    disabled={locked}
                    title="Mức suy nghĩ (effort). Transcript không ghi lại giá trị này — chip hiện giá trị ccmon truyền vào."
                    onChange={(e) => update({ ...overlay, effort: e.target.value || null })}
                >
                    <option value="">effort: mặc định</option>
                    {(dto?.efforts ?? []).map((m) => (
                        <option key={m} value={m}>
                            effort: {m}
                        </option>
                    ))}
                </select>
                <select
                    className={`cmp-chip ${pm === 'bypassPermissions' ? 'warn' : ''}`}
                    value={pm}
                    disabled={locked}
                    title={pm === 'bypassPermissions' ? 'Bỏ qua mọi kiểm tra quyền — agent tự chạy mọi lệnh. Cẩn thận!' : PERMISSION_HINT}
                    onChange={(e) => update({ ...overlay, permissionMode: e.target.value === 'default' ? null : e.target.value })}
                >
                    {PERMISSION_MODES.map(([v, l]) => (
                        <option key={v} value={v}>
                            quyền: {l}
                        </option>
                    ))}
                </select>
                <span className="cmp-hint" title={PERMISSION_HINT}>
                    {pm === 'bypassPermissions' ? '⚠ Bỏ qua mọi kiểm tra quyền' : 'Tool cần duyệt sẽ tự bị từ chối'}
                </span>
                <button
                    type="button"
                    className={`cmp-chip cmp-gear ${drawer ? 'on' : ''}`}
                    disabled={locked}
                    onClick={() => setDrawer((d) => !d)}
                    title="Capability cho session: bật/tắt skill, plugin, MCP, agent, workflow"
                >
                    ⚙
                </button>
                {dto?.pending && <span className="chip cmp-pending">Áp dụng từ lượt sau</span>}
                <span className="cmp-right">
                    {runLabel(runState) && <span className={`cmp-state cmp-st-${runState?.kind}`}>{runLabel(runState)}</span>}
                    {managed && sid && (
                        <button type="button" className="link-btn" onClick={stop} title="Tắt tiến trình claude do ccmon chạy (transcript giữ nguyên)">
                            Tắt tiến trình
                        </button>
                    )}
                </span>
            </div>
        </div>
    );
}
