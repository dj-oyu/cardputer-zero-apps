// 上限（docs/stage3-design.md §11）。雛形。中身は課題 C1（Worker 側）と C2（DO 側の台帳）。
//
// 守りはサーバ側に置く。実機のトークンは F5 で動かすプログラムからも読めるので、
// 漏れても「ビルド時間を使われる」だけで済むようにする。
//
// | 上限 | 値 | どこで | 超えたら |
// |---|---|---|---|
// | つなぐ回数 | device_id ごとに 60 秒 30 回 | Worker（Rate Limiting の束縛 CONNECT_LIMIT） | 429 + Retry-After: 60 |
// | 同時の接続 | device_id ごとに 4 本 | DO | 新しい接続を 429 で断る |
// | 1 日のジョブ数 | device_id ごとに 400（JOBS_PER_DAY） | DO（SQLite の台帳。UTC の日付で数える） | error {limit, retry_after_s} |
// | 同時に走るジョブ | 2（build / test 1 + check 1） | 受付（zc-buildd） | 待たせる（列は 8 まで） |
// | コンテナ | device_id ごとに 1 つ・全体で 2 つ | DO / アカウントの設定 | error {unavailable} |
// | 1 ジョブの時間 | 15 分 | 受付 | done {timeout} |
// | 1 メッセージ | テキスト 1MiB・バイナリ 64KiB + 16 | DO と受付 | error {too-large} で閉じる |
// | 木 | 2000 ファイル・32MiB・1 ファイル 4MiB | 受付 | error {too-large} |

export const MAX_CONNECTIONS_PER_DEVICE = 4;
export const MAX_TEXT_FRAME = 1 << 20;
export const MAX_BINARY_FRAME = (64 << 10) + 16;

/** つなぐ回数の制限。通してよければ true。 */
export async function checkConnectLimit(limiter: RateLimit, deviceId: string): Promise<boolean> {
  // TODO(C1): limiter.limit({ key: deviceId })
  void limiter;
  void deviceId;
  return true;
}
