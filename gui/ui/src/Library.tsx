import { useEffect, useMemo, useState } from 'react';
import { api, CapKind, CapScope, Capability, Usage } from './api';

const TABS: [CapKind, string][] = [
    ['skill', 'Skills'],
    ['plugin', 'Plugins'],
    ['mcp', 'MCP']
];

const TOGGLE_TIP = 'Chỉ áp dụng cho session chạy từ ccmon';

function scopeLabel(s: CapScope): { text: string; cls: string; title: string } {
    if (typeof s === 'string') {
        const t = { user: 'Người dùng', project: 'Dự án', builtIn: 'Có sẵn', managed: 'Của session' }[s];
        return { text: t, cls: `scope-${s}`, title: t };
    }
    return { text: s.plugin.split('@')[0], cls: 'scope-plugin', title: `Plugin ${s.plugin}` };
}

function UseCell({ u }: { u: Usage | undefined }) {
    if (!u) return <span className="use-unavailable">—</span>;
    if (u.state === 'used') return <span className="use-used">Đã dùng ×{u.count}</span>;
    if (u.state === 'available') return <span className="use-available">Khả dụng</span>;
    return <span className="use-unavailable">Không có</span>;
}

export function Library({ sid, sessionLabel }: { sid: string | null; sessionLabel: string | null }) {
    const [tab, setTab] = useState<CapKind>('skill');
    const [filter, setFilter] = useState('');
    const [caps, setCaps] = useState<Capability[] | null>(null);
    const [usage, setUsage] = useState<Usage[]>([]);

    useEffect(() => {
        let ignore = false;
        api.catalog(sid).then((c) => !ignore && setCaps(c));
        return () => {
            ignore = true;
        };
    }, [sid]);

    useEffect(() => {
        if (!sid) {
            setUsage([]);
            return;
        }
        let ignore = false;
        const load = () => api.sessionUsage(sid).then((u) => !ignore && setUsage(u));
        load();
        const t = setInterval(load, 5000);
        return () => {
            ignore = true;
            clearInterval(t);
        };
    }, [sid]);

    const usageOf = useMemo(() => {
        const m = new Map<string, Usage>();
        for (const u of usage) m.set(`${u.kind}:${u.id}`, u);
        return m;
    }, [usage]);

    const rows = useMemo(() => {
        const q = filter.trim().toLowerCase();
        const list = (caps ?? []).filter(
            (c) => c.kind === tab && (!q || c.name.toLowerCase().includes(q) || c.description.toLowerCase().includes(q))
        );
        const rank = (c: Capability) => (usageOf.get(`${c.kind}:${c.id}`)?.state === 'used' ? 0 : 1);
        return sid ? [...list].sort((a, b) => rank(a) - rank(b)) : list;
    }, [caps, tab, filter, sid, usageOf]);

    const withUse = !!sid && tab !== 'plugin';
    const count = (k: CapKind) => (caps ?? []).filter((c) => c.kind === k).length;

    return (
        <div className="lib">
            <div className="lib-bar">
                <div className="lib-tabs">
                    {TABS.map(([k, label]) => (
                        <button key={k} type="button" className={`sb-toggle ${tab === k ? 'on' : ''}`} onClick={() => setTab(k)}>
                            {label} · {count(k)}
                        </button>
                    ))}
                </div>
                {sessionLabel && sid && <span className="muted small">Session: {sessionLabel}</span>}
                <input
                    className="lib-filter"
                    placeholder="Lọc theo tên hoặc mô tả…"
                    value={filter}
                    onChange={(e) => setFilter(e.target.value)}
                />
            </div>
            <div className="lib-list">
                <div className="lib-head">
                    <span>Tên</span>
                    <span>Mô tả</span>
                    <span>Phạm vi</span>
                    <span>Toàn cục</span>
                    <span>{withUse ? 'Session này' : ''}</span>
                    <span />
                </div>
                {caps === null && <div className="muted pad small">Đang tải…</div>}
                {caps !== null && rows.length === 0 && <div className="muted pad small">Không có mục nào.</div>}
                {rows.map((c) => {
                    const sc = scopeLabel(c.scope);
                    const u = usageOf.get(`${c.kind}:${c.id}`);
                    return (
                        <div key={`${c.kind}:${c.id}:${c.path ?? ''}`} className="lib-row" title={c.path ?? c.id}>
                            <span className="lib-name">{c.name}</span>
                            <span className="lib-desc" title={c.description}>
                                {c.description.split('\n')[0]}
                            </span>
                            <span className={`chip ${sc.cls}`} title={sc.title}>
                                {sc.text}
                            </span>
                            <span className={c.enabledGlobally ? 'glob-on' : 'glob-off'}>{c.enabledGlobally ? 'Bật' : 'Tắt'}</span>
                            <span>{withUse && <UseCell u={u} />}</span>
                            <span
                                className={`switch ${c.enabledGlobally ? 'on' : ''}`}
                                role="switch"
                                aria-checked={c.enabledGlobally}
                                aria-disabled="true"
                                title={TOGGLE_TIP}
                            />
                        </div>
                    );
                })}
            </div>
        </div>
    );
}
