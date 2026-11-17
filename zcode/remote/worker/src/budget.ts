// 費用の守り（docs/stage3-design.md §6.6）。コンテナの大きさ・月の使用量の計り方・超過の上限。
//
// 決めたこと（2026-11-17 ユーザー）: 安い大きさ（既定は 1 vCPU・3GiB・12GB の custom）にし、
// **月の超過は 5 ドルまで**（Workers Paid の 5 ドルと合わせて月 10 ドル前後）。
//
// 形:
//
//   実機ごとの DO（BuildSession）── 自分の分を計る（SQLite の Meter）
//        │  報告（自分の今月の合計の写し。差分ではないので、届かなくても次の報告で追いつく）
//        ▼
//   アカウントに 1 つの DO（BudgetLedger、名前 "budget"）── 全部の実機の最新の写しを持つ（BudgetBook）
//        │  返事: ほかの実機の分の合計（動いているものは、今の時刻まで延ばした見込み）
//        ▼
//   実機ごとの DO が「自分の分 + ほかの分」で上限を判断する
//
// **上限はアカウント全体で 1 つ**（実機ごとではない）。実機ごとにすると 2 台で 10 ドルになり、
// 決めた上限の意味が変わるため。判断そのものは実機ごとの DO が手元の値でする（台帳の DO に
// 毎回聞かない）: ジョブの受け付けの途中で外へ聞くと、その間に同じ接続の次のメッセージ（cancel
// など）が割り込み、順番が崩れるため。台帳の DO への報告は、コンテナを起こす前（返事を待つ）、
// ジョブの受け付け・終わり（待たない）、alarm（30 秒ごと）、止めたとき。台帳に届かないときは、
// 前に聞いたほかの実機の分（と、そのとき動いていたものの延長）で判断を続け、ログに残す。
// 自分の分はいつも自分の SQLite にあるので、台帳が落ちても自分の分を数え損ねることはない。
//
// 計るもの（公式の料金表。2026-11-17 に確認）:
//   - メモリ: 割り当てた大きさ × コンテナが動いている時間（起こしてから止まるまで）
//   - ディスク: 割り当てた大きさ × 同じ時間
//   - CPU: 実際に使った分だけ課金されるが、DO からは見えない。**ジョブが走っている時間 × vCPU**
//     を上限の見積もりとして数える（ビルドは CPU を使い切ることが多いので、大きく外れない）。
//     ジョブの有無は 30 秒ごとの alarm と受付の done で見るので、区間の途中で始まった・終わった
//     ジョブは区間の全部を「走っていた」として数える（多めに数える側）
//   - CPU（LSP）: rust-analyzer はジョブの外で動く（索引・問い合わせ）。受付が rust-analyzer の CPU の
//     通算（ms）を lsp_result / lsp_state と /v1/status に載せるので、増えた分をそのまま足す
//     （addLspCpu）。ジョブの間の分は「ジョブの時間 × vCPU」と重なるが、多めに数える側なので足す
//
// 止まった時刻が分からないとき（DO が作り直された・眠っていた）は、最後に記録した時刻
// （last・最後の動き）+ KEEP_WARM_S + 60 秒 と「今」の早い方で閉じる。コンテナは最後の動きから
// KEEP_WARM_S で止まる（DO が alarm で自分で止め、setInactivityTimeout も同じ時間で掛けてある）
// ので、これより後まで動いていたことはない = 少なく数えない。

import { json, type Env } from "./index";
import type { SqlExec } from "./session";

// ───────────────────────── コンテナの大きさ ─────────────────────────

export interface InstanceSpec {
  /** 名前つきの大きさ（lite・standard-1〜4）。custom なら null。 */
  name: string | null;
  vcpu: number;
  memoryMib: number;
  diskMb: number;
}

/**
 * 名前つきの大きさ（公式の Limits の表）。`durable_object` 方式の start() が受けるのは
 * lite・standard-1〜4 だけ（basic と古い dev / standard は受けない。Scheduling Policy の文書）。
 */
