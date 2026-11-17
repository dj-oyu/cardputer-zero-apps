// 課題 C1: Access の JWT の確認（src/access.ts の末尾の一覧）。鍵はその場で作る。

import { beforeAll, beforeEach, describe, expect, it } from "vitest";
import { CLOCK_SKEW_S, JWKS_MIN_REFETCH_S, JWKS_TTL_S, resetJwksCache, verifyAccessJwt } from "../src/access";
import {
  AUD,
  CLIENT_ID,
  NOW,
  TEAM,
  b64url,
  fakeJwks,
  makeKey,
  reqWithJwt,
  servicePayload,
  signJwt,
  type TestKey,
} from "./jwt-helpers";

const cfg = { teamDomain: TEAM, aud: AUD };
let k1: TestKey;
let k2: TestKey;
let other: TestKey;

beforeAll(async () => {
  [k1, k2, other] = await Promise.all([makeKey("kid-1"), makeKey("kid-2"), makeKey("kid-1")]);
});

beforeEach(() => resetJwksCache());

async function verify(jwt: string | null, jwks = fakeJwks([k1]), now = NOW) {
  return verifyAccessJwt(reqWithJwt(jwt), cfg, now, jwks.fetch);
}

describe("verifyAccessJwt", () => {
  it("正しいサービストークンの JWT を通す", async () => {
    const jwks = fakeJwks([k1]);
    const r = await verify(await signJwt(k1, servicePayload()), jwks);
    expect(r).toEqual({ ok: true, identity: { clientId: CLIENT_ID, exp: NOW + 3600 } });
    expect(jwks.urls).toEqual([`https://${TEAM}/cdn-cgi/access/certs`]);
  });

  it("aud が文字列 1 つでも通す", async () => {
    const r = await verify(await signJwt(k1, servicePayload({ aud: AUD })));
    expect(r.ok).toBe(true);
  });

  it("ヘッダが無ければ 401", async () => {
    const jwks = fakeJwks([k1]);
    expect(await verify(null, jwks)).toMatchObject({ ok: false, status: 401 });
    expect(jwks.calls).toBe(0);
  });

  it("壊れた JWT は 401（JWKS を取りに行かない）", async () => {
    const jwks = fakeJwks([k1]);
    for (const bad of ["", "a.b", "a.b.c.d", "..", "!!!.e30.sig", `${b64url("{")}.${b64url("{}")}.AAAA`]) {
      expect(await verify(bad, jwks)).toMatchObject({ ok: false, status: 401 });
    }
    expect(jwks.calls).toBe(0);
  });

  it("署名の違い（別の鍵で署名・中身の書き換え）は 401", async () => {
    // 同じ kid の別の鍵で署名
    expect(await verify(await signJwt(other, servicePayload()))).toMatchObject({
      ok: false,
      status: 401,
      reason: "bad signature",
    });
    // 署名のあとで中身を書き換える
    const good = await signJwt(k1, servicePayload());
    const [h, , s] = good.split(".");
    const forged = `${h}.${b64url(JSON.stringify(servicePayload({ common_name: "evil.access" })))}.${s}`;
    expect(await verify(forged)).toMatchObject({ ok: false, status: 401, reason: "bad signature" });
  });

  it("alg=none・HS256 は断る", async () => {
    const p = b64url(JSON.stringify(servicePayload()));
    const none = `${b64url(JSON.stringify({ alg: "none", kid: "kid-1" }))}.${p}.`;
    expect(await verify(none)).toMatchObject({ ok: false, status: 401 });
    const noneSig = `${b64url(JSON.stringify({ alg: "none", kid: "kid-1" }))}.${p}.AAAA`;
    expect(await verify(noneSig)).toMatchObject({ ok: false, status: 401, reason: "bad alg" });

    // HS256: 公開鍵の n を共有の秘密に使う古典的な取り違え
    const h = b64url(JSON.stringify({ alg: "HS256", kid: "kid-1" }));
    const hmac = await crypto.subtle.importKey(
      "raw",
      new TextEncoder().encode(k1.jwk.n as string),
      { name: "HMAC", hash: "SHA-256" },
      false,
      ["sign"],
    );
    const mac = new Uint8Array(await crypto.subtle.sign("HMAC", hmac, new TextEncoder().encode(`${h}.${p}`)));
    expect(await verify(`${h}.${p}.${b64url(mac)}`)).toMatchObject({ ok: false, status: 401, reason: "bad alg" });

    // RS256 の署名に別の alg を名乗らせても断る
    expect(await verify(await signJwt(k1, servicePayload(), { alg: "RS512" }))).toMatchObject({
      ok: false,
      reason: "bad alg",
    });
  });

  it("aud の違いは 401", async () => {
    for (const aud of [["b".repeat(64)], "b".repeat(64), [], undefined]) {
      const r = await verify(await signJwt(k1, servicePayload({ aud })));
      expect(r).toMatchObject({ ok: false, status: 401, reason: "bad aud" });
    }
  });

  it("iss の違いは 401", async () => {
    for (const iss of [`https://other.cloudflareaccess.com`, TEAM, `http://${TEAM}`, undefined]) {
      const r = await verify(await signJwt(k1, servicePayload({ iss })));
      expect(r).toMatchObject({ ok: false, status: 401, reason: "bad iss" });
    }
  });

  it("期限切れは 401。60 秒のずれまでは通す", async () => {
    const jwt = await signJwt(k1, servicePayload({ exp: NOW - 1 }));
    expect((await verify(jwt, fakeJwks([k1]), NOW + CLOCK_SKEW_S - 1)).ok).toBe(true);
    resetJwksCache();
    expect(await verify(jwt, fakeJwks([k1]), NOW + CLOCK_SKEW_S + 1)).toMatchObject({
      ok: false,
      status: 401,
      reason: "expired",
    });
    expect(await verify(await signJwt(k1, servicePayload({ exp: undefined })))).toMatchObject({
      ok: false,
      reason: "missing exp",
    });
  });

  it("未来の nbf・iat は 401。60 秒のずれまでは通す", async () => {
    expect((await verify(await signJwt(k1, servicePayload({ nbf: NOW + CLOCK_SKEW_S - 1 })))).ok).toBe(true);
    expect(await verify(await signJwt(k1, servicePayload({ nbf: NOW + CLOCK_SKEW_S + 5 })))).toMatchObject({
      ok: false,
      status: 401,
      reason: "nbf in future",
    });
    expect(await verify(await signJwt(k1, servicePayload({ iat: NOW + 600 })))).toMatchObject({
      ok: false,
      reason: "iat in future",
    });
    expect(await verify(await signJwt(k1, servicePayload({ nbf: "soon" })))).toMatchObject({
      ok: false,
      reason: "bad nbf",
    });
  });

  it("人のログインの JWT（email つき）は 403", async () => {
    const human = servicePayload({ email: "someone@example.com", sub: "user-uuid", common_name: undefined });
    expect(await verify(await signJwt(k1, human))).toMatchObject({ ok: false, status: 403 });
    // email だけ・sub だけでも断る
    expect(await verify(await signJwt(k1, servicePayload({ email: "someone@example.com" })))).toMatchObject({
      ok: false,
      status: 403,
    });
    expect(await verify(await signJwt(k1, servicePayload({ sub: "user-uuid" })))).toMatchObject({
      ok: false,
      status: 403,
    });
    // common_name が無い
    expect(await verify(await signJwt(k1, servicePayload({ common_name: "" })))).toMatchObject({
      ok: false,
      status: 403,
    });
  });

  it("JWKS は 1 時間覚える", async () => {
    const jwks = fakeJwks([k1]);
    const jwt = await signJwt(k1, servicePayload({ exp: NOW + 10_000 }));
    expect((await verify(jwt, jwks, NOW)).ok).toBe(true);
    expect((await verify(jwt, jwks, NOW + JWKS_TTL_S - 1)).ok).toBe(true);
    expect(jwks.calls).toBe(1);
    expect((await verify(jwt, jwks, NOW + JWKS_TTL_S)).ok).toBe(true);
    expect(jwks.calls).toBe(2);
  });

  it("kid の入れ替え: 知らない kid で取り直して通る", async () => {
    const jwks = fakeJwks([k1]);
    expect((await verify(await signJwt(k1, servicePayload()), jwks, NOW)).ok).toBe(true);
    // Access が鍵を入れ替えた（k2 が足され、k1 は残る）
    jwks.keys = [k2, k1];
    const t = NOW + JWKS_MIN_REFETCH_S;
    expect((await verify(await signJwt(k2, servicePayload()), jwks, t)).ok).toBe(true);
    expect(jwks.calls).toBe(2);
    expect((await verify(await signJwt(k1, servicePayload()), jwks, t)).ok).toBe(true);
    expect(jwks.calls).toBe(2);
  });

  it("知らない kid の連打でも、JWKS は 60 秒に 1 回しか取らない", async () => {
    const jwks = fakeJwks([k1]);
    expect((await verify(await signJwt(k1, servicePayload()), jwks, NOW)).ok).toBe(true);
    expect(jwks.calls).toBe(1);

    // 取ったばかりなので、知らない kid では取り直さない
    for (let i = 0; i < 20; i++) {
      const jwt = await signJwt(k1, servicePayload(), { kid: `unknown-${i}` });
      expect(await verify(jwt, jwks, NOW + 1)).toMatchObject({ ok: false, status: 401, reason: "unknown kid" });
    }
    expect(jwks.calls).toBe(1);

    // 60 秒たったら 1 回だけ取り直す。そのあとの連打では取らない
    for (let i = 0; i < 20; i++) {
      const jwt = await signJwt(k1, servicePayload(), { kid: `unknown-${i}` });
      expect((await verify(jwt, jwks, NOW + JWKS_MIN_REFETCH_S + i)).ok).toBe(false);
    }
    expect(jwks.calls).toBe(2);
  });

  it("同時に来た知らない kid は、取り直しを 1 回にまとめる", async () => {
    const jwks = fakeJwks([k1]);
    const jwt = await signJwt(k1, servicePayload(), { kid: "nope" });
    const rs = await Promise.all(Array.from({ length: 10 }, () => verify(jwt, jwks, NOW)));
    expect(rs.every((r) => !r.ok)).toBe(true);
    expect(jwks.calls).toBe(1);
  });

  it("取り直しに失敗しても、前の鍵で通し続ける", async () => {
    const jwks = fakeJwks([k1]);
    const jwt = await signJwt(k1, servicePayload({ exp: NOW + 10_000 }));
    expect((await verify(jwt, jwks, NOW)).ok).toBe(true);
    jwks.fail = true;
    expect((await verify(jwt, jwks, NOW + JWKS_TTL_S + 1)).ok).toBe(true);
    expect(jwks.calls).toBe(2);
  });

  it("JWKS が取れなければ 401", async () => {
    const jwks = fakeJwks([k1]);
    jwks.fail = true;
    expect(await verify(await signJwt(k1, servicePayload()), jwks)).toMatchObject({
      ok: false,
      status: 401,
      reason: "unknown kid",
    });
  });

  it("JWKS の RSA 以外・RS256 以外の鍵は使わない", async () => {
    const jwks = fakeJwks([k1]);
    const bad = { ...k1, jwk: { ...k1.jwk, alg: "RS512" } };
    jwks.keys = [bad];
    expect(await verify(await signJwt(k1, servicePayload()), jwks)).toMatchObject({ ok: false, status: 401 });
  });
});
