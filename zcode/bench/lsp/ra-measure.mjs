// rust-analyzer をビルド係のイメージの中で測る（docs/measurements.md「rust-analyzer」）。
//
// node ra-measure.mjs --root <プロジェクト> --file <相対パス> --at <探す字句> [--settings <JSON>]
//                     [--label <名前>] [--timeout <秒>]
//
// 1. rust-analyzer を起動して initialize → didOpen
// 2. 0.5 秒ごとに hover を投げ、最初に中身のある答えが返った時刻を「冷えた最初の hover」とする
// 3. 静かになる（experimental/serverStatus の quiescent）まで待つ
// 4. 温まった状態で hover・definition・references・workspace/symbol・completion を 5 回ずつ測る
// 5. その間ずっと、rust-analyzer と子プロセス（proc-macro-srv・cargo・rustc）の RSS の合計を
//    200ms ごとに測り、最大を残す。コンテナ全体の memory.peak（cgroup v2）も読む
//
// 結果は標準出力に JSON 1 行。
import { spawn } from "node:child_process";
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, all) => {
    if (a.startsWith("--")) acc.push([a.slice(2), all[i + 1]]);
    return acc;
  }, []),
);
const root = resolve(args.root);
const file = args.file;
const label = args.label ?? root;
const timeoutS = Number(args.timeout ?? 900);
const settings = args.settings ? JSON.parse(args.settings) : {};
const text = readFileSync(join(root, file), "utf8");
const uri = pathToFileURL(join(root, file)).href;

// 字句の位置（最後の文字。行は 0 始まり、桁は UTF-16）
function positionOf(needle) {
  const i = text.indexOf(needle);
  if (i < 0) throw new Error(`見つからない: ${needle}`);
  const at = i + needle.length - 1;
  const before = text.slice(0, at);
  const line = before.split("\n").length - 1;
  const col = at - (before.lastIndexOf("\n") + 1);
  return { line, character: col };
}
const pos = positionOf(args.at);

const t0 = performance.now();
const ms = () => Math.round(performance.now() - t0);

const ra = spawn("rust-analyzer", [], { cwd: root, stdio: ["pipe", "pipe", "pipe"] });
let stderr = "";
ra.stderr.on("data", (d) => {
  stderr += d;
  if (stderr.length > 20000) stderr = stderr.slice(-10000);
});

// ---- RSS（rust-analyzer と子孫の合計） ----
function children() {
  const kids = new Map();
  for (const d of readdirSync("/proc")) {
    if (!/^\d+$/.test(d)) continue;
    try {
      const st = readFileSync(`/proc/${d}/stat`, "utf8");
      const ppid = Number(st.slice(st.lastIndexOf(")") + 2).split(" ")[1]);
      if (!kids.has(ppid)) kids.set(ppid, []);
      kids.get(ppid).push(Number(d));
    } catch {}
  }
  return kids;
}
function rssKiB(pid) {
  try {
    const m = /VmRSS:\s+(\d+)/.exec(readFileSync(`/proc/${pid}/status`, "utf8"));
    return m ? Number(m[1]) : 0;
  } catch {
    return 0;
  }
}
function treeRss() {
  const kids = children();
  let total = 0;
  let raOnly = rssKiB(ra.pid);
  const stack = [ra.pid];
  while (stack.length) {
    const p = stack.pop();
    total += rssKiB(p);
    for (const k of kids.get(p) ?? []) stack.push(k);
  }
  return { total, raOnly };
}
let peakTree = 0;
let peakRa = 0;
const sampler = setInterval(() => {
  const { total, raOnly } = treeRss();
  peakTree = Math.max(peakTree, total);
  peakRa = Math.max(peakRa, raOnly);
}, 200);
function cgroup(name) {
  try {
    return Math.round(Number(readFileSync(`/sys/fs/cgroup/${name}`, "utf8").trim()) / 1048576);
  } catch {
    return null;
  }
}

