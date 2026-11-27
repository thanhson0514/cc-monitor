import { useEffect, useMemo, useState } from 'react';
import { AgentDef, AgentItem, api, CapScope, EXISTS_PREFIX, fmtDur, RunRecord } from './api';
import { Markdown } from './Markdown';

type Tab = 'agent' | 'workflow';

const GROUPS: [string, string][] = [
    ['builtIn', 'Có sẵn'],
    ['plugin', 'Plugin'],
    ['user', 'Người dùng'],
    ['project', 'Dự án'],
    ['managed', 'Chỉ còn lịch sử']
];

const groupOf = (s: CapScope): string => (typeof s === 'string' ? s : 'plugin');

const STATUS_VI: Record<string, string> = {
    running: 'đang chạy',
    completed: 'hoàn tất',
    failed: 'lỗi',
    killed: 'đã dừng',
    unknown: 'không rõ'
};
export const statusVi = (s: string) => STATUS_VI[s] ?? s;

const DAYS: [number, string][] = [
    [7, '7 ngày'],
    [30, '30 ngày'],
    [90, '90 ngày'],
    [0, 'Tất cả']
];

const MODELS = ['inherit', 'opus', 'sonnet', 'haiku', 'fable'];

function fmtWhen(ts: number | null): string {
    if (ts == null) return '—';
    const d = new Date(ts);
    return `${d.toLocaleDateString('vi-VN')} ${d.toLocaleTimeString('vi-VN', { hour: '2-digit', minute: '2-digit' })}`;
}

function toolsOf(a: AgentItem): string[] {
    const t = a.meta.tools;
    if (Array.isArray(t)) return t.map(String);
    if (typeof t === 'string')
        return t
            .split(',')
            .map((x) => x.trim())
            .filter(Boolean);
    return [];
}

function scopeText(s: CapScope): string {
    if (typeof s !== 'string') return `Plugin ${s.plugin.split('@')[0]}`;
    return { user: 'Người dùng', project: 'Dự án', builtIn: 'Có sẵn', managed: 'Chỉ còn lịch sử' }[s];
}

function History({ item, kind, onOpenRun }: { item: AgentItem; kind: Tab; onOpenRun: (key: string, sid: string) => void }) {
    const [days, setDays] = useState(30);
    const [runs, setRuns] = useState<RunRecord[] | null>(null);
    useEffect(() => {
        let ignore = false;
        setRuns(null);
        (kind === 'agent' ? api.agentHistory(item.id, days) : api.workflowHistory(item.id, days))
            .then((r) => !ignore && setRuns(r))
            .catch(() => !ignore && setRuns([]));
        return () => {
            ignore = true;
        };
    }, [item.id, kind, days]);
    return (
        <section className="ag-sec">
            <div className="ag-sec-head">
                <span className="ag-sec-title">Lịch sử chạy</span>
                {runs && <span className="muted small">{runs.length} lượt</span>}
                <select className="cmp-chip ag-days" value={days} onChange={(e) => setDays(Number(e.target.value))} title="Khoảng thời gian">
                    {DAYS.map(([d, l]) => (
                        <option key={d} value={d}>
                            {l}
                        </option>
                    ))}
                </select>
            </div>
            {item.disabled && <div className="muted small ag-note">Đang tắt trong ccmon — lịch sử vẫn được giữ và xem được.</div>}
            {runs === null && <div className="muted small">Đang tải…</div>}
            {runs !== null && runs.length === 0 && <div className="muted small">Chưa có lượt chạy nào trong khoảng này.</div>}
            {runs !== null && runs.length > 0 && (
                <div className="ag-runs">
                    <div className="ag-run ag-run-head">
                        <span>Bắt đầu</span>
                        <span>Thời lượng</span>
                        <span>Trạng thái</span>
                        <span>Mô tả</span>
                        <span>Session</span>
                    </div>
                    {runs.map((r) => (
                        <button key={r.nodeKey} type="button" className="ag-run" onClick={() => onOpenRun(r.nodeKey, r.sessionId)} title="Mở luồng của lượt chạy này">
                            <span>{fmtWhen(r.started)}</span>
                            <span className="muted">{r.started != null && r.ended != null ? fmtDur(Math.max(0, r.ended - r.started)) : '—'}</span>
                            <span className={`ag-st ag-st-${r.status}`}>{statusVi(r.status)}</span>
                            <span className="lib-desc" title={r.description}>
                                {r.description || '—'}
                            </span>
                            <span className="muted mono" title={`${r.project} · ${r.sessionId}`}>
                                {r.sessionId.slice(0, 8)}
                            </span>
                        </button>
                    ))}
                </div>
            )}
        </section>
    );
}

