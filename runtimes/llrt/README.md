# LLRT（Cortex-A53 版）

[LLRT](https://github.com/awslabs/llrt)（AWS の QuickJS ベースの JavaScript ランタイム）を、
CardputerZero の CPU（Cortex-A53、ARMv8.0）向けにビルドする手順。

配布版の `llrt-linux-arm64` は AWS Graviton 向け（`target-cpu=neoverse-n1` と LSE アトミック）で
作られていて、A53 では **SIGILL で落ちる**（LSE 命令 7,284 個・RCpc 1,135 個。2026-11-16 確認）。
ここでは CPU の指定だけを差し替えて、LLRT 公式の Makefile でビルドする。

## 使い方

Windows（WSL コンテナー `wslc` と開発コンテナのイメージ `cz-dev:latest` が要る。`container/` 参照）:

```powershell
cd runtimes\llrt
.\build.ps1                       # v0.10.0-beta の no-sdk → out\llrt-0.10.0-beta-linux-a53-no-sdk
                                  #   と型定義 out\llrt-types-0.10.0-beta.tar（どちらも隣に .sha256）
.\build.ps1 v0.11.0-beta          # 新しい版
.\build.ps1 v0.10.0-beta std      # AWS SDK 入り（no-sdk / std / full-sdk）
```

1 回目はツールチェーンと JS の依存を落とすので数分かかる。2 回目からは名前付きボリューム
`llrt-build` を使い回す（不要になったら `wslc volume rm llrt-build`）。`out/` は git 管理外。

## 差し替えていること（`build.sh`）

| 場所 | 公式 | ここ |
|---|---|---|
| `.cargo/config.toml`（aarch64-musl） | `-Ctarget-cpu=neoverse-n1`、`+lse` | `-Ctarget-cpu=cortex-a53`、`+lse` なし |
| Makefile（zstd の `zig cc`） | 指定なし | `-mcpu=cortex_a53` |
| C の依存（QuickJS など） | 指定なし | `CFLAGS_aarch64_unknown_linux_musl=-mcpu=cortex_a53` |

LLRT は `build-std` で Rust の標準ライブラリもビルドし直すので、std も A53 向けになる。
差し替えが効かなかったとき（上流の書き方が変わったとき）と、出来上がりに A53 に無い命令が
多く残っているときは、`build.sh` がエラーで止まる。

## 型定義の tar

LLRT のリポジトリの `types/`（パッケージ名 `llrt-types`）の `*.d.ts` を、同じタグのソースから
1 つの tar にまとめる（`build.sh` の終わり）。zcode の TS / JS の型検査が `--lib es2022,dom` の
代わりに使う（`zcode/languages/README.md`）。名前の順・時刻（0）・持ち主（0）・権限をそろえるので、
中身が同じなら SHA-256 も同じになる（v0.10.0-beta で 2 回作って一致を確かめた）。

## 実機に入れる

`zcode/tools/install-runtimes.sh`（sudo なし。`~/.local/share/zcode/runtimes/` に版ごとに置き、
`~/.local/bin/llrt` と `~/.local/share/zcode/types/llrt` を張り替える）。版と SHA-256 は
`zcode/tools/runtimes.lock` に書く。**作り直したら lock の SHA-256 も書き換える**（ビルドは再現可能
とは限らない）。

## 中で使うもの

LLRT の Makefile が指定する nightly の Rust、cargo-zigbuild、zig（ziglang.org。SHA256 を確かめる）、
Node（nodejs.org。SHASUMS256 で確かめる）と corepack・yarn 1（LLRT の `yarn.lock` のとおりに入れる）、
`zstd` コマンド。**yarn は依存のインストールスクリプトを実行する**（LLRT の JS 部分のビルドに要る）が、
使い捨てのコンテナの中だけで、手元の環境や実機には触れない。

## 実機での結果（v0.10.0-beta、no-sdk、2026-11-16）

| 項目 | 結果 |
|---|---|
| 実行ファイル | 10.8MB（静的リンク）。LSE 34 個（実行時に CPU を見る補助関数）、RCpc 0 |
| 起動（`hello.js`） | 0.02 秒・約 11MB |
| 200 万回のループ | 0.70 秒（Python 3.13 は 3.84 秒） |
| `node:fs`・`node:os`・タイマー・ES モジュール | 動く |
| `Intl` | **無い**（feature `intl402` が既定のビルドに入っていない） |

実機に置くときは、ほかの実験と同じく `/tmp`（RAM）に置き、`timeout` を付けて動かす。
