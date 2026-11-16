#!/usr/bin/env node
// 見本: テンプレートの入れ子、正規表現と割り算、private、デコレータ
'use strict';

import { readFile } from 'node:fs/promises';
import * as path from "node:path";

/**
 * 行を数える。
 * @param {string} text 本文
 * @returns {number}
 */
export async function countLines(text, limit = 1_000) {
  const lines = text.split(/\r?\n/);
  return lines.length > limit ? limit : lines.length;
}

const MAX_LEN = 0xFF + 0b101 + 0o17 + 12n + 1.5e-3 + .5;
const ratio = MAX_LEN / 2 / 3, big = 10n ** 3n;
const re = /[/\]]+\/(?:a|b)+$/gi.test("a/b");
let x = (a + b) / c;
x = cond ? /^\d+$/ : null;

@sealed
class Counter extends EventTarget {
  #count = 0;
  static DEFAULT = Object.freeze({ step: 1, name: 'it\'s\n' });

  constructor(step = 1) {
    super();
    this.step = step ?? Counter.DEFAULT.step;
  }

  get value() { return this.#count; }

  *items() { yield* [1, 2, 3]; }

  inc() {
    this.#count += this.step;
    return /x/.test(`${this.#count}`) ? this?.value : undefined;
  }
}

const fmt = (n, w = 8) => `${String(n).padStart(w)}|${n > 9 ? `big ${n / 10 | 0}` : `${[1, 2].map(v => `<${v}>`).join(`, ${w}`)}`}`;
const label = `a${ {k: 1}.k }b\u{1F600}\x41`;
const obj = { type: 'x', default: 1, in: true };
const { NaN: nan, Infinity: inf } = Number;

for (const [k, v] of Object.entries(obj)) {
  if (v === null || typeof v === 'undefined') continue;
  console.log(k, v, obj.default.type, obj?.in);
}

const p = new Promise((resolve) => setTimeout(resolve, 100));
const t = await Promise.all([p, readFile(path.join('a', 'b'), 'utf8')]);
module.exports = { Counter, countLines, fmt, label, t };