function Detail({
    item,
    kind,
    onToggle,
    onEdit,
    onOpenRun
}: {
    item: AgentItem;
    kind: Tab;
    onToggle: () => void;
    onEdit: () => void;
    onOpenRun: (key: string, sid: string) => void;
}) {
    const tools = toolsOf(item);
    const model = typeof item.meta.model === 'string' ? item.meta.model : null;
    const readOnly =
        item.scope === 'builtIn'
            ? 'Agent có sẵn của Claude Code — không có file, chỉ xem.'
            : typeof item.scope !== 'string'
              ? 'Agent của plugin — chỉ đọc.'
              : item.scope === 'managed'
                ? 'Không có file trên đĩa (đã xoá, thuộc dự án khác hoặc chạy từ script) — chỉ còn lịch sử.'
                : null;
    return (
        <div className="ag-detail">
            <div className="ag-detail-head">
                <h2 className="ag-title">{item.name}</h2>
                <span className={`chip scope-${groupOf(item.scope)}`}>{scopeText(item.scope)}</span>
                {item.disabled && <span className="chip ag-off">Đã tắt</span>}
                <span className="ag-head-right">
                    {item.editable && (
                        <button type="button" className="btn ag-btn" onClick={onEdit}>
                            Sửa
                        </button>
                    )}
                    <label className="ag-switch" title={kind === 'agent' ? 'Tắt: mọi session chạy từ ccmon sẽ chặn agent này. File và lịch sử được giữ nguyên.' : 'Tắt: mọi session chạy từ ccmon sẽ chặn workflow này. File và lịch sử được giữ nguyên.'}>
                        <span>Bật trong ccmon</span>
                        <button type="button" className={`switch live ${item.disabled ? '' : 'on'}`} role="switch" aria-checked={!item.disabled} onClick={onToggle} />
                    </label>
                </span>
            </div>
            {item.description ? <p className="ag-desc">{item.description}</p> : <p className="ag-desc muted">Không có mô tả.</p>}
            <dl className="ag-meta">
                {kind === 'agent' && item.path && (
                    <>
                        <dt>Tools</dt>
                        <dd>
                            {tools.length ? (
                                tools.map((t) => (
                                    <span key={t} className="chip ag-tool">
                                        {t}
                                    </span>
                                ))
                            ) : (
                                <span className="muted">Kế thừa mọi tool</span>
                            )}
                        </dd>
                        <dt>Model</dt>
                        <dd>{model ?? <span className="muted">Kế thừa</span>}</dd>
                    </>
                )}
                <dt>Lượt chạy</dt>
                <dd>
                    {item.runs} <span className="muted">trong 30 ngày{item.lastRun ? ` · hoạt động gần nhất ${fmtWhen(item.lastRun)}` : ''}</span>
                </dd>
                {item.path && (
                    <>
                        <dt>File</dt>
                        <dd className="mono ag-path" title={item.path}>
                            {item.path}
                        </dd>
                    </>
                )}
            </dl>
            {readOnly && <div className="muted small ag-note">{readOnly}</div>}
            {item.truncated && <div className="muted small ag-note">Prompt quá dài để sửa trong ccmon — chỉ hiện phần đầu; hãy sửa file trực tiếp.</div>}
            {item.body != null && item.body.trim() !== '' && (
                <section className="ag-sec">
                    <div className="ag-sec-head">
                        <span className="ag-sec-title">{kind === 'agent' ? 'Prompt' : 'Script'}</span>
                    </div>
                    <div className="ag-body">
                        <Markdown text={kind === 'agent' ? item.body : '```js\n' + item.body + '\n```'} />
                    </div>
                </section>
            )}
            <History item={item} kind={kind} onOpenRun={onOpenRun} />
        </div>
    );
}

