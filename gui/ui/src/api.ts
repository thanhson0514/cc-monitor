import { invoke } from '@tauri-apps/api/core';

export type Status = 'running' | 'done' | 'error' | 'idle';

export interface TreeRow {
    depth: number;
    key: string;
    label: string;
    status: Status;
    kind: 'main' | 'agent' | 'workflow' | 'workflowAgent';
    agentType: string | null;
    progress: [number, number] | null;
    live: 'busy' | 'idle' | string | null;
    liveName: string | null;
    tmux: string | null;
    mtime: number | null;
    tool: 'claude' | 'codex' | null;
    open: boolean;
}

export interface LinkInfo {
    kind: 'agent' | 'workflow';
    key: string | null;
    label: string | null;
    status: Status | null;
    progress: [number, number] | null;
    report: string | null;
}

export interface BgTask {
    id: string;
    kind: 'shell' | 'monitor' | 'agent' | 'workflow';
    label: string;
    state: 'running' | 'completed' | 'failed' | 'killed' | 'unknown';
    started: number | null;
    ended: number | null;
    summary: string | null;
    outputFile: string | null;
}

export interface TailDto {
    text: string;
    truncated: boolean;
    gone: boolean;
    size: number;
}

export type Item =
    | { type: 'prompt'; idx: number; ts: number | null; text: string }
    | { type: 'turn'; idx: number; ts: number | null; model: string | null; input: number; output: number }
    | { type: 'thinking'; idx: number; ts: number | null; text: string; ms: number | null }
    | { type: 'text'; idx: number; ts: number | null; text: string }
    | {
          type: 'tool';
          idx: number;
          id: string;
          name: string;
          summary: string;
          state: 'running' | 'ok' | 'error';
          started: number | null;
          ended: number | null;
          prompt: string | null;
          handback: string | null;
          input: Record<string, unknown>;
          resultPreview: string;
          hasMore: boolean;
          link: LinkInfo | null;
          bg: BgTask | null;
          waits: BgTask[];
          wokeBy: string | null;
      }
    | { type: 'compact'; idx: number; ts: number | null; pre: number; post: number }
    | { type: 'system'; idx: number; ts: number | null; text: string };

export interface ThreadView {
    key: string;
    kind: 'main' | 'agent' | 'workflow';
    title: string;
    userLabel: string;
    assistantLabel: string;
    total: number;
    items: Item[];
}

export interface StatusInfo {
    model: string | null;
    ctx: number | null;
    win: number | null;
    winEstimated: boolean;
    ctxPct: number | null;
    branch: string | null;
    lines: [number, number] | null;
    mem: [number, number] | null;
    effort: string | null;
    fiveH: number | null;
    sevenD: number | null;
    compactions: number;
    runningAgents: number;
    sessions: number;
}

export interface ToolDetail {
    input: string;
    result: string;
    source: string | null;
}

export interface HistorySession {
    sid: string;
    mtime: number;
    size: number;
    title: string | null;
}

export interface HistoryProject {
    project: string;
    label: string;
    tool: 'claude' | 'codex';
    sessions: HistorySession[];
}

export interface FileEntry {
    path: string;
    kind: 'image' | 'file' | 'dir';
    ts: number | null;
    exists: boolean;
    size: number | null;
    source: string | null;
}

export interface TouchedFile {
    path: string;
    reads: number;
    edits: number;
    writes: number;
    lastTs: number | null;
    lastOp: string;
    threads: string[];
    exists: boolean;
}

export interface LogEntry {
    ts: number | null;
    thread: string;
    threadKey: string;
    toolId: string;
    tool: string;
    title: string;
    command: string;
    state: 'running' | 'ok' | 'error';
    durMs: number | null;
}

export interface Assets {
    tmpDir: string | null;
    userFiles: FileEntry[];
    images: FileEntry[];
    files: TouchedFile[];
    scratch: FileEntry[];
    log: LogEntry[];
}

export interface Preview {
    kind: 'image' | 'text' | 'dir' | 'binary' | 'missing';
    path: string;
    size: number;
    dataUrl: string | null;
    text: string | null;
    truncated: boolean;
    entries: string[];
}

