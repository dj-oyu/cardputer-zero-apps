// Durable Object「BuildSession」= ジョブの持ち主（docs/stage3-design.md §6.3〜§6.5）。課題 C2。
//
// 実機 1 台につき 1 つ（名前 = device_id）。コンテナも 1 つ。
//
//   実機 ◀─WebSocket（ハイバネーション）─▶ DO ◀─WebSocket─▶ コンテナの zc-buildd（:7411）
//
// DO がやること:
//   1. 実機の WebSocket を受ける（acceptWebSocket）。同時に 4 本まで（超えたら 429）
//   2. コンテナを起こす（無ければ）。zc-buildd の /healthz が 200 を返すまで待つ（最大 30 秒）。
//      起こせなければ error {unavailable} を返して閉じる
//   3. 実機の接続 1 本につき、コンテナへ WebSocket を 1 本張り、**中身を変えずに中継する**
//      （テキストもバイナリも）。約束（attach・cancel・ログの保持）を知っているのは zc-buildd で、
//      DO は中継と見張りだけ（同じ実装を PC 直結でも使い、Cloudflare なしで試せるようにするため）
//   4. 通り過ぎるテキストを見て**ジョブの台帳**（SQLite）を付ける:
//        実機 → job      … 1 日の上限を数える。超えていたら中継せず error {limit} を返す
//        受付 → started  … running にする（始まった時刻を付け直す）
//        受付 → done     … done にする（status・時刻を残す）
//      バイナリは見ない。JSON として読めないテキストは中継せず error {protocol} で閉じる
//   5. **接続が切れてもジョブは続ける**: 実機の WebSocket が切れたら、その接続の上流だけ閉じる
//      （zc-buildd はジョブを止めない）。running のジョブがある間は、alarm（30 秒ごと）で
//      zc-buildd の /v1/status を聞き、setInactivityTimeout を掛け直す。台帳で running なのに
//      /v1/status に無いジョブは lost にする
//   6. lost のジョブへの attach には、zc-buildd へ流さずに done {status:"lost"} を返す
//      （実機は同じ依頼を出し直す。job id が同じなので、同じジョブとして数える）
//   7. 最後の接続が切れ、running が無くなってから KEEP_WARM_S 秒でコンテナが止まる
//      （alarm をやめ、setInactivityTimeout に任せる）
//
// **コンテナが止まっているときの接続（「冷えた接続」）**: 6 の「コンテナを起こさずに返す」ために、
// コンテナが止まっている間は、上流が要るメッセージが来るまでコンテナを起こさない。
//   - hello（v = 1）には DO が自分で hello を返す（実機が hello の返事を待ってから attach を
//     送る作りでも止まらないように）。あとで上流を張ったら、覚えておいた hello を最初に送り、
//     上流の hello の返事は実機へ流さない（実機はもう受け取っている）
//   - attach: 台帳で lost なら done {lost}、それ以外は error {unknown-job}（止まった受付は
//     何も覚えていないので、起こしても同じ返事になる）
//   - cancel: error {unknown-job}（同じ理由）
//   - それ以外（manifest・job・バイナリ…）: コンテナを起こして上流を張り、順に流す
// 中身を作って返すのはこの場面だけ。コンテナが動いている間は、何も足さず何も引かない。
//
// **DO が眠る・作り直されるとき**（ハイバネーション・deploy・追い出し）: 公式の文書では
// 「外へ張った WebSocket はハイバネーションしない」「開いている外向きの WebSocket は、DO の
// 追い出しを最大 15 分止める」。つまり、上流を張っている間は DO は起きたまま（費用は接続が
// ある間だけ。実機は最後のジョブの 75 秒後に閉じる）。それでも DO が作り直されたら、メモリの
// 中の上流は無くなる。そのとき残っている実機の WebSocket は、**閉じる**（コード 4012）。
// 実機は「切れた」ときの決まり（§5.5）どおり、つなぎ直して hello → attach する。ジョブは
// zc-buildd が持っているので失われない。上流を張り直して続ける方式にしなかった理由:
// zc-buildd の接続ごとの状態（hello・ready を受けた木）は張り直すと消えるので、DO が約束の
// 中身を覚えて再生することになり、「DO は中継だけ」が崩れる。
//
// 確かめたこと（公式の文書、2026-11-17。実物では未確認）:
//   - ctx.container の口（start・running・images・getTcpPort・setInactivityTimeout）と、
//     durable_object 方式の start({ image, instance, enableInternet })
//     https://developers.cloudflare.com/durable-objects/api/container/
//     https://developers.cloudflare.com/containers/configuration/scheduling-policy/
//   - getTcpPort(port).fetch() に Upgrade つきの依頼を渡して WebSocket を張る例がある
//     https://developers.cloudflare.com/containers/examples/websocket/
//   - setInactivityTimeout は DO の再起動で消える（constructor で掛け直す）。数えるのは
//     「DO が動かなくなってから」。最大 6 時間
//   - 外向きの WebSocket とハイバネーション、serializeAttachment、作り直しでメモリが消えること
//     https://developers.cloudflare.com/durable-objects/best-practices/websockets/
//   - 受け取れる WebSocket の 1 メッセージは 32MiB まで
//     https://developers.cloudflare.com/durable-objects/platform/limits/
//
// 未確認（最初の deploy で確かめる。§15）:
//   - getTcpPort(7411).fetch() の返事の webSocket を accept() して、DO の中から読み書きできるか
//     （公式の例は、返事をそのまま実機へ返す形。だめなら connect() の生の TCP に WebSocket の
//     枠を自分で載せる。直すのは ContainerBuilder.open だけ）
//   - alarm が動くたびに「DO が動いた」と数えられ、コンテナの止まるまでの時間が延びるか
//     （実機が切れたまま 10 分を超えるジョブで確かめる。だめなら KEEP_WARM_S を 15 分以上に）
//   - start() の直後の running の値、/healthz が通るまでの時間（GB 級のイメージの冷えた起動）
//   - WebSocket の close に使えるコード（ここでは必ず通る 4000 番台だけを使う）
//   - /v1/status の jobs の state の文字列（ここでは "done" だけを「終わった」と読む。課題 B1 と合わせる）
//
// このファイルの形: 判断は全部 `SessionCore`（I/O の口を引数で受ける。試験は偽物を渡す）。
// `BuildSession` は Cloudflare の口（ctx・WebSocketPair・ctx.container）につなぐだけ。

