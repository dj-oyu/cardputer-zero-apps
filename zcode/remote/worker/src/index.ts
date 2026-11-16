// Worker の入口（docs/stage3-design.md §6.2）。課題 C1。
//
//   実機 ──TLS──▶ Access（サービストークンを確かめ、JWT を付ける）──▶ この Worker
//     1. 口は 1 つだけ: GET /v1/ws（WebSocket の Upgrade）。ほかは 404。
//        Upgrade でなければ 426、サブプロトコルに zc-build.v1 が無ければ 400
//     2. Access の JWT（ヘッダ Cf-Access-Jwt-Assertion）を確かめる        → access.ts
//     3. 身元を device_id に直す（DEVICES の表。無いトークンは 403）
//     4. つなぐ回数の制限（CONNECT_LIMIT。超えたら 429 + Retry-After）   → limits.ts
//     5. DO（名前 = device_id）へ渡す。依頼には X-Zc-Device だけを足し、
//        Access のヘッダ（JWT・Client-Id・Client-Secret など cf-access-*）と Cookie は**落とす**
//
// 安い確かめ（口・Upgrade・サブプロトコル）を先にする。JWT の前でも、Access を通らない
// 依頼はそもそも届かない（§12.1 の 9: ヘッダありの curl は 426 になる）。
//
// Worker に秘密は置かない（JWT の確認は Access の公開鍵。ACCESS_* と DEVICES は公開しても
// 害の小さい識別子だが、アカウントごとの値なので secret として入れる）。
// 失敗の返事は短い JSON（{"error":"…"}）。理由を細かく教えない（細かい理由はログにだけ）。

import { verifyAccessJwt, type AccessIdentity, type JwksFetcher } from "./access";
import { checkConnectLimit, CONNECT_LIMIT_PERIOD_S } from "./limits";

export { BuildSession } from "./session";

export interface Env {
  BUILD: DurableObjectNamespace;
  CONNECT_LIMIT: RateLimit;
  ACCESS_TEAM_DOMAIN: string;
  ACCESS_AUD: string;
  /** JSON: { "<client-id>.access": "<device_id>" } */
  DEVICES: string;
  INSTANCE_TYPE: string;
  KEEP_WARM_S: string;
  JOBS_PER_DAY: string;
}

/** WebSocket の口（実機側の zc-wire の WS_PATH と同じ）。 */
export const WS_PATH = "/v1/ws";
/** Sec-WebSocket-Protocol（zc-wire の SUBPROTOCOL と同じ）。 */
export const SUBPROTOCOL = "zc-build.v1";
/** DO と受付に渡す、身元のヘッダ。 */
export const DEVICE_HEADER = "X-Zc-Device";

/** 試験で、時計と JWKS の取り方を差し替える口。本番は既定のまま。 */
export interface Deps {
  nowS?: () => number;
  fetchJwks?: JwksFetcher;
}

export default {
  async fetch(request: Request, env: Env, _ctx: ExecutionContext): Promise<Response> {
    return handle(request, env);
  },
} satisfies ExportedHandler<Env>;

/** fetch の中身（試験から直接呼ぶ）。 */
export async function handle(request: Request, env: Env, deps: Deps = {}): Promise<Response> {
  // 1. 口
  const url = new URL(request.url);
  if (url.pathname !== WS_PATH || request.method !== "GET") return json(404, "not found");
  if ((request.headers.get("Upgrade") ?? "").toLowerCase() !== "websocket") {
    return json(426, "upgrade required", { Upgrade: "websocket" });
  }
  if (!offersSubprotocol(request.headers.get("Sec-WebSocket-Protocol"))) {
    return json(400, "bad subprotocol");
  }

  // 2. JWT
  if (!env.ACCESS_TEAM_DOMAIN || !env.ACCESS_AUD) {
    console.error("worker: ACCESS_TEAM_DOMAIN / ACCESS_AUD not set");
    return json(500, "misconfigured");
  }
  const nowS = deps.nowS ? deps.nowS() : Math.floor(Date.now() / 1000);
  const auth = await verifyAccessJwt(
    request,
    { teamDomain: env.ACCESS_TEAM_DOMAIN, aud: env.ACCESS_AUD },
    nowS,
    deps.fetchJwks,
  );
  if (!auth.ok) {
    console.log("worker: access denied", auth.status, auth.reason);
    return json(auth.status, auth.status === 401 ? "unauthorized" : "forbidden");
  }

  // 3. device_id
  const deviceId = deviceIdOf(auth.identity, env.DEVICES ?? "");
  if (!deviceId) {
    console.log("worker: unknown service token");
    return json(403, "forbidden");
  }

  // 4. つなぐ回数
  if (!(await checkConnectLimit(env.CONNECT_LIMIT, deviceId))) {
    return json(429, "rate limited", { "Retry-After": String(CONNECT_LIMIT_PERIOD_S) });
  }

  // 5. DO へ
  const stub = env.BUILD.get(env.BUILD.idFromName(deviceId));
  return stub.fetch(forwardRequest(request, deviceId));
}

/** Sec-WebSocket-Protocol（カンマ区切り）に zc-build.v1 があるか。 */
function offersSubprotocol(header: string | null): boolean {
  if (!header) return false;
  return header.split(",").some((p) => p.trim() === SUBPROTOCOL);
}

/** DO へ渡す依頼。Access のヘッダと Cookie を落とし、X-Zc-Device だけを足す。 */
export function forwardRequest(request: Request, deviceId: string): Request {
  const headers = new Headers();
  for (const [name, value] of request.headers) {
    const n = name.toLowerCase();
    if (n.startsWith("cf-access-") || n === "cookie" || n === DEVICE_HEADER.toLowerCase()) continue;
    headers.append(name, value);
  }
  headers.set(DEVICE_HEADER, deviceId);
  return new Request(request.url, { method: "GET", headers });
}

const DEVICE_ID_RE = /^[a-z0-9-]{1,32}$/;
let devicesMemo: { json: string; table: Map<string, string> | null } | null = null;

/** Client ID → device_id。表に無ければ null（403）。device_id は [a-z0-9-]{1,32}。 */
export function deviceIdOf(identity: AccessIdentity, devicesJson: string): string | null {
  if (!devicesMemo || devicesMemo.json !== devicesJson) {
    devicesMemo = { json: devicesJson, table: parseDevices(devicesJson) };
  }
  return devicesMemo.table?.get(identity.clientId) ?? null;
}

/** DEVICES の表を読む。形が違えば null（全部断る）。device_id の形が違う行は捨てる。 */
function parseDevices(devicesJson: string): Map<string, string> | null {
  let v: unknown;
  try {
    v = JSON.parse(devicesJson);
  } catch {
    console.error("worker: DEVICES is not JSON");
    return null;
  }
  if (!v || typeof v !== "object" || Array.isArray(v)) {
    console.error("worker: DEVICES is not an object");
    return null;
  }
  const table = new Map<string, string>();
  for (const [clientId, deviceId] of Object.entries(v as Record<string, unknown>)) {
    if (typeof deviceId === "string" && DEVICE_ID_RE.test(deviceId) && clientId.length > 0) {
      table.set(clientId, deviceId);
    } else {
      console.error("worker: DEVICES has a bad entry");
    }
  }
  return table;
}

export function json(status: number, error: string, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify({ error }), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store", ...headers },
  });
}
