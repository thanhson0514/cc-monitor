import { useEffect, useState } from 'react';
import { api, BgTask, fmtDur, TailDto } from './api';
import { Code, ToolItem } from './Tools';

const KIND_ICON: Record<BgTask['kind'], string> = { shell: '⌘', monitor: '◉', agent: '🤖', workflow: '⚙' };
const KIND_NAME: Record<BgTask['kind'], string> = { shell: 'shell', monitor: 'monitor', agent: 'agent', workflow: 'workflow' };
const STATE_TEXT: Record<BgTask['state'], string> = {
    running: 'đang chạy',
    completed: 'đã xong',
    failed: 'lỗi',
    killed: 'đã dừng',
    unknown: 'không rõ'
};

function TaskOutputView({ path, running }: { path: string; running: boolean }) {
    const [tail, setTail] = useState<TailDto | null>(null);
    useEffect(() => {
        let alive = true;
        const load = () => {
            api.tailTaskOutput(path)
                .then((t) => alive && setTail(t))
                .catch(() => alive && setTail({ text: '', truncated: false, gone: true, size: 0 }));
        };
        load();
        // Poll only while the task runs; this component exists only while the block is expanded.
        const h = running ? setInterval(load, 1000) : undefined;
        return () => {
            alive = false;
            if (h) clearInterval(h);
        };
    }, [path, running]);

    if (!tail) return <div className="muted small">Đang đọc output…</div>;
    if (tail.gone) return <div className="muted small">File output không còn (đã bị xoá hoặc ngoài thư mục cho phép).</div>;
    return (
        <div className="bg-out">
            {tail.truncated && <div className="muted small">Chỉ hiện phần cuối của output (file {Math.round(tail.size / 1024)} KiB).</div>}
            {tail.text ? <Code code={tail.text} maxLines={20} fromEnd /> : <div className="muted small">Output trống.</div>}
        </div>
    );
}

export function BgTaskView({ task, now }: { task: BgTask; now: number }) {
    const [open, setOpen] = useState(false);
    const dur =
        task.started == null
            ? ''
            : fmtDur(Math.max(0, (task.state === 'running' ? now : (task.ended ?? now)) - task.started));
    return (
        <div className={`bgtask bg-${task.state}`}>
            <div className="bgtask-head">
                <span className="bgtask-icon" title={KIND_NAME[task.kind]}>
                    {KIND_ICON[task.kind]}
                </span>
                <span className="bgtask-label">{task.label}</span>
                <span className={`bgtask-state bgs-${task.state}`}>{STATE_TEXT[task.state]}</span>
                {dur && <span className="tdur">{dur}</span>}
            </div>
            {task.summary && <div className="muted small">{task.summary}</div>}
            {task.outputFile && (
                <>
                    <button type="button" className="more-lines" onClick={() => setOpen(!open)}>
                        {open ? 'Ẩn output' : 'Xem output'}
                    </button>
                    {open && <TaskOutputView path={task.outputFile} running={task.state === 'running'} />}
                </>
            )}
        </div>
    );
}

export function YieldBody({ tool, now }: { tool: ToolItem; now: number }) {
    return (
        <>
            {tool.waits.map((t) => (
                <BgTaskView key={t.id} task={t} now={now} />
            ))}
            {tool.wokeBy && (
                <div className="muted small">
                    Đánh thức bởi: {tool.waits.find((t) => t.id === tool.wokeBy)?.label ?? tool.wokeBy}
                </div>
            )}
        </>
    );
}
