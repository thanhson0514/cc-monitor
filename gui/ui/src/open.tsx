import { createContext, useContext, useEffect, useState } from 'react';
import { api } from './api';

/** Opens a path from the transcript in the app's file viewer. */
export interface PathOpener {
    open: (path: string) => void;
    /** Working directory of the shown thread (relative paths resolve against it). */
    base: string | null;
}

export const OpenPath = createContext<PathOpener>({ open: () => {}, base: null });

export const usePathOpener = () => useContext(OpenPath);

const IMAGE_EXT = /\.(png|jpe?g|gif|webp|bmp|svg|avif|ico)$/i;
export const isImagePath = (p: string) => IMAGE_EXT.test(p);

/** `src/a.rs:12:3` → `src/a.rs` (the viewer opens the file, not a line). */
export const stripLine = (p: string) => p.replace(/(:\d+){1,2}$/, '');

/** Open http(s) links outside the app instead of navigating the webview. */
export function openUrl(url: string) {
    api.openExternal(url).catch(() => {});
}

const cache = new Map<string, string | null>();

/** An image file shown inline; click opens it in the viewer. */
export function InlineImage({ path, label, base }: { path: string; label?: string; base?: string | null }) {
    const opener = usePathOpener();
    const b = base ?? opener.base;
    const key = `${b ?? ''}|${path}`;
    const [src, setSrc] = useState<string | null | undefined>(cache.get(key));
    useEffect(() => {
        if (cache.has(key)) {
            setSrc(cache.get(key));
            return;
        }
        let live = true;
        api.preview(path, b)
            .then((p) => {
                const v = p.kind === 'image' ? p.dataUrl : null;
                if (cache.size > 200) cache.clear();
                cache.set(key, v);
                if (live) setSrc(v);
            })
            .catch(() => live && setSrc(null));
        return () => {
            live = false;
        };
    }, [key, path, b]);
    const name = path.split(/[\\/]/).pop();
    return (
        <button type="button" className="inline-img" onClick={() => opener.open(path)} title={path}>
            {src ? (
                <img src={src} alt={label ?? name} loading="lazy" />
            ) : (
                <span className="inline-img-ph">{src === null ? '✕ ảnh không còn' : '…'}</span>
            )}
            {label && <span className="inline-img-label">{label}</span>}
        </button>
    );
}

/** A path rendered as a link that opens the viewer. */
export function PathLink({ path, children }: { path: string; children: React.ReactNode }) {
    const { open } = usePathOpener();
    return (
        <span
            className="path-link"
            role="link"
            tabIndex={0}
            onClick={(e) => {
                e.stopPropagation();
                open(stripLine(path));
            }}
            onKeyDown={(e) => {
                if (e.key === 'Enter') {
                    e.stopPropagation();
                    open(stripLine(path));
                }
            }}
        >
            {children}
        </span>
    );
}