export const NAMED_INSTANCES: Readonly<Record<string, InstanceSpec>> = {
  lite: { name: "lite", vcpu: 1 / 16, memoryMib: 256, diskMb: 2000 },
  "standard-1": { name: "standard-1", vcpu: 0.5, memoryMib: 4096, diskMb: 8000 },
  "standard-2": { name: "standard-2", vcpu: 1, memoryMib: 6144, diskMb: 12000 },
  "standard-3": { name: "standard-3", vcpu: 2, memoryMib: 8192, diskMb: 16000 },
  "standard-4": { name: "standard-4", vcpu: 4, memoryMib: 12288, diskMb: 20000 },
};

/**
 * 既定の大きさ: 1 vCPU・3GiB・12GB の custom。メモリは custom の下限（1 vCPU あたり 3GiB）、
 * ディスクはイメージ（展開後 1.5GB）・cargo の target・crate と npm のキャッシュが入る量。
 */
export const DEFAULT_INSTANCE: InstanceSpec = { name: null, vcpu: 1, memoryMib: 3072, diskMb: 12000 };

/** custom の制限（公式の Limits → Custom Instance Types と Scheduling Policy、2026-11-17 に確認）。 */
export const CUSTOM_LIMITS = {
  minVcpu: 1 / 16,
  maxVcpu: 4,
  minMemoryMib: 256,
  maxMemoryMib: 12288,
  /** 1 vCPU あたりのメモリの下限。 */
  memoryMibPerVcpu: 3072,
  minDiskMb: 2000,
  maxDiskMb: 20000,
} as const;

/** custom の大きさが制限に収まっていれば null、収まらなければ理由。 */
export function customProblem(s: { vcpu: number; memoryMib: number; diskMb: number }): string | null {
  const L = CUSTOM_LIMITS;
  if (!Number.isFinite(s.vcpu) || s.vcpu < L.minVcpu || s.vcpu > L.maxVcpu) return `vcpu ${s.vcpu} は ${L.minVcpu}〜${L.maxVcpu}`;
  if (!Number.isInteger(s.memoryMib) || s.memoryMib < L.minMemoryMib || s.memoryMib > L.maxMemoryMib) {
    return `memoryMib ${s.memoryMib} は ${L.minMemoryMib}〜${L.maxMemoryMib} の整数`;
  }
  if (s.memoryMib < L.memoryMibPerVcpu * s.vcpu) return `memoryMib ${s.memoryMib} は 1 vCPU あたり 3GiB 以上（${L.memoryMibPerVcpu * s.vcpu} 以上）`;
  if (!Number.isInteger(s.diskMb) || s.diskMb < L.minDiskMb || s.diskMb > L.maxDiskMb) return `diskMb ${s.diskMb} は ${L.minDiskMb}〜${L.maxDiskMb} の整数`;
  return null;
}

export type InstanceVars = Partial<Pick<Env, "INSTANCE_TYPE" | "INSTANCE_VCPU" | "INSTANCE_MEMORY_MIB" | "INSTANCE_DISK_MB">>;

/**
 * vars からコンテナの大きさを決める。INSTANCE_TYPE が空でなければ名前つき、空なら
 * INSTANCE_VCPU・INSTANCE_MEMORY_MIB・INSTANCE_DISK_MB の custom。おかしな値は
 * 既定（DEFAULT_INSTANCE）に戻し、ログに 1 行残す。
 */
export function instanceOf(env: InstanceVars, log: (m: string) => void = (m) => console.error(m)): InstanceSpec {
  const type = (env.INSTANCE_TYPE ?? "").trim();
  if (type !== "") {
    const named = NAMED_INSTANCES[type];
    if (named !== undefined) return named;
    log(`budget: INSTANCE_TYPE "${type}" は使えない（lite / standard-1〜4）。既定の custom にする`);
    return DEFAULT_INSTANCE;
  }
  const num = (s: string | undefined, def: number): number => {
    if (s === undefined || s.trim() === "") return def;
    return /^[0-9]+(\.[0-9]+)?$/.test(s.trim()) ? Number(s.trim()) : NaN;
  };
  const spec: InstanceSpec = {
    name: null,
    vcpu: num(env.INSTANCE_VCPU, DEFAULT_INSTANCE.vcpu),
    memoryMib: num(env.INSTANCE_MEMORY_MIB, DEFAULT_INSTANCE.memoryMib),
    diskMb: num(env.INSTANCE_DISK_MB, DEFAULT_INSTANCE.diskMb),
  };
  const problem = customProblem(spec);
  if (problem !== null) {
    log(`budget: custom の大きさがおかしい（${problem}）。既定の 1 vCPU・3GiB・12GB にする`);
    return DEFAULT_INSTANCE;
  }
  return spec;
}

