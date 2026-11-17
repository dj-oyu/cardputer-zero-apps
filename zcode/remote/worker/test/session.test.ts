// 課題 C2 の試験（docs/stage3-design.md §13・§14）。
//
// 試すのは `SessionCore`（判断の全部）と `SqlLedger`。Cloudflare の口（ctx・WebSocketPair・
// ctx.container）は使わないので、workerd が無くても node の vitest で回る。
// コンテナは偽物の上流、台帳は本物の SQLite（node:sqlite）。`BuildSession` の薄い包み
// （101 の返事・acceptWebSocket・ctx.container の呼び方）は、ここでは試していない
// （最初の deploy で確かめる）。

import { beforeEach, describe, expect, it } from "vitest";
import { MAX_BINARY_FRAME, MAX_CONNECTIONS_PER_DEVICE, MAX_TEXT_FRAME } from "../src/limits";
import {
  ALARM_INTERVAL_MS,
  CLOSE,
  DO_SERVER_NAME,
  RECONCILE_GRACE_MS,
  SessionCore,
  SqlLedger,
  START_WAIT_MS,
  configOf,
  dayOf,
  parseStatus,
  secondsToNextDay,
  type Builder,
  type BuilddStatus,
  type Clock,
  type DeviceSocket,
  type SqlExec,
  type UpstreamHandlers,
  type UpstreamSocket,
} from "../src/session";

// ── 偽物 ──

/** node:sqlite を SqlExec にする（vite を通さずに取る。型は入れていないので any）。 */
function memorySql(): SqlExec {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const sqlite = (globalThis as any).process.getBuiltinModule("node:sqlite");
  const db = new sqlite.DatabaseSync(":memory:");
  return (q, ...b) => db.prepare(q).all(...b) as Record<string, unknown>[];
}

class FakeDevice implements DeviceSocket {
  sent: (string | ArrayBuffer)[] = [];
  closed: { code?: number; reason?: string } | null = null;
  send(data: string | ArrayBuffer): void {
    if (this.closed) throw new Error("closed");
    this.sent.push(data);
  }
  close(code?: number, reason?: string): void {
    this.closed ??= { code, reason };
  }
  json(): Record<string, unknown>[] {
    return this.sent.filter((d): d is string => typeof d === "string").map((d) => JSON.parse(d) as Record<string, unknown>);
  }
}

class FakeUpstream implements UpstreamSocket {
  sent: (string | ArrayBuffer)[] = [];
  closed = false;
  constructor(
    readonly deviceId: string,
    readonly handlers: UpstreamHandlers,
  ) {}
  send(data: string | ArrayBuffer): void {
    if (this.closed) throw new Error("closed");
    this.sent.push(data);
  }
  close(): void {
    this.closed = true;
  }
  /** 受付から届いたことにする。 */
  emit(data: string | ArrayBuffer | Record<string, unknown>): void {
    this.handlers.message(typeof data === "string" || data instanceof ArrayBuffer ? data : JSON.stringify(data));
  }
}

class FakeBuilder implements Builder {
  isRunning = false;
  starts = 0;
  /** start() のあと、/healthz が通るまでに聞かれる回数（Infinity = 通らない）。 */
  healthyAfter = 2;
  healthChecks = 0;
  startThrows = false;
  openThrows = false;
  statusValue: BuilddStatus | null = { running: 0, queued: 0, jobs: [] };
  statusCalls = 0;
  keepWarmCalls: number[] = [];
  ups: FakeUpstream[] = [];

  running(): boolean {
    return this.isRunning;
  }
  start(): void {
    this.starts++;
    if (this.startThrows) throw new Error("no capacity");
    this.isRunning = true;
    this.healthChecks = 0;
  }
  async healthy(): Promise<boolean> {
    return this.isRunning && ++this.healthChecks >= this.healthyAfter;
  }
  async status(): Promise<BuilddStatus | null> {
    this.statusCalls++;
    return this.statusValue;
  }
  async keepWarm(ms: number): Promise<void> {
    this.keepWarmCalls.push(ms);
  }
  async open(deviceId: string, handlers: UpstreamHandlers): Promise<UpstreamSocket> {
    if (this.openThrows) throw new Error("refused");
    const up = new FakeUpstream(deviceId, handlers);
    this.ups.push(up);
    return up;
  }
}

