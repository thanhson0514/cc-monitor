import { ReactNode } from 'react';

export type Page = 'thread' | 'library' | 'agents';

const PAGES: [Page, string, string][] = [
    ['thread', 'Luồng', 'Xem luồng (Ctrl+1)'],
    ['library', 'Thư viện', 'Skills, plugin, MCP (Ctrl+2)'],
    ['agents', 'Agents', 'Agents và workflow (Ctrl+3)']
];

export const PAGE_TITLE: Record<Page, string> = { thread: 'Luồng', library: 'Thư viện', agents: 'Agents' };

function PageSwitch({ page, onPage }: { page: Page; onPage: (p: Page) => void }) {
    return (
        <div className="seg" role="group" aria-label="Trang">
            {PAGES.map(([p, label, title]) => (
                <button
                    key={p}
                    type="button"
                    className={`seg-btn ${page === p ? 'on' : ''}`}
                    aria-pressed={page === p}
                    onClick={() => onPage(p)}
                    title={title}
                >
                    {label}
                </button>
            ))}
        </div>
    );
}

/** Icon button for the header: 26×26, stroke icon, tooltip + aria-label. */
export function HeadIconBtn({ label, on, onClick, children }: { label: string; on?: boolean; onClick: () => void; children: ReactNode }) {
    return (
        <button type="button" className={`head-icon ${on ? 'on' : ''}`} aria-label={label} aria-pressed={on} title={label} onClick={onClick}>
            {children}
        </button>
    );
}

const svg = { width: 16, height: 16, viewBox: '0 0 16 16', fill: 'none', stroke: 'currentColor', strokeWidth: 1.4, 'aria-hidden': true } as const;

/** Folder with an image: assets panel (images, files, logs). */
export function AssetsIcon() {
    return (
        <svg {...svg} strokeLinecap="round" strokeLinejoin="round">
            <path d="M1.75 4.25a1.5 1.5 0 0 1 1.5-1.5h2.6l1.4 1.5h5.5a1.5 1.5 0 0 1 1.5 1.5v6a1.5 1.5 0 0 1-1.5 1.5H3.25a1.5 1.5 0 0 1-1.5-1.5z" />
            <path d="M5 9.5h6M5 7.25h3.5" />
        </svg>
    );
}

/** Two-column diff: changes panel. */
export function DiffIcon() {
    return (
        <svg {...svg} strokeLinecap="round" strokeLinejoin="round">
            <rect x="1.75" y="2.75" width="12.5" height="10.5" rx="2" />
            <path d="M8 3v10" />
            <path d="M3.75 8h2.5M11 6.75v2.5M9.75 8h2.5" />
        </svg>
    );
}

/** Single-line header shared by every page; `lead` is the left side, `actions` the far right. */
export function AppHeader({
    page,
    onPage,
    lead,
    actions
}: {
    page: Page;
    onPage: (p: Page) => void;
    lead: ReactNode;
    actions?: ReactNode;
}) {
    return (
        <header className="app-head">
            <div className="head-lead">{lead}</div>
            <PageSwitch page={page} onPage={onPage} />
            {/* Always rendered with a fixed width so the page switch stays put across pages. */}
            <div className="head-actions">{actions}</div>
        </header>
    );
}