/** ctx.container.start() の `instance` に渡す形。 */
export function startInstance(spec: InstanceSpec): NonNullable<ContainerStartupOptions["instance"]> {
  if (spec.name !== null) return spec.name as "lite" | "standard-1" | "standard-2" | "standard-3" | "standard-4";
  return { vcpu: spec.vcpu, memoryMib: spec.memoryMib, diskMb: spec.diskMb };
}

// ───────────────────────── 料金 ─────────────────────────

/** 単価（ドル）。メモリは GiB 秒、CPU は vCPU 秒（使った分）、ディスクは GB 秒。 */
export const PRICE = { memGibS: 0.0000025, vcpuS: 0.00002, diskGbS: 0.00000007 } as const;
/** 月に含まれる分（Workers Paid）: メモリ 25GiB 時間・CPU 375 vCPU 分・ディスク 200GB 時間。 */
export const INCLUDED = { memGibS: 25 * 3600, vcpuS: 375 * 60, diskGbS: 200 * 3600 } as const;
/** 超過の上限の既定（ドル）。 */
export const DEFAULT_BUDGET_USD = 5;
/** 1 つのジョブの時間の上限（受付 zc-buildd の 15 分）。判断で「次の 1 ジョブ」の分を見込む。 */
export const JOB_RESERVE_MS = 15 * 60_000;
/** 止まった時刻が分からないときの、keep-warm に足すゆとり（alarm 2 回分）。 */
export const STOP_SLACK_MS = 60_000;
/** touch（実機のメッセージごと）で SQLite に書くのは、前に書いてからこれだけたったときだけ。 */
export const TOUCH_PERSIST_MS = 5_000;

export interface Usage {
  memGibS: number;
  vcpuS: number;
  diskGbS: number;
}
export const ZERO_USAGE: Usage = Object.freeze({ memGibS: 0, vcpuS: 0, diskGbS: 0 });

/** 1 秒あたりの使用量。 */
export interface Rate {
  memGib: number;
  vcpu: number;
  diskGb: number;
}
export const ZERO_RATE: Rate = Object.freeze({ memGib: 0, vcpu: 0, diskGb: 0 });

export function addUsage(a: Usage, b: Usage): Usage {
  return { memGibS: a.memGibS + b.memGibS, vcpuS: a.vcpuS + b.vcpuS, diskGbS: a.diskGbS + b.diskGbS };
}

export function addRate(a: Rate, b: Rate): Rate {
  return { memGib: a.memGib + b.memGib, vcpu: a.vcpu + b.vcpu, diskGb: a.diskGb + b.diskGb };
}

/** 大きさから 1 秒あたりの量。CPU は `busy`（ジョブが走っている）のときだけ。 */
export function rateOf(spec: InstanceSpec, busy: boolean): Rate {
  return { memGib: spec.memoryMib / 1024, vcpu: busy ? spec.vcpu : 0, diskGb: spec.diskMb / 1000 };
}

export function usageFor(rate: Rate, ms: number): Usage {
  const s = Math.max(0, ms) / 1000;
  return { memGibS: rate.memGib * s, vcpuS: rate.vcpu * s, diskGbS: rate.diskGb * s };
}

export interface Overage {
  memoryUsd: number;
  cpuUsd: number;
  diskUsd: number;
  totalUsd: number;
}

/** 含まれる分を超えた分の料金（ドル）。 */
export function overageOf(u: Usage): Overage {
  const memoryUsd = Math.max(0, u.memGibS - INCLUDED.memGibS) * PRICE.memGibS;
  const cpuUsd = Math.max(0, u.vcpuS - INCLUDED.vcpuS) * PRICE.vcpuS;
  const diskUsd = Math.max(0, u.diskGbS - INCLUDED.diskGbS) * PRICE.diskGbS;
  return { memoryUsd, cpuUsd, diskUsd, totalUsd: memoryUsd + cpuUsd + diskUsd };
}

