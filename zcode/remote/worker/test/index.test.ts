// 課題 C1: Worker の入口（src/index.ts）。DO と Rate Limiting の束縛は偽物。

import { beforeAll, beforeEach, describe, expect, it } from "vitest";
import { resetJwksCache } from "../src/access";
import { DEVICE_HEADER, SUBPROTOCOL, deviceIdOf, handle, type Env } from "../src/index";
import { CONNECT_LIMIT_PERIOD_S, checkConnectLimit, positiveInt, secondsUntilUtcMidnight, utcDay } from "../src/limits";
import { AUD, CLIENT_ID, NOW, TEAM, fakeJwks, makeKey, servicePayload, signJwt, type TestKey } from "./jwt-helpers";

let key: TestKey;
beforeAll(async () => {
  key = await makeKey("kid-1");
});
beforeEach(() => resetJwksCache());

/** DO の偽物: 渡された依頼を覚え、名前ごとに返す。 */
function fakeDo() {
  const seen: { name: string; request: Request }[] = [];
  const ns = {
    idFromName: (name: string) => ({ name, toString: () => `id:${name}` }),
    get: (id: { name: string }) => ({
      fetch: async (request: Request) => {
        seen.push({ name: id.name, request });
        return new Response("to-do", { status: 200 });
      },
    }),
  };
  return { ns: ns as unknown as DurableObjectNamespace, seen };
}

/** Rate Limiting の偽物: key ごとに数え、limit を超えたら success=false。 */
function fakeLimiter(limit = 30) {
  const counts = new Map<string, number>();
  const rl = {
    keys: [] as string[],
    throws: false,
    limit: async ({ key }: { key: string }) => {
      if (rl.throws) throw new Error("binding down");
      rl.keys.push(key);
      const n = (counts.get(key) ?? 0) + 1;
      counts.set(key, n);
      return { success: n <= limit };
    },
  };
  return rl;
}

function makeEnv(over: Partial<Env> = {}) {
  const d = fakeDo();
  const limiter = fakeLimiter();
  const env: Env = {
    BUILD: d.ns,
    CONNECT_LIMIT: limiter as unknown as RateLimit,
    ACCESS_TEAM_DOMAIN: TEAM,
    ACCESS_AUD: AUD,
    DEVICES: JSON.stringify({ [CLIENT_ID]: "cz-1", "other.access": "cz-2" }),
    INSTANCE_TYPE: "standard-4",
    KEEP_WARM_S: "600",
    JOBS_PER_DAY: "400",
    ...over,
  };
  return { env, seen: d.seen, limiter };
}

async function wsRequest(opts: { jwt?: string | null; path?: string; headers?: Record<string, string> } = {}) {
  const jwt = opts.jwt === undefined ? await signJwt(key, servicePayload()) : opts.jwt;
  const headers: Record<string, string> = {
    Upgrade: "websocket",
    Connection: "Upgrade",
    "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
    "Sec-WebSocket-Version": "13",
    "Sec-WebSocket-Protocol": SUBPROTOCOL,
    ...opts.headers,
  };
  if (jwt !== null) headers["Cf-Access-Jwt-Assertion"] = jwt;
  return new Request(`https://build.example.com${opts.path ?? "/v1/ws"}`, { headers });
}

function run(req: Request, env: Env, jwks = fakeJwks([key])) {
  return handle(req, env, { nowS: () => NOW, fetchJwks: jwks.fetch });
}

