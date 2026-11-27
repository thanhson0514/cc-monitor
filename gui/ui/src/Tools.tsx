import { useState } from 'react';
import hljs from 'highlight.js/lib/common';
import { api, fmtDur, Item, ToolDetail } from './api';
import { BgTaskView, YieldBody } from './BgTask';
import { InlineImage, isImagePath, PathLink } from './open';
import { matchPath } from './Markdown';

export type ToolItem = Extract<Item, { type: 'tool' }>;

const escapeHtml = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

const EXT_LANG: Record<string, string> = {
    ts: 'typescript', tsx: 'typescript', js: 'javascript', jsx: 'javascript', mjs: 'javascript', rs: 'rust',
    py: 'python', sh: 'bash', bash: 'bash', zsh: 'bash', json: 'json', md: 'markdown', css: 'css', scss: 'scss',
    html: 'xml', xml: 'xml', yml: 'yaml', yaml: 'yaml', toml: 'ini', go: 'go', java: 'java', c: 'c', h: 'c',
    cpp: 'cpp', hpp: 'cpp', cs: 'csharp', sql: 'sql', rb: 'ruby', php: 'php', kt: 'kotlin', swift: 'swift'
};

export function langOf(path: string): string | undefined {
    const ext = path.split('.').pop()?.toLowerCase() ?? '';
    return EXT_LANG[ext];
}

export function highlight(code: string, lang?: string): string {
    try {
        if (lang && hljs.getLanguage(lang)) return hljs.highlight(code, { language: lang }).value;
    } catch {
        /* fall through */
    }
    return escapeHtml(code);
}

const str = (v: unknown): string | undefined => (typeof v === 'string' ? v : undefined);

export function PathLabel({ path, link }: { path: string; link?: boolean }) {
    const i = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
    const label = (
        <span className="path" title={link ? `${path}\nBấm để xem` : path}>
            {i >= 0 && <span className="path-dir">{path.slice(0, i + 1)}</span>}
            <span className="path-base">{path.slice(i + 1)}</span>
        </span>
    );
    return link ? <PathLink path={path}>{label}</PathLink> : label;
}

export function Code({
    code,
    lang,
    maxLines,
    numbered,
    fromEnd
}: {
    code: string;
    lang?: string;
    maxLines: number;
    numbered?: boolean;
    /** Collapsed view shows the last `maxLines` lines instead of the first (for logs). */
    fromEnd?: boolean;
}) {
    const [all, setAll] = useState(false);
    const lines = code.replace(/\n$/, '').split('\n');
    const shown = all ? lines : fromEnd ? lines.slice(-maxLines) : lines.slice(0, maxLines);
    const html = highlight(shown.join('\n'), lang);
    return (
        <div className="tcode">
            <div className="tcode-body">
                {numbered && <pre className="gutter">{shown.map((_, i) => i + 1).join('\n')}</pre>}
                <pre className="code">
                    <code dangerouslySetInnerHTML={{ __html: html }} />
                </pre>
            </div>
            {lines.length > maxLines && (
                <button type="button" className="more-lines" onClick={() => setAll(!all)}>
                    {all ? 'Thu gọn' : `… ${lines.length - maxLines} dòng nữa`}
                </button>
            )}
        </div>
    );
}