/** vars の BUDGET_OVERAGE_USD を読む（0〜1000 のドル）。おかしな値は既定の 5 ドル。 */
export function budgetOf(value: string | undefined, log: (m: string) => void = (m) => console.error(m)): number {
  if (value === undefined || value.trim() === "") return DEFAULT_BUDGET_USD;
  const v = value.trim();
  const n = /^[0-9]+(\.[0-9]+)?$/.test(v) ? Number(v) : NaN;
  if (!Number.isFinite(n) || n > 1000) {
    log(`budget: BUDGET_OVERAGE_USD "${v}" は読めない。${DEFAULT_BUDGET_USD} ドルにする`);
    return DEFAULT_BUDGET_USD;
  }
  return n;
}

// ───────────────────────── 月（UTC） ─────────────────────────

/** UTC の暦月（YYYY-MM）。 */
export function monthOf(ms: number): string {
  return new Date(ms).toISOString().slice(0, 7);
}

export function monthStartMs(ms: number): number {
  const d = new Date(ms);
  return Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), 1);
}

export function nextMonthMs(ms: number): number {
  const d = new Date(ms);
  return Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + 1, 1);
}

/** 次の月の 1 日 0 時（UTC）までの秒。最低 1。 */
export function secondsToNextMonth(ms: number): number {
  return Math.max(1, Math.ceil((nextMonthMs(ms) - ms) / 1000));
}

/** [a, b) を暦月ごとに分ける。 */
export function splitMonths(a: number, b: number): { month: string; ms: number }[] {
  const out: { month: string; ms: number }[] = [];
  let t = a;
  while (t < b) {
    const end = Math.min(b, nextMonthMs(t));
    out.push({ month: monthOf(t), ms: end - t });
    t = end;
  }
  return out;
}

// ───────────────────────── 見込み（写し） ─────────────────────────

/**
 * ある時点の使用量の写し。`month` の合計が `atMs` までの分で、`untilMs` までは `rate` で
 * 増え続けているかもしれない（動いているコンテナ）。止まっていれば rate は 0。
 */
export interface UsageView {
  month: string;
  usage: Usage;
  rate: Rate;
  atMs: number;
  untilMs: number;
}

/** 実機ごとの DO が台帳へ送る、自分の写し。 */
export interface Snapshot extends UsageView {
  /** 人が読む名前（device_id）。 */
  label: string;
  open: boolean;
}

/** 写しを `now` の今月の分に直す（動いているものは min(now, untilMs) まで延ばす）。 */
export function extrapolate(v: UsageView, now: number): Usage {
  let u = v.month === monthOf(now) ? v.usage : ZERO_USAGE;
  const from = Math.max(v.atMs, monthStartMs(now));
  const to = Math.min(now, v.untilMs);
  if (to > from) u = addUsage(u, usageFor(v.rate, to - from));
  return u;
}

// ───────────────────────── 判断 ─────────────────────────

export interface Guard {
  /** 新しいジョブ・コンテナの起動を断る。 */
  exhausted: boolean;
  /** 今の超過の見込み（ドル）。 */
  overageUsd: number;
  budgetUsd: number;
  /** 次の月まで（error {limit} の retry_after_s）。 */
  retryAfterS: number;
  /** 実機に見せる文。 */
  message: string;
}

/**
 * 上限を超えるか。「今の合計 + 次の 1 ジョブ（15 分）と、その後の keep-warm」の超過が
 * 上限を超えるなら断る（走り出したジョブは止めないので、その分を先に見込む）。
 */
export function guardOf(total: Usage, spec: InstanceSpec, keepWarmMs: number, budgetUsd: number, now: number): Guard {
  const reserve = usageFor(rateOf(spec, true), JOB_RESERVE_MS + keepWarmMs);
  const overageUsd = overageOf(total).totalUsd;
  const projected = overageOf(addUsage(total, reserve)).totalUsd;
  const reset = new Date(nextMonthMs(now)).toISOString().slice(0, 10);
  return {
    exhausted: projected > budgetUsd,
    overageUsd,
    budgetUsd,
    retryAfterS: secondsToNextMonth(now),
    message: `今月のビルド係の費用が上限に達した（超過 ${overageUsd.toFixed(2)} ドル・上限 ${budgetUsd.toFixed(2)} ドル）。${reset} 0 時（UTC）に戻る`,
  };
}

