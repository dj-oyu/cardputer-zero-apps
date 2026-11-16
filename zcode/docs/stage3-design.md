# zcode 段階 3 の残り: 外部ビルドと JS / TS の実行（設計）

2026-11-17。段階 3 のうち、ターミナルと F5（`zc-term`・パネル・終了コード）は実装済み。
この文書は残りの設計: **外でビルド・型検査・試験をする仕組み**（`remote-build.html` の
段階 1〜3）と、**実機での JS / TS の F5**、**人がやる準備**。API の雛形（コンパイルは通る・
中身は `todo!()`）はこのブランチに入っている。実装は §13 の課題に分けて行う。

もとの設計は `remote-build.html`（理由と却下した案はそちら）。この文書は、それを実装できる
細かさまで決めたもの。食い違うところはこの文書が優先（変えた点は §16）。

## 1. 範囲と、決まっていること

やること:

- A. 外部ビルド: ビルド係の受付のプログラム、実機側の `zc-remote`、Cloudflare 側（Worker・
  Durable Object・コンテナ）、zcode へのつなぎ込み（Rust / C の F5、保存時の check、
  「試験を実行」、診断の持ち方の作り直し）
- B. 実機での JS / TS の F5（LLRT と TypeScript 7 の `tsc` の入れ方、`run` の書き方）
- C. 人がやる準備（Cloudflare・Tailscale・実機）

ユーザーの決定（2026-11-16）と、レビューで決めた点（どれも守る）:

| 決定 | 中身 |
|---|---|
| Cloudflare が前提 | ビルド係は Cloudflare Containers。PC の開発コンテナは「届けば使う」もう 1 つの経路 |
| TLS の暗号は ring | 実機側の `zc-remote` だけの例外。zcode 本体は pure Rust のまま |
| 最後に成功した実行ファイルを SD に 1 つ | `~/.cache/zcode/bin/`。プロジェクトごとに 1 つ。再起動後のオフライン用 |
| 実機側は別プロセス | `zc-remote`（CLI + `serve`）。tokio なし・自前の `poll()`・使わなければ終わる |
| 送り方 | 目録 → 足りない中身だけ。WebSocket 1 本。mtime は運ばない |
| ジョブは接続から切り離す | 切れても続く。つなぎ直して `attach`。取り消しは `cancel` |
| 診断 | `DiagnosticSet`（出どころ・完全かどうか・版）。出どころごとに置き換える |
| リモートの仕事 | zcode の直列の batch ワーカーに載せない |
| 打鍵を遅らせない | メインを起こすのは最短 33ms に 1 回 |
| F5 のビルド | `--release` にしない（opt-level 2・LTO なし・strip・zstd） |

## 2. 全体の形

```text
 実機 ─────────────────────────────────────────────┐
  zcode                                              │
   ├ メイン（打鍵・描画）                             │
   ├ zc-remote スレッド ──UNIX ソケット──┐            │
   └ ターミナル(pty): zc-remote run ─────┤            │
                                         ▼            │
                              zc-remote serve（別プロセス・スレッド 1 本）
                               走査・ハッシュ / 状態機械 / TLS / 成果物の受け取り
 ────────────────────────────────┬──────────────┬────┘
        wss（TLS 1.3）            │              │ ws（TLS なし・Tailscale）
                                  ▼              ▼
   Access → Worker → Durable Object      PC の WSL コンテナー
   （JWT・回数制限）（ジョブの持ち主）     zc-buildd（:7411）
                          │ WebSocket
                          ▼
                 コンテナ: zc-buildd（:7411）
                 cargo / gcc / npm / tsc / esbuild
```

- 実機 ⇔ ビルド係の約束（§5）は、Cloudflare 経由でも PC 直結でも同じ。相手はどちらも
  受付のプログラム `zc-buildd`。Cloudflare 側の Worker と DO は、門番と中継と見張り
- zcode の中には TLS も JSON も入らない。zcode が話すのは、UNIX ソケットの固定の並びの
  メッセージ（§8.2）だけ
- **F5 は、ターミナルの pty で `zc-remote run <file>` を動かす**（§8.4）。ログの間引き・
  Ctrl+C・終了コードは、もうあるターミナルの仕組みのまま使う。診断だけは serve が zcode の
  専用スレッドへも流し、波線になる

## 3. 置き場所

```text
zcode/
  Cargo.toml                 zcode 本体のワークスペース（remote/ は入れない）
  crates/
    zc-core/src/diag.rs      診断の型（Diagnostic・DiagnosticSet …）            ← 足した
    zc-intel/src/diagstore.rs  診断の置き場と、版に合わせたずらし                 ← 足した
    zc-remote-ipc/           zcode ⇔ zc-remote の UNIX ソケットの約束（依存は zc-core だけ） ← 足した
    zcode/src/remote.rs      zc-remote の相手をする専用スレッド                  ← 足した
  remote/                    **別のワークスペース**（自分の Cargo.lock とプロファイル）
    Cargo.toml  build.sh
    crates/
      zc-wire/               実機 ⇔ ビルド係の約束（メッセージ・目録と ID・WebSocket の枠）
      zc-remote/             実機側: lib + 実行ファイル zc-remote（CLI と serve）
      zc-buildd/             ビルド係の受付: lib + 実行ファイル zc-buildd
    worker/                  Cloudflare: Worker と Durable Object（TypeScript）・wrangler.jsonc
    container/               ビルド係のイメージ（Dockerfile）・PC で動かすスクリプト
  tools/
    install-runtimes.sh  runtimes.lock   実機に LLRT・tsc・zc-remote を入れる
```

`remote/` を別のワークスペースにした理由:

- `zc-remote` は ring（C とアセンブリ）・rustls・serde_json を使う。zcode 本体の
  `Cargo.lock` にも実行ファイルにも入れない（「zcode は pure Rust」を lock で確かめられる）
- プロファイルを分ける。zcode は fat LTO。`toml` が fat LTO の aarch64 で異常終了した件が
  未解決なので、serde 系は thin LTO にする（`remote/Cargo.toml` の `[profile.release]`:
  `opt-level = "s"`・thin LTO・`panic = "abort"`）
- zcode の `cargo test --workspace` と CI に、ring のビルドを足さない

`remote/` から `../crates/zc-core` と `../crates/zc-remote-ipc` を path で使う（どちらも
依存は libc だけ）。zcode 本体から `remote/` の crate は使わない。

確かめたこと（2026-11-17、開発コンテナ）: 雛形の `remote/build.sh` で ring が
`aarch64-unknown-linux-gnu` にクロスビルドでき（クロスの gcc だけで足りる）、できた
`zc-remote` の LSE / RCpc の命令は 12 個（実行時に CPU を見て切り替える分だけ）。
rustls を「TLS 1.3・X25519・ChaCha20-Poly1305 だけ」に絞る設定も試験で確かめてある
（`zc_remote::net::tls_config`）。

## 4. ビルド係の受付 `zc-buildd`

Cloudflare のコンテナの中でも PC でも、同じ実行ファイルがポート **7411** で待つ。x86_64 で
動き、実機用（aarch64・Cortex-A53）にクロスビルドする。tokio は使わない（接続は同時に数本。
待ち受け 1 本 + 接続ごとに 1 本 + ジョブごとに 1 本のスレッド）。

**依頼してきた側（実機）を信用しない**: パスは目録の検査を通ったものにだけ書き、作業の木の
外に出ない。上限を超えたら断る。受付の環境に秘密を置かない。

### 4.1 口

| 依頼 | 返事 |
|---|---|
| `GET /v1/ws`（Upgrade、`Sec-WebSocket-Protocol: zc-build.v1`） | WebSocket（§5） |
| `GET /healthz` | `200 ok` |
| `GET /v1/status` | `{"running":n,"queued":n,"jobs":[…],"uptime_s":n}`（DO が、コンテナを起こしておくかの判断に使う） |

認証はしない（Cloudflare では Access と Worker、PC では Tailscale の ACL が門番）。Worker が
付ける `X-Zc-Device` は、ログに出す名前として使うだけ。

### 4.2 置き場と台帳

置き場（`--data`。既定 `/var/lib/zc-buildd`）:

```text
blobs/<sha256 の先頭 2 桁>/<sha256>   中身（内容で引く）
proj/<project id>/src/                作業の木（目録のとおりに写す）
proj/<project id>/target/ …           cargo の出力・node_modules
out/<job id>/                         成果物（圧縮済み）
```

- 作業の木への写しは、**中身のハッシュが変わったファイルだけ書く**。変わっていない
  ファイルには触らない（mtime を変えない → cargo が作り直さない）。目録から消えたファイルは
  消す。実機の mtime は持ち込まない
- ジョブは**台帳**（`jobs::Registry`）のもので、接続のものではない。接続は「見ている人」
  として付いたり離れたりする
- 同じ job id の依頼は同じジョブに当たる。走っていれば見る人として付く。終わって結果が
  残っていれば、実行せずに結果を流す（成功と「コンパイルエラー」の結果だけ残す。
  取り消し・時間切れ・受付の失敗はやり直す）。結果は 30 分・32 個まで残す