import { DEVICE_HEADER, SUBPROTOCOL, WS_PATH, json, type Env } from "./index";
import { MAX_BINARY_FRAME, MAX_CONNECTIONS_PER_DEVICE, MAX_TEXT_FRAME } from "./limits";

/** zc-buildd のポート（zc-wire の DEFAULT_PORT と同じ）。 */
export const BUILDD_PORT = 7411;
/** 約束の版（zc-wire の PROTOCOL_VERSION と同じ）。冷えた接続で hello に答えるときだけ使う。 */
export const PROTOCOL_VERSION = 1;
/** 冷えた接続で DO が返す hello の名前。 */
export const DO_SERVER_NAME = "zc-build-do/0.1.0";
/** wrangler.jsonc の containers[].images の名前。 */
export const IMAGE_NAME = "builder";
/** alarm の間隔。 */
export const ALARM_INTERVAL_MS = 30_000;
/** コンテナを起こしてから /healthz が通るまで待つ時間。 */
export const START_WAIT_MS = 30_000;
/** /healthz を聞く間隔。 */
export const HEALTH_POLL_MS = 250;
/** 受け付けたばかりのジョブを lost にしない時間（job を流してから受付の台帳に載るまでの間）。 */
export const RECONCILE_GRACE_MS = 10_000;
/** 上流が開くまでにためておける量（hello と目録が入ればよい）。超えたら too-large で閉じる。 */
export const MAX_PENDING_BYTES = 2 * MAX_TEXT_FRAME;
/** 台帳の行を残す日数。 */
export const LEDGER_KEEP_DAYS = 7;

/** WebSocket を閉じるコード（4000 番台 = アプリが決めてよい範囲。理由は error で先に送る）。 */
export const CLOSE = {
  /** DO が作り直され、上流が無くなった。実機はつなぎ直す。 */
  restart: 4012,
  protocol: 4400,
  tooLarge: 4413,
  internal: 4500,
  /** 上流（zc-buildd）が閉じた。 */
  upstream: 4502,
  unavailable: 4503,
} as const;

// ───────────────────────── 台帳 ─────────────────────────

/** 台帳の 1 行（SQLite の表 jobs）。 */
export interface JobRow {
  job: string;
  kind: "build" | "check" | "test";
  /** running は「受け付けた〜終わるまで」（待ちの列にいる間も含む）。 */
  state: "running" | "done" | "lost";
  status: string | null;
  /** UTC の日付（YYYY-MM-DD）。1 日の上限を数える鍵。 */
  day: string;
  /** 受け付けた時刻。started が来たら、始まった時刻に付け直す。 */
  started_ms: number;
  finished_ms: number | null;
}

