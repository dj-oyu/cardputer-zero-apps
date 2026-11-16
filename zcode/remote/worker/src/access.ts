// Access の JWT の確認（docs/stage3-design.md §6.2）。課題 C1。
//
// - ヘッダ Cf-Access-Jwt-Assertion の JWT を Web Crypto で確かめる（ライブラリを足さない）
// - 公開鍵は https://<ACCESS_TEAM_DOMAIN>/cdn-cgi/access/certs（JWKS）。kid で選ぶ。
//   モジュールの変数に覚え、1 時間で取り直す。知らない kid が来たら 1 回だけ取り直す
//   （鍵の入れ替え）。取り直しは 60 秒に 1 回まで（知らない kid を投げ続けられても、
//   Access を叩き続けない）
// - 確かめる項目: 署名（RS256。alg がそれ以外なら断る。"none" は当然断る）、
//   iss = https://<ACCESS_TEAM_DOMAIN>、aud に ACCESS_AUD を含む、exp（60 秒のずれまで許す）、
//   nbf / iat が未来でない（同じく 60 秒のずれまで許す）
// - サービストークンの JWT は sub が空で、common_name にトークンの Client ID が入る。
//   人のログイン（email がある・sub が空でない）は受け付けない（このアプリは機械専用）
// - 確かめるのはつなぐとき（Upgrade）だけ。長い接続の途中で exp が来ても切らない
//
// 返事の status: JWT として正しくない（無い・壊れている・署名・iss・aud・期限・知らない kid）は
// 401、正しいがサービストークンではない（人のログイン）は 403。
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

/** JWT を運ぶヘッダ（Access が付ける）。 */
export const JWT_HEADER = "Cf-Access-Jwt-Assertion";
/** exp・nbf・iat に許す時計のずれ（秒）。 */
export const CLOCK_SKEW_S = 60;
/** JWKS を覚えておく時間（秒）。 */
export const JWKS_TTL_S = 3600;
/** JWKS を取り直す最短の間隔（秒）。 */
export const JWKS_MIN_REFETCH_S = 60;
/** 受け付ける JWT の長さの上限（Access の JWT は 1KB 前後）。 */
const MAX_JWT_LEN = 8192;

/** 公開鍵の覚え（モジュールの変数。isolate ごとに 1 つ）。 */
interface JwksCache {
  url: string;
  keys: Map<string, CryptoKey>;
  /** 最後に取れた時刻（秒）。 */
  fetchedS: number;
  /** 最後に取りに行った時刻（秒。失敗も含む）。 */
  attemptS: number;
  /** 取りに行っている途中なら、その約束（同時に来た依頼で 1 回にまとめる）。 */
  inflight: Promise<void> | null;
}

let cache: JwksCache | null = null;

/** 試験用: 覚えている公開鍵を捨てる。 */
export function resetJwksCache(): void {
  cache = null;
}

export async function verifyAccessJwt(
  request: Request,
  cfg: AccessConfig,
  nowS: number,
  fetchJwks: JwksFetcher = defaultFetchJwks,
): Promise<AccessResult> {
  const token = request.headers.get(JWT_HEADER);
  if (!token) return deny(401, "missing token");
  if (token.length > MAX_JWT_LEN) return deny(401, "token too long");

  const parts = token.split(".");
  if (parts.length !== 3 || parts.some((p) => p.length === 0)) return deny(401, "malformed token");
  const [h64, p64, s64] = parts as [string, string, string];

  const header = decodeJson(h64);
  const payload = decodeJson(p64);
  const sig = b64urlDecode(s64);
  if (!header || !payload || !sig) return deny(401, "malformed token");

  // alg は RS256 だけ（none・HS256 などの取り違えを断る）。
  if (header.alg !== "RS256") return deny(401, "bad alg");
  if (typeof header.kid !== "string" || header.kid.length === 0) return deny(401, "missing kid");

  // 署名（鍵の取得は設定の値から作る URL だけ。JWT の中の値で取りに行かない）。
  const key = await keyFor(header.kid, cfg.teamDomain, nowS, fetchJwks);
  if (!key) return deny(401, "unknown kid");
  const signed = new TextEncoder().encode(`${h64}.${p64}`);
  let valid = false;
  try {
    valid = await crypto.subtle.verify("RSASSA-PKCS1-v1_5", key, sig, signed);
  } catch {
    valid = false;
  }
  if (!valid) return deny(401, "bad signature");

  // 中身。
  if (payload.iss !== `https://${cfg.teamDomain}`) return deny(401, "bad iss");
  const aud = payload.aud;
  const audOk = typeof aud === "string" ? aud === cfg.aud : Array.isArray(aud) && aud.includes(cfg.aud);
  if (!cfg.aud || !audOk) return deny(401, "bad aud");
  const exp = payload.exp;
  if (typeof exp !== "number" || !Number.isFinite(exp)) return deny(401, "missing exp");
  if (nowS > exp + CLOCK_SKEW_S) return deny(401, "expired");
  for (const name of ["nbf", "iat"] as const) {
    const v = payload[name];
    if (v === undefined) continue;
    if (typeof v !== "number" || !Number.isFinite(v)) return deny(401, `bad ${name}`);
    if (v > nowS + CLOCK_SKEW_S) return deny(401, `${name} in future`);
  }

  // サービストークンだけ（人のログインは email と sub を持つ）。
  if (typeof payload.email === "string" && payload.email.length > 0) return deny(403, "not a service token");
  if (typeof payload.sub === "string" && payload.sub.length > 0) return deny(403, "not a service token");
  const cn = payload.common_name;
  if (typeof cn !== "string" || cn.length === 0) return deny(403, "not a service token");

  return { ok: true, identity: { clientId: cn, exp } };
}