- 同時に走らせるのは 2 つまで。**同じプロジェクトのジョブは同時に走らせない**（作業の木と
  `target/` を取り合う）。待ちの順は check が先（短く、保存のたびに来る）。列は 8 まで
- ログは全部台帳に残す（上限 1MiB。超えたら先頭を捨てて「省略」の 1 行）。見る人へは
  50〜100ms か 4KB ごとにまとめて渡す。**遅い相手を待たない**（送り待ちが 4MB を超えた
  接続は閉じる。ジョブは続く）
- 取り消し・時間切れ（15 分）: 道具のプロセスグループに SIGTERM、2 秒後に SIGKILL

### 4.3 実行の口

`runner::Runner`（`run(&JobCtx, &dyn JobOutput) -> Result<JobResult, JobFailure>`）。台帳は
ジョブごとのスレッドでこれを呼ぶ。試験では偽物を渡す。道具には環境変数を絞って渡す
（`runner::tool_env`）。

### 4.4 言語ごとの中身

| 言語 | build | check | test |
|---|---|---|---|
| Rust | `cargo build --target aarch64-unknown-linux-gnu`、F5 用のプロファイル（下）、`-Ctarget-cpu=cortex-a53` | `cargo check --all-targets`（同じ target） | `cargo test --no-fail-fast`（**x86_64 のまま**） |
| C | `aarch64-linux-gnu-gcc -mcpu=cortex-a53 -O2 -Wall -Wextra` | 同じ引数で `-fsyntax-only` | 未対応 |
| Node（JS / TS、npm の依存あり） | `npm ci --ignore-scripts` → `tsc` で型検査 → `esbuild --bundle` で 1 ファイルの JS | `tsc --noEmit` | `npm test` |

- F5 用のプロファイル: `opt-level = 2`・`debug = 0`・`strip`・LTO なし・`incremental`。
  プロジェクトの `Cargo.toml` は書き換えず、`--config` で渡す。配布用は `release` の指定で
- `Cargo.toml` の無い「1 ファイルだけ」の Rust は、受付が仮の `Cargo.toml` を作って同じ
  手順で回す
- `Cargo.lock` / `package-lock.json` が無い・古いときは、道具が作った lock を実機へ返して
  書き戻す（無いと job id が安定しない）
- 診断の位置は **0 始まりの行と、行の中の UTF-8 のバイト位置**に直してから返す
  （rustc は 1 始まりの文字数、tsc は 1 始まりの UTF-16 の単位、gcc は 1 始まりのバイト）。
  マクロの展開先・依存 crate を指す診断は手元の呼び出し元に寄せ、寄せられなければ
  位置なし（パスが空）にする。細かい決まりは `runner/rust.rs` の先頭
- 試験を x86_64 で動かすので、`target/` は aarch64 用とホスト用の 2 つになる（承知の上）

### 4.5 成果物

strip → 大きさの上限（12MiB）を確かめる → SHA-256 → `zstd -19`、**窓は 2MiB まで**
（実機の展開メモリを抑える）。aarch64 の ELF であることも確かめる。`zstd` はコマンドを使う
（C のライブラリをリンクしない）。

## 5. 約束（実機 ⇔ ビルド係）

`zc-wire` crate。WebSocket 1 本（パス `/v1/ws`、サブプロトコル `zc-build.v1`、
permessage-deflate なし）。制御は JSON のテキストフレーム、ファイルの中身と成果物は
バイナリフレーム。約束の版は 1（`hello` の `v`）。知らない `type` と知らない項目は無視する
（版を上げずに足せるように）。

### 5.1 目録と ID

- **目録**: `{p: パス, s: 大きさ, h: SHA-256, x: 実行権限}` の一覧。パスは木の根からの
  相対・`/` 区切り・正規形（`..`・絶対パス・`\`・空の要素は受け取らない）。mtime は入れない
- **tree id** = 目録の正規形の SHA-256（16 進 64 桁）。正規形は、パスのバイト列の順に並べた
  `<パス>\0<大きさ>\0<sha256>\0<x か ->\n` の連結
- **job id** = `SHA-256("zc-job-v1\0" + tree id + "\0" + kind + "\0" + lang + "\0" + オプションの JSON)`
  の先頭 16 バイト（16 進 32 桁）。同じ木・同じ種類・同じオプションなら同じ id
- **project id** = プロジェクトの根の絶対パスの SHA-256 の先頭 8 バイト（16 進 16 桁）。
  受付は作業の木をこの名前で分ける（パスそのものは送らない）

### 5.2 メッセージ

実機 → ビルド係:

| type | 項目 | 意味 |
|---|---|---|
| `hello` | `v`, `client` | 最初に 1 回 |
| `manifest` | `project`, `tree`, `files` | 目録。返事は `need` |
| `job` | `job`, `project`, `tree`, `kind`, `lang`, `opts`, `log` | ジョブを頼む（`ready` を受けた木に対して）。`log` はログも欲しいか |
| `attach` | `job`, `log`, `artifact` | つなぎ直した後の続き（受け取り済みの位置） |
| `cancel` | `job` | 取り消す |

ビルド係 → 実機:

| type | 項目 | 意味 |
|---|---|---|
| `hello` | `v`, `server` | |
| `need` | `tree`, `blobs` | 持っていない中身の SHA-256。空ならすぐ `ready` |
| `ready` | `tree` | 木がそろった |
| `accepted` | `job`, `cached`, `queued` | 受け付けた（`cached` = 残っていた結果を返す） |
| `started` | `job` | 実行を始めた |
| `log` | `job`, `offset`, `text` | ログ。`offset` はログ全体の中の位置 |
| `diagnostics` | `job`, `complete`, `truncated`, `files` | 診断。1 ジョブに 1 回、`done` の前 |
| `tests` | `job`, `passed`, `failed`, `ignored`, `cases` | 試験の結果 |
| `file` | `job`, `path`, `sha256`, `text` | 実機に書き戻すファイル（lock） |
| `artifact` | `job`, `chan`, `kind`, `name`, `sha256`, `bytes`, `zbytes`, `offset` | 成果物の始まり。このあとバイナリフレーム |
| `done` | `job`, `status`, `exit`, `queue_ms`, `run_ms` | 終わり。`status`: ok / failed / cancelled / timeout / lost / error |
| `error` | `job?`, `code`, `message`, `retry_after_s?` | `job` が無ければ接続全体の失敗（このあと切る）。`code`: version / protocol / too-large / limit / unknown-job / bad-tree / bad-blob / unavailable / internal |

型の正本は `zc-wire/src/msg.rs`（serde の定義がそのまま JSON の形）。

### 5.3 バイナリフレーム

16 バイトの頭（リトルエンディアン）+ 中身（64KiB まで）。base64 にしない。

```text
u8  kind      1 = 中身（実機 → ビルド係）  2 = 成果物（ビルド係 → 実機）
u8  flags     bit0 = この流れの最後のフレーム
u16 reserved  0
u32 id        中身: 直前の need の配列の添字 / 成果物: artifact の chan
u64 offset    この中身の、流れの中での位置
```

成果物は zstd で圧縮した流れ。`offset` は圧縮後の位置。

### 5.4 例

```text
→ {"type":"hello","v":1,"client":"zc-remote/0.1.0"}
← {"type":"hello","v":1,"server":"zc-buildd/0.1.0"}
→ {"type":"manifest","project":"3f9a…","tree":"b1c2…","files":[{"p":"Cargo.toml","s":120,"h":"…"},{"p":"src/main.rs","s":812,"h":"…"}]}
← {"type":"need","tree":"b1c2…","blobs":["9e0f…"]}
→ （バイナリ: kind=1 flags=1 id=0 offset=0 + src/main.rs の中身）
← {"type":"ready","tree":"b1c2…"}
→ {"type":"job","job":"7f3a…","project":"3f9a…","tree":"b1c2…","kind":"build","lang":"rust","opts":{},"log":true}
← {"type":"accepted","job":"7f3a…","cached":false,"queued":0}
← {"type":"started","job":"7f3a…"}
← {"type":"log","job":"7f3a…","offset":0,"text":"   Compiling foo v0.1.0\n"}
← {"type":"diagnostics","job":"7f3a…","complete":true,"truncated":false,"files":[]}
← {"type":"artifact","job":"7f3a…","chan":1,"kind":"exe","name":"foo","sha256":"…","bytes":912384,"zbytes":301220,"offset":0}
← （バイナリ: kind=2 id=1 offset=0 …、最後のフレームは flags=1）
← {"type":"done","job":"7f3a…","status":"ok","exit":0,"queue_ms":3,"run_ms":4210}
```

### 5.5 流れと、切れたとき

- 順番: `hello` → `manifest` → `need` → 中身 → `ready` → `job` → … → `done`。`job` は
  その接続で `ready` を受けた木に対してだけ受け付ける
- 保存のたびの check: 実機は、前の check に `cancel` を送ってから新しい `job` を送る
  （前の `done` は待たない）。F5 の build は、次の F5 でだけ取り消す。編集しただけ（保存なし）
  では取り消さない
- 切れたとき: 実機はつなぎ直して `hello` → 走っていたジョブごとに `attach`（受け取り済みの
  ログの位置と、成果物の圧縮後の位置）。受付は続きだけを流す
- 受付が `unknown-job` と言ったら（受付が再起動した・コンテナが冷えた）、目録からやり直す。
  `done {lost}` も同じ。job id が同じなので、同じジョブとして数える
- やり直しの回数: build / test は 0.5 秒から倍々で 3 回まで。check はやり直さない（次の保存が
  代わり）。`error {limit}` はやり直さない
- 受付は 15 秒何も送っていなければ ping を打つ。実機は、ジョブの実行中に 40 秒何も
  届かなければ切れたとみなす。**待機中は、実機から ping を打たない**（無線を起こし続けない）

## 6. Cloudflare 側

`zcode/remote/worker/`（TypeScript）と `zcode/remote/container/`。

### 6.1 形

| 部品 | 名前 | 役目 |
|---|---|---|
| Access のアプリケーション | `zcode-build`（ホスト名 `<build-host>`） | サービストークンを確かめ、JWT を付ける。合わなければ Worker に届かない |
| Worker | `zcode-build` | JWT を確かめる・身元を `device_id` に直す・つなぐ回数の制限・DO へ渡す |
| Durable Object | `BuildSession`（名前 = `device_id`、SQLite） | ジョブの持ち主: 中継・台帳・1 日の上限・コンテナの寿命 |
| コンテナ | イメージ `builder` | `zc-buildd` と道具 |

**DO は実機 1 台につき 1 つ**（名前 = `device_id`）。2 台の実機が同じプロジェクトを触っても、
作業フォルダを取り合わない。キャッシュは共有しない（共有したくなったら §6.5 の sccache）。

### 6.2 Worker

1. 口は `GET /v1/ws`（Upgrade）だけ。ほかは 404
2. Access の JWT（ヘッダ `Cf-Access-Jwt-Assertion`）を Web Crypto で確かめる: 署名（RS256）、
   `iss` = `https://<チーム名>.cloudflareaccess.com`、`aud` にアプリケーションの AUD、`exp`。
   公開鍵は `https://<チーム名>.cloudflareaccess.com/cdn-cgi/access/certs`（1 時間覚える）