export function AgentForm({
    initial,
    project,
    onClose,
    onSaved
}: {
    initial: { def: AgentDef; scope: 'user' | 'project' } | null;
    project: string;
    onClose: () => void;
    onSaved: (name: string) => void;
}) {
    const [def, setDef] = useState<AgentDef>(initial?.def ?? { name: '', description: '', tools: null, model: null, prompt: '' });
    const [scope, setScope] = useState<'user' | 'project'>(initial?.scope ?? 'user');
    const [allTools, setAllTools] = useState<string[]>([]);
    const [toolFilter, setToolFilter] = useState('');
    const [err, setErr] = useState<string | null>(null);
    const [exists, setExists] = useState<string | null>(null);
    const [saving, setSaving] = useState(false);
    useEffect(() => {
        api.agentTools().then(setAllTools).catch(() => setAllTools([]));
    }, []);
    const picked = def.tools ?? [];
    const toolList = useMemo(() => {
        const q = toolFilter.trim().toLowerCase();
        const all = [...new Set([...picked, ...allTools])];
        return all.filter((t) => !q || t.toLowerCase().includes(q)).slice(0, 200);
    }, [allTools, picked, toolFilter]);
    const nameOk = /^[a-z0-9][a-z0-9-]{0,63}$/.test(def.name);
    const canSave = nameOk && def.description.trim() !== '' && (scope === 'user' || project !== '') && !saving;
    const target = scope === 'user' ? `~/.claude/agents/${def.name || '<tên>'}.md` : `${project || '<dự án>'}/.claude/agents/${def.name || '<tên>'}.md`;

    const toggleTool = (t: string) => {
        const cur = def.tools ?? [];
        const next = cur.includes(t) ? cur.filter((x) => x !== t) : [...cur, t];
        setDef({ ...def, tools: next.length ? next : null });
    };

    const save = (overwrite: boolean) => {
        setSaving(true);
        setErr(null);
        api.saveAgent(scope, scope === 'project' ? project : null, def, overwrite)
            .then(() => onSaved(def.name))
            .catch((e) => {
                const s = String(e);
                if (s.startsWith(EXISTS_PREFIX)) setExists(s.slice(EXISTS_PREFIX.length));
                else setErr(s);
            })
            .finally(() => setSaving(false));
    };

    return (
        <div className="viewer-backdrop" onClick={onClose}>
            <div className="setup ag-form" onClick={(e) => e.stopPropagation()}>
                <h2>{initial ? `Sửa agent ${initial.def.name}` : 'Agent mới'}</h2>
                <div className="ag-form-grid">
                    <label>Tên</label>
                    <div>
                        <input className="lib-filter ag-input" value={def.name} autoFocus={!initial} placeholder="vd. code-reviewer" onChange={(e) => setDef({ ...def, name: e.target.value.trim() })} />
                        {def.name !== '' && !nameOk && <div className="setup-error">Chỉ chữ thường a–z, số và dấu “-”, tối đa 64 ký tự, không bắt đầu bằng “-”.</div>}
                        {initial && def.name !== initial.def.name && <div className="muted small">Đổi tên sẽ tạo file mới; file cũ giữ nguyên.</div>}
                    </div>
                    <label>Lưu ở</label>
                    <div className="ag-scope">
                        <button type="button" className={`sb-toggle ${scope === 'user' ? 'on' : ''}`} onClick={() => setScope('user')}>
                            Người dùng
                        </button>
                        <button
                            type="button"
                            className={`sb-toggle ${scope === 'project' ? 'on' : ''}`}
                            disabled={!project}
                            title={project ? project : 'Chọn một dự án ở thanh trên'}
                            onClick={() => setScope('project')}
                        >
                            Dự án
                        </button>
                        <span className="muted small mono ag-target" title={target}>
                            {target}
                        </span>
                    </div>
                    <label>Mô tả</label>
                    <textarea className="cmp-input ag-input" rows={3} value={def.description} placeholder="Khi nào Claude nên dùng agent này…" onChange={(e) => setDef({ ...def, description: e.target.value })} />
                    <label>Tools</label>
                    <div>
                        <div className="ag-tools-bar">
                            <span className="muted small">{picked.length ? `${picked.length} tool đã chọn` : 'Không chọn = kế thừa mọi tool'}</span>
                            <input className="lib-filter ag-tool-filter" placeholder="Lọc tool…" value={toolFilter} onChange={(e) => setToolFilter(e.target.value)} />
                        </div>
                        <div className="ag-tools">
                            {toolList.map((t) => (
                                <button key={t} type="button" className={`sb-toggle ag-tool-pick ${picked.includes(t) ? 'on' : ''}`} onClick={() => toggleTool(t)}>
                                    {t}
                                </button>
                            ))}
                        </div>
                    </div>
                    <label>Model</label>
                    <div>
                        <input className="lib-filter ag-input ag-model" list="ag-models" value={def.model ?? ''} placeholder="kế thừa" onChange={(e) => setDef({ ...def, model: e.target.value.trim() || null })} />
                        <datalist id="ag-models">
                            {MODELS.map((m) => (
                                <option key={m} value={m} />
                            ))}
                        </datalist>
                    </div>
                    <label>Prompt</label>
                    <textarea className="cmp-input ag-input ag-prompt" rows={10} value={def.prompt} placeholder="System prompt của agent (Markdown)…" onChange={(e) => setDef({ ...def, prompt: e.target.value })} />
                </div>
                {err && <div className="setup-error">{err}</div>}
                {exists && (
                    <div className="ag-confirm" role="alert">
                        <span>
                            File <code>{exists}</code> đã tồn tại. Ghi đè nội dung hiện có?
                        </span>
                        <button type="button" className="btn" onClick={() => setExists(null)}>
                            Không
                        </button>
                        <button type="button" className="btn ag-danger" disabled={saving} onClick={() => save(true)}>
                            Ghi đè
                        </button>
                    </div>
                )}
                <div className="setup-actions">
                    <button type="button" className="btn" onClick={onClose}>
                        Huỷ
                    </button>
                    <button type="button" className="btn btn-primary" disabled={!canSave || !!exists} onClick={() => save(false)}>
                        {saving ? 'Đang lưu…' : 'Lưu'}
                    </button>
                </div>
            </div>
        </div>
    );
}

