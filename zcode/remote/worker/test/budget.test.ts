// 費用の守り（src/budget.ts）の単体の試験。SessionCore とのつなぎは session.test.ts の「費用の守り」。

import { describe, expect, it } from "vitest";
import {
  BudgetBook,
  DEFAULT_INSTANCE,
  INCLUDED,
  Meter,
  PRICE,
  STOP_SLACK_MS,
  budgetOf,
  extrapolate,
  guardOf,
  instanceOf,
  ledgerFetch,
  monthOf,
  overageOf,
  parseSnapshot,
  rateOf,
  secondsToNextMonth,
  splitMonths,
  startInstance,
  usageFor,
  type Snapshot,
} from "../src/budget";
import { configOf, type SqlExec } from "../src/session";

function memorySql(): SqlExec {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const sqlite = (globalThis as any).process.getBuiltinModule("node:sqlite");
  const db = new sqlite.DatabaseSync(":memory:");
  return (q, ...b) => db.prepare(q).all(...b) as Record<string, unknown>[];
}

/** 2026-11-17T12:00:00Z */
const T0 = Date.UTC(2026, 10, 17, 12);
const H = 3_600_000;

describe("コンテナの大きさ", () => {
  const quiet = () => {};

  it("既定は custom の 1 vCPU・3GiB・12GB", () => {
    expect(instanceOf({})).toEqual({ name: null, vcpu: 1, memoryMib: 3072, diskMb: 12000 });
    expect(instanceOf({ INSTANCE_TYPE: "", INSTANCE_VCPU: "2", INSTANCE_MEMORY_MIB: "6144", INSTANCE_DISK_MB: "20000" })).toEqual({
      name: null,
      vcpu: 2,
      memoryMib: 6144,
      diskMb: 20000,
    });
    expect(startInstance(DEFAULT_INSTANCE)).toEqual({ vcpu: 1, memoryMib: 3072, diskMb: 12000 });
  });

  it("名前つきの大きさ。start() には名前で渡す", () => {
    const s = instanceOf({ INSTANCE_TYPE: "standard-4", INSTANCE_VCPU: "1" });
    expect(s).toEqual({ name: "standard-4", vcpu: 4, memoryMib: 12288, diskMb: 20000 });
    expect(startInstance(s)).toBe("standard-4");
    expect(instanceOf({ INSTANCE_TYPE: " lite " }).name).toBe("lite");
  });

  it("おかしな値は既定に戻し、ログに残す", () => {
    const bad = [
      { INSTANCE_TYPE: "basic" }, // durable_object 方式の start() は受けない
      { INSTANCE_TYPE: "standard" },
      { INSTANCE_VCPU: "1", INSTANCE_MEMORY_MIB: "2048" }, // 1 vCPU あたり 3GiB 未満
      { INSTANCE_VCPU: "2", INSTANCE_MEMORY_MIB: "6000" },
      { INSTANCE_VCPU: "8", INSTANCE_MEMORY_MIB: "12288" }, // vCPU が多すぎる
      { INSTANCE_MEMORY_MIB: "16384" },
      { INSTANCE_DISK_MB: "25000" },
      { INSTANCE_DISK_MB: "1000" },
      { INSTANCE_VCPU: "abc" },
      { INSTANCE_VCPU: "-1" },
      { INSTANCE_MEMORY_MIB: "3072.5" },
    ];
    for (const env of bad) {
      const logs: string[] = [];
      expect(instanceOf(env, (m) => logs.push(m))).toEqual(DEFAULT_INSTANCE);
      expect(logs).toHaveLength(1);
    }
    // 下限ちょうど（1/4 vCPU・768MiB）は通る
    expect(instanceOf({ INSTANCE_VCPU: "0.25", INSTANCE_MEMORY_MIB: "768", INSTANCE_DISK_MB: "2000" }, quiet)).toEqual({
      name: null,
      vcpu: 0.25,
      memoryMib: 768,
      diskMb: 2000,
    });
  });

  it("上限の値を読む。おかしな値は 5 ドル", () => {
    const quietLog = () => {};
    expect(budgetOf(undefined)).toBe(5);
    expect(budgetOf("5.00")).toBe(5);
    expect(budgetOf("0")).toBe(0);
    expect(budgetOf("12.5")).toBe(12.5);
    for (const v of ["-1", "abc", "1e3", "5,00", "100000"]) expect(budgetOf(v, quietLog)).toBe(5);
  });
});

