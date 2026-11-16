# zcode ベンチマーク

`docs/measurements.md` の数字を出したプログラム。設計の根拠を後から確かめ直せる
ように残している。どれも実機を壊さない作りで、fb0 には読んだ内容と同じバイトを
書き戻すだけ、一時ファイルは `/tmp` に作って消す。

## ビルドと実行

```powershell
cd zcode\bench
..\..\container\dev.ps1 ./build.sh            # 全部。名前を並べればそれだけ
```

```bash
scp out/mcbench <実機>:/tmp/ && ssh <実機> 'chmod +x /tmp/mcbench && /tmp/mcbench fb fb2; rm -f /tmp/mcbench'
```

Python のスクリプト（`py/`）は実機にそのまま送って `python3` / `sh` で動かす。

## 一覧

| 名前 | 測るもの | 実行のしかた | 結果の載っている節 |
|---|---|---|---|
| `mcbench` | スレッドと液晶まわり | 引数で区分を選ぶ（`all` で全部）: `mem` スレッドの生成コストと rlimit / `freq`・`ramp` ondemand ガバナが上がるまで / `handoff` eventfd・mpsc の受け渡し / `jitter` sleep・timerfd のずれ / `pool` 描画処理を n 分割したときの所要 / `scale` n スレッドの合計スループット / `fb` fsync が呼び出し側を止めるか / `fb2` fsync 後の SPI の時系列と連続 fsync / `sd <パス>` 64KB の書き込みと fsync / `pf <ファイル>` ページイン（※） | measurements.md §1〜§4 |
| `glyphbench` | swash による字形のラスタライズ（DejaVu Sans Mono 11px ヒンティングあり、Noto Sans CJK 12px / 20px）、初回（SD から読む）と 2 回目、並列化 | 引数なし | §5 |
| `hlbench` | syntect（`.sublime-syntax` を実行時にコンパイル）の読み込み・ハイライト | 引数なし（`.sublime-syntax` のパスを渡すと YAML からの読み込みも測る） | §6 |
| `dfabench` | 規則をビルド時に DFA 化する方式（regex-automata）のコンパイル・読み込み・トークン化 | 引数なし | §6 |
| `py/fb_spi_latency.py` | fb0 に書いてから SPI 転送が始まるまで（fsync あり / なし） | `python3 fb_spi_latency.py` | §1 |
| `py/jedi_bench.py` | jedi の補完・構文チェックの時間とメモリ | `python3 jedi_bench.py [見本.py]` | §7 |
| `py/ast_check.sh` | `ast.parse` を使い捨てプロセスで実行したときの時間 | `sh ast_check.sh [見本.py]` | §7 |

※ `mcbench pf` は `mincore` で「まだ RAM に無いページ」を探してから触る作りだが、
`mincore` は自分が持ち主でないファイルでは常に「常駐」と返すため、システムの
フォントや辞書には使えない（測れない）。ページインの時間は `glyphbench` の初回の値で見る。