class FakeClock implements Clock {
  /** 2026-11-17T12:00:00Z */
  t = Date.UTC(2026, 10, 17, 12);
  alarm: number | null = null;
  now(): number {
    return this.t;
  }
  async sleep(ms: number): Promise<void> {
    this.t += ms;
  }
  async ensureAlarm(atMs: number): Promise<void> {
    this.alarm ??= atMs;
  }
}

/** 待たずに始めた処理（上流を張る）が落ち着くまで待つ。 */
const settle = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

const J1 = "7f3a".repeat(8);
const J2 = "00ff".repeat(8);
const J3 = "a1b2".repeat(8);
const hello = JSON.stringify({ type: "hello", v: 1, client: "zc-remote/0.1.0" });
const jobMsg = (job: string, kind = "build"): string =>
  JSON.stringify({ type: "job", job, project: "3f9a", tree: "b1c2", kind, lang: "rust", opts: {}, log: true });

interface World {
  ledger: SqlLedger;
  builder: FakeBuilder;
  clock: FakeClock;
  core: SessionCore;
}

function world(jobsPerDay = 400, warm = true): World {
  const ledger = new SqlLedger(memorySql());
  const builder = new FakeBuilder();
  builder.isRunning = warm;
  builder.healthyAfter = 1;
  const clock = new FakeClock();
  const core = new SessionCore(ledger, builder, clock, { keepWarmMs: 600_000, jobsPerDay });
  return { ledger, builder, clock, core };
}

/** 動いているコンテナへ 1 本つなぐ。 */
async function attachDevice(w: World, id = "cz-1"): Promise<{ dev: FakeDevice; up: FakeUpstream }> {
  const dev = new FakeDevice();
  const before = w.builder.ups.length;
  w.core.connect(dev, id);
  await settle();
  const up = w.builder.ups[before];
  if (up === undefined) throw new Error("上流が張られていない");
  return { dev, up };
}

// ── 台帳 ──