3. サービストークンの JWT の `common_name`（= トークンの Client ID）を、表 `DEVICES` で
   `device_id` に直す。**表に無いトークンは、Access を通っても断る**（403）。人のログインの
   JWT も断る
4. つなぐ回数: `device_id` ごとに 60 秒 30 回（Rate Limiting の束縛）。超えたら 429
5. DO へ渡す。足すのは `X-Zc-Device` だけ。Access のヘッダと Cookie は落とす

確かめるのはつなぐときだけ。長い接続の途中でトークンの期限が来ても切らない。Worker に
秘密は置かない（`ACCESS_TEAM_DOMAIN`・`ACCESS_AUD`・`DEVICES` は識別子。ただしアカウントごとの
値なので、リポジトリには書かず `wrangler secret put` で入れる）。

### 6.3 Durable Object（ジョブの持ち主）

```text
実機 ◀─WebSocket─▶ DO ◀─WebSocket─▶ コンテナの zc-buildd（:7411）
```

- 実機の接続 1 本につき、コンテナへ WebSocket を 1 本張り、**中身を変えずに中継する**
- 通り過ぎるテキストを見て、**ジョブの台帳**（SQLite）を付ける: 実機の `job` で 1 日の上限を
  数え（超えていたら中継せず `error {limit}`。同じ job id の出し直しは数えない）、受付の
  `started` / `done` で状態を変える。バイナリは見ない
- **接続が切れてもジョブは続く**: 実機の WebSocket が切れたら、その接続の上流だけ閉じる
  （`zc-buildd` はジョブを止めない）。走っているジョブがある間は、30 秒ごとの alarm で
  `zc-buildd` の `/v1/status` を聞いて、コンテナを止めさせない
- コンテナが止まっていた（台帳では走っていたのに `/v1/status` に無い）ジョブは `lost` にする。
  `lost` のジョブへの `attach` には、コンテナを起こさずに `done {lost}` を返す
- 最後の接続が切れ、走っているジョブが無くなってから 10 分（`KEEP_WARM_S`）でコンテナを止める
- コンテナを起こせないときは `error {unavailable}`

`attach`・`cancel`・ログの保持といった約束の中身を知っているのは `zc-buildd` で、DO は中継と
見張りだけにする。理由は §16。

### 6.4 コンテナのイメージ

`remote/container/Dockerfile`（linux/amd64、Debian 13）。開発コンテナの Rust の部分と同じもの
（rustup の stable・aarch64 のクロス）に、Node・npm・esbuild・TypeScript 7 の `tsc`・zstd・
`zc-buildd` を足す。Python とフォントは入れない。道具は root で動かさない。版とチェックサムは
固定する。**秘密を入れない**（`build.rs`・`postinstall`・試験という任意のコードが動く）。

起動の方式は、DO が自分でイメージ・大きさ・スナップショットを指定する形
（`scheduling_policy = "durable_object"`。ベータ）。大きさは `standard-4`（4 vCPU・12GiB・20GB）。

### 6.5 キャッシュ（3 段で進める）

ディスクは使い捨て。コンテナが止まると `target/`・crate と npm のキャッシュが消える。

| 段 | やること | いつ |
|---|---|---|
| 1 | 温めておく（10 分）。まず、冷えた起動と初回のビルドの時間を**測る** | 最初の deploy（課題 C3 のあと） |
| 2 | **スナップショット**: ビルドが成功し、lock のハッシュが前回の写しと違うとき、走っているジョブが無くなってから `snapshotContainer` で写す。id とイメージの版を DO に覚え、次に冷えたらそこから起こす。イメージが変わったら写しは捨てる。写しは 30 日保持（起こすたびに延びる） | 課題 C4 |
| 3 | sccache（置き場は R2）。コンテナから外への HTTPS を DO が横取りして R2 の束縛へ流す形にすれば、**コンテナに秘密を置かずに済む** | 段 2 を測って足りなければ |

### 6.6 費用

含まれる分（月 5 ドルの有料プラン）: CPU 375 vCPU 分・メモリ 25GiB 時間・ディスク 200GB 時間。

- 1 回のビルド（`standard-4` で 1 分）は CPU 4 vCPU 分で、含まれる分の約 1%
- **温めておく間のメモリが本当の費用**。`standard-4`（12GiB）は 1 時間 0.108 ドルで、含まれる
  25GiB 時間は約 2 時間分。10 分温めると 1 回で含まれる分の 8%。1 日 2 時間 × 30 日なら
  月 6 ドル強の超過の見込み
- 守り: 1 日のジョブ数（400）、実機 1 台にコンテナ 1 つ、温める時間は 10 分。Cloudflare の
  利用量の通知を入れる（§12.1）

数字は公式の料金表からの計算で、実測ではない。

### 6.7 PC をビルド係にする場合

同じイメージ（`cz-builder`）を PC の WSL コンテナーで動かす（`remote/container/run-pc.ps1`）。
実機からは Tailscale で直接 `ws://<PC>:7411`（TLS なし。WireGuard で暗号化済み）。Access・
Worker・DO は通らない。`cargo` のキャッシュは名前付きボリュームに残るので速い。認証は
Tailscale の ACL（実機 → PC の 7411 番だけ）。

設定が `auto` のとき、`zc-remote` は PC へ 300ms だけ接続を試し、届けば PC、届かなければ
Cloudflare を使う（結果は 60 秒覚える）。

## 7. 実機側 `zc-remote`

### 7.1 プロセスの形

実行ファイルは 1 つ（`~/.local/bin/zc-remote`）で 2 通りに動く。

- **serve**: `$XDG_RUNTIME_DIR/zcode/remote.sock`（実機では `/run/user/1000/zcode/remote.sock`）
  で待つ。スレッドは 1 本（名前解決の間だけ使い捨てが 1 本）。1 つの `poll()` で、UNIX
  ソケット・ビルド係への接続・timerfd を待つ。相手もジョブも無いまま 180 秒で終わる。
  systemd のサービスにはしない
- **CLI**: `zc-remote build|check|test|run|applies|status|stop`（§7.8）。serve に頼むだけ

serve は、ソケットが無いのに気づいた側が起動する。zcode の専用スレッドが起動したときは
zcode と同じプロセスグループ（APPLaunch の Esc 長押しで一緒に終わる）。CLI が起動したときは
切り離す（`setsid`）。ジョブはビルド係が持っているので、serve が終わっても害は無い。

相手は何人でもよい。**zcode の専用スレッドには、ほかの相手（F5 の `zc-remote run`）が頼んだ
ジョブの進み具合・診断・終了も流す。** ログと成果物は頼んだ相手にだけ。

### 7.2 プロジェクトの根と、木の走査

根の決め方（保存したファイルから上へ。`$HOME` の手前まで）:

