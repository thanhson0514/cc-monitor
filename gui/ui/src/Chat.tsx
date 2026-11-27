import { memo, useState } from 'react';
import { Draft, DraftBlock, fmtDur, fmtTime, fmtTokens, Item, ThreadView } from './api';
import { Markdown } from './Markdown';
import { InlineImage, usePathOpener } from './open';
import { SubagentControl } from './SubagentControl';
import { ToolCard, ToolItem } from './Tools';

type Part = Exclude<Item, { type: 'prompt' } | { type: 'system' } | { type: 'compact' }>;

type Block =
    | { kind: 'user'; item: Extract<Item, { type: 'prompt' }> }
    | { kind: 'system'; item: Extract<Item, { type: 'system' }> }
    | { kind: 'compact'; item: Extract<Item, { type: 'compact' }> }
    | { kind: 'assistant'; parts: Part[] };

export function groupBlocks(items: Item[]): Block[] {
    const out: Block[] = [];
    for (const it of items) {
        if (it.type === 'prompt') out.push({ kind: 'user', item: it });
        else if (it.type === 'system') out.push({ kind: 'system', item: it });
        else if (it.type === 'compact') out.push({ kind: 'compact', item: it });
        else {
            const last = out[out.length - 1];
            if (last && last.kind === 'assistant') last.parts.push(it);
            else out.push({ kind: 'assistant', parts: [it] });
        }
    }
    return out;
}

function systemText(text: string): string {
    const summary = text.match(/<summary>([\s\S]*?)<\/summary>/);
    if (summary) return `🔔 ${summary[1].trim()}`;
    const cmd = text.match(/<command-name>([\s\S]*?)<\/command-name>/);
    if (cmd) return `⌘ ${cmd[1].trim()}`;
    return text.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim().slice(0, 300);
}

/** Number of a pasted image file (`…/images/9.png` → 9). */
const imageNo = (p: string) => p.match(/(\d+)\.[A-Za-z]+$/)?.[1];

function UserPrompt({ text, images }: { text: string; images: string[] }) {
    const { open } = usePathOpener();
    return (
        <>
            <div
                className="user-text"
                onClick={(e) => {
                    const ref = (e.target as HTMLElement).closest<HTMLElement>('[data-img]');
                    const hit = ref && images.find((p) => imageNo(p) === ref.dataset.img);
                    if (hit) open(hit);
                }}
            >
                <Markdown text={text} />
            </div>
            {images.length > 0 && (
                <div className="prompt-images">
                    {images.map((p) => (
                        <InlineImage key={p} path={p} label={imageNo(p) ? `Ảnh #${imageNo(p)}` : undefined} />
                    ))}
                </div>
            )}
        </>
    );
}

/** Best-effort readable view of a tool's partial input JSON while it streams. */
function partialInput(raw: string): string {
    const m = raw.match(/"(command|content|new_string|prompt|query|pattern|url|file_path|description)"\s*:\s*"((?:[^"\\]|\\.)*)/);
    if (!m) return raw;
    return m[2]
        .replace(/\\n/g, '\n')
        .replace(/\\t/g, '\t')
        .replace(/\\"/g, '"')
        .replace(/\\\\/g, '\\');
}

function LiveBlock({ b }: { b: DraftBlock }) {
    if (b.kind === 'text') return b.text ? <Markdown text={b.text} /> : null;
    if (b.kind === 'thinking')
        return (
            <div className="thinking-note live">
                <span className="thinking-icon">💭</span>
                <span>{b.text ? b.text.slice(-600) : 'Đang suy nghĩ…'}</span>
            </div>
        );
    const body = partialInput(b.text);
    const tail = body.split('\n').slice(-6).join('\n');
    return (
        <div className="tcard tcard-running live">
            <div className="tcard-head">
                <span className="tdot st-running">●</span>
                <span className="tname">{b.name ?? 'Tool'}</span>
                <span className="ttitle muted">{b.done ? 'đã soạn xong, đang chờ chạy…' : 'đang soạn…'}</span>
                <span className="tdur">{b.len > 0 ? `${(b.len / 1000).toFixed(1)}k ký tự` : ''}</span>
            </div>
            {tail && (
                <div className="tcard-body">
                    <pre className="live-input">{tail}</pre>
                </div>
            )}
        </div>
    );
}