describe("SqlLedger", () => {
  let l: SqlLedger;
  const t0 = Date.UTC(2026, 10, 17, 12);
  beforeEach(() => {
    l = new SqlLedger(memorySql());
  });

  it("受け付け → started → done", () => {
    expect(l.stateOf(J1)).toBeNull();
    expect(l.admit(J1, "build", t0, 10)).toBe(true);
    expect(l.stateOf(J1)).toBe("running");
    expect(l.running()).toBe(1);
    l.started(J1, t0 + 5);
    l.done(J1, "ok", t0 + 100);
    expect(l.stateOf(J1)).toBe("done");
    expect(l.running()).toBe(0);
  });

  it("1 日の上限。同じ job id の出し直しは数えない", () => {
    expect(l.admit("a1", "build", t0, 2)).toBe(true);
    expect(l.admit("a2", "test", t0, 2)).toBe(true);
    expect(l.admit("a3", "build", t0, 2)).toBe(false);
    expect(l.stateOf("a3")).toBeNull();
    // 出し直しは、上限に達していても通る
    l.done("a1", "failed", t0 + 1);
    expect(l.admit("a1", "build", t0 + 2, 2)).toBe(true);
    expect(l.stateOf("a1")).toBe("running");
    expect(l.admit("a3", "test", t0 + 3, 2)).toBe(false);
  });

  it("check は数えない: 上限に達していても通り、build / test の枠も減らさない", () => {
    for (let i = 0; i < 5; i++) expect(l.admit(`c${i}`, "check", t0, 2)).toBe(true);
    // 台帳には載る（running の見張り・lost の判定に使う）
    expect(l.stateOf("c0")).toBe("running");
    expect(l.running()).toBe(5);
    expect(l.admit("b1", "build", t0, 2)).toBe(true);
    expect(l.admit("b2", "test", t0, 2)).toBe(true);
    expect(l.admit("b3", "build", t0, 2)).toBe(false);
    expect(l.admit("c5", "check", t0, 2)).toBe(true);
    // 上限 0 でも check は通る
    expect(l.admit("c6", "check", t0, 0)).toBe(true);
  });

  it("日付（UTC）が変わると数え直す。前の日の job id は新しい日に数える", () => {
    expect(l.admit("a1", "build", t0, 1)).toBe(true);
    expect(l.admit("a2", "build", t0, 1)).toBe(false);
    const next = t0 + 86_400_000;
    expect(l.admit("a2", "build", next, 1)).toBe(true);
    expect(l.admit("a1", "build", next, 1)).toBe(false);
  });

  it("上限 0 は全部断る", () => {
    expect(l.admit("a1", "build", t0, 0)).toBe(false);
  });

  it("reconcile: alive に無い running を lost にする。受け付けたばかりは触らない", () => {
    l.admit("a1", "build", t0, 10);
    l.admit("a2", "build", t0, 10);
    l.admit("a3", "build", t0 + RECONCILE_GRACE_MS, 10);
    l.done("a2", "ok", t0 + 1);
    expect(l.reconcile(["zz"], t0 + RECONCILE_GRACE_MS - 1)).toEqual([]);
    expect(l.reconcile(["zz"], t0 + RECONCILE_GRACE_MS)).toEqual(["a1"]);
    expect(l.stateOf("a1")).toBe("lost");
    expect(l.stateOf("a2")).toBe("done");
    expect(l.stateOf("a3")).toBe("running");
    expect(l.reconcile(["a3"], t0 + 10 * RECONCILE_GRACE_MS)).toEqual([]);
    expect(l.stateOf("a3")).toBe("running");
  });

  it("lost の出し直しは running に戻り、数えない。started は lost を running に戻す", () => {
    l.admit("a1", "build", t0, 1);
    l.reconcile([], t0 + RECONCILE_GRACE_MS);
    expect(l.stateOf("a1")).toBe("lost");
    expect(l.admit("a1", "build", t0 + 60_000, 1)).toBe(true);
    expect(l.stateOf("a1")).toBe("running");
    l.reconcile([], t0 + 120_000);
    l.started("a1", t0 + 130_000);
    expect(l.stateOf("a1")).toBe("running");
  });

  it("知らないジョブの started / done は何もしない", () => {
    l.started("zz", t0);
    l.done("zz", "ok", t0);
    expect(l.stateOf("zz")).toBeNull();
    expect(l.running()).toBe(0);
  });

  it("prune: 7 日より古い行を消す", () => {
    l.admit("a1", "build", t0, 10);
    l.done("a1", "ok", t0 + 1);
    l.admit("a2", "build", t0 + 3 * 86_400_000, 10);
    l.prune(t0 + 7 * 86_400_000 + 2);
    expect(l.stateOf("a1")).toBeNull();
    expect(l.stateOf("a2")).toBe("running");
  });

  it("表を 2 回作っても壊れない（DO の再起動）", () => {
    const sql = memorySql();
    new SqlLedger(sql).admit("a1", "build", t0, 10);
    expect(new SqlLedger(sql).stateOf("a1")).toBe("running");
  });

  it("日付と、次の日までの秒数", () => {
    expect(dayOf(t0)).toBe("2026-11-17");
    expect(secondsToNextDay(t0)).toBe(12 * 3600);
    expect(secondsToNextDay(Date.UTC(2026, 10, 17, 23, 59, 59, 500))).toBe(1);
  });
});

// ── 中継 ──