| 言語 | 根 | 無いとき |
|---|---|---|
| Rust | いちばん近い `Cargo.toml`。その上に `[workspace]` のある `Cargo.toml` があればそちら | そのファイル 1 つだけを送る |
| C | ファイルのあるフォルダ（直下の `*.c` `*.h` だけ） | — |
| Node | いちばん近い `package.json` で、依存が空でないもの | 対象ではない（実機の `tsc` に回す） |

- 送らないもの: `target/`・`node_modules/`・`.git/`・`dist/`・`build/`・`.` で始まるフォルダ
  （`.cargo/`・`.github/` は送る）、根の `.gitignore` に書かれたもの、シンボリックリンク
- 根の外を指す `path = "../x"` の依存は送らない（ビルドが失敗し、その診断が出る）
- 上限: 2000 ファイル・合計 32MiB・1 ファイル 4MiB。超えたら、何が超えたかを添えて断る
- 変わったファイルの見つけ方: RAM に `{パス → 大きさ・mtime・inode・SHA-256}` を覚え、依頼の
  たびに `stat` で歩いて、食い違ったものだけ読み直す。**走査の直前 2 秒以内の mtime のものは
  無条件に読み直す。** SD には何も書かない。再起動後の初回は全部ハッシュし直す
- 送る直前にもう一度ハッシュを確かめながら読む。走査のあとで書き換わっていたら、目録を
  作り直す

### 7.3 通信

- rustls の `read_tls` / `write_tls` を自前のループに載せる。暗号は ring。**TLS 1.3 だけ・
  鍵交換は X25519 だけ・暗号は ChaCha20-Poly1305 だけ**（実機の CPU には AES の命令が無い）。
  ALPN は `http/1.1`。根の証明書は webpki-roots の埋め込み。再開のチケットは RAM だけ
- HTTP/1.1 の Upgrade と WebSocket の枠は自前（`zc-wire`）。tokio・hyper・reqwest は使わない
- 時間切れ: 名前解決 2 秒・接続 3 秒・ハンドシェイク 5 秒。IPv4 と IPv6 は 250ms ずらして
  並行に。前回つながった IP を RAM に覚えて先に試す
- 既定の経路が無ければ、待たずに「オフライン」
- 証明書が「まだ有効でない」で落ちたら「時計が合っていない」（時計の電池が無いので、NTP の
  同期前に起きる）
- **302 と HTML は認証の失敗。たどらない**
- 最後のジョブのあと 75 秒は接続を保ち、静かに閉じる（Cloudflare は無通信の WebSocket を
  100 秒前後で切る見込み）

### 7.4 状態機械

`session::Session`。**I/O を持たない**: 入力は「つながった・切れた・届いた・時間が来た・
依頼が来た」、出力は「これを送れ・この中身を送れ・これを知らせろ・つなぎ直せ」。切断・
再接続・順番の入れ替わりは、入力の列を流すだけで全部試せる。決まりは §5.5。

### 7.5 認証

`auth::Credential`（`headers()` と `rejected()`）の 1 つの口にまとめる。実装は 2 つ:
`NoCredential`（PC）と `AccessServiceToken`（`CF-Access-Client-Id` /
`CF-Access-Client-Secret`）。Tailscale のノード ID トークンが使えるようになったら、実装を
1 つ足すだけで乗り換えられる（Worker が身元を `device_id` に直すので、DO と受付は方式を
知らない）。

### 7.6 設定

`~/.config/zcode/remote.json`（JSON。zcode 本体は読まない）:

```json
{
  "backend": "auto",
  "cloudflare": { "url": "wss://<build-host>/v1/ws" },
  "pc": { "addr": "<PC の tailnet 上の名前>:7411" },
  "check_on_save": "always"
}
```

- `backend`: `auto`（PC に届けば PC）/ `pc` / `cloudflare`
- `check_on_save`: `always` / `warm`（接続が温かいときだけ）/ `manual`

`~/.config/zcode/remote-token.json`（**権限 0600。広ければ読まない**。ログに出さない）:

```json
{ "scheme": "cf-access-service-token", "client_id": "<…>.access", "client_secret": "<…>" }
```

### 7.7 成果物の置き場

| もの | 置き場 | 備考 |
|---|---|---|
| 受け取った実行ファイル | `/run/user/1000/zcode/bin/<名前>-<tree id の先頭 8 桁>` | 一時名に**展開しながら**書く → 大きさと SHA-256 を確かめる → `chmod` → `rename`。途中のものは実行できる名前にしない。ヒープに全体を持たない |
| 圧縮されたままの写し | `/run/user/1000/zcode/bin/.zst-<project id>` | SD の控えを書くまでの間だけ |
| 最後に成功した版（SD） | `~/.cache/zcode/bin/<project id>.zcb` | プロジェクトごとに 1 つ・8 プロジェクトまで |

- tmpfs の `bin/` は合計 16MiB まで（控えの段 1 と共用の 35.3MB のうち）。プロジェクトごとに
  最新 1 つ。超えたら古い順に消す
- SD に書く時機: 成功した build の 60 秒後（その間に次が成功したら、そちらだけ）と、serve が
  終わるとき。**中身が同じなら書かない。** 一時ファイル → fsync → rename。受け取った zstd の
  流れをそのまま書く（実機で圧縮し直さない）

もとの設計は「控えの段 2 と同じ時機に、zc-store で」だった。別プロセスの `zc-remote` は
zcode の無操作を知らないので、自分のタイマー（60 秒）で書く。原子的な書き方は同じ。

### 7.8 コマンドライン

```text
zc-remote build <file> [--release]   ビルドして、置いた実行ファイルのパスを出す
zc-remote check <file>               型検査。「パス:行:桁: error[E0308]: …」で出す
zc-remote test  <file> [filter]      試験をビルド係で実行する
zc-remote run   <file> [-- 引数…]    ビルドして、その場で実行する（F5 が使う）
zc-remote applies <file>             ビルド係の対象か（0 / 1。通信しない）
zc-remote serve | status | stop
```

終了コード: 0 成功 / 1 ビルド・型検査・試験の失敗 / 2 使い方 / 3 オフライン / 4 認証・設定 /
5 上限 / 6 そのほか / 130 取り消し。

## 8. zcode 側

### 8.1 専用スレッドと、UNIX ソケットの約束

`zcode/src/remote.rs` の `RemoteClient`。最初の依頼でスレッドを 1 本作る（スタック 256KB）。
メインからは mpsc で依頼し、結果は `AppEvent::Remote(RemoteEvent)` で届く。

- リモートの仕事を batch ワーカーに載せない
- ソケットが無ければ `zc-remote serve` を起動して、最大 1 秒待つ。`zc-remote` が入って
  いなければ、1 回だけ知らせて、以後の依頼は「対象ではない」で返す
- **メインを起こすのは最短 33ms に 1 回**: 進み具合とログはスレッドの中でまとめる（上限
  64KB。あふれたら古い方を捨てる）。段階の変化・診断・終了はすぐ送る

約束は `zc-remote-ipc` crate（依存は `zc-core` だけ）。JSON を使わない。

### 8.2 メッセージの並び

頭は 12 バイト（リトルエンディアン）: `u32 len`（このあとに続くバイト数）・`u16 kind`・
`u16 flags`（0）・`u32 req`（依頼の番号。0 は「自分の依頼ではない知らせ」）。文字列とパスは
`u16 長さ + バイト列`。`Option<u32>` は `u32::MAX` が「無い」。上限は 256KiB。

zcode / CLI → serve:

| kind | 名前 | 本体 |
|---|---|---|
| 0x0001 | Hello | `u16 version`, `u8 client`（1 = エディタ、2 = CLI） |
| 0x0002 | Warm | `path file` |
| 0x0003 | Submit | `u8 kind`（1 build / 2 check / 3 test）, `u8 lang`（1 rust / 2 c / 3 node）, `u16 flags`, `path file` |
| 0x0004 | Cancel | — |
| 0x0005 | Status | — |
| 0x0006 | Shutdown | — |

serve → zcode / CLI:

| kind | 名前 | 本体 |
|---|---|---|
| 0x8001 | Hello | `u16 version` |
| 0x8002 | Status | `u8 backend`, `u8 connected`, `u16 running_jobs` |
| 0x8003 | Phase | `u32 tag`, `u8 phase`, `u32 done`, `u32 total`, `path root` |
| 0x8004 | Log | `u32 tag`, `bytes` |
| 0x8005 | Diagnostics | `u32 tag`, `u8 complete`, `u8 last`, `path root`, `u16 ファイル数`, ファイルごとに `str path`, `u16 件数`, 診断ごとに `u32 line`, `u32 col`, `u32 end_line`, `u32 end_col`, `u8 severity`, `str code`, `str message` |
| 0x8006 | Tests | `u32 tag`, `path root`, `u32 passed`, `u32 failed`, `u32 ignored`, `u16 件数`, 件ごとに `str name`, `u8 passed`, `str path`, `u32 line`, `str message` |
| 0x8007 | Artifact | `u32 tag`, `u8 kind`, `u8 stale`, `path path` |
| 0x8008 | FileWritten | `u32 tag`, `path path` |
| 0x8009 | Done | `u32 tag`, `u8 status`, `i32 exit`, `u8 backend`, `u32 elapsed_ms`, `str message` |