/** 台帳（純粋な部分。SQLite の口だけに依存させ、試験しやすくする）。 */
export interface Ledger {
  /** job を数える。その日の上限を超えていたら false。同じ job id の出し直しは数えない。 */
  admit(job: string, kind: JobRow["kind"], nowMs: number, perDay: number): boolean;
  started(job: string, nowMs: number): void;
  done(job: string, status: string, nowMs: number): void;
  /**
   * 台帳で running のジョブのうち、`alive` に無いものを lost にして返す。
   * 受け付けてから RECONCILE_GRACE_MS たっていないものは触らない。
   */
  reconcile(alive: string[], nowMs: number): string[];
  stateOf(job: string): JobRow["state"] | null;
  running(): number;
  /** 7 日より古い行を消す。 */
  prune(nowMs: number): void;
}

/** SQLite の口。DO では `ctx.storage.sql.exec(q, ...b).toArray()`、試験では node:sqlite。 */
export type SqlExec = (query: string, ...bindings: (string | number | null)[]) => Record<string, unknown>[];

/** UTC の日付（YYYY-MM-DD）。 */
export function dayOf(nowMs: number): string {
  return new Date(nowMs).toISOString().slice(0, 10);
}

/** 次の UTC の 0 時までの秒数（error {limit} の retry_after_s）。 */
export function secondsToNextDay(nowMs: number): number {
  const day = 86_400_000;
  return Math.max(1, Math.ceil((day - (nowMs % day)) / 1000));
}

export class SqlLedger implements Ledger {
  constructor(private readonly sql: SqlExec) {
    sql(
      `CREATE TABLE IF NOT EXISTS jobs (
         job TEXT PRIMARY KEY,
         kind TEXT NOT NULL,
         state TEXT NOT NULL,
         status TEXT,
         day TEXT NOT NULL,
         started_ms INTEGER NOT NULL,
         finished_ms INTEGER
       )`,
    );
    sql(`CREATE INDEX IF NOT EXISTS jobs_day ON jobs (day)`);
    sql(`CREATE INDEX IF NOT EXISTS jobs_state ON jobs (state)`);
  }

  admit(job: string, kind: JobRow["kind"], nowMs: number, perDay: number): boolean {
    const day = dayOf(nowMs);
    const row = this.sql(`SELECT day FROM jobs WHERE job = ?`, job)[0];
    if (row !== undefined && row.day === day) {
      // 同じ日の出し直し（lost・失敗のやり直し、結果の使い回し）。数えない。
      this.sql(`UPDATE jobs SET kind = ?, state = 'running', status = NULL, started_ms = ?, finished_ms = NULL WHERE job = ?`, kind, nowMs, job);
      return true;
    }
    const used = Number(this.sql(`SELECT COUNT(*) AS n FROM jobs WHERE day = ?`, day)[0]?.n ?? 0);
    if (used >= perDay) return false;
    this.sql(
      `INSERT OR REPLACE INTO jobs (job, kind, state, status, day, started_ms, finished_ms)
       VALUES (?, ?, 'running', NULL, ?, ?, NULL)`,
      job,
      kind,
      day,
      nowMs,
    );
    return true;
  }

  started(job: string, nowMs: number): void {
    // lost / done に見えていても、受付が「始めた」と言うならそちらが正しい。
    this.sql(`UPDATE jobs SET state = 'running', status = NULL, started_ms = ?, finished_ms = NULL WHERE job = ?`, nowMs, job);
  }

  done(job: string, status: string, nowMs: number): void {
    this.sql(`UPDATE jobs SET state = 'done', status = ?, finished_ms = ? WHERE job = ?`, status, nowMs, job);
  }

  reconcile(alive: string[], nowMs: number): string[] {
    const keep = new Set(alive);
    const rows = this.sql(`SELECT job FROM jobs WHERE state = 'running' AND started_ms <= ?`, nowMs - RECONCILE_GRACE_MS);
    const lost: string[] = [];
    for (const r of rows) {
      const job = String(r.job);
      if (keep.has(job)) continue;
      this.sql(`UPDATE jobs SET state = 'lost', status = 'lost', finished_ms = ? WHERE job = ?`, nowMs, job);
      lost.push(job);
    }
    return lost;
  }

  stateOf(job: string): JobRow["state"] | null {
    const row = this.sql(`SELECT state FROM jobs WHERE job = ?`, job)[0];
    return row === undefined ? null : (String(row.state) as JobRow["state"]);
  }

  running(): number {
    return Number(this.sql(`SELECT COUNT(*) AS n FROM jobs WHERE state = 'running'`)[0]?.n ?? 0);
  }

  prune(nowMs: number): void {
    this.sql(`DELETE FROM jobs WHERE COALESCE(finished_ms, started_ms) < ?`, nowMs - LEDGER_KEEP_DAYS * 86_400_000);
  }
}

// ───────────────────────── I/O の口 ─────────────────────────

/** 実機側の WebSocket のうち、使う部分。 */
export interface DeviceSocket {
  send(data: string | ArrayBuffer): void;
  close(code?: number, reason?: string): void;
}

