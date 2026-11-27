import { memo } from 'react';
import { Marked, Renderer, TokenizerAndRendererExtension } from 'marked';
import hljs from 'highlight.js/lib/common';
import { openUrl, stripLine, usePathOpener } from './open';

const escapeHtml = (s: string) =>
    s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

const renderer = new Renderer();
renderer.code = ({ text, lang }) => {
    const language = (lang || '').trim().split(/\s+/)[0];
    let body: string;
    try {
        body =
            language && hljs.getLanguage(language)
                ? hljs.highlight(text, { language }).value
                : escapeHtml(text);
    } catch {
        body = escapeHtml(text);
    }
    const n = text.replace(/\n$/, '').split('\n').length;
    const gutter = Array.from({ length: n }, (_, i) => i + 1).join('\n');
    return `<div class="codeblock"><div class="codeblock-head"><span>${escapeHtml(
        (language || 'text').toUpperCase()
    )}</span><span class="codeblock-meta">${n} lines <button class="copy-btn" type="button">⧉ Copy</button></span></div><div class="codeblock-body"><pre class="gutter">${gutter}</pre><pre class="code"><code>${body}</code></pre></div></div>`;
};
// Raw HTML from transcripts is shown as text, never injected.
renderer.html = ({ text }) => escapeHtml(text);

// File paths in prose and inline code become links to the file viewer:
// /abs/path, ~/x, ./x, C:\x, and relative dir/file.ext — each with an optional :line[:col].
const SEG = String.raw`[\w.@+\-]+`;
const PATH_BODY = new RegExp(
    String.raw`^(?:` +
        String.raw`(?:~|\.{1,2})?/${SEG}(?:/${SEG})*/?` +
        String.raw`|[A-Za-z]:\\${SEG}(?:\\${SEG})*` +
        String.raw`|[\w@+\-][\w.@+\-]*(?:/${SEG})+\.[A-Za-z0-9]{1,10}` +
        String.raw`)(?::\d+(?::\d+)?)?`
);
const PATH_LEAD = /(^|[\s(\[“"'`])(?=~\/|\.{1,2}\/|\/[\w.@+-]|[A-Za-z]:\\|[\w@+-][\w.@+-]*\/[\w.@+-])/;

/** The path at the start of `src`, if it is one worth linking. */
export function matchPath(src: string): string | null {
    const m = PATH_BODY.exec(src);
    if (!m) return null;
    const raw = m[0].replace(/[.,;:]+$/, '');
    const bare = stripLine(raw);
    const segs = bare.split(/[\\/]/).filter(Boolean);
    // A lone `/word` or `~/` is more likely prose than a file.
    if (segs.length < 2 && !/\.[A-Za-z0-9]{1,10}$/.test(bare)) return null;
    if (bare.length < 3) return null;
    return raw;
}

const pathLink = (raw: string, inner: string) =>
    `<span class="path-link" role="link" data-path="${escapeHtml(raw)}">${inner}</span>`;

const pathExt: TokenizerAndRendererExtension = {
    name: 'fspath',
    level: 'inline',
    start(src) {
        const m = PATH_LEAD.exec(src);
        return m ? m.index + m[1].length : undefined;
    },
    tokenizer(src, tokens) {
        // Only at a word start: not inside `input/output`, a URL or `a:b`.
        const prev = tokens[tokens.length - 1]?.raw.slice(-1) ?? '';
        if (/[\w/:.@+\-\\]/.test(prev)) return undefined;
        const raw = matchPath(src);
        if (!raw) return undefined;
        return { type: 'fspath', raw, text: raw };
    },
    renderer(token) {
        return pathLink(token.text as string, escapeHtml(token.text as string));
    }
};

// `[Image #3]` placeholders Claude Code puts in a prompt for pasted images.
const imageRefExt: TokenizerAndRendererExtension = {
    name: 'imgref',
    level: 'inline',
    start(src) {
        const i = src.indexOf('[Image #');
        return i >= 0 ? i : undefined;
    },
    tokenizer(src) {
        const m = /^\[Image #(\d+)\]/.exec(src);
        return m ? { type: 'imgref', raw: m[0], text: m[1] } : undefined;
    },
    renderer(token) {
        return `<span class="img-ref" role="link" data-img="${escapeHtml(token.text as string)}">🖼 Ảnh #${escapeHtml(token.text as string)}</span>`;
    }
};

renderer.codespan = ({ text }) => {
    // `text` is already HTML-escaped by marked.
    const plain = text.replace(/&amp;/g, '&').replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&#39;/g, "'");
    const raw = matchPath(plain);
    if (raw && raw === plain) return pathLink(raw, `<code>${text}</code>`);
    return `<code>${text}</code>`;
};

const md = new Marked({ renderer, gfm: true, breaks: false, extensions: [imageRefExt, pathExt] });

const cache = new Map<string, string>();
export function render(text: string): string {
    let html = cache.get(text);
    if (html === undefined) {
        html = md.parse(text, { async: false }) as string;
        if (cache.size > 3000) cache.clear();
        cache.set(text, html);
    }
    return html;
}

export function onCopyClick(e: React.MouseEvent) {
    const btn = (e.target as HTMLElement).closest('.copy-btn');
    if (!btn) return;
    const code = btn.closest('.codeblock')?.querySelector('pre.code')?.textContent ?? '';
    navigator.clipboard?.writeText(code).then(() => {
        btn.textContent = '✓ Copied';
        setTimeout(() => (btn.textContent = '⧉ Copy'), 1200);
    });
}

/** Clicks on path links and http links inside rendered markdown. */
export function useLinkClick() {
    const { open } = usePathOpener();
    return (e: React.MouseEvent) => {
        const t = e.target as HTMLElement;
        const p = t.closest<HTMLElement>('[data-path]');
        if (p?.dataset.path) {
            e.preventDefault();
            open(stripLine(p.dataset.path));
            return;
        }
        const a = t.closest('a');
        const href = a?.getAttribute('href');
        if (a && href) {
            e.preventDefault();
            if (/^https?:\/\//i.test(href)) openUrl(href);
            else if (matchPath(href)) open(stripLine(href));
        }
    };
}

export const Markdown = memo(function Markdown({ text }: { text: string }) {
    const onLink = useLinkClick();
    return (
        <div
            className="md"
            onClick={(e) => {
                onCopyClick(e);
                onLink(e);
            }}
            dangerouslySetInnerHTML={{ __html: render(text) }}
        />
    );
});
