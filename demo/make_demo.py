#!/usr/bin/env python3
"""Generate a self-contained demo dataset for ccmon (Claude Code sessions).

Used for screenshots and for the CI smoke test on Windows/Linux. Everything is fake.

    python demo/make_demo.py <out-dir>
    # then run ccmon-gui with the environment printed at the end

Timestamps are relative to "now" so the sessions show as active.
"""

import json
import os
import struct
import sys
import tempfile
import time
import zlib
from datetime import datetime, timedelta, timezone

OUT = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "demo-data")
NOW = datetime.now(timezone.utc)
CLAUDE = os.path.join(OUT, "claude")
WORK = os.path.join(OUT, "workspace", "acme-shop")
PROJECT_KEY = "".join("-" if c in "/\\.:" else c for c in WORK)
SID = "7d3c2a10-4b5e-4f60-9a71-0c2d3e4f5a6b"


def ts(sec_ago):
    return (NOW - timedelta(seconds=sec_ago)).strftime("%Y-%m-%dT%H:%M:%S.") + f"{(NOW - timedelta(seconds=sec_ago)).microsecond // 1000:03d}Z"


def write(path, text, mode="w"):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, mode, encoding=None if "b" in mode else "utf-8") as f:
        f.write(text)


def png(path, w, h, color):
    raw = b"".join(b"\x00" + bytes(color) * w for _ in range(h))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    data = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
    write(path, data, "wb")


class Transcript:
    def __init__(self, path, cwd, branch, sidechain=False):
        self.path, self.cwd, self.branch, self.side = path, cwd, branch, sidechain
        self.lines, self.last, self.n = [], None, 0

    def _base(self, typ, ago):
        self.n += 1
        u = f"{'s' if self.side else 'm'}{self.n:04d}-0000-4000-8000-000000000000"
        d = {"parentUuid": self.last, "isSidechain": self.side, "type": typ, "uuid": u, "timestamp": ts(ago),
             "cwd": self.cwd, "gitBranch": self.branch, "sessionId": SID, "version": "2.1.0"}
        self.last = u
        return d

    def user(self, text, ago):
        d = self._base("user", ago)
        d["message"] = {"role": "user", "content": text}
        self.lines.append(d)

    def assistant(self, blocks, ago, msg, out_tokens=120, ctx=48_000, effort="medium"):
        d = self._base("assistant", ago)
        d["effort"] = effort
        d["message"] = {"id": msg, "role": "assistant", "model": "claude-opus-4-5", "content": blocks,
                        "usage": {"input_tokens": 6, "cache_read_input_tokens": ctx - 2000, "cache_creation_input_tokens": 1994, "output_tokens": out_tokens}}
        self.lines.append(d)

    def tool(self, tid, name, inp, ago, msg, result=None, is_error=False, tur=None, took=1.2):
        self.assistant([{"type": "tool_use", "id": tid, "name": name, "input": inp}], ago, msg)
        if result is not None:
            d = self._base("user", ago - took)
            d["message"] = {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tid, "content": result, "is_error": is_error}]}
            if tur:
                d["toolUseResult"] = tur
            self.lines.append(d)

    def raw(self, d):
        self.lines.append(d)

    def save(self):
        write(self.path, "".join(json.dumps(l, ensure_ascii=False) + "\n" for l in self.lines))


def workspace():
    old = """import { CartSummary } from './CartSummary';

export function CheckoutPage({ cart }: Props) {
    return (
        <section>
            <CartSummary cart={cart} />
            <CardPayment amount={cart.total} />
        </section>
    );
}
"""
    new = """import { CartSummary } from './CartSummary';
import { QrPayment } from './QrPayment';

export function CheckoutPage({ cart }: Props) {
    const [method, setMethod] = useState<'card' | 'qr'>('card');
    return (
        <section>
            <CartSummary cart={cart} />
            <PaymentTabs value={method} onChange={setMethod} />
            {method === 'card' ? <CardPayment amount={cart.total} /> : <QrPayment amount={cart.total} />}
        </section>
    );
}
"""
    qr = """import { useEffect, useState } from 'react';
import { createQr } from '../api/payments';

/** Shows a payment QR code and polls until the bank confirms. */
export function QrPayment({ amount }: { amount: number }) {
    const [qr, setQr] = useState<string | null>(null);
    useEffect(() => {
        createQr(amount).then((r) => setQr(r.imageUrl));
    }, [amount]);
    return qr ? <img src={qr} alt="QR" width={240} /> : <p>Đang tạo mã…</p>;
}
"""
    readme = "# Acme Shop\n\nDemo workspace for ccmon screenshots.\n\n| Lệnh | Mô tả |\n|---|---|\n| `npm test` | chạy test |\n| `npm run build` | build |\n"
    page = os.path.join(WORK, "src/checkout/CheckoutPage.tsx")
    write(page, new)
    write(os.path.join(WORK, "src/checkout/QrPayment.tsx"), qr)
    write(os.path.join(WORK, "README.md"), readme)
    return page, old, new, qr