/** 上流（zc-buildd）への WebSocket。 */
export interface UpstreamSocket {
  send(data: string | ArrayBuffer): void;
  close(code?: number, reason?: string): void;
}

export interface UpstreamHandlers {
  message(data: string | ArrayBuffer): void;
  close(code: number, reason: string): void;
}

/** zc-buildd の /v1/status の返事（§4.1）。 */
export interface BuilddStatus {
  running: number;
  queued: number;
  jobs: { job: string; state?: string; status?: string }[];
}

/** ビルド係（コンテナと、その中の zc-buildd）の口。 */
export interface Builder {
  /** コンテナが動いているか（受付が待ち受けているとは限らない）。 */
  running(): boolean;
  /** コンテナを起こす。待たない。起こせなければ投げる。 */
  start(): void;
  /** /healthz が 200 か（中で時間切れを持つ。投げない）。 */
  healthy(): Promise<boolean>;
  /** /v1/status。聞けなければ null（投げない）。 */
  status(): Promise<BuilddStatus | null>;
  /** DO が動かなくなってから、コンテナを止めるまでの時間を掛ける。 */
  keepWarm(ms: number): Promise<void>;
  /** 上流の WebSocket を 1 本張る。張れなければ投げる。 */
  open(deviceId: string, handlers: UpstreamHandlers): Promise<UpstreamSocket>;
}

export interface Clock {
  now(): number;
  sleep(ms: number): Promise<void>;
  /** alarm が掛かっていなければ、`atMs` に掛ける。 */
  ensureAlarm(atMs: number): Promise<void>;
}

export interface SessionConfig {
  keepWarmMs: number;
  jobsPerDay: number;
}

/** 設定を読む（おかしな値は既定に戻す）。 */
export function configOf(env: Pick<Env, "KEEP_WARM_S" | "JOBS_PER_DAY">): SessionConfig {
  const num = (s: string | undefined, def: number, min: number, max: number): number => {
    const n = Number(s);
    return Number.isFinite(n) && n >= min && n <= max ? Math.floor(n) : def;
  };
  return {
    // setInactivityTimeout は 6 時間まで
    keepWarmMs: num(env.KEEP_WARM_S, 600, 1, 21_600) * 1000,
    jobsPerDay: num(env.JOBS_PER_DAY, 400, 0, 1_000_000),
  };
}

// ───────────────────────── 中継と見張り ─────────────────────────

type ConnMode =
  /** コンテナが止まっている。上流はまだ張っていない。 */
  | "cold"
  /** 上流を張っているところ（コンテナの起動待ちを含む）。届いたものは queue にためる。 */
  | "opening"
  /** 中継中。 */
  | "open"
  | "closed";

interface Conn {
  device: DeviceSocket;
  deviceId: string;
  mode: ConnMode;
  up: UpstreamSocket | null;
  queue: (string | ArrayBuffer)[];
  queuedBytes: number;
  /** 冷えた接続で DO が自分で答えた hello（上流を張ったら最初に送る）。 */
  hello: string | null;
  /** 上流の最初の hello を実機へ流さない。 */
  swallowHello: boolean;
}

