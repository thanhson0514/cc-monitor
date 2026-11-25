import { memo } from 'react';
import { Marked, Renderer } from 'marked';
import hljs from 'highlight.js/lib/common';

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

const md = new Marked({ renderer, gfm: true, breaks: false });

const cache = new Map<string, string>();
function render(text: string): string {
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

export const Markdown = memo(function Markdown({ text }: { text: string }) {
    return <div className="md" onClick={onCopyClick} dangerouslySetInnerHTML={{ __html: render(text) }} />;
});
