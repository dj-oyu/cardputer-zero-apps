// 見本: 型・ジェネリクスと比較、テンプレートの入れ子、正規表現と割り算
import type { Readable } from "node:stream";
import { readFile } from 'node:fs/promises';

/** 設定。@deprecated ではない */
export interface Options<T = string> extends Base {
  readonly name: string;
  limit?: number;
  tags: Array<T> | null;
  onDone(result: Map<string, number[]>): void;
}

type Pair<K, V> = [key: K, value: V];
export type Mode = 'fast' | "slow" | `x-${string}`;
declare const VERSION: string;
const type = 1, from = type;

enum Color { Red = 1, Green = 0x10, Blue = 1_0 }

namespace Util {
  export const MAX_LEN = 100n;
}

abstract class Shape<T extends object> implements Options<T> {
  private readonly #id: number = 0;
  protected static count = 0;
  public tags: T[] = [];
  abstract area(): number;

  constructor(public name: string, private limit?: number) {
    super();
  }

  describe(this: Shape<T>): string {
    return `${this.name}:${this.area().toFixed(2)} ${this.tags.map((t) => `${String(t)}`).join(",")}`;
  }
}

@Injectable({ providedIn: 'root' })
export class Service {
  async load<T>(path: string, fallback?: T): Promise<T | undefined> {
    const text = (await readFile(path, "utf8")) as string;
    const parsed = JSON.parse(text) as unknown as T;
    return parsed ?? fallback;
  }
}

function first<T>(xs: readonly T[]): T | undefined {
  const n = xs.length, i = 0;
  if (i < n && n > (i + 1)) return xs[i];
  return undefined;
}

const ratio = (a: number, b: number): number => a / b / 2;
const ok = /^[a-z]+\/\d$/.test("a/1") && first<number>([1, 2]) !== void 0;
const cfg = { port: 8080, host: "0.0.0.0" } satisfies Record<string, unknown>;
const keys = Object.keys(cfg) as Array<keyof typeof cfg>;
const el = new Set<string>();
let u: unknown = NaN, big: bigint = 10n, s: symbol = Symbol("s");
