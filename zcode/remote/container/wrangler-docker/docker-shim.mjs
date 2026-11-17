#!/usr/bin/env node
// wrangler が呼ぶ docker のコマンドを、WSL コンテナー（wslc）のコマンドに直す（Windows で
// docker 無しに `wrangler deploy` するため）。deploy.ps1 が spawn-hook.cjs と組にして使う。
//
// wrangler 4.163.0 が deploy（durable_object 方式のイメージ）で呼ぶもの（wrangler-dist/cli.js
// を読んで確かめた）と、ここでの扱い:
//
//   docker info                                       → wslc info
//   docker build --load -t T --platform linux/amd64 --provenance=false
//                [--build-arg K=V]... -f - <文脈>     → wslc build -t T [--build-arg K=V]... -f - <文脈>
//                （Dockerfile は標準入力。--load・--provenance は wslc に無いので落とす。
//                  wslc はホストの linux/amd64 で作るので、それ以外の --platform は断る）
//   docker image inspect T --format <Go の書式>       → wslc image inspect T（JSON）を読んで、
//                wrangler が使う書式（{{ json .RepoDigests }}・{{ .Size }} {{ len .RootFS.Layers }}・
//                {{ .Os }}/{{ .Architecture }}）をここで埋める。wslc の --format は json だけ
//   docker login --password-stdin --username U <レジストリ> → wslc login（同じ引数）
//   docker tag S D                                   → wslc tag S D
//   docker push D                                    → wslc push D（出力の digest を控える。下）
//   docker manifest inspect -v R                     → wslc に無い。push で控えた digest から
//                {"Descriptor":{"digest":…}} を返す。知らなければ失敗で返す（wrangler は「まだ
//                無い」と読んで push する）
//   docker image rm T                                → wslc image remove T
//   それ以外（image ls・context ls など。wrangler の deploy では使わない） → 失敗で返す
//
// push の digest: docker は push のあと RepoDigests に「レジストリ/リポジトリ@digest」を足し、
// wrangler はそれを読む。wslc が足さない場合に備え、push の出力の `digest: sha256:…` を
// 控えのファイル（ZC_DOCKER_SHIM_STATE。秘密は入らない）に書き、image inspect の RepoDigests に
// 足して返す。
//
// 試し方（Cloudflare につながない）: zcode/AGENTS.md「deploy（wslc）」。
import { spawn, spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const WSLC = process.env.ZC_WSLC_BIN || "wslc";
const STATE = process.env.ZC_DOCKER_SHIM_STATE || join(tmpdir(), "zc-docker-shim-state.json");

function fail(msg, code = 1) {
  process.stderr.write(`docker-shim: ${msg}\n`);
  process.exit(code);
}

/** wslc を、標準入出力をそのまま渡して動かす（終了コードを返す）。 */
function passthrough(args) {
  return new Promise((resolve) => {
    const child = spawn(WSLC, args, { stdio: "inherit", windowsHide: true });
    child.on("error", (e) => fail(`wslc を起動できない: ${e.message}`, 127));
    child.on("close", (code) => resolve(code ?? 1));
  });
}

function readState() {
  try {
    return JSON.parse(readFileSync(STATE, "utf8"));
  } catch {
    return {};
  }
}

function writeState(s) {
  writeFileSync(STATE, JSON.stringify(s, null, 2));
}

/** 「レジストリ/リポジトリ:タグ」から「レジストリ/リポジトリ」（タグ・digest を外す）。 */
export function repoOf(ref) {
  const at = ref.indexOf("@");
  if (at >= 0) ref = ref.slice(0, at);
  const slash = ref.lastIndexOf("/");
  const colon = ref.lastIndexOf(":");
  return colon > slash ? ref.slice(0, colon) : ref;
}

/** push の出力から digest（sha256:…）を拾う。 */
export function digestFromPushOutput(text) {
  const m = /digest:\s*(sha256:[0-9a-f]{64})/i.exec(text);
  return m ? m[1] : null;
}

/** Go の書式のうち wrangler が使う形（{{ .A.B }}・{{ json .A }}・{{ len .A.B }}）を埋める。 */
export function renderFormat(fmt, obj) {
  const get = (path) => path.split(".").filter(Boolean).reduce((o, k) => (o == null ? undefined : o[k]), obj);
  return fmt.replace(/\{\{\s*(json\s+|len\s+)?(\.[A-Za-z0-9_.]*)\s*\}\}/g, (_, fn, path) => {
    const v = get(path);
    if (fn && fn.trim() === "json") return JSON.stringify(v ?? null);
    if (fn && fn.trim() === "len") return String(Array.isArray(v) || typeof v === "string" ? v.length : v && typeof v === "object" ? Object.keys(v).length : 0);
    return v == null ? "<no value>" : typeof v === "object" ? JSON.stringify(v) : String(v);
  });
}

/** wslc image inspect の結果に、控えた digest を RepoDigests として足す。 */
export function withRecordedDigests(info, ref, state) {
  const digests = new Set(Array.isArray(info.RepoDigests) ? info.RepoDigests : []);
  const tags = [ref, ...(Array.isArray(info.RepoTags) ? info.RepoTags : [])];
  for (const t of tags) {
    const d = state[t];
    if (d) digests.add(`${repoOf(t)}@${d}`);
  }
  return { ...info, RepoDigests: [...digests] };
}

async function build(args) {
  const out = ["build"];
  let context = null;
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    const next = () => {
      if (i + 1 >= args.length) fail(`${a} の値が無い`);
      return args[++i];
    };
    if (a === "--load" || a === "--provenance=false" || a === "--pull") {
      if (a === "--pull") out.push("--pull");
    } else if (a === "--provenance") {
      next();
    } else if (a === "--platform" || a.startsWith("--platform=")) {
      const p = a.includes("=") ? a.slice(a.indexOf("=") + 1) : next();
      if (p !== "linux/amd64") fail(`wslc は linux/amd64 でしか作れない（--platform ${p}）`);
    } else if (a === "-t" || a === "--tag" || a === "--build-arg" || a === "-f" || a === "--file" || a === "--target") {
      out.push(a, next());
    } else if (a.startsWith("--build-arg=") || a.startsWith("--tag=") || a.startsWith("--file=") || a.startsWith("--target=")) {
      const [k, ...v] = a.split("=");
      out.push(k, v.join("="));
    } else if (a === "--output" || a.startsWith("--output=") || a === "--network" || a.startsWith("--network=")) {
      fail(`${a} は wslc では使えない（WRANGLER_CONTAINER_IMAGE_COMPRESSION・WRANGLER_CI_OVERRIDE_NETWORK_MODE_HOST を外す）`);
    } else if (a.startsWith("-")) {
      fail(`build の知らない引数: ${a}`);
    } else {
      context = a;
    }
  }
  if (context === null) fail("build の文脈が無い");
  out.push(context);
  return passthrough(out);
}