const PROJECT_NONE = '';

export function Agents({ sid, onOpenRun }: { sid: string | null; onOpenRun: (key: string, sid: string) => void }) {
    const [tab, setTab] = useState<Tab>('agent');
    const [filter, setFilter] = useState('');
    const [projects, setProjects] = useState<string[]>([]);
    const [project, setProject] = useState<string | null>(null); // null: not resolved yet
    const [agents, setAgents] = useState<AgentItem[] | null>(null);
    const [workflows, setWorkflows] = useState<AgentItem[] | null>(null);
    const [sel, setSel] = useState<Record<Tab, string | null>>({ agent: null, workflow: null });
    const [err, setErr] = useState<string | null>(null);
    const [form, setForm] = useState<{ def: AgentDef; scope: 'user' | 'project' } | 'new' | null>(null);
    const [tick, setTick] = useState(0);

    useEffect(() => {
        api.projects().then(setProjects).catch(() => setProjects([]));
    }, []);
    // Default project: the selected session's cwd (until the user picks one).
    const [projectPicked, setProjectPicked] = useState(false);
    useEffect(() => {
        if (projectPicked) return;
        if (!sid) {
            setProject(PROJECT_NONE);
            return;
        }
        api.sessionProject(sid)
            .then((p) => setProject(p ?? PROJECT_NONE))
            .catch(() => setProject(PROJECT_NONE));
    }, [sid, projectPicked]);

    useEffect(() => {
        if (project === null) return;
        let ignore = false;
        setErr(null);
        const cwd = project || null;
        api.listAgents(null, cwd)
            .then((a) => !ignore && setAgents(a))
            .catch((e) => !ignore && (setErr(String(e)), setAgents([])));
        api.listWorkflows(null, cwd)
            .then((w) => !ignore && setWorkflows(w))
            .catch((e) => !ignore && (setErr(String(e)), setWorkflows([])));
        return () => {
            ignore = true;
        };
    }, [project, tick]);

    const items = tab === 'agent' ? agents : workflows;
    const shown = useMemo(() => {
        const q = filter.trim().toLowerCase();
        return (items ?? []).filter((c) => !q || c.name.toLowerCase().includes(q) || c.description.toLowerCase().includes(q));
    }, [items, filter]);
    const current = shown.find((c) => c.id === sel[tab]) ?? shown[0] ?? null;

    const toggle = (c: AgentItem) => {
        const disabled = !c.disabled;
        const patch = (l: AgentItem[] | null) => l && l.map((x) => (x.id === c.id ? { ...x, disabled } : x));
        if (tab === 'agent') setAgents(patch);
        else setWorkflows(patch);
        (tab === 'agent' ? api.setAgentDisabled(c.id, disabled) : api.setWorkflowDisabled(c.id, disabled))
            .catch((e) => setErr(String(e)))
            .finally(() => setTick((t) => t + 1));
    };

    const edit = (c: AgentItem) => {
        const tools = toolsOf(c);
        setForm({
            scope: c.scope === 'project' ? 'project' : 'user',
            def: {
                name: c.name,
                description: c.description,
                tools: tools.length ? tools : null,
                model: typeof c.meta.model === 'string' ? c.meta.model : null,
                prompt: c.body ?? ''
            }
        });
    };

    const projectOptions = project && !projects.includes(project) ? [project, ...projects] : projects;

    return (
        <div className="lib">
            <div className="lib-bar ag-bar">
                <div className="lib-tabs">
                    <button type="button" className={`sb-toggle ${tab === 'agent' ? 'on' : ''}`} onClick={() => setTab('agent')}>
                        Agents · {agents?.length ?? '…'}
                    </button>
                    <button type="button" className={`sb-toggle ${tab === 'workflow' ? 'on' : ''}`} onClick={() => setTab('workflow')}>
                        Workflows · {workflows?.length ?? '…'}
                    </button>
                </div>
                <select
                    className="cmp-chip ag-project"
                    value={project ?? PROJECT_NONE}
                    title="Dự án: agent/workflow cấp dự án được đọc từ thư mục này"
                    onChange={(e) => {
                        setProjectPicked(true);
                        setProject(e.target.value);
                    }}
                >
                    <option value={PROJECT_NONE}>dự án: không chọn</option>
                    {projectOptions.map((p) => (
                        <option key={p} value={p}>
                            dự án: {p}
                        </option>
                    ))}
                </select>
                {err && <span className="cmp-err small">{err}</span>}
                <input className="lib-filter" placeholder="Lọc theo tên hoặc mô tả…" value={filter} onChange={(e) => setFilter(e.target.value)} />
                {tab === 'agent' && (
                    <button type="button" className="btn btn-primary ag-new" onClick={() => setForm('new')}>
                        + Agent mới
                    </button>
                )}
            </div>
            <div className="ag-split">
                <div className="ag-list">
                    {items === null && <div className="muted pad small">Đang tải…</div>}
                    {items !== null && shown.length === 0 && <div className="muted pad small">Không có mục nào.</div>}
                    {GROUPS.map(([g, label]) => {
                        const list = shown.filter((c) => groupOf(c.scope) === g);
                        if (!list.length) return null;
                        return (
                            <div key={g} className="ag-group">
                                <div className="ag-group-title">
                                    {label} <span className="muted">· {list.length}</span>
                                </div>
                                {list.map((c) => (
                                    <button
                                        key={c.id}
                                        type="button"
                                        className={`ag-item ${current?.id === c.id ? 'selected' : ''} ${c.disabled ? 'off' : ''}`}
                                        onClick={() => setSel({ ...sel, [tab]: c.id })}
                                        title={c.description || c.id}
                                    >
                                        <span className="ag-item-name">{c.name}</span>
                                        {c.disabled && <span className="chip ag-off">Đã tắt</span>}
                                        {c.runs > 0 && <span className="muted small ag-runs-n">×{c.runs}</span>}
                                        <span className="ag-item-desc">{c.description.split('\n')[0] || (typeof c.scope !== 'string' ? c.scope.plugin : '')}</span>
                                    </button>
                                ))}
                            </div>
                        );
                    })}
                </div>
                {current ? (
                    <Detail key={`${tab}:${current.id}`} item={current} kind={tab} onToggle={() => toggle(current)} onEdit={() => edit(current)} onOpenRun={onOpenRun} />
                ) : (
                    <div className="ag-detail muted pad">Chọn một mục để xem chi tiết.</div>
                )}
            </div>
            {form && (
                <AgentForm
                    initial={form === 'new' ? null : form}
                    project={project ?? ''}
                    onClose={() => setForm(null)}
                    onSaved={(name) => {
                        setForm(null);
                        setTab('agent');
                        setSel((s) => ({ ...s, agent: name }));
                        setTick((t) => t + 1);
                    }}
                />
            )}
        </div>
    );
}
