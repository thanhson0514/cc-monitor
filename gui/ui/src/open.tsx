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

/** Loaded images only: a missing file is never cached, it may still be written. */
const cache = new Map<string, string>();

/** Retry delays while the file is missing or empty. A tool call is often shown
 *  before the command that writes the image has finished (e.g. a parallel
 *  `sleep 20; screenshot` + Read). */
const RETRY_MS = [1000, 2000, 3000, 5000, 8000, 13000, 20000, 30000];

/** An image file shown inline; click opens it in the viewer. `version` changes
 *  (e.g. the tool's state) trigger a fresh look at a file that was not there yet. */
export function InlineImage({ path, label, base, version }: { path: string; label?: string; base?: string | null; version?: string }) {
    const opener = usePathOpener();
    const b = base ?? opener.base;
    const key = `${b ?? ''}|${path}`;
    const [src, setSrc] = useState<string | null | undefined>(cache.get(key));
    /** Bumped when a loaded image fails to decode (read while half written). */
    const [bust, setBust] = useState(0);
    useEffect(() => {
        const hit = cache.get(key);
        if (hit) {
            setSrc(hit);
            return;
        }
        let live = true;
        let attempt = 0;
        let timer: number | undefined;
        const load = () => {
            api.preview(path, b)
                .then((p) => {
                    if (!live) return;
                    // An empty file is still being written: not an image yet.
                    const v = p.kind === 'image' && p.size > 0 ? p.dataUrl : null;
                    if (v) {
                        if (cache.size > 200) cache.clear();
                        cache.set(key, v);
                        setSrc(v);
                        return;
                    }
                    setSrc(null);
                    if (attempt < RETRY_MS.length) timer = window.setTimeout(load, RETRY_MS[attempt++]);
                })
                .catch(() => {
                    if (!live) return;
                    setSrc(null);
                    if (attempt < RETRY_MS.length) timer = window.setTimeout(load, RETRY_MS[attempt++]);
                });
        };
        load();
        return () => {
            live = false;
            window.clearTimeout(timer);
        };
    }, [key, path, b, version, bust]);
    const broken = () => {
        cache.delete(key);
        setSrc(null);
        if (bust < 3) window.setTimeout(() => setBust((n) => n + 1), 1500);
    };
    const name = path.split(/[\\/]/).pop();
    return (
        <button type="button" className="inline-img" onClick={() => opener.open(path)} title={path}>
            {src ? (
                <img src={src} alt={label ?? name} loading="lazy" onError={broken} />
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