describe("handle", () => {
  it("正しい依頼を device_id の DO へ渡す", async () => {
    const { env, seen, limiter } = makeEnv();
    const res = await run(await wsRequest(), env);
    expect(res.status).toBe(200);
    expect(await res.text()).toBe("to-do");
    expect(seen).toHaveLength(1);
    expect(seen[0]!.name).toBe("cz-1");
    expect(limiter.keys).toEqual(["cz-1"]);
  });

  it("Access のヘッダと Cookie を落とし、X-Zc-Device だけ足す", async () => {
    const { env, seen } = makeEnv();
    const req = await wsRequest({
      headers: {
        "CF-Access-Client-Id": CLIENT_ID,
        "CF-Access-Client-Secret": "secret",
        "Cf-Access-Authenticated-User-Email": "x@example.com",
        Cookie: "CF_Authorization=abc",
        [DEVICE_HEADER]: "spoofed",
      },
    });
    await run(req, env);
    const fwd = seen[0]!.request;
    const names = [...fwd.headers.keys()].map((n) => n.toLowerCase());
    expect(names.filter((n) => n.startsWith("cf-access-"))).toEqual([]);
    expect(fwd.headers.get("cookie")).toBeNull();
    expect(fwd.headers.get(DEVICE_HEADER)).toBe("cz-1");
    // WebSocket の Upgrade に要るヘッダは残す
    expect(fwd.headers.get("upgrade")).toBe("websocket");
    expect(fwd.headers.get("sec-websocket-key")).toBe("dGhlIHNhbXBsZSBub25jZQ==");
    expect(fwd.headers.get("sec-websocket-protocol")).toBe(SUBPROTOCOL);
    expect(fwd.method).toBe("GET");
    expect(new URL(fwd.url).pathname).toBe("/v1/ws");
  });

  it("ほかの口は 404", async () => {
    const { env, seen } = makeEnv();
    for (const path of ["/", "/v1/ws/", "/v1/status", "/healthz"]) {
      expect((await run(await wsRequest({ path }), env)).status).toBe(404);
    }
    const post = new Request("https://build.example.com/v1/ws", { method: "POST", body: "x" });
    expect((await run(post, env)).status).toBe(404);
    expect(seen).toHaveLength(0);
  });

  it("Upgrade でなければ 426", async () => {
    const { env, seen } = makeEnv();
    const res = await run(new Request("https://build.example.com/v1/ws"), env);
    expect(res.status).toBe(426);
    expect(res.headers.get("upgrade")).toBe("websocket");
    expect(await res.json()).toEqual({ error: "upgrade required" });
    expect(seen).toHaveLength(0);
  });

  it("サブプロトコルが違えば 400。並びの中にあれば通す", async () => {
    const { env, seen } = makeEnv();
    expect((await run(await wsRequest({ headers: { "Sec-WebSocket-Protocol": "chat" } }), env)).status).toBe(400);
    const noProto = await wsRequest();
    noProto.headers.delete("Sec-WebSocket-Protocol");
    expect((await run(noProto, env)).status).toBe(400);
    expect(seen).toHaveLength(0);
    const many = await wsRequest({ headers: { "Sec-WebSocket-Protocol": `chat, ${SUBPROTOCOL}` } });
    expect((await run(many, env)).status).toBe(200);
  });

  it("JWT が無い・正しくないと 401", async () => {
    const { env, seen, limiter } = makeEnv();
    expect((await run(await wsRequest({ jwt: null }), env)).status).toBe(401);
    const expired = await signJwt(key, servicePayload({ exp: NOW - 3600 }));
    const res = await run(await wsRequest({ jwt: expired }), env);
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: "unauthorized" });
    expect(seen).toHaveLength(0);
    expect(limiter.keys).toHaveLength(0);
  });

  it("人のログインの JWT は 403", async () => {
    const { env, seen } = makeEnv();
    const jwt = await signJwt(key, servicePayload({ email: "x@example.com", sub: "u" }));
    expect((await run(await wsRequest({ jwt }), env)).status).toBe(403);
    expect(seen).toHaveLength(0);
  });

  it("表に無いトークンは、Access を通っても 403", async () => {
    const { env, seen, limiter } = makeEnv();
    const jwt = await signJwt(key, servicePayload({ common_name: "stranger.access" }));
    const res = await run(await wsRequest({ jwt }), env);
    expect(res.status).toBe(403);
    expect(await res.json()).toEqual({ error: "forbidden" });
    expect(seen).toHaveLength(0);
    expect(limiter.keys).toHaveLength(0);
  });

  it("DEVICES が壊れていれば全部 403", async () => {
    const { env, seen } = makeEnv({ DEVICES: "not json" });
    expect((await run(await wsRequest(), env)).status).toBe(403);
    expect(seen).toHaveLength(0);
  });

  it("設定が無ければ 500（JWT を確かめずに通さない）", async () => {
    const { env, seen } = makeEnv({ ACCESS_AUD: "" });
    expect((await run(await wsRequest(), env)).status).toBe(500);
    expect(seen).toHaveLength(0);
  });

  it("つなぐ回数の上限を超えたら 429 + Retry-After", async () => {
    const { env, seen } = makeEnv();
    for (let i = 0; i < 30; i++) expect((await run(await wsRequest(), env)).status).toBe(200);
    const res = await run(await wsRequest(), env);
    expect(res.status).toBe(429);
    expect(res.headers.get("retry-after")).toBe(String(CONNECT_LIMIT_PERIOD_S));
    expect(seen).toHaveLength(30);

    // ほかの実機は別に数える
    const jwt = await signJwt(key, servicePayload({ common_name: "other.access" }));
    const r2 = await run(await wsRequest({ jwt }), env);
    expect(r2.status).toBe(200);
    expect(seen.at(-1)!.name).toBe("cz-2");
  });
});