/** Plain tool output with absolute/home paths turned into viewer links. */
function LinkedText({ text }: { text: string }) {
    const out: React.ReactNode[] = [];
    const re = /(^|[\s'"`(\[=:])((?:~|\/)[\w.@+\-\/]+|[A-Za-z]:\\[\w.@+\-\\]+)/g;
    let last = 0;
    for (let m = re.exec(text); m; m = re.exec(text)) {
        const start = m.index + m[1].length;
        const raw = matchPath(text.slice(start));
        if (!raw) continue;
        out.push(text.slice(last, start));
        out.push(
            <PathLink key={start} path={raw}>
                {raw}
            </PathLink>
        );
        last = start + raw.length;
        re.lastIndex = last;
    }
    if (last === 0) return <>{text}</>;
    out.push(text.slice(last));
    return <>{out}</>;
}

function Output({ tool, threadKey }: { tool: ToolItem; threadKey: string }) {
    const [detail, setDetail] = useState<ToolDetail | null>(null);
    const [open, setOpen] = useState(false);
    const text = (detail?.result ?? tool.resultPreview).replace(/\s+$/, '');
    if (!text && tool.state !== 'running') return null;
    const lines = text.split('\n');
    const limit = open ? Infinity : 5;
    const shown = lines.slice(0, limit);
    const more = lines.length > shown.length || (tool.hasMore && !detail);
    return (
        <div className={`tout ${tool.state === 'error' ? 'tout-error' : ''}`}>
            <span className="tout-elbow">⎿</span>
            <div className="tout-body">
                {tool.state === 'running' && !text ? (
                    <span className="muted">Đang chạy…</span>
                ) : (
                    <pre>
                        <LinkedText text={shown.join('\n')} />
                    </pre>
                )}
                {(more || open) && (
                    <button
                        type="button"
                        className="more-lines"
                        onClick={() => {
                            if (!detail) api.toolDetail(threadKey, tool.id).then((d) => d && setDetail(d));
                            setOpen(!open);
                        }}
                    >
                        {open ? 'Thu gọn' : `Xem toàn bộ kết quả${tool.hasMore ? ' (file lớn)' : ` (${lines.length} dòng)`}`}
                    </button>
                )}
                {open && detail?.source && <div className="muted small">Nguồn: {detail.source}</div>}
            </div>
        </div>
    );
}

function diffCounts(oldS: string, newS: string): [number, number] {
    const o = oldS ? oldS.split('\n').length : 0;
    const n = newS ? newS.split('\n').length : 0;
    return [n, o];
}

function Diff({ oldS, newS, path }: { oldS: string; newS: string; path: string }) {
    const [all, setAll] = useState(false);
    const lang = langOf(path);
    const rows: { sign: '-' | '+'; html: string }[] = [];
    if (oldS) for (const l of highlight(oldS, lang).split('\n')) rows.push({ sign: '-', html: l });
    if (newS) for (const l of highlight(newS, lang).split('\n')) rows.push({ sign: '+', html: l });
    const max = 14;
    const shown = all ? rows : rows.slice(0, max);
    return (
        <div className="diff">
            {shown.map((r, i) => (
                <div key={i} className={r.sign === '-' ? 'diff-del' : 'diff-add'}>
                    <span className="diff-sign">{r.sign}</span>
                    <code dangerouslySetInnerHTML={{ __html: r.html || ' ' }} />
                </div>
            ))}
            {rows.length > max && (
                <button type="button" className="more-lines" onClick={() => setAll(!all)}>
                    {all ? 'Thu gọn' : `… ${rows.length - max} dòng nữa`}
                </button>
            )}
        </div>
    );
}

function Todos({ todos }: { todos: { content?: string; status?: string; activeForm?: string }[] }) {
    const icon = (s?: string) => (s === 'completed' ? '☑' : s === 'in_progress' ? '◐' : '☐');
    return (
        <ul className="todos">
            {todos.map((t, i) => (
                <li key={i} className={`todo todo-${t.status}`}>
                    <span>{icon(t.status)}</span> {t.content}
                </li>
            ))}
        </ul>
    );
}

function KeyValues({ input }: { input: Record<string, unknown> }) {
    const entries = Object.entries(input).filter(([, v]) => v !== null && v !== undefined && v !== '');
    if (entries.length === 0) return null;
    return (
        <div className="kv">
            {entries.map(([k, v]) => {
                const s = typeof v === 'string' ? v : JSON.stringify(v);
                return (
                    <div key={k} className="kv-row">
                        <span className="kv-key">{k}</span>
                        <span className="kv-val">{s.length > 300 ? `${s.slice(0, 300)}…` : s}</span>
                    </div>
                );
            })}
        </div>
    );
}

/** One tool call, rendered by tool type (like Claude Code's own transcript). */
export function ToolCard({ tool, threadKey, now }: { tool: ToolItem; threadKey: string; now: number }) {
    const input = (tool.input ?? {}) as Record<string, unknown>;
    const dur =
        tool.state === 'running'
            ? tool.started != null
                ? fmtDur(Math.max(0, now - tool.started))
                : ''
            : tool.started != null && tool.ended != null
              ? fmtDur(Math.max(0, tool.ended - tool.started))
              : '';

    let title: React.ReactNode = tool.summary;
    let badge: React.ReactNode = null;
    let body: React.ReactNode = null;
    let showOutput = true;
    const path = str(input.file_path) ?? str(input.notebook_path);

    switch (tool.name) {
        case 'Bash':
        case 'PowerShell': {
            const cmd = str(input.command) ?? '';
            title = str(input.description) ?? cmd.split('\n')[0];
            body = <Code code={cmd} lang={tool.name === 'Bash' ? 'bash' : 'powershell'} maxLines={4} />;
            if (input.run_in_background) badge = <span className="tbadge">nền</span>;
            if (tool.bg) {
                body = (
                    <>
                        {body}
                        <BgTaskView task={tool.bg} now={now} />
                    </>
                );
                // The launch result ("Command running in background…") is replaced by the real state.
                showOutput = tool.state === 'error';
            }
            break;
        }
        case 'Yield': {
            const n = tool.waits.length;
            title = n === 0 ? 'Không có tác vụ nền đang chạy' : n === 1 ? `Chờ: ${tool.waits[0].label}` : `Chờ ${n} tác vụ nền`;
            body = <YieldBody tool={tool} now={now} />;
            showOutput = false;
            break;
        }
        case 'Monitor': {
            const { description: _d, ...rest } = input;
            title = str(input.description) ?? tool.summary;
            badge = <span className="tbadge">monitor</span>;
            body = (
                <>
                    <KeyValues input={rest} />
                    {tool.bg && <BgTaskView task={tool.bg} now={now} />}
                </>
            );
            break;
        }
        case 'TaskOutput':
        case 'BashOutput':
        case 'TaskStop':
        case 'KillShell': {
            const stop = tool.name === 'TaskStop' || tool.name === 'KillShell';
            const label = tool.bg?.label ?? str(input.task_id) ?? str(input.bash_id) ?? str(input.shell_id) ?? '';
            title = `${stop ? 'Dừng' : 'Đọc output'}: ${label}`;
            if (tool.bg) body = <BgTaskView task={tool.bg} now={now} />;
            break;
        }
        case 'Edit': {
            const o = str(input.old_string) ?? '';
            const n = str(input.new_string) ?? '';
            const [a, r] = diffCounts(o, n);
            title = path ? <PathLabel path={path} link /> : tool.summary;
            badge = (
                <span className="tbadge">
                    <span className="add">+{a}</span> <span className="del">−{r}</span>
                    {input.replace_all ? ' · tất cả' : ''}
                </span>
            );
            body = <Diff oldS={o} newS={n} path={path ?? ''} />;
            showOutput = tool.state === 'error';
            break;
        }
        case 'MultiEdit': {
            const edits = (Array.isArray(input.edits) ? input.edits : []) as { old_string?: string; new_string?: string }[];
            title = path ? <PathLabel path={path} link /> : tool.summary;
            badge = <span className="tbadge">{edits.length} chỗ sửa</span>;
            body = edits.map((e, i) => <Diff key={i} oldS={e.old_string ?? ''} newS={e.new_string ?? ''} path={path ?? ''} />);
            showOutput = tool.state === 'error';
            break;
        }
        case 'Write': {
            const content = str(input.content) ?? '';
            title = path ? <PathLabel path={path} link /> : tool.summary;
            badge = <span className="tbadge">{content.split('\n').length} dòng</span>;
            body = <Code code={content} lang={langOf(path ?? '')} maxLines={8} numbered />;
            showOutput = tool.state === 'error';
            break;
        }
        case 'Read': {
            title = path ? <PathLabel path={path} link /> : tool.summary;
            const off = input.offset as number | undefined;
            const lim = input.limit as number | undefined;
            if (off || lim) badge = <span className="tbadge">dòng {off ?? 1}–{(off ?? 1) + (lim ?? 0)}</span>;
            if (path && isImagePath(path) && tool.state !== 'error') {
                body = <InlineImage path={path} />;
                showOutput = false;
            }
            break;
        }
        case 'Grep':
        case 'Glob': {
            title = (
                <span>
                    <code className="pattern">{str(input.pattern)}</code>
                    {str(input.path) && (
                        <span className="muted">
                            {' '}
                            trong <PathLabel path={str(input.path)!} link />
                        </span>
                    )}
                    {str(input.glob) && <span className="muted"> · {str(input.glob)}</span>}
                </span>
            );
            break;
        }
        case 'TodoWrite': {
            const todos = (Array.isArray(input.todos) ? input.todos : []) as { content?: string; status?: string }[];
            title = `${todos.filter((t) => t.status === 'completed').length}/${todos.length} việc xong`;
            body = <Todos todos={todos} />;
            showOutput = false;
            break;
        }
        case 'Patch': {
            const files = (Array.isArray(input.files) ? input.files : []) as string[];
            title = files.length === 1 ? <PathLabel path={files[0]} link /> : `${files.length} file`;
            body =
                files.length > 1 ? (
                    <ul className="todos">
                        {files.map((f) => (
                            <li key={f}>
                                <PathLabel path={f} link />
                            </li>
                        ))}
                    </ul>
                ) : null;
            break;
        }
        case 'WebFetch':
            title = str(input.url) ?? tool.summary;
            break;
        case 'WebSearch':
            title = `🔎 ${str(input.query) ?? ''}`;
            break;
        case 'Skill':
            title = `skill: ${str(input.skill) ?? ''}`;
            break;
        default:
            body = <KeyValues input={input} />;
    }

    return (
        <div className={`tcard tcard-${tool.state}`}>
            <div className="tcard-head">
                <span className={`tdot st-${tool.state}`}>●</span>
                <span className="tname">{tool.name}</span>
                <span className="ttitle">{title}</span>
                {badge}
                <span className="tdur">{dur}</span>
            </div>
            {body && <div className="tcard-body">{body}</div>}
            {showOutput && <Output tool={tool} threadKey={threadKey} />}
        </div>
    );
}