export interface ChangedFile {
    path: string;
    edits: number;
    lastTs: number | null;
    exists: boolean;
    createdInSession: boolean;
    hasSessionBaseline: boolean;
    repo: string | null;
    gitStatus: string | null;
    added: number;
    removed: number;
}

export interface DiffRow {
    kind: 'equal' | 'delete' | 'insert' | 'replace' | 'fold';
    left: { no: number; text: string } | null;
    right: { no: number; text: string } | null;
    hidden: number;
}

export interface DiffView {
    path: string;
    baseline: string;
    baselineLabel: string;
    available: string[];
    rows: DiffRow[];
    added: number;
    removed: number;
    note: string | null;
}

export interface SetupInfo {
    config: { setupDone: boolean; claude: boolean; codex: boolean };
    claudeDir: string;
    claudeFound: boolean;
    codexDir: string;
    codexFound: boolean;
    statusline: string | null;
    statuslineWrapped: boolean;
}

export type CapKind = 'skill' | 'plugin' | 'mcp' | 'agent' | 'workflow';
export type CapScope = 'user' | 'project' | 'builtIn' | 'managed' | { plugin: string };

export interface Capability {
    kind: CapKind;
    id: string;
    name: string;
    description: string;
    scope: CapScope;
    path: string | null;
    enabledGlobally: boolean;
    meta: Record<string, unknown>;
}

export interface Usage {
    kind: CapKind;
    id: string;
    state: 'unavailable' | 'available' | 'used';
    count: number;
    last: number | null;
}

export const api = {
    catalog: (cwd: string | null) => invoke<Capability[]>('get_catalog', { cwd }),
    sessionCwd: (sid: string) => invoke<string | null>('get_session_cwd', { sid }),
    sessionUsage: (sid: string) => invoke<Usage[]>('get_session_usage', { sid }),
    setup: () => invoke<SetupInfo>('get_setup'),
    saveSetup: (claude: boolean, codex: boolean, statusline: boolean) =>
        invoke<SetupInfo>('save_setup', { claude, codex, statusline }),
    removeStatusline: () => invoke<SetupInfo>('remove_statusline'),
    tree: () => invoke<TreeRow[]>('get_tree'),
    initialSelect: () => invoke<string | null>('get_initial_select'),
    startup: () => invoke<{ diff: string | null; view: string | null }>('get_startup'),
    thread: (key: string, tail: number) => invoke<ThreadView | null>('get_thread', { key, tail }),
    status: (key: string | null) => invoke<StatusInfo>('get_status', { key }),
    toolDetail: (key: string, toolId: string) => invoke<ToolDetail | null>('get_tool_detail', { key, toolId }),
    history: (days: number) => invoke<HistoryProject[]>('list_history', { days }),
    openSession: (sid: string) => invoke<void>('open_session', { sid }),
    assets: (key: string, scope: 'session' | 'thread') => invoke<Assets | null>('get_assets', { key, scope }),
    tailTaskOutput: (path: string) => invoke<TailDto>('tail_task_output', { path }),
    preview: (path: string) => invoke<Preview>('preview_file', { path }),
    changes: (key: string) => invoke<ChangedFile[] | null>('get_changes', { key }),
    diff: (key: string, path: string, baseline: string | null) => invoke<DiffView | null>('get_diff', { key, path, baseline })
};

export function fmtTokens(n: number): string {
    if (n < 1000) return String(n);
    if (n < 1_000_000) return `${(n / 1000).toFixed(1)}K`;
    return `${(n / 1_000_000).toFixed(1)}M`;
}

export function fmtTime(ts: number | null): string {
    if (ts == null) return '';
    return new Date(ts).toLocaleTimeString('en-US');
}

export function fmtDur(ms: number): string {
    if (ms < 1000) return `${ms}ms`;
    if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
    return `${Math.floor(ms / 60_000)}m${String(Math.floor((ms % 60_000) / 1000)).padStart(2, '0')}s`;
}