function deny(status: 401 | 403, reason: string): AccessResult {
  return { ok: false, status, reason };
}

// ---- 公開鍵（JWKS）----

async function keyFor(
  kid: string,
  teamDomain: string,
  nowS: number,
  fetchJwks: JwksFetcher,
): Promise<CryptoKey | null> {
  const url = `https://${teamDomain}/cdn-cgi/access/certs`;
  if (!cache || cache.url !== url) {
    cache = { url, keys: new Map(), fetchedS: -Infinity, attemptS: -Infinity, inflight: null };
  }
  const c = cache;

  const stale = nowS - c.fetchedS >= JWKS_TTL_S;
  const missing = !c.keys.has(kid);
  if ((stale || missing) && (c.inflight || nowS - c.attemptS >= JWKS_MIN_REFETCH_S)) {
    if (!c.inflight) {
      c.attemptS = nowS;
      c.inflight = refresh(c, nowS, fetchJwks).finally(() => {
        c.inflight = null;
      });
    }
    await c.inflight;
  }
  return c.keys.get(kid) ?? null;
}

/** 取り直す。失敗したら、前の鍵をそのまま使い続ける。 */
async function refresh(c: JwksCache, nowS: number, fetchJwks: JwksFetcher): Promise<void> {
  let jwks: { keys: JsonWebKey[] };
  try {
    jwks = await fetchJwks(c.url);
  } catch (e) {
    console.error("access: JWKS fetch failed", String(e));
    return;
  }
  if (!jwks || !Array.isArray(jwks.keys)) {
    console.error("access: JWKS has no keys");
    return;
  }
  const keys = new Map<string, CryptoKey>();
  for (const jwk of jwks.keys as Array<JsonWebKey & { kid?: unknown }>) {
    if (!jwk || jwk.kty !== "RSA" || typeof jwk.kid !== "string") continue;
    if (jwk.alg !== undefined && jwk.alg !== "RS256") continue;
    try {
      const key = await crypto.subtle.importKey(
        "jwk",
        { kty: "RSA", n: jwk.n, e: jwk.e, alg: "RS256", ext: true },
        { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
        false,
        ["verify"],
      );
      keys.set(jwk.kid, key);
    } catch {
      // 読めない鍵は飛ばす
    }
  }
  if (keys.size === 0) {
    console.error("access: JWKS has no usable RSA key");
    return;
  }
  c.keys = keys;
  c.fetchedS = nowS;
}

async function defaultFetchJwks(url: string): Promise<{ keys: JsonWebKey[] }> {
  const res = await fetch(url, { headers: { accept: "application/json" } });
  if (!res.ok) throw new Error(`JWKS ${res.status}`);
  return (await res.json()) as { keys: JsonWebKey[] };
}

// ---- base64url と JSON ----

function b64urlDecode(s: string): Uint8Array | null {
  if (!/^[A-Za-z0-9_-]+$/.test(s)) return null;
  const b64 = s.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((s.length + 3) % 4);
  try {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

function decodeJson(s: string): Record<string, unknown> | null {
  const bytes = b64urlDecode(s);
  if (!bytes) return null;
  try {
    const v: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes));
    return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

// 受け入れ試験（test/access.test.ts。鍵はその場で作る）:
// 正しい JWT、署名の違い、alg=none・HS256、aud の違い、iss の違い、期限切れ、未来の nbf、
// 人のログインの JWT（email つき）、kid の入れ替え（取り直して通る）、知らない kid の連打で
// JWKS を 1 回しか取らない、ヘッダが無い。