// ───────────────────────── 実機ごとの DO の計り（Meter） ─────────────────────────

interface MeterState {
  open: boolean;
  sinceMs: number;
  /** ここまで数えた。 */
  lastMs: number;
  /** 最後の動き（実機のメッセージ・接続・ジョブ）。keep-warm はここから数える。 */
  activeMs: number;
  /** 起こしたときの大きさ（あとで vars を変えても、動いているコンテナはこの大きさ）。 */
  spec: InstanceSpec | null;
  /** ジョブが走っている（CPU を数える）。 */
  busy: boolean;
  /** 今のコンテナで、受付から聞いた rust-analyzer の CPU の通算（ms）。コンテナを起こすと 0。 */
  lspCpuMs: number;
  label: string;
  /** 台帳から聞いた、ほかの実機の分。 */
  others: UsageView | null;
}

const INITIAL: MeterState = { open: false, sinceMs: 0, lastMs: 0, activeMs: 0, spec: null, busy: false, lspCpuMs: 0, label: "", others: null };

/**
 * コンテナが動いている区間を数え、暦月ごとの合計を SQLite に持つ。DO の再起動をまたぐ
 * （状態はすべて SQLite。メモリにあるのは、その写しと、5 秒以内の最後の動きだけ）。
 */
export class Meter {
  private st: MeterState;

  constructor(private readonly sql: SqlExec) {
    sql(
      `CREATE TABLE IF NOT EXISTS usage_month (
         month TEXT PRIMARY KEY,
         mem_gib_s REAL NOT NULL,
         vcpu_s REAL NOT NULL,
         disk_gb_s REAL NOT NULL
       )`,
    );
    sql(`CREATE TABLE IF NOT EXISTS meter (id INTEGER PRIMARY KEY, data TEXT NOT NULL)`);
    const row = sql(`SELECT data FROM meter WHERE id = 1`)[0];
    let st: MeterState = { ...INITIAL };
    if (row !== undefined) {
      try {
        st = { ...INITIAL, ...(JSON.parse(String(row.data)) as Partial<MeterState>) };
      } catch {
        console.error("budget: meter state is broken; starting over");
      }
    }
    this.st = st;
  }

  isOpen(): boolean {
    return this.st.open;
  }

  /** 動いているコンテナの大きさ（止まっていれば null）。 */
  spec(): InstanceSpec | null {
    return this.st.open ? this.st.spec : null;
  }

  activeMs(): number {
    return this.st.activeMs;
  }

  /** 止まった時刻の上限（これより後まで動いていたことはない）。 */
  untilMs(keepWarmMs: number): number {
    return Math.max(this.st.lastMs, this.st.activeMs) + keepWarmMs + STOP_SLACK_MS;
  }

  /** コンテナを起こした（または、動いているのを見つけた）。 */
  open(now: number, spec: InstanceSpec): void {
    if (this.st.open) {
      this.accrue(now);
      return;
    }
    this.st = { ...this.st, open: true, sinceMs: now, lastMs: now, activeMs: now, spec, busy: false, lspCpuMs: 0 };
    this.save();
  }

  /**
   * 受付から聞いた rust-analyzer の CPU の通算（ms）。前に聞いた値から増えた分を、今月の CPU に足す。
   * 減っていたら（受付が起動し直した）、その値をそのまま 0 からの分として足す。止まっていれば数えない。
   */
  addLspCpu(now: number, totalMs: number): void {
    if (!this.st.open || !Number.isFinite(totalMs) || totalMs < 0) return;
    const delta = totalMs >= this.st.lspCpuMs ? totalMs - this.st.lspCpuMs : totalMs;
    this.st.lspCpuMs = totalMs;
    if (delta > 0) this.add(monthOf(now), { memGibS: 0, vcpuS: delta / 1000, diskGbS: 0 });
    this.save();
  }