例（Submit、`req` = 7、check、rust、`/a/b.rs`）:
`15 00 00 00 | 03 00 | 00 00 | 07 00 00 00 | 02 | 01 | 00 00 | 07 00 2f 61 2f 62 2e 72 73`

`tag` は serve が振るジョブの通し番号。同じジョブについての知らせを突き合わせる。型の正本は
`zc-remote-ipc/src/lib.rs`。

### 8.3 保存したとき

1. 言語に `remote` があれば（§10.1）、専用スレッドに check を頼む
2. 返事が「対象ではない・オフライン・認証できない・`zc-remote` が無い」で、言語に実機の
   `check` があれば、実機の構文チェックに回す（TS は、npm の依存があればビルド係、無ければ
   実機の `tsc`）
3. **実機の構文チェックは、新しい `check` ワーカーで走らせる**（`WorkerConfig::CHECK`）。
   今は batch に載っていて、`tsc` の 3.8 秒の間ファイルが開けなくなる

### 8.4 F5

`run` をターミナルの pty で動かす今の仕組みのまま。Rust と C の `run` を
`zc-remote run {file}` にするだけ。

```text
F5 → 保存 → 新しい端末で: zc-remote run main.rs
       ビルドのログ（標準エラー）→ 端末にそのまま流れる
       成功 → zc-remote が、置かれた実行ファイルに exec で入れ替わる
            → 終了コードは今の OSC 777 で zcode へ
       失敗 → 終了コード 1。診断は serve から専用スレッドへ → 波線
       Ctrl+C → ビルド中なら取り消して 130、実行中ならそのプログラムへ
       ネットが無い → 「前の版（<日時>）を実行する？ [y/N]」と端末で聞く
```

ログの間引きは、受付（50〜100ms か 4KB ごとにまとめる）とターミナル（1 回の起床で 64KB か
2ms まで）の両方で効く。zcode に新しい描画の経路を足さない。

### 8.5 試験を実行

新しい操作 `Command::RunTests`（表に 1 行。キーは F6 = Fn+6 の 2 つ押し。実機で押せるか
確かめてから決める）。専用スレッドに test を頼み、進み具合をステータスバーに、結果を一覧
（アウトラインと同じ部品）に出す。落ちた試験を選んで Enter でその行へ。画面は先にモックを
作る（課題 Z3）。

### 8.6 表示

- 進み具合はステータスバーの右（「ビルド係: 送信中 12KB」「実行中 8 秒」）。33ms より細かく
  変えない
- 診断は §9。件数 `⊗ n` はプロジェクト全体ではなく、今のタブの件数のまま（モック 13）
- 位置なしの診断（パスが空）は、ステータスバーの右のメッセージにだけ出す

## 9. 診断

### 9.1 型

`zc_core::diag`（`zc-intel` からも同じ名前で使える）:

```rust
pub struct Diagnostic { line, col: Option<u32>, end: Option<(u32, u32)>, severity, code: Option<Box<str>>, message }
pub struct FileDiagnostics { path: Arc<Path>, base: Option<Version>, diags: Vec<Diagnostic> }
pub enum DiagSource { Syntax, Remote }
pub struct DiagnosticSet { source, complete: bool, files: Vec<FileDiagnostics> }
```

`zc-core` に置いた理由: `zc-remote-ipc`（UNIX ソケットの約束）も同じ `Diagnostic` を運ぶ。
横の crate 同士は依存しない決まりなので、共通の型は `zc-core`。

### 9.2 置き換え

`zc_intel::DiagStore`（プロジェクトに 1 つ）。鍵は（出どころ, パス）。

- 置き換えは**出どころごと**。実機の構文チェックとビルド係の check が、互いを消さない
- `complete` のときだけ、「同じ出どころで、今回出てこなかったファイルの診断」を消す。
  取り消し・失敗・途中で切れた結果で波線を消さない
- 開いていないファイルの分も持つ。開いたときに付ける（あとでツリーの赤い印・問題一覧に使える）
- 今の `t.diags = diags`（タブごとに丸ごと差し替え）と `AppEvent::Checked` は、これに置き換える

### 9.3 版（届くまでの編集）

ビルド係の結果は数秒遅れて届く。その間に行を足すと、波線がずれる。

- タブごとに `zc_intel::ShiftLog`（行の増減の記録。上限 256 個）を持ち、編集のたびに記録する
- 診断の位置が指しているのは「**保存した中身**」。メインは、ジョブの「走査が済んだ」の知らせ
  （`Phase::Scanned`）を受けたときに、そのプロジェクトの下の開いているタブの「保存済みの版」を
  ジョブの `tag` に結び付けて控える。診断が届いたら、その版を `FileDiagnostics::base` に入れ、
  `ShiftLog::shift` で今の位置へずらしてから `DiagStore` に取り込む
- 記録があふれていたら、行は残っている分だけずらし、**桁は捨てる**（波線は行全体になる）
- 依頼したときに開いていなかったファイルは `base = None`（ディスクの中身に対する位置）
- 実機の構文チェックも同じ道を通す（`base` = 保存した版）

`DiagnosticSet` 全体に版を 1 つ持たせる形（もとの設計）にしなかった理由: ビルド係の結果は
複数のファイルにまたがり、版はタブごとに別だから（§16）。

### 9.4 受け入れ（`zc-intel/src/diagstore.rs` の試験）

| 場面 | 期待 |
|---|---|
| 構文チェックが空で返る | ビルド係の診断は残る |
| 途中で切れた結果（`complete = false`） | 出てこなかったファイルの診断は残る |
| 完全な結果 | 出てこなかったファイルの診断は消える |
| 依頼のあとで行を足した・消した | 届いた診断が、今の行に付く |
| 記録があふれた | 行はずれたまま近似、桁は無し |

## 10. 言語の設定と、JS / TS

### 10.1 `language.toml`

足す項目は 1 つ（`zc-highlight` の `LanguageInfo::remote`。雛形に入っている）:

```toml
remote = "rust"   # ビルド係での扱い: rust / c / node。あれば保存時にビルド係の check、「試験を実行」が使える
```

各言語の行（課題 H1 が書く）:

| 言語 | `remote` | `run` | `check`（実機） |
|---|---|---|---|
| Rust | `rust` | `zc-remote run {file}` | 無し |
| C | `c` | `zc-remote run {file}` | 今のまま（`gcc -fsyntax-only`。実機に gcc があれば効く） |
| JavaScript | `node` | 下 | 今の `tsc --allowJs`。`--lib es2022,dom` を下の形に |
| TypeScript | `node` | 下 | 今の `tsc`。`--lib es2022,dom` を下の形に |
| Python・Shell | — | 今のまま | 今のまま |

### 10.2 JS / TS の F5（実機で）

- JavaScript: `run = "llrt {file}"`。npm の依存があるプロジェクトなら `zc-remote run`
- TypeScript: 型検査して JS を出し、それを LLRT で実行する（型を剥がすだけの esbuild は使わない）

TypeScript の `run`（1 行に書く。ここでは折ってある）:

```sh
bash -c '
  if zc-remote applies "$0" 2>/dev/null; then exec zc-remote run "$0"; fi
  d="${XDG_RUNTIME_DIR:-/tmp}/zcode/js"; mkdir -p "$d" &&
  tsc --pretty false --strict --target es2022 --module esnext --moduleResolution bundler \
      --lib es2022 --skipLibCheck --noEmitOnError --outDir "$d" \
      "$HOME/.local/share/zcode/types/llrt/index.d.ts" "$0" &&
  exec llrt "$d/$(basename "${0%.*}").js"
' {file}
```

- npm の依存があれば（`zc-remote applies` が 0）、ビルド係で 1 ファイルの JS にして実行する
- 出力先は RAM（`/run/user/1000/zcode/js/`）。SD に書かない
- `.mts` → `.mjs`、`.cts` → `.cjs` の読み替えは課題 H1 が足す
- **型定義は DOM をやめ、LLRT のものにする。** LLRT のリポジトリは `types/` に型定義
  （`index.d.ts` と 30 個ほどの `.d.ts`。パッケージ名 `llrt-types`）を持っている
  （v0.10.0-beta で確認）。`--lib es2022,dom` は実機で 3.8 秒・91MB だった（実測）。
  `--lib es2022` + LLRT の型定義でどこまで減るかは、実機で測る（§15）
- 実機の `check` も同じ引数にする（`--noEmit`）。`check` はシェルを通さないので、
  `bash -c '… "$HOME/…" "$0"' {file}` の形で書く

### 10.3 実機への入れ方

sudo を使わない。置き場は jimmy のホームの下。

