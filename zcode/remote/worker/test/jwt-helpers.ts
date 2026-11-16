// 試験用: その場で作った RSA の鍵で Access 風の JWT を作る（課題 C1 の試験が使う）。

export const TEAM = "team.example.cloudflareaccess.com";
export const AUD = "a".repeat(64);
export const CLIENT_ID = "0123456789abcdef.access";
export const NOW = 1_800_000_000;

export interface TestKey {
  kid: string;
  pair: CryptoKeyPair;
  jwk: JsonWebKey & { kid: string };
}

export async function makeKey(kid: string): Promise<TestKey> {
  const pair = (await crypto.subtle.generateKey(
    {
      name: "RSASSA-PKCS1-v1_5",
      modulusLength: 2048,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash: "SHA-256",
    },
    true,
    ["sign", "verify"],
  )) as CryptoKeyPair;
  const pub = (await crypto.subtle.exportKey("jwk", pair.publicKey)) as JsonWebKey;
  return { kid, pair, jwk: { kty: "RSA", n: pub.n, e: pub.e, alg: "RS256", use: "sig", kid } as JsonWebKey & { kid: string } };
}

export function b64url(data: Uint8Array | string): string {
  const bytes = typeof data === "string" ? new TextEncoder().encode(data) : data;
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** サービストークンで通ったときの JWT の中身（Access の実物に近い形）。 */
export function servicePayload(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    type: "app",
    aud: [AUD],
    iss: `https://${TEAM}`,
    common_name: CLIENT_ID,
    sub: "",
    iat: NOW - 10,
    nbf: NOW - 10,
    exp: NOW + 3600,
    ...over,
  };
}

/** RS256 で署名した JWT。header を上書きすれば alg・kid を変えられる。 */
export async function signJwt(
  key: TestKey,
  payload: Record<string, unknown>,
  header: Record<string, unknown> = {},
): Promise<string> {
  const h = b64url(JSON.stringify({ alg: "RS256", kid: key.kid, typ: "JWT", ...header }));
  const p = b64url(JSON.stringify(payload));
  const sig = new Uint8Array(
    await crypto.subtle.sign("RSASSA-PKCS1-v1_5", key.pair.privateKey, new TextEncoder().encode(`${h}.${p}`)),
  );
  return `${h}.${p}.${b64url(sig)}`;
}

/** JWKS を返す偽物。呼ばれた回数を数え、返す鍵を差し替えられる。 */
export function fakeJwks(keys: TestKey[]) {
  const state = {
    calls: 0,
    urls: [] as string[],
    keys,
    fail: false,
    fetch: async (url: string): Promise<{ keys: JsonWebKey[] }> => {
      state.calls++;
      state.urls.push(url);
      if (state.fail) throw new Error("down");
      return { keys: state.keys.map((k) => k.jwk) };
    },
  };
  return state;
}

export function reqWithJwt(jwt: string | null, extra: Record<string, string> = {}): Request {
  const headers: Record<string, string> = { ...extra };
  if (jwt !== null) headers["Cf-Access-Jwt-Assertion"] = jwt;
  return new Request("https://build.example.com/v1/ws", { headers });
}
