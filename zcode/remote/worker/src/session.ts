// Durable Object「BuildSession」= ジョブの持ち主（docs/stage3-design.md §6.3〜§6.5）。
// 雛形: 形と決まりだけ。中身は課題 C2。
//
// 実機 1 台につき 1 つ（名前 = device_id）。コンテナも 1 つ。
//
//   実機 ◀─WebSocket（ハイバネーション）─▶ DO ◀─WebSocket─▶ コンテナの zc-buildd（:7411）
//
// DO がやること:
//   1. 実機の WebSocket を受ける（acceptWebSocket。送受信が無い間は DO を眠らせる）
//   2. コンテナを起こす（無ければ。スナップショットがあればそこから）。zc-buildd の
//      /healthz が 200 を返すまで待つ（最大 30 秒）。起こせなければ error {unavailable}
//   3. 実機の接続 1 本につき、コンテナへ WebSocket を 1 本張り、**中身を変えずに中継する**
//      （テキストもバイナリも）。約束（attach・cancel・ログの保持）を知っているのは zc-buildd で、
//      DO は中継と見張りだけ（同じ実装を PC 直結でも使い、Cloudflare なしで試せるようにするため）
//   4. 通り過ぎるテキストを見て**ジョブの台帳**（SQLite）を付ける:
//        実機 → job      … 1 日の上限を数える。超えていたら中継せず error {limit} を返す
//        受付 → started  … running にする
//        受付 → done     … done にする（status・時間を残す）
//      バイナリは見ない。JSON として読めないテキストは中継せず error {protocol} で閉じる
//   5. **接続が切れてもジョブは続ける**: 実機の WebSocket が切れたら、その接続の上流だけ閉じる
//      （zc-buildd はジョブを止めない）。running のジョブがある間は、コンテナを止めない:
//      alarm（30 秒ごと）で zc-buildd の /v1/status を聞き、走っていれば
//      setInactivityTimeout を延ばす。台帳で running なのに /v1/status に無いジョブは lost にする
//   6. つなぎ直した実機の attach は、そのまま zc-buildd へ流す。ただしコンテナが止まっていた
//      （台帳で lost）ジョブへの attach には、コンテナを起こさずに done {status:"lost"} を返す
//      （実機は同じ依頼を出し直す。job id が同じなので、同じジョブとして数える）
//   7. 最後の接続が切れ、running が無くなってから KEEP_WARM_S 秒でコンテナを止める
//      （setInactivityTimeout に任せる）。止める前にスナップショットを撮る（§6.5）
//
// 未確認（最初の deploy で確かめる。§15）:
//   - ctx.container.getTcpPort(7411).fetch() に Upgrade を付けて WebSocket を張れるか
//     （張れなければ connect() の生の TCP に、zc-wire と同じ WebSocket の枠を自分で載せる）
//   - WebSocket のハイバネーション中に、上流（コンテナ側）の WebSocket が保たれるか
//     （保たれないなら、ジョブの実行中は DO を眠らせない = 費用は実行中だけ）
//   - GB 級のイメージの冷えた起動の時間、スナップショットから起こす時間
//   - 1 メッセージの大きさの上限（1MiB のテキスト・64KiB のバイナリが通るか）

import type { Env } from "./index";

/** 台帳の 1 行（SQLite の表 jobs）。 */
export interface JobRow {
  job: string;
  kind: "build" | "check" | "test";
  state: "running" | "done" | "lost";
  status: string | null;
  /** UTC の日付（YYYY-MM-DD）。1 日の上限を数える鍵。 */
  day: string;
  started_ms: number;
  finished_ms: number | null;
}

/** 台帳（純粋な部分。SQLite の口だけに依存させ、試験しやすくする）。 */
export interface Ledger {
  /** job を数える。その日の上限を超えていたら false。同じ job id の出し直しは数えない。 */
  admit(job: string, kind: JobRow["kind"], nowMs: number, perDay: number): boolean;
  started(job: string, nowMs: number): void;
  done(job: string, status: string, nowMs: number): void;
  /** 台帳で running のジョブのうち、`alive` に無いものを lost にして返す。 */
  reconcile(alive: string[], nowMs: number): string[];
  stateOf(job: string): JobRow["state"] | null;
  running(): number;
  /** 7 日より古い行を消す。 */
  prune(nowMs: number): void;
}

export class BuildSession implements DurableObject {
  constructor(
    private readonly ctx: DurableObjectState,
    private readonly env: Env,
  ) {
    // TODO(C2): 表を作る。コンテナが動いていれば setInactivityTimeout を掛け直す
    // （この設定は DO の再起動で消える）。
  }

  /** Worker から。WebSocket の Upgrade だけが来る（X-Zc-Device つき）。 */
  async fetch(request: Request): Promise<Response> {
    // TODO(C2): 上の 1〜3。
    void request;
    void this.ctx;
    void this.env;
    return new Response("not implemented", { status: 501 });
  }

  /** 実機からのメッセージ（ハイバネーションの口）。 */
  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    // TODO(C2): 上の 4・6。
    void ws;
    void message;
  }

  async webSocketClose(ws: WebSocket, code: number, reason: string, wasClean: boolean): Promise<void> {
    // TODO(C2): 上の 5。
    void ws;
    void code;
    void reason;
    void wasClean;
  }

  async webSocketError(ws: WebSocket, error: unknown): Promise<void> {
    void ws;
    void error;
  }

  /** 30 秒ごと（running がある間だけ）。 */
  async alarm(): Promise<void> {
    // TODO(C2): 上の 5・7 と、スナップショット（§6.5）。
  }
}

// 受け入れ試験（課題 C2、test/session.test.ts。コンテナは偽物の上流に差し替える）:
// 中継（テキスト・バイナリが変わらない）、1 日の上限で error{limit}・同じ job id は数えない、
// 実機が切れても上流のジョブが done まで台帳に付く、alarm で running → lost、
// lost のジョブへの attach に done{lost}（コンテナを起こさない）、同時の接続 5 本目を断る、
// 壊れた JSON で閉じる、コンテナを起こせないとき error{unavailable}。
// Ledger は SQLite の偽物で、表の全部の場面を単体で。