  /** `now` まで数える。 */
  accrue(now: number): void {
    if (!this.st.open || this.st.spec === null) return;
    if (now > this.st.lastMs) {
      const rate = rateOf(this.st.spec, this.st.busy);
      for (const seg of splitMonths(this.st.lastMs, now)) this.add(seg.month, usageFor(rate, seg.ms));
      this.st.lastMs = now;
    }
    this.save();
  }

  /** 動きがあった（keep-warm を数え直す）。SQLite に書くのは 5 秒に 1 回まで。 */
  touch(now: number): void {
    if (!this.st.open) return;
    if (now > this.st.activeMs) this.st.activeMs = now;
    if (now - this.st.lastMs >= TOUCH_PERSIST_MS) this.accrue(now);
  }

  /**
   * ジョブが走っているか（CPU を数えるか）を変える。ここまでは前の状態で数える。
   * 走っている間・走り終わった時刻は「動き」とみなす（keep-warm はジョブが終わってから数える）。
   */
  setBusy(now: number, busy: boolean): void {
    if (!this.st.open) return;
    this.accrue(now);
    if ((busy || this.st.busy) && now > this.st.activeMs) this.st.activeMs = now;
    this.st.busy = busy;
    this.save();
  }

  /** コンテナが止まった（止めた）。`stopAt` まで数えて閉じる。 */
  close(stopAt: number): void {
    if (!this.st.open) return;
    this.accrue(Math.max(this.st.lastMs, stopAt));
    this.st = { ...this.st, open: false, busy: false };
    this.save();
  }

  /** 今月（`now` の月）の自分の分。動いていれば `now` までの分も足す（書かない）。 */
  usage(now: number): Usage {
    const month = monthOf(now);
    let u = this.stored(month);
    if (this.st.open && this.st.spec !== null && now > this.st.lastMs) {
      const rate = rateOf(this.st.spec, this.st.busy);
      for (const seg of splitMonths(this.st.lastMs, now)) if (seg.month === month) u = addUsage(u, usageFor(rate, seg.ms));
    }
    return u;
  }

  /** ある月の、SQLite にある分。 */
  stored(month: string): Usage {
    const r = this.sql(`SELECT mem_gib_s, vcpu_s, disk_gb_s FROM usage_month WHERE month = ?`, month)[0];
    if (r === undefined) return { ...ZERO_USAGE };
    return { memGibS: Number(r.mem_gib_s), vcpuS: Number(r.vcpu_s), diskGbS: Number(r.disk_gb_s) };
  }

  /** 台帳へ送る写し。 */
  snapshot(now: number, keepWarmMs: number): Snapshot {
    const open = this.st.open && this.st.spec !== null;
    return {
      label: this.st.label,
      open,
      month: monthOf(now),
      usage: this.usage(now),
      // 見込みは多めに: これから先はジョブが走るかもしれないので CPU も入れる
      rate: open ? rateOf(this.st.spec!, true) : { ...ZERO_RATE },
      atMs: now,
      untilMs: open ? this.untilMs(keepWarmMs) : now,
    };
  }

  /** ほかの実機の分（前に台帳から聞いた値を `now` まで延ばす）。聞いたことが無ければ 0。 */
  others(now: number): Usage {
    return this.st.others === null ? { ...ZERO_USAGE } : extrapolate(this.st.others, now);
  }

  setOthers(v: UsageView): void {
    if (this.st.others !== null && v.atMs < this.st.others.atMs) return;
    this.st.others = v;
    this.save();
  }

  setLabel(label: string): void {
    if (this.st.label === label) return;
    this.st.label = label;
    this.save();
  }

  /** 13 か月より古い月を消す。 */
  prune(now: number): void {
    this.sql(`DELETE FROM usage_month WHERE month < ?`, monthOf(now - 400 * 86_400_000));
  }

  private add(month: string, u: Usage): void {
    this.sql(
      `INSERT INTO usage_month (month, mem_gib_s, vcpu_s, disk_gb_s) VALUES (?, ?, ?, ?)
       ON CONFLICT(month) DO UPDATE SET
         mem_gib_s = mem_gib_s + excluded.mem_gib_s,
         vcpu_s = vcpu_s + excluded.vcpu_s,
         disk_gb_s = disk_gb_s + excluded.disk_gb_s`,
      month,
      u.memGibS,
      u.vcpuS,
      u.diskGbS,
    );
  }

