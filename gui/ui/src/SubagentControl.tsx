import { useCallback, useEffect, useMemo, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { AgentItem, api, SubagentAction, SubagentDto } from './api';
import { AgentForm } from './Agents';
import { LOCK_REASON } from './Composer';

const RESTART_WARNING = 'Session sẽ khởi động lại để nạp agent mới (các tác vụ nền khác của session sẽ dừng).';

function SwapPicker({
    sid,
    dto,
    onClose,
    onConfirm
}: {
    sid: string | null;
    dto: SubagentDto;
    onClose: () => void;
    onConfirm: (name: string) => Promise<void>;
}) {
    const [project, setProject] = useState<string | null>(null);
    const [agents, setAgents] = useState<AgentItem[]>([]);
    const [filter, setFilter] = useState('');
    const [picked, setPicked] = useState<string | null>(null);
    const [created, setCreated] = useState<string | null>(null);
    const [form, setForm] = useState(false);
    const [busy, setBusy] = useState(false);
    const [err, setErr] = useState<string | null>(null);
    const [tick, setTick] = useState(0);

    const [projectReady, setProjectReady] = useState(!sid);
    useEffect(() => {
        if (!sid) return;
        let live = true;
        api.sessionProject(sid)
            .then((p) => live && setProject(p))
            .catch(() => live && setProject(null))
            .finally(() => live && setProjectReady(true));
        return () => {
            live = false;
        };
    }, [sid]);
    useEffect(() => {
        if (!projectReady) return;
        let live = true;
        api.listAgents(sid, project, true)
            .then((a) => live && setAgents(a))
            .catch(() => live && setAgents([]));
        return () => {
            live = false;
        };
    }, [sid, project, projectReady, tick]);

    const list = useMemo(() => {
        const q = filter.trim().toLowerCase();
        return agents.filter((a) => !a.disabled && a.name !== dto.call.agentType && (!q || a.name.toLowerCase().includes(q)));
    }, [agents, filter, dto.call.agentType]);

    const confirm = () => {
        if (!picked) return;
        setBusy(true);
        setErr(null);
        onConfirm(picked)
            .catch((e) => setErr(String(e)))
            .finally(() => setBusy(false));
    };

    return (
        <div className="viewer-backdrop" onClick={onClose}>
            <div className="setup sa-picker" onClick={(e) => e.stopPropagation()}>
                <h2>Đổi agent</h2>
                <div className="sa-picker-bar">
                    <input className="lib-filter" style={{ marginLeft: 0 }} value={filter} autoFocus placeholder="Lọc agent…" onChange={(e) => setFilter(e.target.value)} />
                    <button className="link-btn" type="button" style={{ marginLeft: 0 }} onClick={() => setForm(true)}>
                        + Agent mới
                    </button>
                </div>
                <div className="sa-picker-list">
                    {list.map((a) => (
                        <button key={a.name} type="button" className={`sa-picker-item ${picked === a.name ? 'on' : ''}`} onClick={() => setPicked(a.name)}>
                            <b>{a.name}</b>
                            <span className="muted small">{a.description}</span>
                        </button>
                    ))}
                    {list.length === 0 && <div className="muted small">Không có agent phù hợp.</div>}
                </div>
                {picked && picked === created && dto.channel === 'managed' && <div className="setup-error">{RESTART_WARNING}</div>}
                {err && <div className="setup-error">{err}</div>}
                <div className="sa-picker-actions">
                    <button className="btn" type="button" onClick={onClose} disabled={busy}>
                        Đóng
                    </button>
                    <button className="btn btn-primary" type="button" disabled={!picked || busy} onClick={confirm}>
                        {picked ? `Chạy lại bằng ${picked}` : 'Chạy lại bằng …'}
                    </button>
                </div>
            </div>
            {form && (
                <div onClick={(e) => e.stopPropagation()}>
                <AgentForm
                    initial={null}
                    project={project ?? ''}
                    onClose={() => setForm(false)}
                    onSaved={(name) => {
                        setForm(false);
                        setCreated(name);
                        setPicked(name);
                        setTick((t) => t + 1);
                    }}
                />
                </div>
            )}
        </div>
    );
}

export function SubagentControl({ agentKey, compact }: { agentKey: string; compact?: boolean }) {
    const [dto, setDto] = useState<SubagentDto | null>(null);
    const [busy, setBusy] = useState(false);
    const [err, setErr] = useState<string | null>(null);
    const [swap, setSwap] = useState(false);
    const sid = agentKey.split(':')[1] ?? null;

    const load = useCallback(() => {
        api.subagent(agentKey).then(setDto).catch(() => setDto(null));
    }, [agentKey]);
    useEffect(() => {
        let live = true;
        const fetchDto = () => {
            api.subagent(agentKey)
                .then((d) => {
                    if (!live) return;
                    setDto(d);
                })
                .catch(() => live && setDto(null));
        };
        setDto(null);
        fetchDto();
        const un = listen('store-changed', fetchDto);
        return () => {
            live = false;
            un.then((f) => f());
        };
    }, [agentKey]);

    const run = (action: SubagentAction, agent?: string) => {
        setBusy(true);
        setErr(null);
        return api
            .subagentAction(agentKey, action, agent)
            .then(load)
            .finally(() => setBusy(false));
    };
    const act = (action: SubagentAction) => {
        run(action).catch((e) => setErr(String(e)));
    };

    if (!dto) return null;
    const active = dto.status === 'running' || dto.status === 'idle';
    if (!active && !dto.paused) return null;
    const cancel = () => {
        if (window.confirm('Hủy subagent này? Main thread sẽ coi nó là thất bại.')) act('cancel');
    };
    const cls = `sa-ctl${compact ? ' compact' : ''}`;
    return (
        <span className={cls}>
            {dto.channel === null ? (
                <>
                    {dto.paused && <span className="badge">tạm dừng</span>}
                    <span className="sa-disabled" title={LOCK_REASON[dto.reason ?? ''] ?? 'Không điều khiển được session này'}>
                        <button className="link-btn" type="button" disabled>
                            {dto.paused ? 'Tiếp tục' : 'Dừng'}
                        </button>
                    </span>
                </>
            ) : dto.paused ? (
                <>
                    <span className="badge">tạm dừng</span>
                    <button className="link-btn" type="button" disabled={busy} title="Chạy lại cùng agent, tiếp tục từ chỗ đã dừng" onClick={() => act('resume')}>
                        Tiếp tục
                    </button>
                    <button className="link-btn" type="button" disabled={busy} onClick={() => setSwap(true)}>
                        Đổi agent
                    </button>
                    <button className="link-btn" type="button" disabled={busy} onClick={cancel}>
                        Hủy
                    </button>
                </>
            ) : (
                <>
                    <button className="link-btn" type="button" disabled={busy} onClick={() => act('pause')}>
                        Dừng
                    </button>
                    <button className="link-btn" type="button" disabled={busy} onClick={cancel}>
                        Hủy
                    </button>
                </>
            )}
            {err && <span className="setup-error sa-err">{err}</span>}
            {swap && (
                <SwapPicker
                    sid={sid}
                    dto={dto}
                    onClose={() => setSwap(false)}
                    onConfirm={(name) => run('swap', name).then(() => setSwap(false))}
                />
            )}
        </span>
    );
}