| もの | 置き場 | 出どころ |
|---|---|---|
| `llrt` | `~/.local/share/zcode/runtimes/llrt-<版>/llrt` → `~/.local/bin/llrt` | `runtimes/llrt/build.ps1`（A53 向けにソースから） |
| `tsc` | `~/.local/share/zcode/runtimes/typescript-<版>/` → `~/.local/bin/tsc` | npm の `@typescript/typescript-linux-arm64`（tgz） |
| LLRT の型定義 | `~/.local/share/zcode/runtimes/llrt-types-<版>/` → `~/.local/share/zcode/types/llrt` | LLRT のリポジトリの `types/`（LLRT と同じタグ） |
| `zc-remote` | `~/.local/share/zcode/runtimes/zc-remote-<版>/zc-remote` → `~/.local/bin/zc-remote` | `zcode/remote/build.sh` |

- 入れるのは `zcode/tools/install-runtimes.sh`（実機で動かす）。版と SHA-256 は
  `zcode/tools/runtimes.lock` に固定し、**合わないファイルは入れない**
- 実機からはダウンロードしない。PC で取って・作って、scp で実機の `/tmp`（RAM）に置き、
  `--from /tmp/…` で渡す。終わったら `/tmp` から消す
- 入れる前に、実行ファイルを `--version` で動かして、SIGILL で落ちないことを確かめる
- 版によらない名前（シンボリックリンク）を張り替えるので、`language.toml` は版を知らない
- **zcode は、起動時に `~/.local/bin` を `PATH` の先頭に足す**（APPLaunch から起動したときの
  `PATH` には入っていない。課題 Z3）
- .deb にしない理由: `dpkg -i` には sudo が要り、LLRT は自分でビルドしたもので、版の入れ替えが
  多い。zcode の .deb（段階 6）に `zc-remote` を同梱するかは、そのとき決める

## 11. 失敗・オフライン・上限

### 11.1 失敗したとき

| 場面 | F5（端末） | 保存時の check |
|---|---|---|
| `zc-remote` が入っていない | `run` が「コマンドが無い」で終わる | 1 回だけ知らせて、実機の `check` に回す |
| 設定・トークンが無い、権限が広い | 「設定が無い」と理由を出して終了コード 4 | 1 回だけ知らせる |
| ネットが無い | 前の版があれば「実行する？」と聞く。無ければ終了コード 3 | 何も出さない（波線はそのまま） |
| 時計が合っていない | 「時計が合っていない（NTP の同期待ち）」 | 1 回だけ知らせる |
| 認証を断られた（401 / 403 / 302） | 理由を出して終了コード 4 | 1 回だけ知らせる |
| 上限（429・`error {limit}`） | 「上限。あと n 秒」で終了コード 5 | 何も出さない |
| 途中で切れた | つなぎ直して続き（3 回まで） | やり直さない |
| コンテナが冷えていた・止まった | 目録からやり直す（数秒〜） | 同じ |
| ビルドの失敗 | ログと終了コード 1、波線 | 波線 |
| 成果物が壊れていた（SHA-256） | 実行しない。1 回だけやり直す | — |

「1 回だけ知らせる」は、同じ理由が続く間は繰り返さない（保存のたびに出さない）。

### 11.2 上限

| 上限 | 値 | どこで |
|---|---|---|
| つなぐ回数 | 実機ごとに 60 秒 30 回 | Worker |
| 同時の接続 | 実機ごとに 4 本 | DO |
| 1 日のジョブ数 | 実機ごとに 400 | DO |
| 同時に走るジョブ | 2（同じプロジェクトは 1 つ）。待ちは 8 | `zc-buildd` |
| 1 ジョブの時間 | 15 分 | `zc-buildd` |
| 木 | 2000 ファイル・32MiB・1 ファイル 4MiB | 実機（送る前）と `zc-buildd` |
| メッセージ | テキスト 1MiB・バイナリ 64KiB + 16 | DO と `zc-buildd` |
| 成果物（展開後） | 12MiB | `zc-buildd`（作る側）と実機（受ける側） |
| ログ | 1 ジョブ 1MiB | `zc-buildd` |
| 診断 | 1 ジョブ 500 件 | `zc-buildd` |
| 実機の tmpfs の `bin/` | 16MiB | `zc-remote` |
| SD の控え | 8 プロジェクト | `zc-remote` |
| UNIX ソケットのメッセージ | 256KiB | `zc-remote-ipc` |

トークンは、F5 で動かすプログラム（同じ jimmy）からも読める。守りはサーバ側に置き、漏れても
「ビルド時間を使われる」だけにする。漏れたら、そのトークンだけ無効にする（§12.1）。

## 12. 人がやること

名前はここで決めたもの。`<…>` は自分の環境の値に置き換える（**リポジトリには書かない**）。

### 12.1 Cloudflare

前提: Cloudflare のアカウント。**独自ドメインは使わない**（2026-11-17 ユーザーの決定: 持っていない）。入口は `workers.dev`。
Worker 単位の Access（「Protect this Worker」）は WebSocket の Upgrade が 403 になる（公式の文書）ので使わず、
ホスト名に対する self-hosted の Access のアプリケーションを手で作る。`wrangler.jsonc` は `workers_dev: true`・`preview_urls: false`。
人向けの手順は `docs/setup-remote-build.md`。

1. **Workers の有料プラン**（月 5 ドル）にする。Containers は有料プランだけ
2. ビルド係のホスト名を決める: `<build-host>` = `zcode-build.<subdomain>.workers.dev`
3. **Zero Trust** を有効にする（無料の枠でよい）。チーム名 `<team-name>` を控える
   （`<team-name>.cloudflareaccess.com`）
4. **サービストークンを実機ごとに作る**（Zero Trust → Access → Service Auth → Service Tokens）:
   - 名前 `zcode-cz-1`・`zcode-cz-2`、期限 1 年
   - Client ID（`….access`）と Client Secret を控える。**Secret はこの 1 回しか見えない**
5. **Access のアプリケーションを作る**（Access → Applications → Add → Self-hosted）:
   - 名前 `zcode-build`、ドメイン `<build-host>`
   - ポリシーを 1 つ: 名前 `devices`、**Action = Service Auth**、Include = Service Token →
     手順 4 のトークン。**Allow のポリシーは作らない**（人のログインは通さない）
   - アプリケーションの **AUD タグ**（64 桁）を控える
6. 手元の PC に Node（22 以上）と、**`docker` コマンド**を用意する（wrangler がイメージを
   作って送るのに使う。WSL の Ubuntu に Docker を入れるのが手早い。wslc で代用できるかは
   未確認。§15）
7. deploy する:
   ```sh
   cd zcode/remote/worker
   npm ci
   npx wrangler login
   npx wrangler deploy
   ```
   （2026-11-17 更新: 実際の手順は `docs/setup-remote-build.md` §2 が正。この PC には docker が
   無いので `remote\container\deploy.ps1` が wrangler の docker を wslc に読み替える。Access は
   deploy の後に Worker → Settings → Domains & Routes → workers.dev → Enable Cloudflare Access で
   掛け、ポリシーを Service Auth にして AUD を読む。上の手順 5 のように self-hosted の
   アプリケーションへ workers.dev のホスト名を入れる形は、Zero Trust に断られる）
8. 値を入れる（聞かれたら貼る）:
   ```sh
   npx wrangler secret put ACCESS_TEAM_DOMAIN   # <team-name>.cloudflareaccess.com
   npx wrangler secret put ACCESS_AUD           # 手順 5 の AUD タグ
   npx wrangler secret put DEVICES              # {"<cz-1 の Client ID>":"cz-1","<cz-2 の Client ID>":"cz-2"}
   ```
9. 確かめる:
   - ヘッダなし: `curl -si https://<build-host>/v1/ws` → 302 か 403（Worker に届かない）
   - ヘッダあり: `curl -si -H "CF-Access-Client-Id: <…>" -H "CF-Access-Client-Secret: <…>" https://<build-host>/v1/ws`
     → 426 か 400（Worker に届いて、Upgrade でないと断られる）
10. 利用量の通知を入れる（Notifications → Billing → Usage Based Billing。Containers の
    超過が出たらメール）

トークンを失くした・漏れたとき: Service Tokens でそのトークンを Revoke し、`DEVICES` から
その行を消す。期限が近づいたら Refresh（1 年延びる）。秘密を入れ替えるときは Rotate
（古い秘密を並行して使える時間を選べる）。

### 12.2 Tailscale と PC（PC をビルド係にする場合だけ）

1. ACL に、実機 → PC の 7411 番を足す（タグは自分の環境のもの）:
   ```json
   { "action": "accept", "src": ["<実機のタグ>"], "dst": ["<PC のタグ>:7411"] }
   ```
2. PC でビルド係を動かす: `zcode\remote\container\run-pc.ps1`
3. Windows のファイアウォールで、7411 番の受信を **Tailscale のインターフェースだけ**に許す
4. 実機から届くか: `curl -s http://<PC の tailnet 上の名前>:7411/healthz` → `ok`

### 12.3 実機（1 台ごとに）

1. 実行環境を入れる（PC で用意して、実機へ送る）:
   ```sh
   # PC: runtimes\llrt\build.ps1、zcode\remote の build.sh、tsc の tgz と LLRT の型定義を用意
   scp <用意したファイル> zcode/tools/install-runtimes.sh zcode/tools/runtimes.lock <実機>:/tmp/zc-rt/
   ssh <実機> 'cd /tmp/zc-rt && ./install-runtimes.sh --from . && rm -rf /tmp/zc-rt'
   ssh <実機> '~/.local/bin/llrt --version; ~/.local/bin/tsc --version; ~/.local/bin/zc-remote --version'
   ```