def claude_session():
    page, old, new, qr = workspace()
    pdir = os.path.join(CLAUDE, "projects", PROJECT_KEY)
    t = Transcript(os.path.join(pdir, f"{SID}.jsonl"), WORK, "feat/qr-checkout")
    tmp = os.path.join(tempfile.gettempdir(), "claude-demo", PROJECT_KEY, SID)
    shot = os.path.join(tmp, "scratchpad", "checkout-qr.png")
    png(os.path.join(tmp, "images", "1.png"), 320, 200, (240, 240, 245))
    png(shot, 480, 300, (36, 40, 64))
    t.raw({"type": "ai-title", "aiTitle": "Thêm thanh toán bằng mã QR", "sessionId": SID})
    t.user(f"Thêm thanh toán bằng mã QR vào trang checkout, viết test luôn. Giao diện như ảnh [Image #1]\n[Image: source: {tmp}/images/1.png]", 900)
    t.assistant([{"type": "thinking", "thinking": "", "signature": "x"}], 896, "msg_01")
    t.assistant([{"type": "text", "text": "Mình xem luồng checkout hiện tại trước, rồi thêm tab **QR** cạnh thanh toán thẻ."}], 895, "msg_01")
    t.tool("t1", "Bash", {"command": "rg -n \"CardPayment\" src --type ts", "description": "Tìm chỗ dùng CardPayment"}, 890, "msg_02",
           "src/checkout/CheckoutPage.tsx:7:            <CardPayment amount={cart.total} />\nsrc/checkout/CardPayment.tsx:3:export function CardPayment(", took=0.4)
    t.tool("t2", "Read", {"file_path": page}, 880, "msg_03", "     1\timport { CartSummary } from './CartSummary';\n     2\t…", took=0.1)
    t.tool("t3", "Agent", {"description": "Khảo sát API thanh toán QR", "subagent_type": "Explore",
                           "prompt": "Tìm trong src/api các hàm tạo mã QR thanh toán và định dạng phản hồi. Trả lời kèm file:dòng."},
           870, "msg_04", "Agent đang chạy nền.", tur={"agentId": "a1b2c3d4e5f60718", "status": "async_launched"}, took=0.5)
    t.tool("t4", "Edit", {"file_path": page, "old_string": old.split("\n\n", 1)[1], "new_string": new.split("\n\n", 1)[1]}, 700, "msg_05",
           "The file has been updated.", took=0.3)
    t.tool("t5", "Write", {"file_path": os.path.join(WORK, "src/checkout/QrPayment.tsx"), "content": qr}, 690, "msg_06", "File created.", took=0.2)
    t.tool("t6", "Bash", {"command": "npm test -- checkout", "description": "Chạy test checkout"}, 600, "msg_07",
           " PASS  src/checkout/CheckoutPage.test.tsx\n  ✓ shows card payment by default (31 ms)\n  ✓ switches to QR tab (18 ms)\n\nTests: 2 passed, 2 total", took=8.3)
    t.tool("t7", "Bash", {"command": f"node scripts/screenshot.mjs /checkout {shot}", "description": "Chụp màn hình trang checkout"}, 560, "msg_08",
           f"Saved {shot}", took=3.1)
    t.tool("t8", "Workflow", {"script": "export const meta = {\n  name: 'review-checkout',\n  description: 'Review thay đổi checkout',\n}"},
           500, "msg_09", "Workflow launched in background.", tur={"runId": "wf_demo01", "workflowName": "review-checkout", "status": "async_launched"}, took=0.4)
    t.assistant([{"type": "text", "text": (
        "Đã thêm thanh toán QR:\n\n"
        "| File | Thay đổi |\n|---|---|\n"
        "| `src/checkout/CheckoutPage.tsx` | thêm tab chọn phương thức |\n"
        "| `src/checkout/QrPayment.tsx` | component mới, gọi `createQr` |\n\n"
        "```ts\nconst [method, setMethod] = useState<'card' | 'qr'>('card');\n```\n\n"
        "Test: **2/2 pass**. Workflow review đang chạy, mình sẽ báo khi xong.")}], 300, "msg_10", out_tokens=860, ctx=61_200)
    t.tool("t9", "Bash", {"command": "npm run build", "description": "Build production"}, 20, "msg_11")
    t.save()

    # file-history baseline for the edited page (before the session)
    hist = os.path.join(CLAUDE, "file-history", SID)
    write(os.path.join(hist, "a1c9e0f2@v1"), old)
    with open(t.path, "a", encoding="utf-8") as f:
        for path, backup in [(page, "a1c9e0f2@v1"), (os.path.join(WORK, "src/checkout/QrPayment.tsx"), None)]:
            f.write(json.dumps({"type": "file-history-delta", "trackingPath": path, "timestamp": ts(700),
                                "backup": {"backupFileName": backup, "version": 1, "backupTime": ts(700)}}) + "\n")

    # explore subagent
    sub = os.path.join(pdir, SID, "subagents")
    a = Transcript(os.path.join(sub, "agent-a1b2c3d4e5f60718.jsonl"), WORK, "feat/qr-checkout", sidechain=True)
    a.user("Tìm trong src/api các hàm tạo mã QR thanh toán và định dạng phản hồi. Trả lời kèm file:dòng.", 868)
    a.tool("s1", "Grep", {"pattern": "createQr|qr", "path": "src/api"}, 860, "sm1", "src/api/payments.ts", took=0.2)
    a.tool("s2", "Read", {"file_path": os.path.join(WORK, "src/api/payments.ts")}, 850, "sm2", "export async function createQr(amount: number) { … }", took=0.1)
    a.tool("s3", "SubagentHandback", {"message": "**Kết quả:** `createQr(amount)` ở `src/api/payments.ts:12` trả `{ imageUrl, expiresAt }`. Chưa có hàm kiểm tra trạng thái; cần poll `GET /payments/:id`."},
           840, "sm3", "ok", took=0.1)
    a.save()
    write(os.path.join(sub, "agent-a1b2c3d4e5f60718.meta.json"), json.dumps({"agentType": "Explore", "description": "Khảo sát API thanh toán QR", "toolUseId": "t3"}))

    # workflow agents
    for i, (desc, phase, done) in enumerate([("Soát lỗi logic", "Review", True), ("Soát truy cập (a11y)", "Review", True), ("Kiểm chứng phát hiện", "Verify", False)]):
        w = Transcript(os.path.join(sub, "workflows", "wf_demo01", f"agent-w{i}.jsonl"), WORK, "feat/qr-checkout", sidechain=True)
        w.user(f"{desc} cho thay đổi checkout trên nhánh feat/qr-checkout.", 480 - i * 10)
        w.tool(f"w{i}a", "Bash", {"command": "git diff main...HEAD -- src/checkout", "description": "Xem diff"}, 470 - i * 10, f"wm{i}a", "diff --git …", took=0.3)
        if done:
            w.tool(f"w{i}b", "StructuredOutput", {"findings": []}, 400 - i * 10, f"wm{i}b", "ok", took=0.1)
        w.save()
        write(os.path.join(sub, "workflows", "wf_demo01", f"agent-w{i}.meta.json"),
              json.dumps({"agentType": "workflow-subagent", "description": desc, "workflowPhase": phase}))

    # live registry: the parent process stays alive while ccmon runs in CI/screenshot scripts
    write(os.path.join(CLAUDE, "sessions", "1.json"), json.dumps({"pid": os.getppid(), "sessionId": SID, "status": "busy", "name": "acme-shop"}))
    write(os.path.join(CLAUDE, "settings.json"), json.dumps({"statusLine": {"type": "command", "command": "ccmon tee --"}}, indent=2))
    # status-line snapshot (what `ccmon tee` would have saved)
    write(os.path.join(OUT, "cache", "statusline", f"{SID}.json"), json.dumps({
        "session_id": SID, "context_window": {"context_window_size": 200000, "used_percentage": 30.6},
        "rate_limits": {"five_hour": {"used_percentage": 18.0}, "seven_day": {"used_percentage": 41.0}},
        "cost": {"total_lines_added": 24, "total_lines_removed": 3}, "effort": {"level": "medium"}}))


def main():
    claude_session()
    cfg = os.path.join(OUT, "config.json")
    write(cfg, json.dumps({"setupDone": True, "claude": True, "codex": False}))
    # make everything look recently written
    now = time.time()
    for root, _, files in os.walk(OUT):
        for f in files:
            os.utime(os.path.join(root, f), (now, now))
    env = {"CCMON_CLAUDE_DIR": CLAUDE, "CCMON_CACHE_DIR": os.path.join(OUT, "cache"), "CCMON_CONFIG": cfg}
    print(json.dumps(env))


if __name__ == "__main__":
    main()