describe("中継", () => {
  it("テキストもバイナリも、両方向に変えずに流す。上流には device_id を渡す", async () => {
    const w = world();
    const { dev, up } = await attachDevice(w);
    expect(up.deviceId).toBe("cz-1");
    expect(w.builder.starts).toBe(0);

    // 空白や項目の順番もそのまま（JSON を作り直さない）
    const manifest = '{ "files":[{"p":"日本語.rs","s":3,"h":"00"}], "type":"manifest","project":"p","tree":"t" }';
    const blob = new Uint8Array([1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9, 8, 7]).buffer;
    await w.core.message(dev, hello);
    await w.core.message(dev, manifest);
    await w.core.message(dev, blob);
    expect(up.sent).toEqual([hello, manifest, blob]);
    expect(up.sent[2]).toBe(blob);

    const reply = '{"type":"hello","v":1,"server":"zc-buildd/0.1.0"}';
    const art = new Uint8Array([2, 1, 0, 0, 1, 0, 0, 0]).buffer;
    const unknown = '{"type":"future_thing","x":1}';
    up.emit(reply);
    up.emit(art);
    up.emit(unknown);
    up.emit("not json at all");
    expect(dev.sent).toEqual([reply, art, unknown, "not json at all"]);
    expect(dev.closed).toBeNull();
  });

  it("上流が開く前に届いたものは、順番どおりにためて流す", async () => {
    const w = world();
    const dev = new FakeDevice();
    w.core.connect(dev, "cz-1");
    const bin = new ArrayBuffer(20);
    await w.core.message(dev, hello);
    await w.core.message(dev, '{"type":"manifest","project":"p","tree":"t","files":[]}');
    await w.core.message(dev, bin);
    expect(w.builder.ups).toHaveLength(0);
    await settle();
    expect(w.builder.ups[0]!.sent).toEqual([hello, '{"type":"manifest","project":"p","tree":"t","files":[]}', bin]);
    expect(dev.sent).toEqual([]);
  });

  it("壊れた JSON・オブジェクトでない JSON は、中継せず error{protocol} で閉じる", async () => {
    for (const bad of ["{oops", "[1,2]", '"x"', "null"]) {
      const w = world();
      const { dev, up } = await attachDevice(w);
      await w.core.message(dev, bad);
      expect(up.sent).toEqual([]);
      expect(dev.json()).toEqual([{ type: "error", code: "protocol", message: expect.any(String) }]);
      expect(dev.closed?.code).toBe(CLOSE.protocol);
      expect(up.closed).toBe(true);
      expect(w.core.connections).toBe(0);
    }
  });

  it("job id・kind がおかしい job / attach / cancel は error{protocol}", async () => {
    const bads = [
      { type: "job", job: 12, kind: "build" },
      { type: "job", job: "ZZ", kind: "build" },
      { type: "job", job: J1, kind: "deploy" },
      { type: "attach", job: "" },
      { type: "cancel" },
    ];
    for (const bad of bads) {
      const w = world();
      const { dev, up } = await attachDevice(w);
      await w.core.message(dev, JSON.stringify(bad));
      expect(up.sent).toEqual([]);
      expect(dev.json()[0]).toMatchObject({ type: "error", code: "protocol" });
      expect(dev.closed?.code).toBe(CLOSE.protocol);
      expect(w.ledger.running()).toBe(0);
    }
  });

  it("大きすぎるメッセージは error{too-large} で閉じる。上限ちょうどは通る", async () => {
    const w = world();
    const { dev, up } = await attachDevice(w);
    await w.core.message(dev, new ArrayBuffer(MAX_BINARY_FRAME));
    const pad = "x".repeat(MAX_TEXT_FRAME - '{"type":"manifest","p":""}'.length);
    await w.core.message(dev, `{"type":"manifest","p":"${pad}"}`);
    expect(up.sent).toHaveLength(2);
    await w.core.message(dev, new ArrayBuffer(MAX_BINARY_FRAME + 1));
    expect(up.sent).toHaveLength(2);
    expect(dev.json()).toEqual([{ type: "error", code: "too-large", message: expect.any(String) }]);
    expect(dev.closed?.code).toBe(CLOSE.tooLarge);

    const w2 = world();
    const b = await attachDevice(w2);
    // 文字数は上限の中でも、UTF-8 では超える
    await w2.core.message(b.dev, JSON.stringify({ type: "manifest", p: "あ".repeat(MAX_TEXT_FRAME / 3) }));
    expect(b.up.sent).toEqual([]);
    expect(b.dev.json()[0]).toMatchObject({ code: "too-large" });
  });

  it("上流が開く前にためすぎたら閉じる", async () => {
    const w = world();
    w.builder.healthyAfter = Infinity;
    w.builder.isRunning = false;
    const dev = new FakeDevice();
    w.core.connect(dev, "cz-1");
    const big = JSON.stringify({ type: "manifest", p: "x".repeat(MAX_TEXT_FRAME - 100) });
    await w.core.message(dev, big);
    await w.core.message(dev, big);
    expect(dev.closed).toBeNull();
    await w.core.message(dev, big);
    expect(dev.json().at(-1)).toMatchObject({ type: "error", code: "too-large" });
    expect(dev.closed?.code).toBe(CLOSE.tooLarge);
  });

  it("同時の接続は 4 本まで。1 本閉じれば空く", async () => {
    const w = world();
    const devs: FakeDevice[] = [];
    for (let i = 0; i < MAX_CONNECTIONS_PER_DEVICE; i++) {
      expect(w.core.hasRoom()).toBe(true);
      devs.push((await attachDevice(w)).dev);
    }
    expect(w.core.hasRoom()).toBe(false);
    w.core.deviceClosed(devs[0]!);
    expect(w.core.hasRoom()).toBe(true);
  });

  it("上流が閉じたら、実機の側も閉じる", async () => {
    const w = world();
    const { dev, up } = await attachDevice(w);
    up.handlers.close(1000, "bye");
    expect(dev.closed?.code).toBe(CLOSE.upstream);
    expect(w.core.connections).toBe(0);
    // そのあと届いたものは捨てる
    up.emit({ type: "log", job: J1, offset: 0, text: "x" });
    expect(dev.sent).toEqual([]);
  });

  it("知らない WebSocket（DO が作り直された）からのメッセージは、閉じてつなぎ直させる", async () => {
    const w = world();
    const dev = new FakeDevice();
    await w.core.message(dev, hello);
    expect(dev.closed).toEqual({ code: CLOSE.restart, reason: "restart" });
    expect(dev.sent).toEqual([]);
    expect(w.builder.ups).toHaveLength(0);
  });
});