2. 設定を書く（§7.6）:
   ```sh
   mkdir -p ~/.config/zcode && chmod 700 ~/.config/zcode
   # ~/.config/zcode/remote.json        … backend と行き先
   # ~/.config/zcode/remote-token.json  … その実機のサービストークン
   chmod 600 ~/.config/zcode/remote-token.json
   ```
3. 確かめる: `zc-remote status`（つながるビルド係が出る）、Rust の hello で
   `zc-remote run main.rs`

## 13. 課題

1 つの課題は 1 つの worktree で作る。**課題どうしで同じファイルに触らない**（「持ち物」の列）。
雛形の公開の形（型・関数の名前と引数）は変えない。変えたいときは統合側に戻す。

担当の決め方: 状態機械・再接続・`poll()` のループ・版に合わせたずらし・統合は Fable。
仕様と試験がはっきりしているものは Opus。

| id | 題 | 持ち物 | 要るもの | 受け入れ | 担当 |
|---|---|---|---|---|---|
| W1 | WebSocket の枠と Upgrade | `remote/crates/zc-wire/src/{ws,http}.rs` | — | RFC 6455 の例・1 バイトずつの入力・分割と途中の ping・マスクの向き・302 / 403 / 429 の読み分け（各ファイルの試験の欄） | Opus |
| W2 | メッセージ・目録・ID | `remote/crates/zc-wire/src/{msg,tree}.rs` | — | 正解の JSON と一致・知らない type を無視・ID が順番によらない・悪いパスを断る | Opus |
| I1 | UNIX ソケットの約束の符号化 | `crates/zc-remote-ipc/` | — | 全部の種類の往復・§8.2 の正解のバイト列・半端な入力・知らない kind・上限 | Opus |
| B1 | 受付: 接続の状態機械とジョブの台帳 | `remote/crates/zc-buildd/src/{server,jobs,lib,main}.rs`、`tests/{jobs,protocol}.rs` | W1・W2（形は雛形にあるので、並行して始められる） | 同じ id の 2 人・結果の使い回し・切っても続く → attach で続きだけ・cancel でプロセスグループが消える・直列と並列・遅い相手を外す・約束の間違いを断る | **Fable** |
| B2 | 受付: 中身の置き場・作業の木・成果物の仕上げ | `remote/crates/zc-buildd/src/{store,pack}.rs`、`tests/store.rs` | W2 | 2 回目は 1 つも書かない（mtime が変わらない）・消えたファイルが消える・木の外に書かない・zstd の窓 2MiB・x86_64 の実行ファイルを断る | Opus |
| B3 | 受付: 道具の起動と Rust | `remote/crates/zc-buildd/src/runner.rs`（`tool_env`・`spawn_tool`・`which`）、`runner/rust.rs`、`tests/data/` | — | rustc の JSON の実物との突き合わせ（桁はバイト位置・マクロは呼び出し元）・開発コンテナで依存なしの crate の build / check / test の通し・1 ファイルだけ・lock の書き戻し | Opus |
| B4 | 受付: C と Node | `remote/crates/zc-buildd/src/runner/{c,node}.rs` | B3（道具の起動） | gcc の JSON・tsc の出力（桁は UTF-16 → バイト）・開発コンテナで C の通し・Node の通しはビルド係のイメージの中で | Opus |
| D1 | 実機: つなぐ・TLS・WebSocket・認証 | `remote/crates/zc-remote/src/{net,auth}.rs`、`tests/net.rs` | W1 | ループバックと試験用の TLS サーバで: 往復・切断・沈黙・302 / 403 / 429・期限切れと未来の証明書・AES-GCM だけのサーバにつながらない・再開・少しずつしか書けないソケット・トークンの権限 | **Fable** |
| D2 | 実機: 会話の状態機械 | `remote/crates/zc-remote/src/session.rs`、`tests/session.rs` | W2 | 台本の試験（`session.rs` の末尾の一覧）: 途中で切れて attach・unknown-job → 目録から・cancel・まとめ・やり直しの回数 | **Fable** |
| D3 | 実機: 根・走査・ハッシュの覚え | `remote/crates/zc-remote/src/scan.rs`、`tests/scan.rs` | W2 | 根の探し方・除外・2 回目は読まない・2 秒の決まり・上限・走査のあとの書き換え | Opus |
| D4 | 実機: 成果物の受け取りと SD の控え | `remote/crates/zc-remote/src/artifact.rs`、`tests/artifact.rs` | — | 刻み方によらず同じ結果・食い違いで何も残らない・続きから・途中は実行できる名前が無い・同じ中身は SD に書かない・壊れた控え | Opus |
| D5 | 実機: serve・CLI・設定 | `remote/crates/zc-remote/src/{serve,cli,config,main}.rs`、`tests/{serve,e2e}.rs` | D1〜D4・I1・B1 | 偽物と本物の受付で: 依頼 → 知らせ → Done・エディタの相手に他人の診断・auto の切り替え・オフラインで控え・無通信で終わる・二重起動。`e2e`: 開発コンテナで `zc-buildd` を立てて `zc-remote build` → aarch64 の実行ファイル | **Fable** |
| Z1 | 診断の置き場と、版に合わせたずらし | `crates/zc-intel/src/diagstore.rs` | — | §9.4 の表（ファイルの中の試験の `#[ignore]` を外す）+ 乱数の編集列で「ずらした結果 = 編集後に付け直した結果」 | **Fable** |
| Z2 | zcode の専用スレッド | `crates/zcode/src/remote.rs`、`crates/zcode/tests/remote.rs` | I1 | 偽物の serve（UNIX ソケット）で: 依頼 → `RemoteEvent` の列・必ず Done が届く（serve が死んでも）・ログが 33ms より細かくメインを起こさない・`zc-remote` が無いとき | Opus |
| H1 | 言語の行 | `languages/*/language.toml`、`languages/README.md` | —（置き場は §10 で確定） | `cargo test`（言語の正解）・§10.1 の表のとおり・README に `remote` の説明 | Opus |
| R1 | 実機への入れ方 | `tools/install-runtimes.sh`、`tools/runtimes.lock`、`runtimes/llrt/`（型定義を tar にまとめる 1 行） | — | 開発コンテナで、偽の `$HOME` に入れる・SHA-256 の違うファイルを断る・張り替え・`--remove`・2 回入れても同じ | Opus |
| C1 | Worker の入口 | `remote/worker/src/{index,access,limits}.ts`、`test/{access,index}.test.ts` | — | `access.ts` の末尾の一覧（鍵はその場で作る）・表に無いトークンを断る・ヘッダを落とす・429 | Opus |
| C2 | Durable Object | `remote/worker/src/session.ts`、`test/session.test.ts` | —（`Env` は雛形にある） | `session.ts` の末尾の一覧（コンテナは偽物の上流に差し替える） | **Fable** |
| C3 | イメージ・wrangler・PC のスクリプト・CI | `remote/container/`、`remote/worker/{wrangler.jsonc,package.json,tsconfig.json}`、`zcode/.dockerignore`、`.github/workflows/ci.yml` | B1（動く `zc-buildd`） | イメージが作れる・中で `zc-buildd` が立ち、Rust / C / Node の通しが通る・`wrangler deploy --dry-run`・CI に `remote/` の `cargo test` と Worker の `npm test` | Opus |
| C4 | キャッシュ（スナップショット） | `remote/worker/src/snapshot.ts`（新規）と、`session.ts` から呼ぶ 1 か所 | C2 と、最初の deploy での計測（人） | 写す時機・イメージが変わったら捨てる・冷えた起動が写しから始まる（実物で測る） | **Fable** |
| Z3 | zcode へのつなぎ込み | `crates/zcode/src/{editor,app,intel,commands,main}.rs`、`crates/zc-core/src/command.rs`、`crates/zcode/tests/`、`docs/ui-mock.html`、`tests/golden/` | Z1・Z2・H1 | 下 | **Fable** |
| M1 | 計測の道具 | `bench/remote/`（新規） | — | SHA-256・zstd の展開・木の `stat` 走査の小さなベンチが aarch64 でビルドできる | Opus |

Z3 の中身と受け入れ:

- `t.diags` と `AppEvent::Checked` を `DiagStore` + `ShiftLog` に置き換える（実機の構文チェックも
  `DiagnosticSet` で届ける）
- 保存時: §8.3 の順。実機の構文チェックを `check` ワーカーへ移す
- `AppEvent::Remote` を受ける: `Phase::Scanned` で版を控える・診断を取り込む・進み具合を
  ステータスバーに・「1 回だけ知らせる」
- `Command::RunTests` と結果の一覧（先にモック）
- 起動時に `~/.local/bin` を `PATH` に足す
- 受け入れ: 今の描画の正解画像が変わらない（モック 13）。新しい試験: 構文チェックとビルド係の
  診断が両方出る・遅れて届いた診断がずれない・不完全な結果で波線が消えない・「対象ではない」で
  実機の check に回る・試験の一覧の正解画像

