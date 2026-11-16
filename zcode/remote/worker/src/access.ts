// Access の JWT の確認（docs/stage3-design.md §6.2）。雛形。中身は課題 C1。
//
// - ヘッダ Cf-Access-Jwt-Assertion の JWT を Web Crypto で確かめる（ライブラリを足さない）
// - 公開鍵は https://<ACCESS_TEAM_DOMAIN>/cdn-cgi/access/certs（JWKS）。kid で選ぶ。
//   モジュールの変数に覚え、1 時間で取り直す。知らない kid が来たら 1 回だけ取り直す
//   （鍵の入れ替え）。取り直しは 60 秒に 1 回まで（知らない kid を投げ続けられても、
//   Access を叩き続けない）
// - 確かめる項目: 署名（RS256。alg がそれ以外なら断る。"none" は当然断る）、
//   iss = https://<ACCESS_TEAM_DOMAIN>、aud に ACCESS_AUD を含む、exp（60 秒のずれまで許す）、
//   nbf / iat が未来でない
// - サービストークンの JWT は sub が空で、common_name にトークンの Client ID が入る。
//   人のログイン（email がある）は受け付けない（このアプリは機械専用）
// - 確かめるのはつなぐとき（Upgrade）だけ。長い接続の途中で exp が来ても切らない
//
// 未確認（最初の deploy で確かめる。§15）: サービストークンで通ったときの JWT の実物の形
// （common_name・type の項目）、alg（RS256 のはず）。

export interface AccessIdentity {
  /** サービストークンの Client ID（`….access`）。 */
  clientId: string;
  /** JWT の exp（秒）。 */
  exp: number;
}

export type AccessResult =
  | { ok: true; identity: AccessIdentity }
  | { ok: false; status: 401 | 403; reason: string };

export interface AccessConfig {
  teamDomain: string;
  aud: string;
}

/** JWKS を取る口（試験で差し替える）。 */
export type JwksFetcher = (url: string) => Promise<{ keys: JsonWebKey[] }>;

export async function verifyAccessJwt(
  request: Request,
  cfg: AccessConfig,
  nowS: number,
  fetchJwks?: JwksFetcher,
): Promise<AccessResult> {
  // TODO(C1)
  void request;
  void cfg;
  void nowS;
  void fetchJwks;
  return { ok: false, status: 401, reason: "not implemented" };
}

// 受け入れ試験（課題 C1、test/access.test.ts。鍵はその場で作る）:
// 正しい JWT、署名の違い、alg=none・HS256、aud の違い、iss の違い、期限切れ、未来の nbf、
// 人のログインの JWT（email つき）、kid の入れ替え（取り直して通る）、知らない kid の連打で
// JWKS を 1 回しか取らない、ヘッダが無い。