/** Draft blocks the transcript does not have yet. */
export function pendingDraft(items: Item[], draft: Draft | null | undefined): DraftBlock[] {
    if (!draft || draft.blocks.length === 0) return [];
    const at = items.findIndex((i) => i.type === 'turn' && i.msgId === draft.msgId);
    if (at < 0) return draft.blocks;
    let have = 0;
    for (let i = at + 1; i < items.length; i++) {
        const t = items[i].type;
        if (t === 'turn' || t === 'prompt') break;
        if (t === 'text' || t === 'thinking' || t === 'tool') have++;
    }
    return draft.blocks.slice(have);
}

function LiveBlocks({ blocks }: { blocks: DraftBlock[] }) {
    return (
        <div className="live-draft">
            {blocks.map((b, i) => (
                <LiveBlock key={i} b={b} />
            ))}
            <div className="live-caret">● đang viết…</div>
        </div>
    );
}

const STATE_ICON: Record<string, string> = { running: '◌', ok: '✓', error: '✗', done: '✓', idle: '·' };

function Thinking({ text, ms }: { text: string; ms: number | null }) {
    // Claude Code usually stores only short progress notes here; the full reasoning is not saved.
    if (!text)
        return ms ? <div className="thinking-mini">💭 Suy nghĩ {fmtDur(ms)}</div> : null;
    return (
        <div className="thinking-note">
            <span className="thinking-icon">💭</span>
            <span>{text}</span>
            {ms ? <span className="muted small"> · {fmtDur(ms)}</span> : null}
        </div>
    );
}

function AgentCard({ tool, onOpen }: { tool: ToolItem; onOpen: (key: string) => void }) {
    const link = tool.link!;
    const [showPrompt, setShowPrompt] = useState(false);
    const status = link.status ?? (tool.state === 'running' ? 'running' : 'idle');
    const isWf = link.kind === 'workflow';
    return (
        <div className={`agent-card st-border-${status}`} data-tool-id={tool.id}>
            <div className="agent-card-head">
                <span className="agent-card-title">
                    {isWf ? '⚙ Giao cho Workflow' : '↪ Giao cho Agent'} <b>{link.label ?? tool.summary}</b>
                </span>
                <span className={`badge st-${status}`}>
                    {STATE_ICON[status]} {status}
                    {link.progress ? ` · ${link.progress[0]}/${link.progress[1]}` : ''}
                </span>
                {/* Controls only for a live or paused agent: a finished one has none (no polling). */}
                {link.kind === 'agent' && link.key && (status === 'running' || status === 'idle' || link.paused) && (
                    <SubagentControl agentKey={link.key} status={link.status} compact />
                )}
                {link.key && (
                    <button className="link-btn" type="button" onClick={() => onOpen(link.key!)}>
                        Mở luồng →
                    </button>
                )}
            </div>
            {tool.prompt && (
                <div className="agent-section">
                    <button className="agent-section-label" type="button" onClick={() => setShowPrompt(!showPrompt)}>
                        {showPrompt ? '▾' : '▸'} Giao việc
                    </button>
                    <div className={showPrompt ? 'agent-prompt' : 'agent-prompt clamp'}>{tool.prompt}</div>
                </div>
            )}
            {link.report && (
                <div className="agent-section">
                    <div className="agent-section-label static">Báo cáo về</div>
                    <div className="agent-report">
                        <Markdown text={link.report} />
                    </div>
                </div>
            )}
        </div>
    );
}

