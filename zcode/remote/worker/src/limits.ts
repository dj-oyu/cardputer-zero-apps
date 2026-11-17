// 上限（docs/stage3-design.md §11）。課題 C1（Worker 側）。DO 側の台帳（C2）が使う小さな
// 道具（UTC の日付・翌日までの秒・vars の数の読み方）もここに置く。
//
// 守りはサーバ側に置く。実機のトークンは F5 で動かすプログラムからも読めるので、
// 漏れても「ビルド時間を使われる」だけで済むようにする。
//
// | 上限 | 値 | どこで | 超えたら |
// |---|---|---|---|
// | つなぐ回数 | device_id ごとに 60 秒 30 回 | Worker（Rate Limiting の束縛 CONNECT_LIMIT） | 429 + Retry-After: 60 |
// | 同時の接続 | device_id ごとに 4 本 | DO | 新しい接続を 429 で断る |
// | 1 日のジョブ数 | device_id ごとに 400（JOBS_PER_DAY）。check は数えない | DO（SQLite の台帳。UTC の日付で数える） | error {limit, retry_after_s} |
// | 月の費用 | 超過 5 ドル（BUDGET_OVERAGE_USD）。アカウント全体 | DO と台帳の DO（budget.ts） | 新しい job に error {limit}。冷えていれば起こさず閉じる |
// | 同時に走るジョブ | 2（build / test 1 + check 1） | 受付（zc-buildd） | 待たせる（列は 8 まで） |
// | コンテナ | device_id ごとに 1 つ・全体で 2 つ | DO / アカウントの設定 | error {unavailable} |
// | 1 ジョブの時間 | 15 分 | 受付 | done {timeout} |
// | 1 メッセージ | テキスト 1MiB・バイナリ 64KiB + 16 | DO と受付 | error {too-large} で閉じる |
// | 木 | 2000 ファイル・32MiB・1 ファイル 4MiB | 受付 | error {too-large} |
//
// Rate Limiting の束縛は、場所（Cloudflare の拠点）ごとに数える緩い制限（公式の説明）。
// 正確な上限は DO の側（同時の接続・1 日のジョブ数）で守る。

export const MAX_CONNECTIONS_PER_DEVICE = 4;
export const MAX_TEXT_FRAME = 1 << 20;
export const MAX_BINARY_FRAME = (64 << 10) + 16;

/** つなぐ回数の制限の窓（秒）。429 の Retry-After に使う（wrangler.jsonc の period と同じ）。 */
export const CONNECT_LIMIT_PERIOD_S = 60;
/** 1 日のジョブ数の既定（JOBS_PER_DAY が読めないとき）。 */
export const DEFAULT_JOBS_PER_DAY = 400;

/**
 * つなぐ回数の制限。通してよければ true。
 *
 * 束縛そのものが失敗したとき（例外）は通す。ここは緩い第 1 の守りで、正確な上限
 * （同時の接続・1 日のジョブ数）は DO が守るため。失敗はログに残す。
 */
export async function checkConnectLimit(limiter: RateLimit, deviceId: string): Promise<boolean> {
  try {
    const { success } = await limiter.limit({ key: deviceId });
    return success;
  } catch (e) {
    console.error("limits: rate limiter failed", String(e));
    return true;
  }
}

/** UTC の日付（YYYY-MM-DD）。1 日の上限を数える鍵（台帳の day）。 */
export function utcDay(nowMs: number): string {
  return new Date(nowMs).toISOString().slice(0, 10);
}

/** 次の UTC の 0 時までの秒（1 日の上限を超えたときの retry_after_s）。最低 1。 */
export function secondsUntilUtcMidnight(nowMs: number): number {
  const d = new Date(nowMs);
  const next = Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate() + 1);
  return Math.max(1, Math.ceil((next - nowMs) / 1000));
}

/** vars の数（文字列）を読む。正の整数でなければ既定の値。 */
export function positiveInt(value: string | undefined, fallback: number): number {
  if (value === undefined || !/^[0-9]{1,9}$/.test(value.trim())) return fallback;
  const n = Number(value.trim());
  return n > 0 ? n : fallback;
}
