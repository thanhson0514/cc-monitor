/**
 * `@agent` mention tokens in the prompt text. Pure helpers (no React/DOM) so the
 * highlight, send text and atomic delete all agree on what a mention is.
 *
 * A mention is `@<id>` at the start of the text or after whitespace, where `<id>`
 * is a usable agent id. Trailing `.`, `:` or `-` (sentence punctuation) is not part
 * of the id unless the full run is itself a known id.
 */

export interface Segment {
    text: string;
    /** Agent id when this segment is a mention token (`text` = `@id`). */
    mention?: string;
}

interface Hit {
    start: number;
    end: number;
    id: string;
}

const RUN = /(^|\s)@([\w:.-]+)/g;

function hits(text: string, ids: ReadonlySet<string>): Hit[] {
    const out: Hit[] = [];
    for (const m of text.matchAll(RUN)) {
        let id = m[2];
        if (!ids.has(id)) id = id.replace(/[.:-]+$/, '');
        if (!id || !ids.has(id)) continue;
        const start = (m.index ?? 0) + m[1].length;
        out.push({ start, end: start + 1 + id.length, id });
    }
    return out;
}

/** Text split into plain runs and mention tokens; joining `text` gives the input back. */
export function segments(text: string, ids: ReadonlySet<string>): Segment[] {
    const out: Segment[] = [];
    let pos = 0;
    for (const h of hits(text, ids)) {
        if (h.start > pos) out.push({ text: text.slice(pos, h.start) });
        out.push({ text: text.slice(h.start, h.end), mention: h.id });
        pos = h.end;
    }
    if (pos < text.length) out.push({ text: text.slice(pos) });
    return out;
}

/** Distinct mentioned agent ids, in order of first appearance. */
export function mentionsIn(text: string, ids: ReadonlySet<string>): string[] {
    return [...new Set(hits(text, ids).map((h) => h.id))];
}

/** Token inserted by the `@` picker: `@id` plus a space to keep typing. */
export function mentionToken(id: string): string {
    return `@${id} `;
}

/**
 * Prompt actually sent: the trimmed text (tokens kept) plus one explicit line
 * naming the agents, only when at least one valid mention exists.
 */
export function buildSendText(text: string, ids: ReadonlySet<string>): string {
    const t = text.trim();
    const used = mentionsIn(t, ids);
    if (used.length === 0) return t;
    return `${t}\n\n(Tham chiếu agent: ${used.join(', ')} — hãy dùng Agent tool với subagent_type tương ứng.)`;
}

/**
 * Range Backspace should delete when the caret sits right after a mention token
 * (collapsed selection only): the whole token, so it never half-survives as `@Expl`.
 */
export function atomicDeleteRange(
    text: string,
    selStart: number,
    selEnd: number,
    ids: ReadonlySet<string>
): [number, number] | null {
    if (selStart !== selEnd) return null;
    const h = hits(text, ids).find((x) => x.end === selStart);
    return h ? [h.start, h.end] : null;
}

/** Text with every token of `id` removed (and the single space after each). */
export function removeMention(text: string, id: string, ids: ReadonlySet<string>): string {
    let out = '';
    let pos = 0;
    for (const h of hits(text, ids)) {
        if (h.id !== id) continue;
        out += text.slice(pos, h.start);
        pos = text[h.end] === ' ' ? h.end + 1 : h.end;
    }
    return out + text.slice(pos);
}