describe("deviceIdOf", () => {
  const id = (clientId: string) => ({ clientId, exp: NOW });

  it("表のとおりに直す", () => {
    const t = JSON.stringify({ "a.access": "cz-1", "b.access": "cz-2" });
    expect(deviceIdOf(id("a.access"), t)).toBe("cz-1");
    expect(deviceIdOf(id("b.access"), t)).toBe("cz-2");
    expect(deviceIdOf(id("c.access"), t)).toBeNull();
  });

  it("device_id の形が違う行は使わない", () => {
    const t = JSON.stringify({
      "a.access": "CZ-1",
      "b.access": "x".repeat(33),
      "c.access": "",
      "d.access": 7,
      "e.access": "cz/1",
      "f.access": "ok-1",
    });
    for (const c of ["a", "b", "c", "d", "e"]) expect(deviceIdOf(id(`${c}.access`), t)).toBeNull();
    expect(deviceIdOf(id("f.access"), t)).toBe("ok-1");
  });

  it("プロトタイプの名前に引っかからない", () => {
    const t = JSON.stringify({ "a.access": "cz-1" });
    for (const c of ["__proto__", "constructor", "toString", "hasOwnProperty"]) {
      expect(deviceIdOf(id(c), t)).toBeNull();
    }
  });

  it("JSON でない・オブジェクトでない表は全部断る", () => {
    for (const t of ["", "nope", "[]", "null", '"cz-1"', "42"]) expect(deviceIdOf(id("a.access"), t)).toBeNull();
  });
});

describe("limits", () => {
  it("束縛が失敗したら通す（DO が正確な上限を守る）", async () => {
    const rl = fakeLimiter();
    rl.throws = true;
    expect(await checkConnectLimit(rl as unknown as RateLimit, "cz-1")).toBe(true);
  });

  it("UTC の日付と、翌日までの秒", () => {
    const t = Date.UTC(2026, 10, 17, 23, 59, 30);
    expect(utcDay(t)).toBe("2026-11-17");
    expect(secondsUntilUtcMidnight(t)).toBe(30);
    expect(utcDay(t + 30_000)).toBe("2026-11-18");
    expect(secondsUntilUtcMidnight(t + 30_000)).toBe(86_400);
    expect(secondsUntilUtcMidnight(Date.UTC(2026, 11, 31, 23, 59, 59, 999))).toBe(1);
  });

  it("vars の数を読む", () => {
    expect(positiveInt("400", 1)).toBe(400);
    expect(positiveInt(" 30 ", 1)).toBe(30);
    for (const v of [undefined, "", "0", "-1", "1.5", "abc", "1e3", "9999999999"]) expect(positiveInt(v, 7)).toBe(7);
  });
});