  private save(): void {
    this.sql(`INSERT OR REPLACE INTO meter (id, data) VALUES (1, ?)`, JSON.stringify(this.st));
  }
}

// ───────────────────────── アカウントの台帳（BudgetBook / BudgetLedger） ─────────────────────────

/** 台帳への報告の口（実機ごとの DO から）。届かなければ null（投げない）。 */
export interface Accounts {
  report(snap: Snapshot): Promise<UsageView | null>;
}

/** 台帳の中身（純粋な部分）。実機ごとに、最新の写しを 1 行持つ。 */
export class BudgetBook {
  constructor(private readonly sql: SqlExec) {
    sql(`CREATE TABLE IF NOT EXISTS reports (key TEXT PRIMARY KEY, at_ms INTEGER NOT NULL, data TEXT NOT NULL)`);
  }

  /** 写しを入れる（届く順が前後しても、新しい方を残す）。 */
  report(key: string, snap: Snapshot): void {
    const row = this.sql(`SELECT at_ms FROM reports WHERE key = ?`, key)[0];
    if (row !== undefined && Number(row.at_ms) > snap.atMs) return;
    this.sql(`INSERT OR REPLACE INTO reports (key, at_ms, data) VALUES (?, ?, ?)`, key, snap.atMs, JSON.stringify(snap));
  }

  /** 全部の写し。 */
  all(): { key: string; snap: Snapshot }[] {
    const out: { key: string; snap: Snapshot }[] = [];
    for (const r of this.sql(`SELECT key, data FROM reports`)) {
      try {
        out.push({ key: String(r.key), snap: JSON.parse(String(r.data)) as Snapshot });
      } catch {
        // 壊れた行は無視する（次の報告で上書きされる）
      }
    }
    return out;
  }

  /** `key` 以外の実機の分を `now` の今月に直した合計と、これからの増え方。 */
  othersOf(key: string | null, now: number): UsageView {
    let usage: Usage = { ...ZERO_USAGE };
    let rate: Rate = { ...ZERO_RATE };
    let untilMs = now;
    for (const r of this.all()) {
      if (r.key === key) continue;
      usage = addUsage(usage, extrapolate(r.snap, now));
      if (r.snap.untilMs > now) {
        rate = addRate(rate, r.snap.rate);
        untilMs = Math.max(untilMs, r.snap.untilMs);
      }
    }
    return { month: monthOf(now), usage, rate, atMs: now, untilMs };
  }

  /** /v1/usage の返事。 */
  summary(now: number, budgetUsd: number): Record<string, unknown> {
    const hours = (u: Usage) => ({
      memory_gib_h: round(u.memGibS / 3600, 3),
      vcpu_min_est: round(u.vcpuS / 60, 2),
      disk_gb_h: round(u.diskGbS / 3600, 3),
    });
    const devices = this.all().map((r) => ({
      device: r.snap.label || r.key.slice(0, 8),
      running: r.snap.untilMs > now,
      ...hours(extrapolate(r.snap, now)),
      reported_at: new Date(r.snap.atMs).toISOString(),
    }));
    const total = this.othersOf(null, now).usage;
    const o = overageOf(total);
    return {
      month: monthOf(now),
      resets_at: new Date(nextMonthMs(now)).toISOString(),
      budget_usd: budgetUsd,
      exhausted: o.totalUsd >= budgetUsd,
      usage: hours(total),
      included: { memory_gib_h: INCLUDED.memGibS / 3600, vcpu_min: INCLUDED.vcpuS / 60, disk_gb_h: INCLUDED.diskGbS / 3600 },
      overage_usd: { memory: round(o.memoryUsd, 4), cpu: round(o.cpuUsd, 4), disk: round(o.diskUsd, 4), total: round(o.totalUsd, 4) },
      devices,
      note: "CPU は「ジョブが走っていた時間 × vCPU」と rust-analyzer の CPU の合計の見積もり（実際の使用量はこれ以下）。料金表からの計算で、請求額ではない",
    };
  }
}

