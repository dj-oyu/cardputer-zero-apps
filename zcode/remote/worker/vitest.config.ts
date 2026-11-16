// 試験は workerd ではなく Node の上で回す（session.test.ts が node:sqlite を使うので
// Node 22.13 以上）。Cloudflare の口（ctx.container・WebSocketPair）は試験では使わない。
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["test/**/*.test.ts"],
  },
});
