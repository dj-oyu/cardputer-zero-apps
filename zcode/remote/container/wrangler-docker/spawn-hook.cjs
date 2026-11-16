// wrangler の中で、docker（= WRANGLER_DOCKER_BIN）を起動する呼び出しだけを
// `node docker-shim.mjs <引数>` に差し替える。deploy.ps1 が NODE_OPTIONS の --require で読ませる。
//
// なぜ要るか: wrangler は WRANGLER_DOCKER_BIN を shell なしで spawn / execFileSync する。
// Windows の Node は .cmd / .bat を shell なしでは起動しない（CVE-2024-27980 の対策で EINVAL）、
// .ps1 も起動できない。そこで WRANGLER_DOCKER_BIN には docker-shim.mjs のパスを入れておき、
// それを起動しようとした呼び出しだけを、Node 自身で shim を動かす形に直す。
// ほかの子プロセス（esbuild・workerd など）には触らない。
"use strict";
const cp = require("node:child_process");
const path = require("node:path");

const target = process.env.WRANGLER_DOCKER_BIN;
if (target && target.toLowerCase().endsWith(".mjs")) {
  const shim = path.resolve(target);
  const isShim = (file) => typeof file === "string" && (file === target || path.resolve(file) === shim);
  // (file, args?, options?) の形の呼び出しを (node, [shim, ...args], options) にする
  const wrap = (orig) =>
    function (file, args, ...rest) {
      if (isShim(file)) {
        const list = Array.isArray(args) ? args : [];
        const tail = Array.isArray(args) ? rest : [args, ...rest].filter((x) => x !== undefined);
        return orig.call(this, process.execPath, [shim, ...list], ...tail);
      }
      return orig.call(this, file, args, ...rest);
    };
  for (const name of ["spawn", "spawnSync", "execFile", "execFileSync"]) {
    cp[name] = wrap(cp[name]);
  }
}
