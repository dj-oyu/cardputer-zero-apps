// npm の依存がある TypeScript（ビルド係が npm ci → tsc → esbuild で 1 ファイルにして、実機の LLRT で動かす）。
import { nanoid } from "nanoid";

const ids: string[] = Array.from({ length: 3 }, () => nanoid(8));
console.log("ID:", ids.join(" "));