const JOB_ID = /^[0-9a-f]{1,64}$/;
const KINDS: readonly string[] = ["build", "check", "test"];
/** 受付 → 実機のうち、台帳に関わるもの（serde は type を先頭に書く）。 */
const PEEK = /^\{"type":"(started|done|hello)"/;

function byteLength(data: string | ArrayBuffer): number {
  if (typeof data !== "string") return data.byteLength;
  // UTF-16 の 1 単位は UTF-8 で 3 バイトまで。上限に届きえないときは数えない
  if (data.length * 3 <= MAX_TEXT_FRAME) return data.length;
  return new TextEncoder().encode(data).byteLength;
}

function parseObject(text: string): Record<string, unknown> | null {
  try {
    const v: unknown = JSON.parse(text);
    return typeof v === "object" && v !== null && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

export class SessionCore {
  private readonly conns = new Map<DeviceSocket, Conn>();
  /** コンテナの起動待ち（同時の接続で 1 つを分け合う）。 */
  private starting: Promise<void> | null = null;
  /** この DO が生きている間に /healthz が通ったか。 */
  private ready = false;

  constructor(
    private readonly ledger: Ledger,
    private readonly builder: Builder,
    private readonly clock: Clock,
    private readonly cfg: SessionConfig,
  ) {}

  /** 開いている接続の数。 */
  get connections(): number {
    return this.conns.size;
  }

  /** 新しい接続を受けてよいか（同時に 4 本まで）。 */
  hasRoom(): boolean {
    return this.conns.size < MAX_CONNECTIONS_PER_DEVICE;
  }

  /** 実機の WebSocket を受けた。 */
  connect(device: DeviceSocket, deviceId: string): void {
    const conn: Conn = { device, deviceId, mode: "cold", up: null, queue: [], queuedBytes: 0, hello: null, swallowHello: false };
    this.conns.set(device, conn);
    if (this.builder.running() || this.starting !== null) {
      this.openUpstream(conn);
      return;
    }
    // コンテナが止まっている = 受付は何も覚えていない。走っていたはずのジョブは全部 lost
    // （受け付けたばかりのものも含めるので、猶予のぶん先の時刻で呼ぶ）。
    this.ready = false;
    this.ledger.reconcile([], this.clock.now() + RECONCILE_GRACE_MS);
  }

  /** 実機からのメッセージ。 */
  async message(device: DeviceSocket, data: string | ArrayBuffer): Promise<void> {
    const conn = this.conns.get(device);
    if (conn === undefined || conn.mode === "closed") {
      // DO が作り直されて上流が無い接続。閉じて、実機につなぎ直させる。
      this.orphan(device);
      return;
    }
    if (typeof data !== "string") {
      if (data.byteLength > MAX_BINARY_FRAME) return this.fail(conn, "too-large", "binary frame too large", CLOSE.tooLarge);
      this.forward(conn, data);
      return;
    }
    if (data.length > MAX_TEXT_FRAME || byteLength(data) > MAX_TEXT_FRAME) {
      return this.fail(conn, "too-large", "text frame too large", CLOSE.tooLarge);
    }
    const msg = parseObject(data);
    if (msg === null) return this.fail(conn, "protocol", "not a JSON object", CLOSE.protocol);

    const type = msg.type;
    if (type === "job" || type === "attach" || type === "cancel") {
      const job = msg.job;
      if (typeof job !== "string" || !JOB_ID.test(job)) return this.fail(conn, "protocol", "bad job id", CLOSE.protocol);

      if (type === "job") {
        const kind = msg.kind;
        if (typeof kind !== "string" || !KINDS.includes(kind)) return this.fail(conn, "protocol", "bad job kind", CLOSE.protocol);
        const now = this.clock.now();
        if (!this.ledger.admit(job, kind as JobRow["kind"], now, this.cfg.jobsPerDay)) {
          this.reply(conn, { type: "error", job, code: "limit", message: "daily job limit reached", retry_after_s: secondsToNextDay(now) });
          return;
        }
        this.forward(conn, data);
        // running がある間は alarm で見張る（コンテナを止めさせない・lost を見つける）
        await this.clock.ensureAlarm(now + ALARM_INTERVAL_MS);
        return;
      }

      const state = this.ledger.stateOf(job);
      if (type === "attach" && state === "lost") {
        this.reply(conn, { type: "done", job, status: "lost", exit: 0, queue_ms: 0, run_ms: 0 });
        return;
      }
      if (conn.mode === "cold") {
        // 止まった受付は何も覚えていない。起こしても返事は同じなので、起こさずに返す。
        this.reply(conn, { type: "error", job, code: "unknown-job", message: "builder was stopped" });
        return;
      }
      this.forward(conn, data);
      return;
    }

    if (type === "hello" && conn.mode === "cold" && conn.hello === null && msg.v === PROTOCOL_VERSION) {
      conn.hello = data;
      this.reply(conn, { type: "hello", v: PROTOCOL_VERSION, server: DO_SERVER_NAME });
      return;
    }
    this.forward(conn, data);
  }

  /** 実機の WebSocket が閉じた（エラーも同じ）。上流だけ閉じる。ジョブは止めない。 */
  deviceClosed(device: DeviceSocket): void {
    const conn = this.conns.get(device);
    if (conn === undefined) return;
    this.drop(conn);
    try {
      conn.up?.close(1000, "device closed");
    } catch {
      // もう閉じている
    }
  }

  /** 上流が無い接続（DO が作り直された）を閉じる。 */
  orphan(device: DeviceSocket): void {
    try {
      device.close(CLOSE.restart, "restart");
    } catch {
      // もう閉じている
    }
  }

  /** 30 秒ごと（running がある間だけ掛け直す）。 */
  async alarm(): Promise<void> {
    const now = this.clock.now();
    this.ledger.prune(now);
    if (this.starting !== null) {
      // 起こしている途中。次で見る。
      await this.clock.ensureAlarm(now + ALARM_INTERVAL_MS);
      return;
    }
    if (!this.builder.running()) {
      // コンテナが止まった。走っていたジョブは無くなった。alarm は掛け直さない。
      this.ready = false;
      this.ledger.reconcile([], now + RECONCILE_GRACE_MS);
      return;
    }
    let st = await this.builder.status();
    if (st === null) {
      await this.clock.sleep(1000);
      st = await this.builder.status();
    }
    if (st === null) {
      // コンテナは動いているのに受付が 2 回続けて答えない。受付が死んだとみなす。
      // コンテナは setInactivityTimeout で止まる（ここでは延ばさない）。
      this.ledger.reconcile([], this.clock.now() + RECONCILE_GRACE_MS);
      return;
    }
    const alive: string[] = [];
    for (const j of st.jobs) {
      if (j.state === "done") {
        // 見ている実機がいない間に終わった（done を中継できなかった）。結果は受付に残っている。
        if (this.ledger.stateOf(j.job) === "running") this.ledger.done(j.job, j.status ?? "unknown", this.clock.now());
      } else {
        alive.push(j.job);
      }
    }
    this.ledger.reconcile(alive, this.clock.now());
    if (this.ledger.running() > 0 || st.running + st.queued > 0) {
      // DO の再起動で消えるので、毎回掛け直す
      await this.builder.keepWarm(this.cfg.keepWarmMs).catch(() => {});
      await this.clock.ensureAlarm(this.clock.now() + ALARM_INTERVAL_MS);
    }
  }

  // ── 中 ──

  private drop(conn: Conn): void {
    conn.mode = "closed";
    conn.queue = [];
    conn.queuedBytes = 0;
    this.conns.delete(conn.device);
  }

  /** 実機へ、DO が作ったメッセージを返す。 */
  private reply(conn: Conn, msg: Record<string, unknown>): void {
    try {
      conn.device.send(JSON.stringify(msg));
    } catch {
      this.closeBoth(conn, CLOSE.internal, "send failed");
    }
  }

  /** 接続全体の失敗: error を返して両側を閉じる。 */
  private fail(conn: Conn, code: string, message: string, closeCode: number): void {
    try {
      conn.device.send(JSON.stringify({ type: "error", code, message }));
    } catch {
      // 閉じるだけ
    }
    this.closeBoth(conn, closeCode, code);
  }

  private closeBoth(conn: Conn, code: number, reason: string): void {
    if (conn.mode === "closed") return;
    const up = conn.up;
    this.drop(conn);
    try {
      conn.device.close(code, reason);
    } catch {
      // もう閉じている
    }
    try {
      up?.close(1000, reason);
    } catch {
      // もう閉じている
    }
  }

  /** 実機 → 上流。中身は変えない。 */
  private forward(conn: Conn, data: string | ArrayBuffer): void {
    if (conn.mode === "open" && conn.up !== null) {
      try {
        conn.up.send(data);
      } catch {
        this.closeBoth(conn, CLOSE.upstream, "upstream closed");
      }
      return;
    }
    const size = typeof data === "string" ? byteLength(data) : data.byteLength;
    if (conn.queuedBytes + size > MAX_PENDING_BYTES) {
      this.fail(conn, "too-large", "too much data before the builder is ready", CLOSE.tooLarge);
      return;
    }
    conn.queue.push(data);
    conn.queuedBytes += size;
    if (conn.mode === "cold") this.openUpstream(conn);
  }

  /** 上流を張る（コンテナが無ければ起こす）。待たずに戻る。 */
  private openUpstream(conn: Conn): void {
    conn.mode = "opening";
    void (async () => {
      try {
        await this.ensureBuilder();
        if (conn.mode !== "opening") return;
        const up = await this.builder.open(conn.deviceId, {
          message: (d) => this.fromUpstream(conn, d),
          close: () => this.closeBoth(conn, CLOSE.upstream, "upstream closed"),
        });
        if (conn.mode !== "opening") {
          up.close(1000, "device closed");
          return;
        }
        conn.up = up;
        if (conn.hello !== null) {
          up.send(conn.hello);
          conn.swallowHello = true;
          conn.hello = null;
        }
        // ここから下は待たない（途中に実機のメッセージが割り込まない = 順番が保たれる）
        const queue = conn.queue;
        conn.queue = [];
        conn.queuedBytes = 0;
        for (const d of queue) up.send(d);
        conn.mode = "open";
      } catch {
        if (conn.mode !== "closed") this.fail(conn, "unavailable", "cannot start the builder", CLOSE.unavailable);
      }
    })();
  }

  /** コンテナが動いていて、受付が答えるところまで持っていく。 */
  private ensureBuilder(): Promise<void> {
    if (this.starting !== null) return this.starting;
    if (this.ready && this.builder.running()) return Promise.resolve();
    this.ready = false;
    const p = this.startAndWait().finally(() => {
      if (this.starting === p) this.starting = null;
    });
    this.starting = p;
    return p;
  }

  private async startAndWait(): Promise<void> {
    if (!this.builder.running()) {
      // TODO(C4): スナップショットがあれば、そこから起こす（snapshot.ts）。
      this.builder.start();
    }
    const deadline = this.clock.now() + START_WAIT_MS;
    for (;;) {
      if (await this.builder.healthy()) break;
      if (this.clock.now() >= deadline) throw new Error("builder did not become healthy");
      await this.clock.sleep(HEALTH_POLL_MS);
    }
    // 掛け損ねても続ける（コンテナが早く止まるだけ。次の alarm でも掛ける）
    await this.builder.keepWarm(this.cfg.keepWarmMs).catch(() => {});
    this.ready = true;
  }

  /** 上流 → 実機。中身は変えない。テキストは台帳のために覗く。 */
  private fromUpstream(conn: Conn, data: string | ArrayBuffer): void {
    if (conn.mode === "closed") return;
    let msg: Record<string, unknown> | null = null;
    if (typeof data === "string") {
      // ログや診断は覗かない（大きい）。type が先頭に無い JSON だけは、念のため全部読む。
      if (PEEK.test(data) || !data.startsWith('{"type":"')) msg = parseObject(data);
      if (conn.swallowHello) {
        conn.swallowHello = false;
        // 実機には DO がもう hello を返してある。版が合っていれば捨てる（違えばそのまま流す）。
        if (msg !== null && msg.type === "hello" && msg.v === PROTOCOL_VERSION) return;
      }
    }
    try {
      conn.device.send(data);
    } catch {
      this.closeBoth(conn, CLOSE.internal, "send failed");
      return;
    }
    if (msg === null || typeof msg.job !== "string") return;
    const now = this.clock.now();
    switch (msg.type) {
      case "started":
        this.ledger.started(msg.job, now);
        break;
      case "done":
        this.ledger.done(msg.job, typeof msg.status === "string" ? msg.status : "unknown", now);
        break;
    }
  }
}

// ───────────────────────── Cloudflare の口 ─────────────────────────

const INSTANCES = ["lite", "standard-1", "standard-2", "standard-3", "standard-4"] as const;
type Instance = (typeof INSTANCES)[number];

/** ctx.container と、その中の zc-buildd。 */
class ContainerBuilder implements Builder {
  constructor(
    private readonly ctx: DurableObjectState,
    private readonly env: Env,
  ) {}

  private container(): Container {
    const c = this.ctx.container;
    if (c === undefined) throw new Error("no container is attached to this Durable Object");
    return c;
  }

  private port(): Fetcher {
    return this.container().getTcpPort(BUILDD_PORT);
  }

  running(): boolean {
    return this.ctx.container?.running ?? false;
  }

  start(): void {
    const c = this.container();
    const image = c.images[IMAGE_NAME];
    if (image === undefined) throw new Error(`image "${IMAGE_NAME}" is not configured`);
    const instance: Instance = (INSTANCES as readonly string[]).includes(this.env.INSTANCE_TYPE) ? (this.env.INSTANCE_TYPE as Instance) : "standard-4";
    // enableInternet: cargo と npm が crates.io・npm へ取りに行く（§15 の未確認の項目）
    c.start({ image, instance, enableInternet: true });
  }

  async healthy(): Promise<boolean> {
    try {
      const res = await this.port().fetch("http://container/healthz", { signal: AbortSignal.timeout(2000) });
      await res.body?.cancel();
      return res.status === 200;
    } catch {
      return false;
    }
  }

  async status(): Promise<BuilddStatus | null> {
    try {
      const res = await this.port().fetch("http://container/v1/status", { signal: AbortSignal.timeout(5000) });
      if (res.status !== 200) {
        await res.body?.cancel();
        return null;
      }
      return parseStatus(await res.json());
    } catch {
      return null;
    }
  }

  keepWarm(ms: number): Promise<void> {
    return this.container().setInactivityTimeout(ms);
  }

  async open(deviceId: string, handlers: UpstreamHandlers): Promise<UpstreamSocket> {
    // 公式の例（containers/examples/websocket）と同じ: Upgrade つきの依頼をポートへ fetch する。
    const res = await this.port().fetch(`http://container${WS_PATH}`, {
      headers: { Upgrade: "websocket", "Sec-WebSocket-Protocol": SUBPROTOCOL, [DEVICE_HEADER]: deviceId },
    });
    const ws = res.webSocket;
    if (ws === null) {
      await res.body?.cancel();
      throw new Error(`builder refused the WebSocket upgrade: ${res.status}`);
    }
    ws.binaryType = "arraybuffer";
    ws.accept();
    let closed = false;
    const close = (code: number, reason: string): void => {
      if (closed) return;
      closed = true;
      handlers.close(code, reason);
    };
    ws.addEventListener("message", (ev) => {
      const d: unknown = ev.data;
      if (typeof d === "string" || d instanceof ArrayBuffer) handlers.message(d);
    });
    ws.addEventListener("close", (ev) => close(ev.code, ev.reason));
    ws.addEventListener("error", () => close(1011, "error"));
    return ws;
  }
}

/** /v1/status の返事を、形を確かめて読む。 */
export function parseStatus(v: unknown): BuilddStatus | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  const jobs: BuilddStatus["jobs"] = [];
  if (Array.isArray(o.jobs)) {
    for (const j of o.jobs as unknown[]) {
      if (typeof j !== "object" || j === null) continue;
      const r = j as Record<string, unknown>;
      if (typeof r.job !== "string") continue;
      jobs.push({
        job: r.job,
        state: typeof r.state === "string" ? r.state : undefined,
        status: typeof r.status === "string" ? r.status : undefined,
      });
    }
  }
  const n = (x: unknown): number => (typeof x === "number" && Number.isFinite(x) ? x : 0);
  return { running: n(o.running), queued: n(o.queued), jobs };
}

export class BuildSession implements DurableObject {
  private readonly core: SessionCore;

  constructor(
    private readonly ctx: DurableObjectState,
    private readonly env: Env,
  ) {
    const sql = ctx.storage.sql;
    const ledger = new SqlLedger((q, ...b) => sql.exec(q, ...b).toArray());
    const cfg = configOf(env);
    const builder = new ContainerBuilder(ctx, env);
    const clock: Clock = {
      now: () => Date.now(),
      sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
      ensureAlarm: async (atMs) => {
        if ((await ctx.storage.getAlarm()) === null) await ctx.storage.setAlarm(atMs);
      },
    };
    this.core = new SessionCore(ledger, builder, clock, cfg);

    // ここに来た = DO が作り直された。メモリの中の上流は無いので、残っている実機の
    // WebSocket は閉じる（実機がつなぎ直して attach する。先頭の説明を参照）。
    for (const ws of ctx.getWebSockets()) this.core.orphan(ws);
    // setInactivityTimeout は DO の再起動で消える。
    if (builder.running()) void builder.keepWarm(cfg.keepWarmMs).catch(() => {});
    // 見張りの続き（alarm は DO の再起動をまたいで残るが、念のため）。
    if (ledger.running() > 0) void clock.ensureAlarm(Date.now() + ALARM_INTERVAL_MS);
  }

  /** Worker から。WebSocket の Upgrade だけが来る（X-Zc-Device つき）。 */
  async fetch(request: Request): Promise<Response> {
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") return json(426, "upgrade required");
    const deviceId = request.headers.get(DEVICE_HEADER) ?? "";
    if (!/^[a-z0-9-]{1,32}$/.test(deviceId)) return json(400, "bad request");
    const offered = (request.headers.get("Sec-WebSocket-Protocol") ?? "").split(",").map((s) => s.trim());
    if (!offered.includes(SUBPROTOCOL)) return json(400, "bad request");
    if (!this.core.hasRoom()) return json(429, "too many connections", { "retry-after": "5" });

    const pair = new WebSocketPair();
    const client = pair[0];
    const server = pair[1];
    this.ctx.acceptWebSocket(server);
    this.core.connect(server, deviceId);
    return new Response(null, { status: 101, webSocket: client, headers: { "Sec-WebSocket-Protocol": SUBPROTOCOL } });
  }

  /** 実機からのメッセージ（ハイバネーションの口）。 */
  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    await this.core.message(ws, message);
  }

  async webSocketClose(ws: WebSocket, code: number, reason: string, wasClean: boolean): Promise<void> {
    void wasClean;
    this.core.deviceClosed(ws);
    try {
      // 閉じる握手の返事（新しい compatibility_date では自動。重ねても害は無い）。
      // 1005 / 1006 などは送れないので 1000 にする。
      ws.close(code === 1000 || (code >= 3000 && code <= 4999) ? code : 1000, reason);
    } catch {
      // もう閉じている
    }
  }

  async webSocketError(ws: WebSocket, error: unknown): Promise<void> {
    void error;
    this.core.deviceClosed(ws);
  }

  /** 30 秒ごと（running がある間だけ）。 */
  async alarm(): Promise<void> {
    await this.core.alarm();
    // TODO(C4): 走っているジョブが無くなったら、スナップショットを撮る（snapshot.ts。§6.5）。
  }
}

// 受け入れ試験（test/session.test.ts。コンテナは偽物の上流に差し替える）:
// 中継（テキスト・バイナリが変わらない）、1 日の上限で error{limit}・同じ job id は数えない、
// 実機が切れても上流のジョブが done まで台帳に付く、alarm で running → lost、
// lost のジョブへの attach に done{lost}（コンテナを起こさない）、同時の接続 5 本目を断る、
// 壊れた JSON で閉じる、コンテナを起こせないとき error{unavailable}。
// Ledger は SQLite（node:sqlite）で、表の全部の場面を単体で。