function round(x: number, digits: number): number {
  const f = 10 ** digits;
  return Math.round(x * f) / f;
}

const finite = (x: unknown): x is number => typeof x === "number" && Number.isFinite(x) && x >= 0;

function parseUsage(v: unknown): Usage | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  return finite(o.memGibS) && finite(o.vcpuS) && finite(o.diskGbS) ? { memGibS: o.memGibS, vcpuS: o.vcpuS, diskGbS: o.diskGbS } : null;
}

function parseRate(v: unknown): Rate | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  return finite(o.memGib) && finite(o.vcpu) && finite(o.diskGb) ? { memGib: o.memGib, vcpu: o.vcpu, diskGb: o.diskGb } : null;
}

/** 写し（報告・返事の JSON）を、形を確かめて読む。 */
export function parseView(v: unknown): UsageView | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  const usage = parseUsage(o.usage);
  const rate = parseRate(o.rate);
  if (typeof o.month !== "string" || !/^[0-9]{4}-[0-9]{2}$/.test(o.month) || usage === null || rate === null) return null;
  if (!finite(o.atMs) || !finite(o.untilMs)) return null;
  return { month: o.month, usage, rate, atMs: o.atMs, untilMs: o.untilMs };
}

export function parseSnapshot(v: unknown): Snapshot | null {
  const view = parseView(v);
  if (view === null) return null;
  const o = v as Record<string, unknown>;
  const label = typeof o.label === "string" && /^[a-z0-9-]{0,32}$/.test(o.label) ? o.label : "";
  return { ...view, label, open: o.open === true };
}

/** 台帳の DO の名前（アカウントに 1 つ）。 */
export const LEDGER_NAME = "budget";

/** 実機ごとの DO から台帳の DO への口。 */
export class LedgerClient implements Accounts {
  constructor(
    private readonly ns: DurableObjectNamespace,
    private readonly key: string,
  ) {}

  async report(snap: Snapshot): Promise<UsageView | null> {
    try {
      const stub = this.ns.get(this.ns.idFromName(LEDGER_NAME));
      const res = await stub.fetch("http://budget/report", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ key: this.key, snapshot: snap }),
        signal: AbortSignal.timeout(3000),
      });
      if (res.status !== 200) {
        await res.body?.cancel();
        console.error("budget: ledger answered", res.status);
        return null;
      }
      return parseView(await res.json());
    } catch (e) {
      console.error("budget: ledger unreachable", String(e));
      return null;
    }
  }
}

/** アカウントに 1 つの台帳（Durable Object「BudgetLedger」、名前 "budget"）。 */
export class BudgetLedger implements DurableObject {
  private readonly book: BudgetBook;
  private readonly budgetUsd: number;

  constructor(ctx: DurableObjectState, env: Env) {
    const sql = ctx.storage.sql;
    this.book = new BudgetBook((q, ...b) => sql.exec(q, ...b).toArray());
    this.budgetUsd = budgetOf(env.BUDGET_OVERAGE_USD);
  }

  async fetch(request: Request): Promise<Response> {
    return ledgerFetch(this.book, this.budgetUsd, request, Date.now());
  }
}

/** 台帳の DO の fetch の中身（試験から直接呼ぶ）。 */
export async function ledgerFetch(book: BudgetBook, budgetUsd: number, request: Request, now: number): Promise<Response> {
  const url = new URL(request.url);
  if (url.pathname === "/report" && request.method === "POST") {
    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return json(400, "bad request");
    }
    const o = (typeof body === "object" && body !== null ? body : {}) as Record<string, unknown>;
    const snap = parseSnapshot(o.snapshot);
    if (typeof o.key !== "string" || o.key.length === 0 || o.key.length > 128 || snap === null) return json(400, "bad request");
    book.report(o.key, snap);
    return jsonBody(book.othersOf(o.key, now));
  }
  if (url.pathname === "/usage" && request.method === "GET") return jsonBody(book.summary(now, budgetUsd));
  return json(404, "not found");
}

export function jsonBody(v: unknown): Response {
  return new Response(JSON.stringify(v), { status: 200, headers: { "content-type": "application/json", "cache-control": "no-store" } });
}
