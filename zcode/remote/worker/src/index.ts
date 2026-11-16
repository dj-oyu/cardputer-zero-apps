// Worker の入口（docs/stage3-design.md §6.2）。雛形: 形と決まりだけ。中身は課題 C1。
//
//   実機 ──TLS──▶ Access（サービストークンを確かめ、JWT を付ける）──▶ この Worker
//     1. 口は 1 つだけ: GET /v1/ws（WebSocket の Upgrade）。ほかは 404
//     2. Access の JWT（ヘッダ Cf-Access-Jwt-Assertion）を確かめる        → access.ts
//     3. 身元を device_id に直す（DEVICES の表。無いトークンは 403）
//     4. つなぐ回数の制限（CONNECT_LIMIT。超えたら 429 + Retry-After）   → limits.ts
//     5. DO（名前 = device_id）へ渡す。依頼には X-Zc-Device だけを足し、
//        Access のヘッダ（JWT・Client-Id・Client-Secret）と Cookie は**落とす**
//
// Worker に秘密は置かない（JWT の確認は Access の公開鍵。ACCESS_* と DEVICES は公開しても
// 害の小さい識別子だが、アカウントごとの値なので secret として入れる）。
// 失敗の返事は短い JSON（{"error":"…"}）。理由を細かく教えない。

import { verifyAccessJwt, type AccessIdentity } from "./access";
import { checkConnectLimit } from "./limits";

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

export default {
  async fetch(request: Request, env: Env, _ctx: ExecutionContext): Promise<Response> {
    // TODO(C1): 上の 1〜5。
    void verifyAccessJwt;
    void checkConnectLimit;
    void env;
    void request;
    return json(501, "not implemented");
  },
} satisfies ExportedHandler<Env>;

/** Client ID → device_id。表に無ければ null（403）。device_id は [a-z0-9-]{1,32}。 */
export function deviceIdOf(identity: AccessIdentity, devicesJson: string): string | null {
  // TODO(C1)
  void identity;
  void devicesJson;
  return null;
}

export function json(status: number, error: string, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify({ error }), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store", ...headers },
  });
}