describe("wrangler.jsonc の vars", () => {
  it("既定の大きさ・keep-warm 300 秒・上限 5 ドル", () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const fs = (globalThis as any).process.getBuiltinModule("node:fs");
    const here = (import.meta as unknown as { url: string }).url;
    const text: string = fs.readFileSync(new URL("../wrangler.jsonc", here), "utf8");
    // 行の頭からのコメントだけを使っている（文字列の中に // は無い）
    const conf = JSON.parse(
      text
        .split("\n")
        .filter((l) => !l.trim().startsWith("//"))
        .join("\n"),
    ) as { vars: Record<string, string>; durable_objects: { bindings: { name: string; class_name: string }[] }; exports: Record<string, unknown> };
    const cfg = configOf(conf.vars, (m) => {
      throw new Error(m);
    });
    expect(cfg.keepWarmMs).toBe(300_000);
    expect(cfg.instance).toEqual(DEFAULT_INSTANCE);
    expect(cfg.budgetUsd).toBe(5);
    expect(conf.durable_objects.bindings).toContainEqual({ name: "BUDGET", class_name: "BudgetLedger" });
    expect(conf.exports.BudgetLedger).toEqual({ type: "durable-object", storage: "sqlite" });
  });
});

describe("料金", () => {
  it("1 vCPU・3GiB は 1 時間でメモリ 0.027 ドル。含まれる 25GiB 時間を超えた分だけ", () => {
    const hour = usageFor(rateOf(DEFAULT_INSTANCE, false), H);
    expect(hour.memGibS * PRICE.memGibS).toBeCloseTo(0.027, 6);
    expect(hour.diskGbS * PRICE.diskGbS).toBeCloseTo(0.003024, 6);
    expect(overageOf({ memGibS: INCLUDED.memGibS, vcpuS: INCLUDED.vcpuS, diskGbS: INCLUDED.diskGbS }).totalUsd).toBe(0);
    const o = overageOf({ memGibS: INCLUDED.memGibS + 1000, vcpuS: INCLUDED.vcpuS + 100, diskGbS: INCLUDED.diskGbS + 1e6 });
    expect(o.memoryUsd).toBeCloseTo(0.0025, 9);
    expect(o.cpuUsd).toBeCloseTo(0.002, 9);
    expect(o.diskUsd).toBeCloseTo(0.07, 9);
    expect(o.totalUsd).toBeCloseTo(0.0745, 9);
  });

  it("guard: 次の 1 ジョブ（15 分）と keep-warm を見込んで、上限を超えるなら断る", () => {
    const keep = 300_000;
    const reserve = usageFor(rateOf(DEFAULT_INSTANCE, true), 15 * 60_000 + keep);
    const reserveUsd = reserve.memGibS * PRICE.memGibS + reserve.vcpuS * PRICE.vcpuS + reserve.diskGbS * PRICE.diskGbS;
    // メモリだけで超過がちょうど (5 - 見込み) ドル → まだ通る。少しでも超えたら断る
    const at = (usd: number) => ({ memGibS: INCLUDED.memGibS + usd / PRICE.memGibS, vcpuS: INCLUDED.vcpuS, diskGbS: INCLUDED.diskGbS });
    expect(guardOf(at(5 - reserveUsd - 0.001), DEFAULT_INSTANCE, keep, 5, T0).exhausted).toBe(false);
    const g = guardOf(at(5 - reserveUsd + 0.001), DEFAULT_INSTANCE, keep, 5, T0);
    expect(g.exhausted).toBe(true);
    expect(g.retryAfterS).toBe(secondsToNextMonth(T0));
    expect(g.message).toContain("2026-12-01");
    expect(g.message).toContain("5.00 ドル");
    // 含まれる分の中なら、上限 0 でも通る
    expect(guardOf({ memGibS: 0, vcpuS: 0, diskGbS: 0 }, DEFAULT_INSTANCE, keep, 0, T0).exhausted).toBe(false);
  });

  it("月の区切り（UTC）", () => {
    expect(monthOf(T0)).toBe("2026-11");
    expect(secondsToNextMonth(Date.UTC(2026, 11, 31, 23, 59, 59))).toBe(1);
    const end = Date.UTC(2026, 11, 1, 1);
    expect(splitMonths(Date.UTC(2026, 10, 30, 23), end)).toEqual([
      { month: "2026-11", ms: H },
      { month: "2026-12", ms: H },
    ]);
    expect(splitMonths(end, end)).toEqual([]);
  });
});