// ── 台帳と上限 ──

describe("ジョブの台帳", () => {
  it("job → started → done が台帳に付き、alarm を掛ける", async () => {
    const w = world();
    const { dev, up } = await attachDevice(w);
    await w.core.message(dev, jobMsg(J1));
    expect(up.sent).toEqual([jobMsg(J1)]);
    expect(w.ledger.stateOf(J1)).toBe("running");
    expect(w.clock.alarm).toBe(w.clock.t + ALARM_INTERVAL_MS);

    up.emit({ type: "accepted", job: J1, cached: false, queued: 0 });
    up.emit({ type: "started", job: J1 });
    up.emit({ type: "log", job: J1, offset: 0, text: '{"type":"done"} に見える文字が入ったログ' });
    expect(w.ledger.stateOf(J1)).toBe("running");
    up.emit({ type: "done", job: J1, status: "failed", exit: 101, queue_ms: 3, run_ms: 4210 });
    expect(w.ledger.stateOf(J1)).toBe("done");
    expect(dev.json().map((m) => m.type)).toEqual(["accepted", "started", "log", "done"]);
  });

  it("type が先頭に無い JSON でも done を読む", async () => {
    const w = world();
    const { dev, up } = await attachDevice(w);
    await w.core.message(dev, jobMsg(J1));
    up.emit(`{"job":"${J1}","status":"ok","type":"done"}`);
    expect(w.ledger.stateOf(J1)).toBe("done");
  });

  it("1 日の上限: 中継せず error{limit} を返し、接続は続く。同じ job id は数えない", async () => {
    const w = world(1);
    const { dev, up } = await attachDevice(w);
    await w.core.message(dev, jobMsg(J1));
    await w.core.message(dev, jobMsg(J2));
    expect(up.sent).toEqual([jobMsg(J1)]);
    expect(dev.json()).toEqual([{ type: "error", job: J2, code: "limit", message: expect.any(String), retry_after_s: 12 * 3600 }]);
    expect(dev.closed).toBeNull();
    expect(w.ledger.stateOf(J2)).toBeNull();
    // 同じ job id の出し直しは通る
    await w.core.message(dev, jobMsg(J1));
    expect(up.sent).toEqual([jobMsg(J1), jobMsg(J1)]);
    // 日付が変われば通る
    w.clock.t += 86_400_000;
    await w.core.message(dev, jobMsg(J2));
    expect(up.sent).toHaveLength(3);
  });

  it("check のジョブは 1 日の上限で断らない", async () => {
    const w = world(1);
    const { dev, up } = await attachDevice(w);
    await w.core.message(dev, jobMsg(J1));
    await w.core.message(dev, jobMsg(J2, "check"));
    await w.core.message(dev, jobMsg(J3, "check"));
    expect(up.sent).toEqual([jobMsg(J1), jobMsg(J2, "check"), jobMsg(J3, "check")]);
    expect(dev.json()).toEqual([]);
    expect(w.ledger.stateOf(J3)).toBe("running");
  });

  it("実機が切れても、ジョブは台帳で running のまま。上流だけ閉じる。つなぎ直した接続で done が付く", async () => {
    const w = world();
    const a = await attachDevice(w);
    await w.core.message(a.dev, jobMsg(J1));
    a.up.emit({ type: "started", job: J1 });
    w.core.deviceClosed(a.dev);
    expect(a.up.closed).toBe(true);
    expect(w.core.connections).toBe(0);
    expect(w.ledger.stateOf(J1)).toBe("running");

    // 見張り: 受付が「走っている」と言う間は、running のまま alarm を掛け直す
    w.clock.t += ALARM_INTERVAL_MS;
    w.clock.alarm = null;
    w.builder.statusValue = { running: 1, queued: 0, jobs: [{ job: J1, state: "running" }] };
    await w.core.alarm();
    expect(w.ledger.stateOf(J1)).toBe("running");
    expect(w.clock.alarm).toBe(w.clock.t + ALARM_INTERVAL_MS);
    expect(w.builder.keepWarmCalls.at(-1)).toBe(600_000);

    // つなぎ直して attach。そのまま上流へ流れ、done で台帳が閉じる
    const b = await attachDevice(w);
    const attach = JSON.stringify({ type: "attach", job: J1, log: 120, artifact: 0 });
    await w.core.message(b.dev, hello);
    await w.core.message(b.dev, attach);
    expect(b.up.sent).toEqual([hello, attach]);
    b.up.emit({ type: "done", job: J1, status: "ok", exit: 0, queue_ms: 0, run_ms: 9 });
    expect(w.ledger.stateOf(J1)).toBe("done");

    // running が無くなったら、alarm を掛け直さない
    w.clock.alarm = null;
    w.builder.statusValue = { running: 0, queued: 0, jobs: [] };
    await w.core.alarm();
    expect(w.clock.alarm).toBeNull();
  });

  it("見ている実機がいない間に終わったジョブは、/v1/status から done にする", async () => {
    const w = world();
    const a = await attachDevice(w);
    await w.core.message(a.dev, jobMsg(J1));
    w.core.deviceClosed(a.dev);
    w.clock.t += ALARM_INTERVAL_MS;
    w.clock.alarm = null;
    w.builder.statusValue = { running: 0, queued: 0, jobs: [{ job: J1, state: "done", status: "ok" }] };
    await w.core.alarm();
    expect(w.ledger.stateOf(J1)).toBe("done");
    expect(w.clock.alarm).toBeNull();
  });

  it("alarm: 台帳で running なのに /v1/status に無いジョブは lost。受け付けたばかりは待つ", async () => {
    const w = world();
    const { dev } = await attachDevice(w);
    await w.core.message(dev, jobMsg(J1));
    w.builder.statusValue = { running: 0, queued: 0, jobs: [] };

    // 受け付けた直後（受付の台帳にまだ載っていないかもしれない）
    w.clock.t += RECONCILE_GRACE_MS - 1;
    w.clock.alarm = null;
    await w.core.alarm();
    expect(w.ledger.stateOf(J1)).toBe("running");
    expect(w.clock.alarm).not.toBeNull();

    w.clock.t += ALARM_INTERVAL_MS;
    w.clock.alarm = null;
    await w.core.alarm();
    expect(w.ledger.stateOf(J1)).toBe("lost");
    expect(w.clock.alarm).toBeNull();
  });

  it("alarm: コンテナが止まっていたら全部 lost。受付が 2 回答えなくても lost", async () => {
    const w = world();
    const { dev } = await attachDevice(w);
    await w.core.message(dev, jobMsg(J1));
    w.builder.isRunning = false;
    w.clock.alarm = null;
    await w.core.alarm();
    expect(w.ledger.stateOf(J1)).toBe("lost");
    expect(w.builder.statusCalls).toBe(0);
    expect(w.clock.alarm).toBeNull();

    const w2 = world();
    const b = await attachDevice(w2);
    await w2.core.message(b.dev, jobMsg(J2));
    w2.builder.statusValue = null;
    w2.clock.alarm = null;
    await w2.core.alarm();
    expect(w2.builder.statusCalls).toBe(2);
    expect(w2.ledger.stateOf(J2)).toBe("lost");
    expect(w2.clock.alarm).toBeNull();
  });

  it("lost のジョブへの attach には、流さずに done{lost}（コンテナが動いていても）", async () => {
    const w = world();
    const { dev, up } = await attachDevice(w);
    await w.core.message(dev, jobMsg(J1));
    w.clock.t += ALARM_INTERVAL_MS;
    w.builder.statusValue = { running: 0, queued: 0, jobs: [] };
    await w.core.alarm();
    await w.core.message(dev, JSON.stringify({ type: "attach", job: J1, log: 0, artifact: 0 }));
    expect(up.sent).toEqual([jobMsg(J1)]);
    expect(dev.json()).toEqual([{ type: "done", job: J1, status: "lost", exit: 0, queue_ms: 0, run_ms: 0 }]);
    // 出し直しは同じジョブとして通り、running に戻る
    await w.core.message(dev, jobMsg(J1));
    expect(w.ledger.stateOf(J1)).toBe("running");
    expect(up.sent).toHaveLength(2);
  });
});