function AssistantBlock({
    parts,
    label,
    threadKey,
    now,
    onOpen,
    live
}: {
    parts: Part[];
    label: string;
    threadKey: string;
    now: number;
    onOpen: (k: string) => void;
    live?: DraftBlock[];
}) {
    const turns = parts.filter((p): p is Extract<Part, { type: 'turn' }> => p.type === 'turn');
    const model = turns.find((t) => t.model)?.model;
    const input = turns.reduce((a, t) => a + t.input, 0);
    const output = turns.reduce((a, t) => a + t.output, 0);
    const ts = turns[0]?.ts ?? null;
    return (
        <div className="msg">
            <div className="avatar avatar-bot">🤖</div>
            <div className="msg-main">
                <div className="msg-head">
                    <span className="msg-author">{label}</span>
                    {model && <span className="badge">{model}</span>}
                    {input + output > 0 && (
                        <span className="tokens">
                            <span className="tok-in">↓ {fmtTokens(input)}</span> · <span className="tok-out">↑ {fmtTokens(output)}</span>
                        </span>
                    )}
                    <span className="msg-time">{fmtTime(ts)}</span>
                </div>
                {parts.map((p) => {
                    switch (p.type) {
                        case 'turn':
                            return null;
                        case 'thinking':
                            return <Thinking key={p.idx} text={p.text} ms={p.ms} />;
                        case 'text':
                            return <Markdown key={p.idx} text={p.text} />;
                        case 'tool':
                            if (p.handback != null)
                                return (
                                    <div key={p.idx} className="handback">
                                        <div className="handback-label">📤 Báo cáo gửi về Claude</div>
                                        <Markdown text={p.handback} />
                                    </div>
                                );
                            return p.link ? (
                                <AgentCard key={p.idx} tool={p} onOpen={onOpen} />
                            ) : (
                                <ToolCard key={p.idx} tool={p} threadKey={threadKey} now={now} />
                            );
                    }
                })}
                {live && live.length > 0 && <LiveBlocks blocks={live} />}
            </div>
        </div>
    );
}

const BlockView = memo(
    function BlockView({
        block,
        view,
        now,
        onOpen,
        live
    }: {
        block: Block;
        sig: string;
        view: ThreadView;
        now: number;
        onOpen: (k: string) => void;
        live?: DraftBlock[];
    }) {
        switch (block.kind) {
            case 'user':
                return (
                    <div className="msg">
                        <div className={view.kind === 'main' ? 'avatar avatar-user' : 'avatar avatar-bot'}>
                            {view.kind === 'main' ? '👤' : '🤖'}
                        </div>
                        <div className="msg-main">
                            <div className="msg-head">
                                <span className="msg-author">{view.userLabel}</span>
                                <span className="msg-time">{fmtTime(block.item.ts)}</span>
                            </div>
                            <UserPrompt text={block.item.text} images={block.item.images ?? []} />
                        </div>
                    </div>
                );
            case 'system':
                return <div className="system-line">{systemText(block.item.text)}</div>;
            case 'compact':
                return (
                    <div className="compact-line">
                        ── Compact: {fmtTokens(block.item.pre)} → {fmtTokens(block.item.post)} token ──
                    </div>
                );
            case 'assistant':
                return (
                    <AssistantBlock
                        parts={block.parts}
                        label={view.assistantLabel}
                        threadKey={view.key}
                        now={now}
                        onOpen={onOpen}
                        live={live}
                    />
                );
        }
    },
    // Re-render only when the block content changes (or a running tool needs its timer).
    (a, b) => a.sig === b.sig && a.view.key === b.view.key && (a.now === b.now || !a.sig.includes('"running"'))
);

export function Chat({
    view,
    now,
    onOpen,
    draft
}: {
    view: ThreadView;
    now: number;
    onOpen: (k: string) => void;
    /** Live draft of a managed session's streaming message (main thread only). */
    draft?: Draft | null;
}) {
    const blocks = groupBlocks(view.items);
    const live = view.kind === 'main' ? pendingDraft(view.items, draft) : [];
    const last = blocks[blocks.length - 1];
    const attach = live.length > 0 && last?.kind === 'assistant';
    return (
        <>
            {blocks.map((b, i) => {
                const first = b.kind === 'assistant' ? b.parts[0].idx : b.item.idx;
                const mine = attach && i === blocks.length - 1 ? live : undefined;
                const sig = JSON.stringify(b) + (mine ? JSON.stringify(mine) : '');
                return <BlockView key={first} block={b} sig={sig} view={view} now={now} onOpen={onOpen} live={mine} />;
            })}
            {live.length > 0 && !attach && (
                <AssistantBlock parts={[]} label={view.assistantLabel} threadKey={view.key} now={now} onOpen={onOpen} live={live} />
            )}
        </>
    );
}