// ---- LSP の枠 ----
let buf = Buffer.alloc(0);
let nextId = 1;
const pending = new Map();
const progress = {}; // title → {begin, end}
const tokens = new Map();
let quiescentAt = null;
let quiescentWaiters = [];
ra.stdout.on("data", (d) => {
  buf = Buffer.concat([buf, d]);
  for (;;) {
    const sep = buf.indexOf("\r\n\r\n");
    if (sep < 0) return;
    const m = /Content-Length: (\d+)/i.exec(buf.slice(0, sep).toString());
    const len = Number(m[1]);
    if (buf.length < sep + 4 + len) return;
    const msg = JSON.parse(buf.slice(sep + 4, sep + 4 + len).toString("utf8"));
    buf = buf.slice(sep + 4 + len);
    onMessage(msg);
  }
});
function send(obj) {
  const body = Buffer.from(JSON.stringify({ jsonrpc: "2.0", ...obj }), "utf8");
  ra.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
  ra.stdin.write(body);
}
function request(method, params) {
  const id = nextId++;
  send({ id, method, params });
  return new Promise((res) => pending.set(id, res));
}
function notify(method, params) {
  send({ method, params });
}
function onMessage(msg) {
  if (msg.id !== undefined && msg.method === undefined) {
    const res = pending.get(msg.id);
    pending.delete(msg.id);
    res?.(msg);
    return;
  }
  if (msg.method && msg.id !== undefined) {
    // サーバからの依頼（workDoneProgress/create・workspace/configuration など）
    let result = null;
    if (msg.method === "workspace/configuration") result = msg.params.items.map(() => settings);
    send({ id: msg.id, result });
    return;
  }
  if (msg.method === "$/progress") {
    const v = msg.params.value;
    if (v.kind === "begin") {
      tokens.set(msg.params.token, v.title);
      progress[v.title] ??= { begin: ms() };
    } else if (v.kind === "end") {
      const title = tokens.get(msg.params.token);
      if (title && progress[title]) progress[title].end = ms();
    }
  } else if (msg.method === "experimental/serverStatus") {
    if (msg.params.quiescent && quiescentAt === null && opened) {
      quiescentAt = ms();
      quiescentWaiters.forEach((f) => f());
      quiescentWaiters = [];
    }
    if (msg.params.message) lastStatusMessage = msg.params.message;
  }
}
let opened = false;
let lastStatusMessage = null;

const sleep = (n) => new Promise((r) => setTimeout(r, n));
function hasContent(res) {
  const r = res?.result;
  if (!r) return false;
  if (Array.isArray(r)) return r.length > 0;
  if (r.contents) return JSON.stringify(r.contents).length > 10;
  if (r.items) return r.items.length > 0;
  return true;
}
async function timed(method, params) {
  const a = performance.now();
  const res = await request(method, params);
  return { ms: +(performance.now() - a).toFixed(1), ok: hasContent(res), err: res.error?.message };
}

const killTimer = setTimeout(() => {
  console.log(JSON.stringify({ label, error: "timeout", progress, stderr: stderr.slice(-2000) }));
  process.exit(2);
}, timeoutS * 1000);

const init = await request("initialize", {
  processId: process.pid,
  rootUri: pathToFileURL(root).href,
  workspaceFolders: [{ uri: pathToFileURL(root).href, name: "w" }],
  capabilities: {
    window: { workDoneProgress: true },
    experimental: { serverStatusNotification: true },
    textDocument: { hover: { contentFormat: ["plaintext"] }, completion: { completionItem: { snippetSupport: false } } },
  },
  initializationOptions: settings,
});
if (init.error) throw new Error(JSON.stringify(init.error));
notify("initialized", {});
notify("textDocument/didOpen", { textDocument: { uri, languageId: "rust", version: 1, text } });
opened = true;
const openedAt = ms();

// 冷えた最初の hover
let firstHover = null;
let tries = 0;
while (firstHover === null && ms() < timeoutS * 1000) {
  tries++;
  const r = await request("textDocument/hover", { textDocument: { uri }, position: pos });
  if (hasContent(r)) {
    firstHover = { ms: ms(), text: JSON.stringify(r.result.contents).slice(0, 160) };
    break;
  }
  await sleep(500);
}
if (quiescentAt === null) await new Promise((r) => quiescentWaiters.push(r));
const rssQuiescent = treeRss();
const cgQuiescent = cgroup("memory.current");

// 温まった問い合わせ
const doc = { textDocument: { uri }, position: pos };
const warm = {};
const sym = args.at.replace(/[^A-Za-z0-9_].*$/, "") || "main";
const queries = {
  hover: ["textDocument/hover", doc],
  definition: ["textDocument/definition", doc],
  references: ["textDocument/references", { ...doc, context: { includeDeclaration: true } }],
  symbol: ["workspace/symbol", { query: sym }],
  completion: ["textDocument/completion", doc],
};
for (const [name, [method, params]] of Object.entries(queries)) {
  const runs = [];
  for (let i = 0; i < 5; i++) runs.push(await timed(method, params));
  warm[name] = { ms: runs.map((r) => r.ms), ok: runs.every((r) => r.ok), err: runs.find((r) => r.err)?.err };
}
const rssEnd = treeRss();

clearInterval(sampler);
clearTimeout(killTimer);
await request("shutdown", null);
notify("exit", null);

console.log(
  JSON.stringify({
    label,
    server: init.result.serverInfo,
    opened_ms: openedAt,
    first_hover_ms: firstHover?.ms ?? null,
    first_hover_tries: tries,
    first_hover_text: firstHover?.text,
    quiescent_ms: quiescentAt,
    progress,
    rss_mib_quiescent: Math.round(rssQuiescent.total / 1024),
    rss_mib_quiescent_ra: Math.round(rssQuiescent.raOnly / 1024),
    rss_mib_end: Math.round(rssEnd.total / 1024),
    rss_mib_peak_tree: Math.round(peakTree / 1024),
    rss_mib_peak_ra: Math.round(peakRa / 1024),
    cgroup_mib_at_quiescent: cgQuiescent,
    cgroup_mib_peak: cgroup("memory.peak"),
    warm,
    last_status: lastStatusMessage,
  }),
);
process.exit(0);