describe("Meter（実機ごとの計り）", () => {
  it("起こす → 数える → 閉じる。CPU は busy の間だけ", () => {
    const m = new Meter(memorySql());
    expect(m.isOpen()).toBe(false);
    m.open(T0, DEFAULT_INSTANCE);
    m.setBusy(T0 + 10_000, true);
    m.setBusy(T0 + 30_000, false);
    m.close(T0 + 60_000);
    const u = m.usage(T0 + H);
    expect(u.memGibS).toBeCloseTo(3 * 60, 9);
    expect(u.diskGbS).toBeCloseTo(12 * 60, 9);
    expect(u.vcpuS).toBeCloseTo(20, 9);
    expect(m.isOpen()).toBe(false);
    // 閉じた後は増えない
    m.accrue(T0 + 2 * H);
    expect(m.usage(T0 + 2 * H).memGibS).toBeCloseTo(180, 9);
  });

  it("動いている間の usage は、書かずに今まで足す", () => {
    const m = new Meter(memorySql());
    m.open(T0, DEFAULT_INSTANCE);
    expect(m.usage(T0 + 100_000).memGibS).toBeCloseTo(300, 9);
    expect(m.stored("2026-11").memGibS).toBe(0);
  });

  it("月をまたぐ区間は月ごとに分ける。usage は今月の分だけ", () => {
    const m = new Meter(memorySql());
    m.open(Date.UTC(2026, 10, 30, 23), DEFAULT_INSTANCE);
    m.close(Date.UTC(2026, 11, 1, 1));
    expect(m.stored("2026-11").memGibS).toBeCloseTo(3 * 3600, 9);
    expect(m.stored("2026-12").memGibS).toBeCloseTo(3 * 3600, 9);
    expect(m.usage(Date.UTC(2026, 11, 2)).memGibS).toBeCloseTo(3 * 3600, 9);
  });

  it("DO の再起動をまたぐ（SQLite の状態から作り直す）", () => {
    const sql = memorySql();
    const a = new Meter(sql);
    a.open(T0, DEFAULT_INSTANCE);
    a.setBusy(T0, true);
    a.accrue(T0 + 60_000);
    a.setLabel("cz-1");
    const b = new Meter(sql);
    expect(b.isOpen()).toBe(true);
    expect(b.spec()).toEqual(DEFAULT_INSTANCE);
    expect(b.usage(T0 + 60_000).vcpuS).toBeCloseTo(60, 9);
    expect(b.untilMs(300_000)).toBe(T0 + 60_000 + 300_000 + STOP_SLACK_MS);
    expect(b.snapshot(T0 + 60_000, 300_000).label).toBe("cz-1");
  });

  it("touch は 5 秒に 1 回だけ書く。最後の動きはメモリでは毎回進む", () => {
    let writes = 0;
    const base = memorySql();
    const sql: SqlExec = (q, ...b) => {
      if (/^\s*(INSERT|UPDATE)/.test(q)) writes++;
      return base(q, ...b);
    };
    const m = new Meter(sql);
    m.open(T0, DEFAULT_INSTANCE);
    const before = writes;
    for (let i = 1; i <= 40; i++) m.touch(T0 + i * 100);
    expect(writes).toBe(before);
    expect(m.activeMs()).toBe(T0 + 4000);
    m.touch(T0 + 5000);
    expect(writes).toBeGreaterThan(before);
  });

  it("ほかの実機の分: 聞いた値を、動いていれば untilMs まで延ばす。古い返事では上書きしない", () => {
    const m = new Meter(memorySql());
    expect(m.others(T0)).toEqual({ memGibS: 0, vcpuS: 0, diskGbS: 0 });
    const rate = rateOf(DEFAULT_INSTANCE, true);
    m.setOthers({ month: "2026-11", usage: { memGibS: 100, vcpuS: 0, diskGbS: 0 }, rate, atMs: T0, untilMs: T0 + 60_000 });
    expect(m.others(T0 + 30_000).memGibS).toBeCloseTo(100 + 90, 9);
    expect(m.others(T0 + H).memGibS).toBeCloseTo(100 + 180, 9);
    m.setOthers({ month: "2026-11", usage: { memGibS: 1, vcpuS: 0, diskGbS: 0 }, rate, atMs: T0 - 1, untilMs: T0 });
    expect(m.others(T0).memGibS).toBeCloseTo(100, 9);
    // 月が変われば 0（前の月の分は数えない）
    expect(m.others(Date.UTC(2026, 11, 1, 1)).memGibS).toBe(0);
  });
});