順番:

```text
すぐ始められる（並行）: W1 W2 I1 B2 B3 D4 Z1 H1 R1 C1 C2 M1
その次:               B1 B4 D1 D2 D3 Z2      （W1 / W2 / I1 / B3 が入ってから。形は雛形にあるので、先に始めてもよい）
その次:               D5 C3                  （D5 は D1〜D4・B1、C3 は B1 のあと）
最後:                 Z3                     （Z1・Z2・H1 のあと。D5 があると通しで試せる）
人が先:               Cloudflare の準備（§12.1）→ 最初の deploy と計測 → C4
                      実機への導入（§12.3。R1 と D5 のあと）→ 実機での計測（§15）
```

段階 1（Cloudflare なしで、開発コンテナの中だけで `zc-remote build` が通る）は、W1・W2・
B1〜B3・D1〜D5 がそろえば確かめられる。

## 14. 試験のやり方（Cloudflare なしで）

| 部品 | やり方 |
|---|---|
| `zc-wire`・`zc-remote-ipc`・`session`・`DiagStore` | I/O が無いので、入力 → 出力の単体試験だけ |
| `zc-buildd` | 実行の口を偽物にして台帳と約束を試す。本物の道具の通しは、開発コンテナの cargo と aarch64 のクロス gcc で（依存なしの crate なのでネットワークは要らない）。Node の通しは道具があるときだけ（ビルド係のイメージの中） |
| `zc-remote` の通信 | ループバック。TLS は、試験の中で rustls のサーバ側と自作の証明書を立て、その根を `tls_config` に渡す。Access の役は、ヘッダを見て 302 / 403 を返す小さな偽物 |
| `zc-remote` の全体 | 開発コンテナで `zc-buildd` を `127.0.0.1` に立て、`ZC_REMOTE_PC` で向ける（PC の経路 = TLS なし）。置き場は `XDG_RUNTIME_DIR`・`XDG_CACHE_HOME`・`XDG_CONFIG_HOME` を一時フォルダに向ける |
| zcode の専用スレッド | UNIX ソケットの偽物の serve |
| zcode の画面 | 今と同じ render-test（`AppEvent::Remote` を直接流す） |
| Worker・DO | vitest（`@cloudflare/vitest-pool-workers`）。JWT は試験で作った鍵、コンテナは偽物の上流。開発コンテナには Node が無いので、CI のランナーか、ビルド係のイメージの中で回す |
| Cloudflare の実物 | 人が deploy してから: §12.1 の 9、`zc-remote build` を開発コンテナから `wss://<build-host>` へ |

回し方: `cd zcode\remote; ..\..\container\dev.ps1 cargo test --locked --workspace` と
`..\..\container\dev.ps1 ./build.sh`。zcode 本体は今までどおり。

## 15. 実測・見込み・未確認

実測（出どころ）:

- `/run/user/1000` は 35.3MB の tmpfs で noexec なし。実機の CPU に AES・SHA の命令は無い。
  `/run/systemd/timesync/synchronized` がある（`remote-build.html`、2026-11-16）
- TypeScript 7 の `tsc`: `--lib es2022,dom` で 3.8 秒・91MB、`--lib es2022` の 1 行で 0.32 秒・
  31MB。LLRT（A53 版）: 起動 0.02 秒・11MB（リポジトリ直下の `AGENTS.md`）
- ring が aarch64 にクロスビルドできる。`zc-remote` の雛形の LSE / RCpc の命令は 12 個
  （2026-11-17、開発コンテナ）
- LLRT のリポジトリに型定義 `types/` がある（v0.10.0-beta、2026-11-17 に確認）

見込み（測っていない）: 暗号と接続の時間、`zc-remote` のメモリ（2〜4MB）、走査とハッシュの
時間、ビルドの時間、費用、成果物の圧縮後の大きさ、テザリングでの受信の時間。

未確認（**最初の deploy で確かめる**。課題 C2・C3 が当たる）:

| 項目 | だめだったとき |
|---|---|
| DO からコンテナのポートへ、`getTcpPort(7411).fetch()` で WebSocket を張れるか | 生の TCP（`connect()`）に、同じ WebSocket の枠を DO が自分で載せる |
| 実機側の WebSocket が眠っている（ハイバネーション）間、上流の WebSocket が保たれるか | ジョブの実行中は DO を眠らせない |
| `scheduling_policy = "durable_object"`（ベータ）とスナップショットが、このアカウントで使えるか | 従来の方式（`Container` クラス・`max_instances`）で始め、キャッシュは sccache へ |
| GB 級のイメージの冷えた起動の時間 | 温める時間を延ばす・イメージを削る |
| コンテナから crates.io・npm へ出られるか | 外への通信を有効にする指定（`enableInternet`）を確かめる |
| サービストークンで通ったときの JWT の実物（`common_name`・alg） | Worker の読み方を合わせる |
| Cloudflare が無通信の WebSocket を切るまでの時間・1 メッセージの上限 | 温める時間・刻む大きさを合わせる |
| `wrangler deploy` のイメージ作りに、wslc が使えるか（`WRANGLER_DOCKER_BIN`） | WSL の Ubuntu の Docker で |
| wslc のポートの公開のしかた（PC の経路） | WSL の Ubuntu で `zc-buildd` を直接動かす |

未確認（実機で確かめる。メインのセッション）:

- F6（Fn+6）が押しやすいか（「試験を実行」のキー）
- `--lib es2022` + LLRT の型定義での `tsc` の時間とメモリ。LLRT の型定義が DOM の型に
  頼っていないか
- LLRT が、`tsc` の出した `.js`（ES モジュール）をそのまま実行できるか
- APPLaunch から起動した zcode の `PATH` と `XDG_RUNTIME_DIR`
- 実機に gcc があるか（無ければ C の実機の `check` は効かない。ビルド係の check が代わり）
- `remote-build.html` §12 の計測（暗号の速さ・接続の内訳・上りと下り・`zc-remote` のメモリ・
  ログを流している間の打鍵）。道具は課題 M1 と `zc-remote --stats`

## 16. もとの設計（`remote-build.html`）から変えたこと

| 変えたこと | 理由 |
|---|---|
| **ジョブの中身（ログの保持・`attach`・`cancel`）を持つのは `zc-buildd`。** DO は、台帳（状態と 1 日の上限）・中継・コンテナを止めさせない見張りを持つ。もとは「DO がジョブの持ち主で、切れても続ける」 | 「接続が切れても続く・つなぎ直して続きを受ける」は変わらない。同じ約束を PC 直結（DO が無い）でも使うので、実装を 1 か所にした。Rust で書け、Cloudflare なしで全部の場面を試験できる。DO（TypeScript・試しにくい）は薄くなる |
| **DO は実機 1 台につき 1 つ**（名前 = `device_id`）。もとは「プロジェクトごとか、直列にするか」が未決 | 2 台が作業フォルダを取り合わない。コンテナの数と費用が読みやすい。同じ実機の中では、受付が同じプロジェクトのジョブを直列にする |
| **F5 は、ターミナルの pty で `zc-remote run` を動かす。** もとは「いちばん簡単な形」として挙げていた案 | ログの間引き・Ctrl+C・終了コードが、もうある仕組みのまま動く。zcode に足すのは診断の受け口だけ |
| 版は `DiagnosticSet` 全体ではなく、**ファイルごと**（`FileDiagnostics::base`）。基準は「依頼したときの版」ではなく「保存済みの版」 | ビルド係の結果は複数のファイルにまたがり、版はタブごとに別。診断の位置が指すのはディスクの中身 |
| 診断の型を `zc-core` に置いた（もとは「`zc-intel`、crate をまたぐなら `zc-core`」） | UNIX ソケットの約束（`zc-remote-ipc`）も同じ型を運ぶ |
| SD の控えを書くのは `zc-remote` 自身（成功の 60 秒後と終了時）。もとは「控えの段 2 と同じ時機に、zc-store で」 | 別プロセスは zcode の無操作を知らない。書き方（一時ファイル → fsync → rename、同じなら書かない）は同じ |
| serve は、CLI が起動したときは切り離す。もとは「同じプロセスグループにしておく」 | F5 の端末を閉じたとき（SIGHUP）に serve まで死なないように。zcode が起動したときは同じグループのまま |
| 実機の構文チェックを batch から `check` ワーカーへ | レビューの指摘（batch は直列）。`tsc` は 3.8 秒かかる |
| コンテナの起動は、DO が自分でイメージとスナップショットを指定する方式 | スナップショットがこの方式でだけ使える（公式の文書、2026-11-17 に確認）。コンテナからの HTTPS を DO が横取りできるので、sccache を足すときも秘密をコンテナに置かずに済む |
| `ring` を使っても、AES のコードは実行ファイルに残りうる。もとは「AES のコードも入らない」 | rustls の設定で AES-GCM を出さないようにはできる（確かめた）が、ring の中のコードが消えるかは LTO しだい。大きさは測ってから |