function inspect(args) {
  let format = null;
  const refs = [];
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === "--format" || a === "-f") format = args[++i];
    else if (a.startsWith("--format=")) format = a.slice("--format=".length);
    else refs.push(a);
  }
  if (refs.length !== 1) fail("image inspect は 1 つずつ");
  const r = spawnSync(WSLC, ["image", "inspect", refs[0]], { encoding: "utf8", windowsHide: true });
  if (r.error) fail(`wslc を起動できない: ${r.error.message}`, 127);
  if (r.status !== 0) {
    process.stderr.write(r.stderr || r.stdout || "");
    return r.status ?? 1;
  }
  let parsed;
  try {
    parsed = JSON.parse(r.stdout);
  } catch {
    fail(`wslc image inspect の出力を JSON として読めない: ${r.stdout.slice(0, 200)}`);
  }
  const info = withRecordedDigests(Array.isArray(parsed) ? parsed[0] : parsed, refs[0], readState());
  process.stdout.write(format === null ? JSON.stringify([info], null, 4) + "\n" : renderFormat(format, info) + "\n");
  return 0;
}

function push(args) {
  const ref = args.filter((a) => !a.startsWith("-"))[0];
  if (!ref) fail("push する名前が無い");
  return new Promise((resolve) => {
    const child = spawn(WSLC, ["push", ref], { stdio: ["inherit", "pipe", "pipe"], windowsHide: true });
    let text = "";
    child.stdout.on("data", (b) => {
      text += b;
      process.stdout.write(b);
    });
    child.stderr.on("data", (b) => {
      text += b;
      process.stderr.write(b);
    });
    child.on("error", (e) => fail(`wslc を起動できない: ${e.message}`, 127));
    child.on("close", (code) => {
      if (code === 0) {
        const d = digestFromPushOutput(text);
        if (d) {
          const s = readState();
          s[ref] = d;
          writeState(s);
        } else {
          process.stderr.write("docker-shim: push の出力に digest が無かった（wrangler は RepoDigests か manifest inspect で探す）\n");
        }
      }
      resolve(code ?? 1);
    });
  });
}

function manifestInspect(args) {
  const ref = args.filter((a) => !a.startsWith("-"))[0];
  if (!ref) fail("manifest inspect する名前が無い");
  const s = readState();
  const at = ref.indexOf("@");
  const digest = at >= 0 ? (Object.values(s).includes(ref.slice(at + 1)) ? ref.slice(at + 1) : null) : s[ref];
  if (!digest) fail(`manifest inspect: ${ref} は知らない（wslc に manifest が無いので、push で控えた分だけ答える）`);
  process.stdout.write(JSON.stringify({ Ref: ref, Descriptor: { digest } }) + "\n");
  return 0;
}

async function main(argv) {
  const [cmd, sub, ...rest] = argv;
  switch (cmd) {
    case "info":
      return passthrough(["info"]);
    case "version":
    case "--version":
      return passthrough(["version"]);
    case "build":
      return build(argv.slice(1));
    case "buildx":
      if (sub === "build") return build(rest);
      break;
    case "login":
      return passthrough(argv);
    case "tag":
      return passthrough(argv);
    case "push":
      return push(argv.slice(1));
    case "rmi":
      return passthrough(["image", "remove", ...argv.slice(1)]);
    case "inspect":
      return inspect(argv.slice(1));
    case "manifest":
      if (sub === "inspect") return manifestInspect(rest);
      break;
    case "image":
      if (sub === "inspect") return inspect(rest);
      if (sub === "rm" || sub === "remove") return passthrough(["image", "remove", ...rest]);
      if (sub === "tag") return passthrough(["tag", ...rest]);
      if (sub === "push") return push(rest);
      break;
  }
  fail(`対応していない docker のコマンド: ${argv.join(" ")}`);
}

// 試験から読み込むときは動かさない
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main(process.argv.slice(2)).then((code) => process.exit(code), (e) => fail(e?.stack ?? String(e)));
}