describe("BudgetBook（アカウントの台帳）", () => {
  const snap = (over: Partial<Snapshot> = {}): Snapshot => ({
    label: "cz-1",
    open: false,
    month: "2026-11",
    usage: { memGibS: 3600, vcpuS: 60, diskGbS: 7200 },
    rate: { memGib: 0, vcpu: 0, diskGb: 0 },
    atMs: T0,
    untilMs: T0,
    ...over,
  });

  it("実機ごとに最新の写しを 1 つ。古い報告は捨てる", () => {
    const b = new BudgetBook(memorySql());
    b.report("a", snap());
    b.report("a", snap({ atMs: T0 + 10, usage: { memGibS: 7200, vcpuS: 60, diskGbS: 0 } }));
    b.report("a", snap({ atMs: T0 + 5, usage: { memGibS: 1, vcpuS: 0, diskGbS: 0 } }));
    expect(b.all()).toHaveLength(1);
    expect(b.othersOf(null, T0 + 10).usage.memGibS).toBe(7200);
  });

  it("othersOf: 自分を除き、動いているものは今まで延ばし、これからの増え方を返す", () => {
    const b = new BudgetBook(memorySql());
    const rate = rateOf(DEFAULT_INSTANCE, true);
    b.report("a", snap({ open: true, rate, untilMs: T0 + 600_000 }));
    b.report("b", snap({ label: "cz-2" }));
    const v = b.othersOf("b", T0 + 60_000);
    expect(v.usage.memGibS).toBeCloseTo(3600 + 180, 9);
    expect(v.rate).toEqual(rate);
    expect(v.untilMs).toBe(T0 + 600_000);
    expect(extrapolate(v, T0 + 120_000).memGibS).toBeCloseTo(3600 + 360, 9);
    // untilMs を過ぎれば延ばさない
    const late = b.othersOf("b", T0 + H);
    expect(late.usage.memGibS).toBeCloseTo(3600 + 1800, 9);
    expect(late.rate).toEqual({ memGib: 0, vcpu: 0, diskGb: 0 });
  });

  it("summary: 今月の合計・超過・実機ごと", () => {
    const b = new BudgetBook(memorySql());
    b.report("a", snap({ usage: { memGibS: INCLUDED.memGibS + 400_000, vcpuS: 0, diskGbS: 0 } }));
    const s = b.summary(T0, 5) as Record<string, unknown>;
    expect(s).toMatchObject({
      month: "2026-11",
      resets_at: "2026-12-01T00:00:00.000Z",
      budget_usd: 5,
      exhausted: false,
      overage_usd: { memory: 1, cpu: 0, disk: 0, total: 1 },
      included: { memory_gib_h: 25, vcpu_min: 375, disk_gb_h: 200 },
    });
    expect(s.devices).toEqual([expect.objectContaining({ device: "cz-1", running: false })]);
    expect((b.summary(T0, 1) as { exhausted: boolean }).exhausted).toBe(true);
  });

  it("ledgerFetch: /report と /usage。形のおかしい報告は 400", async () => {
    const b = new BudgetBook(memorySql());
    const post = (body: unknown) =>
      ledgerFetch(b, 5, new Request("http://budget/report", { method: "POST", body: JSON.stringify(body) }), T0 + 1);
    const ok = await post({ key: "a", snapshot: snap() });
    expect(ok.status).toBe(200);
    expect(await ok.json()).toMatchObject({ month: "2026-11", usage: { memGibS: 0 } });
    const other = await post({ key: "b", snapshot: snap() });
    expect(((await other.json()) as { usage: { memGibS: number } }).usage.memGibS).toBe(3600);

    for (const bad of [{}, { key: "a" }, { key: "", snapshot: snap() }, { key: "a", snapshot: { ...snap(), usage: { memGibS: -1, vcpuS: 0, diskGbS: 0 } } }]) {
      expect((await post(bad)).status).toBe(400);
    }
    expect((await ledgerFetch(b, 5, new Request("http://budget/report", { method: "POST", body: "{oops" }), T0)).status).toBe(400);

    const u = await ledgerFetch(b, 5, new Request("http://budget/usage"), T0 + 1);
    expect(u.status).toBe(200);
    expect(u.headers.get("cache-control")).toBe("no-store");
    expect(((await u.json()) as { devices: unknown[] }).devices).toHaveLength(2);
    expect((await ledgerFetch(b, 5, new Request("http://budget/nope"), T0)).status).toBe(404);
  });

  it("parseSnapshot: 名前の形が違えば空にする", () => {
    expect(parseSnapshot({ ...snap(), label: "Bad Name!" })?.label).toBe("");
    expect(parseSnapshot({ ...snap(), month: "2026-1" })).toBeNull();
    expect(parseSnapshot("x")).toBeNull();
  });
});