// ── コンテナが止まっているとき ──

describe("冷えた接続", () => {
  /** ジョブを 1 つ走らせたまま、コンテナが止まった世界。 */
  async function stopped(): Promise<World> {
    const w = world();
    const a = await attachDevice(w);
    await w.core.message(a.dev, jobMsg(J1));
    w.core.deviceClosed(a.dev);
    w.builder.isRunning = false;
    w.builder.ups = [];
    return w;
  }

  it("lost のジョブへの attach に done{lost}。コンテナを起こさない", async () => {
    const w = await stopped();
    const dev = new FakeDevice();
    w.core.connect(dev, "cz-1");
    // つないだ時点で、走っていたはずのジョブは lost（受け付けた直後でも）
    expect(w.ledger.stateOf(J1)).toBe("lost");

    await w.core.message(dev, hello);
    await w.core.message(dev, JSON.stringify({ type: "attach", job: J1, log: 10, artifact: 0 }));
    await w.core.message(dev, JSON.stringify({ type: "attach", job: J2 }));
    await w.core.message(dev, JSON.stringify({ type: "cancel", job: J2 }));
    await settle();
    expect(dev.json()).toEqual([
      { type: "hello", v: 1, server: DO_SERVER_NAME },
      { type: "done", job: J1, status: "lost", exit: 0, queue_ms: 0, run_ms: 0 },
      { type: "error", job: J2, code: "unknown-job", message: expect.any(String) },
      { type: "error", job: J2, code: "unknown-job", message: expect.any(String) },
    ]);
    expect(w.builder.starts).toBe(0);
    expect(w.builder.ups).toHaveLength(0);
    expect(dev.closed).toBeNull();
  });

  it("目録が来たらコンテナを起こし、覚えた hello から順に流す。上流の hello は 1 回だけ捨てる", async () => {
    const w = await stopped();
    w.builder.healthyAfter = 3;
    const dev = new FakeDevice();
    w.core.connect(dev, "cz-1");
    const manifest = '{"type":"manifest","project":"p","tree":"t","files":[]}';
    await w.core.message(dev, hello);
    await w.core.message(dev, manifest);
    await w.core.message(dev, jobMsg(J1));
    await settle();
    expect(w.builder.starts).toBe(1);
    // 1 回目は準備（stopped）のぶん、2 回目が起こし直したあと
    expect(w.builder.keepWarmCalls).toEqual([600_000, 600_000]);
    const up = w.builder.ups[0]!;
    expect(up.sent).toEqual([hello, manifest, jobMsg(J1)]);
    expect(w.ledger.stateOf(J1)).toBe("running");

    up.emit({ type: "hello", v: 1, server: "zc-buildd/0.1.0" });
    up.emit({ type: "need", tree: "t", blobs: [] });
    up.emit({ type: "hello", v: 1, server: "again" });
    expect(dev.json().map((m) => m.server ?? m.type)).toEqual([DO_SERVER_NAME, "need", "again"]);
  });

  it("上流が hello に error{version} で答えたら、そのまま流す", async () => {
    const w = await stopped();
    const dev = new FakeDevice();
    w.core.connect(dev, "cz-1");
    await w.core.message(dev, hello);
    await w.core.message(dev, '{"type":"manifest","project":"p","tree":"t","files":[]}');
    await settle();
    w.builder.ups[0]!.emit({ type: "error", code: "version", message: "v2 only" });
    expect(dev.json().at(-1)).toEqual({ type: "error", code: "version", message: "v2 only" });
  });

  it("知らない版の hello には自分で答えず、コンテナを起こして受付に任せる", async () => {
    const w = await stopped();
    const dev = new FakeDevice();
    w.core.connect(dev, "cz-1");
    const h2 = JSON.stringify({ type: "hello", v: 2, client: "zc-remote/9" });
    await w.core.message(dev, h2);
    await settle();
    expect(dev.sent).toEqual([]);
    expect(w.builder.starts).toBe(1);
    expect(w.builder.ups[0]!.sent).toEqual([h2]);
    w.builder.ups[0]!.emit({ type: "hello", v: 1, server: "zc-buildd/0.1.0" });
    expect(dev.sent).toHaveLength(1);
  });

  it("コンテナを起こせない（start が投げる）とき error{unavailable} で閉じる", async () => {
    const w = await stopped();
    w.builder.startThrows = true;
    const dev = new FakeDevice();
    w.core.connect(dev, "cz-1");
    await w.core.message(dev, hello);
    await w.core.message(dev, jobMsg(J2));
    await settle();
    expect(dev.json().at(-1)).toEqual({ type: "error", code: "unavailable", message: expect.any(String) });
    expect(dev.closed?.code).toBe(CLOSE.unavailable);
    expect(w.core.connections).toBe(0);
  });

  it("/healthz が 30 秒通らないとき error{unavailable}。次の接続でもう一度試す", async () => {
    const w = await stopped();
    w.builder.healthyAfter = Infinity;
    const dev = new FakeDevice();
    w.core.connect(dev, "cz-1");
    const t0 = w.clock.t;
    await w.core.message(dev, '{"type":"manifest","project":"p","tree":"t","files":[]}');
    await settle();
    expect(w.clock.t - t0).toBeGreaterThanOrEqual(START_WAIT_MS);
    expect(dev.json().at(-1)).toMatchObject({ type: "error", code: "unavailable" });
    expect(dev.closed?.code).toBe(CLOSE.unavailable);

    // コンテナは起きかけのまま。次の接続は start し直さず、/healthz から待つ
    w.builder.healthyAfter = 1;
    const b = await attachDevice(w);
    expect(w.builder.starts).toBe(1);
    await w.core.message(b.dev, hello);
    expect(b.up.sent).toEqual([hello]);
  });

  it("上流の WebSocket を張れないとき error{unavailable}", async () => {
    const w = world();
    w.builder.openThrows = true;
    const dev = new FakeDevice();
    w.core.connect(dev, "cz-1");
    await settle();
    expect(dev.json()).toEqual([{ type: "error", code: "unavailable", message: expect.any(String) }]);
    expect(dev.closed?.code).toBe(CLOSE.unavailable);
  });

  it("起動待ちは同時の接続で 1 つを分け合う。待っている間に切れた接続には上流を張らない", async () => {
    const w = await stopped();
    w.builder.healthyAfter = 5;
    const a = new FakeDevice();
    const b = new FakeDevice();
    w.core.connect(a, "cz-1");
    await w.core.message(a, '{"type":"manifest","project":"p","tree":"a","files":[]}');
    w.core.connect(b, "cz-1");
    await w.core.message(b, hello);
    w.core.deviceClosed(a);
    await settle();
    expect(w.builder.starts).toBe(1);
    expect(w.builder.ups).toHaveLength(1);
    expect(w.builder.ups[0]!.sent).toEqual([hello]);
    // 起動待ちの間につないだ接続は「冷えた接続」ではない（hello は受付が答える）
    expect(b.sent).toEqual([]);
  });

  it("起こしている途中の alarm は、何も lost にせず次を掛ける", async () => {
    const w = await stopped();
    w.builder.healthyAfter = Infinity;
    const dev = new FakeDevice();
    w.core.connect(dev, "cz-1");
    void w.core.message(dev, jobMsg(J2));
    w.clock.alarm = null;
    await w.core.alarm();
    expect(w.ledger.stateOf(J2)).toBe("running");
    expect(w.clock.alarm).not.toBeNull();
    await settle();
  });
});

// ── 小物 ──

describe("設定と /v1/status", () => {
  it("configOf: おかしな値は既定に戻す", () => {
    expect(configOf({ KEEP_WARM_S: "600", JOBS_PER_DAY: "400" })).toEqual({ keepWarmMs: 600_000, jobsPerDay: 400 });
    expect(configOf({ KEEP_WARM_S: "abc", JOBS_PER_DAY: "-1" })).toEqual({ keepWarmMs: 600_000, jobsPerDay: 400 });
    // setInactivityTimeout の上限（6 時間）を超える値
    expect(configOf({ KEEP_WARM_S: "99999", JOBS_PER_DAY: "0" })).toEqual({ keepWarmMs: 600_000, jobsPerDay: 0 });
  });

  it("parseStatus: 形のおかしい行は捨てる", () => {
    expect(parseStatus({ running: 1, queued: 2, jobs: [{ job: "a", kind: "build", state: "running" }, { nope: 1 }, 3], uptime_s: 9 })).toEqual({
      running: 1,
      queued: 2,
      jobs: [{ job: "a", state: "running", status: undefined }],
    });
    expect(parseStatus("ok")).toBeNull();
    expect(parseStatus({})).toEqual({ running: 0, queued: 0, jobs: [] });
  });
});
