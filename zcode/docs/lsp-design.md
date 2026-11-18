# zcode の型の情報（LSP。Rust から始め、2026-11-18 に全言語へ広げた）

> **2026-11-18 の追記**: 言語サーバーを言語の表で動かす汎用の作りに替え、ビルド係を使う全部の言語
> （Rust・Python・JS / TS・Go・Zig・C / C++・Shell・Ruby・MoonBit）で使えるようにした。§9 を先に読む。
> 下の §1〜§8 は v1（Rust だけ）の記録で、「rust-analyzer」はほかの言語ではその言語のサーバーと読み替える。

2026-11-17。ブランチ `zcode-lsp`。実機では LSP を動かさない（`design.md` §10: jedi でも初回 9〜11 秒・
60MB。rust-analyzer は std の索引だけで約 700MB）。**rust-analyzer はビルド係（`stage3-design.md` の
コンテナ・PC）で動かし、zcode は今の外部ビルドの道（serve → zc-wire → 受付）に乗る薄い相手**にする。

## 1. ユーザーの決定（2026-11-17）

| 決定 | 中身 |
|---|---|
| v1 は Rust だけ | rust-analyzer。ほかの言語は後で |
| 起こす時機 | zcode で Rust のファイルを開いたとき（`Warm`）。ビルド係がそのプロジェクトの rust-analyzer を裏で起動し、最初の問い合わせに間に合わせる。コンテナと同じ寿命（keep-warm 5 分のあと DO がコンテナを止めると消える） |
| キー | Ctrl+H 型と説明（hover）・Ctrl+D 定義へ・Ctrl+R 使っている所・Ctrl+T 名前を探す（workspace/symbol）・F8 次の診断（手元の診断だけ。端で反対の端へ）・Shift+F8 前の診断。表は `commands.rs` の `BINDINGS`（F1 の一覧にも出る） |
| 補完 | Ctrl+Space と自動のまま。rust-analyzer の候補は**届いたら**小窓に足す（単語補完はすぐ出る。打鍵をネットワークで待たない） |

## 2. 実測と判断（`measurements.md` §13）

ビルド係のイメージに rust-analyzer を足し、PC の wslc で **1 vCPU・3GiB** に絞って測った:
ripgrep（58 crate）を冷えた状態から索引しながら F5 のビルドを同時に走らせても、コンテナ全体の最大は
1.4GB・OOM 0。rust-analyzer の RSS は hello で約 700MB、ripgrep で 0.9〜1.2GB。問い合わせそのものは
1ms 未満（references・symbols の最初の 1 回だけ 0.1〜0.6 秒）。**3GiB に収まるので進めた**。

設定（`zc-buildd` の `lsp::ra_config`）: `checkOnSave: false`・`cargo.targetDir: <proj>/lsp-target`・
`files.watcher: "client"`・`diagnostics.enable: false`・補完はスニペット・自動 import・postfix なし。
buildScripts・procMacro・cachePriming は既定（有効）のまま。

## 3. 全体の形

```text
 zcode（メイン）── LspAsk ──▶ 専用スレッド ── IPC 版 3（Lsp）──▶ zc-remote serve
   ▲                                                              │ 走査・重ね（未保存の中身）・目録
   └── RemoteEvent::LspResult / LspState ◀────────────────────────┤
                                                                  ▼ zc-wire（lsp_warm / lsp）
                                                    DO（中継・上限・CPU の計り）→ zc-buildd
                                                                  │ lsp の木（proj/<id>/lsp）に写す
                                                                  ▼ didOpen / didChange（中身ごと）
                                                            rust-analyzer（同時に 1 つ）
```

- 位置は道の全部で **0 始まりの行と UTF-8 のバイト位置**（診断と同じ）。UTF-16 に直すのは受付
  （`zc-buildd/src/lsp.rs`）だけ。zcode の文書の桁（文字の数）との変換は `editor_lsp.rs`
- 未保存の中身は、打鍵が止まったときの check と同じく**目録に重ねて**送る（SD に書かない）。受付は
  それを lsp の木に写し、rust-analyzer の起動後に変わった `.rs` は didOpen / didChange で中身ごと渡す
  （ディスクの見張りの遅れに左右されない）

## 4. 約束

### 4.1 zc-wire（実機 ⇔ ビルド係。版は 1 のまま。知らない type は無視されるので足しただけ）

| 向き | type | 項目 |
|---|---|---|
| 実機 → | `lsp_warm` | `project`, `tree`, `path` |
| 実機 → | `lsp` | `id`（u32）, `project`, `tree`, `query`（hover / definition / references / symbols / completion）, `path`, `line`, `col`, `text`（symbols の検索） |
| → 実機 | `lsp_state` | `cpu_ms`, `project`, `state`（loading / ready / failed）, `message`, `error?` |
| → 実機 | `lsp_result` | `cpu_ms`, `id`, `error?`（unavailable / limit / bad-tree / internal）, `message`, `text`（hover）, `items`, `retry_after_s?` |

`items` の 1 つ: `path`（根からの相対。プロジェクトの外なら空）・`ext`（外の見せ方 `core/src/option.rs`・
`regex-1.11.1/src/lib.rs`）・`line`・`col`・`end`・`name`・`kind`（SymbolKind / CompletionItemKind）・
`detail`（その行の中身・入れ物の名前・補完の型）・`insert`（補完で `col` からカーソルまでを置き換える文字列）。

```text
→ {"type":"lsp","id":7,"project":"p","tree":"t","query":"hover","path":"src/main.rs","line":3,"col":12}
← {"type":"lsp_result","cpu_ms":9,"id":7,"text":"fn len(&self) -> usize"}
```

- `lsp` / `lsp_warm` は、ジョブと同じく**その接続で `ready` を受けた木**に対してだけ（違えば `bad-tree`）
- `cpu_ms` は `type` の直後に置く（DO が頭だけ覗いて読む）。受付が起動してからの通算で減らない

### 4.2 IPC（zcode ⇔ serve。版 3）

| kind | 名前 | 本体 |
|---|---|---|
| 0x0007 | Lsp | `u8 kind`（1 hover / 2 definition / 3 references / 4 symbols / 5 completion）, `path file`, `u32 line`, `u32 col`, `str text`, 重ね（Submit と同じ並び） |
| 0x800A | LspResult | `u8 status`（Done と同じ番号）, `str message`, `str text`, `u16 件数`, 件ごとに `path path`（中なら絶対パス・外なら空）, `str label`, `u32 line`, `u32 col`, `u32 end_line`, `u32 end_col`, `u32 kind`, `str name`, `str detail`, `str insert` |
| 0x800B | LspState | `u8 state`（1 loading / 2 ready / 3 failed）, `path root`, `str message`（req = 0。zcode の相手みんなへ） |

`Warm` は形を変えていない: serve は、`Warm` のファイルが `.rs` なら `lsp_warm` も頼む。
**版 3 の zcode は版 2 の serve と話せない**（今までと同じ。`zc-remote` を先に入れ替え、`zc-remote stop`
してから zcode を入れる）。serve は版 1〜3 の相手を受ける。

## 5. 各層で決めたこと

### 受付（`zc-buildd/src/lsp.rs`）

- rust-analyzer は**同時に 1 つ**。別のプロジェクトが来たら前のをプロセスグループごと止める。コンテナが
  動いている間は止めない（費用はメモリの割り当てで決まる）。落ちたら `lsp_state {failed}` と理由（標準
  エラーの末尾）を出し、次の `lsp_warm` / `lsp` で起動し直す
- 木はジョブの作業の木と別（`proj/<id>/lsp`、覚えは `lsp-tree.json`）。走っているビルドの木を書き換えない。
  `Cargo.toml` の無い 1 ファイルだけのときは、ジョブと同じ仮の crate を `proj/<id>/lsp-single/` に作って
  `linkedProjects` で渡す
- **最初の索引が済む（`experimental/serverStatus` の `quiescent`）までは `unavailable`「準備中」で返す**
  （読み込み前の rust-analyzer は空の答えを返すため）。答えを 25 秒待って来なければ `internal`
- ContentModified（-32801）は `unavailable`「読み直している。もう一度」
- 補完は rust-analyzer が打った文字で絞らないので、受付が「置き換えの始まりからカーソルまで」で始まる
  ものに絞り、`sortText` の順に 50 件まで。スニペット・別の行を書き換える候補は捨てる
- CPU: rust-analyzer のプロセスグループの `utime+stime+cutime+cstime` を `/proc` で数える。`/v1/status` にも
  `lsp_cpu_ms` を出す

### DO（`worker/src/session.ts`・`budget.ts`）

- `lsp` / `lsp_warm` はそのまま中継する。実機のメッセージなので**動きとして keep-warm を数え直す**。
  1 日のジョブ数には数えない
- 費用の上限に達していたら中継せず、`lsp_result {error:"limit", retry_after_s}` / `lsp_state {failed, limit}`
  を DO が返す（接続は続く。冷えたコンテナも起こさない）
- CPU の計り: 受付の `lsp_result` / `lsp_state` の頭の `cpu_ms` と、alarm の `/v1/status` の `lsp_cpu_ms` の
  **増えた分**を CPU（vCPU 秒）として今月に足す（`Meter.addLspCpu`。コンテナを起こすと 0 から）。ジョブの
  間は「ジョブの時間 × vCPU」と重なるが、多めに数える側なのでそのまま足す

### serve（`zc-remote`）

- 状態機械（`session.rs`）に LSP の問い合わせを足した（`Session::lsp`）。木の確認はジョブより先に回す
  （打鍵の返事なので）。同じ種類のまだ送っていない問い合わせは `Cancelled` にする。**やり直さない**（切れたら
  `GaveUp`、30 秒答えが無ければ `Failed`）。温まっていた接続が死んでいただけなら 1 回だけ送り直す
- `Lsp` は `.rs` のファイルだけ（ほかは `NotApplicable`）。設定が無ければ `Auth`、つながらなければ
  `Offline` ですぐ返す（zcode を待たせない）

### zcode（`editor_lsp.rs`・`remote.rs`）

- メインは頼むだけ（文書の写しを取るのは O(1)。バイト列にするのは専用スレッド）
- **古い答えは捨てる**: 頼んだときの文書の版が変わっていたら何もしない（hover・定義・補完はカーソルの位置も）
- hover は補完の小窓と同じ部品で、カーソルの下に最大 8 行（空行を詰める）。次の打鍵で閉じる（Esc は閉じるだけ）
- 定義: 1 つでプロジェクトの中ならそのファイルを開いてその桁へ。外なら「定義は <場所>:<行>  <その行>」を
  ステータスバーに（読み取り専用のタブは作らない）。いくつもあれば一覧
- 使っている所・名前を探す: アウトラインと同じ一覧（`PickerList`）。プロジェクトの外の行は薄く出して選べない。
  名前を探すは、カーソルの単語で探し始め、検索欄の打鍵が 300ms 止まったら頼み直す
- 補完: Rust のタブで単語を打っている・`.` / `::` の直後に、打鍵が 300ms 止まったら頼む（打鍵ごとには
  頼まない）。届いた候補（6 件まで）を小窓の先頭に `rs` の札で足し、打った文字で絞って残す
- 知らせ: 「上限」「オフライン」「認証」「設定が無い」は、同じ理由が続く間 1 回だけ。「準備中」は自分で
  押した操作の返事なので毎回。そのあと準備できたら 1 回だけ「準備できた」。補完の失敗は何も出さない
- F8 / Shift+F8 は手元の診断（`DiagStore` の写し）だけを見るので、ビルド係が無くても効く
- Rust のファイルを開いて読み終わったら `Warm`（ビルド係の設定がある人だけ。無い人にはスレッドも serve も作らない）

## 6. 失敗・オフライン・上限

| 場面 | 型の情報（Ctrl+H など） | 補完 |
|---|---|---|
| Rust でないファイル | 「型の情報は Rust だけ」 | 頼まない |
| ビルド係の設定が無い | 1 回だけ「設定が無い」 | 頼まない |
| オフライン・つながらない・認証・時計 | 1 回だけ理由 | 何も出さない |
| 費用の上限（DO） | 1 回だけ「上限」 | 何も出さない |
| rust-analyzer が準備中 | 毎回「準備中」、準備できたら「準備できた」 | 何も出さない |
| 頼んだ後に編集した | 答えを捨てる | 答えを捨てる（止まったら頼み直す） |
| F8 | ビルド係に関係なく効く | — |

## 7. 試験

| 層 | 試験 | 回し方 |
|---|---|---|
| zc-wire | `msg::tests::lsp_messages`（正解の JSON） | 開発コンテナ: `cd zcode\remote; ..\..\container\dev.ps1 cargo test --locked --workspace` |
| 受付 | `lsp::tests`（UTF-16・URI・外の見せ方・答えの訳し方）、`tests/lsp.rs`（**本物の rust-analyzer**: hover・定義（中と外）・references・symbols・補完・重ねで型が変わる・1 ファイルだけ）、`tests/protocol.rs` の `lsp_messages_over_the_socket` | rust-analyzer はビルド係のイメージにだけある: `zcode` で `wslc run --rm -v "${PWD}:/work:ro" -w /work/remote -e CARGO_TARGET_DIR=/var/lib/zc-buildd/t -e ZC_E2E_REQUIRE=1 --entrypoint bash cz-builder:latest -c "cargo test --locked --workspace"` |
| serve | `tests/session.rs`（台本）、`tests/serve.rs` の `lsp_answers_at_once_when_it_cannot_ask`、`tests/e2e.rs` の `rust_analyzer_answers_through_serve`（本物の serve → 本物の受付 → 本物の rust-analyzer） | 同上（e2e はイメージの中で） |
| IPC | 全部の種類の往復・`Lsp` の正解のバイト列 | `..\container\dev.ps1 cargo test --workspace` |
| zcode | `tests/remote.rs`（偽物の serve）、`tests/lsp.rs`（偽物のビルド係で全部の操作・古い答え・知らせ・補完）、正解画像 `lsp-hover.png`・`lsp-references.png` | 同上 |
| Worker | `test/session.test.ts` の「LSP」（中継・keep-warm・CPU の計り・上限） | `cd zcode\remote\worker; npx tsc --noEmit; npx vitest run` |

## 8. 未確認（人が確かめる）

- **deploy が要る**: ビルド係のイメージ（rust-analyzer を足した）と Worker（DO の LSP の扱い）。deploy しないと、
  受付が `lsp` を知らないので、問い合わせは serve の 30 秒の時間切れで終わる
- Cloudflare の 1 vCPU での冷えた起動の時間（PC の 1 コアより遅い。2〜3 倍の見込み）と、往復の通信を
  含めた実機からの hover の時間
- 実機: Ctrl+H / Ctrl+D / Ctrl+R / Ctrl+T・F8（Fn+8）・Shift+F8 が押しやすいか、hover の小窓の読みやすさ
- 実機の `zc-remote` を版 3 に入れ替えること（入れ替えの順は §4.2）
- 戻る（定義へ飛ぶ前の場所へ）は v1 に入れていない

## 9. 全言語への横展開（2026-11-18、ユーザーの指示）

ユーザーの指示: Rust だけの型の情報を、言語サーバーのある言語全部に広げる。Rust 専用の作りなら汎用の部品に
して、後から言語を足しやすくする。**ビルド係のイメージは 1 つのまま**（言語ごとにコンテナを分けない。
分けると温まっている間の費用が言語の数だけ増え、冷えた起動も言語ごとに待つ）。

### 9.1 言語サーバーの表

| 言語（拡張子） | サーバー | 入れ方（`remote/container/Dockerfile`） | 準備の合図 | 木の外に置くもの・環境 |
|---|---|---|---|---|
| Rust（rs） | rust-analyzer | rustup の component | `experimental/serverStatus` の quiescent | 1 ファイルなら仮の crate（`lsp-single/`） |
| Python（py・pyi） | pyright-langserver | npm `pyright`（`/opt/tools`） | initialize の返事 | 設定 `python.analysis.diagnosticMode=openFilesOnly` |
| JS / TS（js・mjs・cjs・ts・mts・cts） | TypeScript 7 の `tsc --lsp --stdio` | npm `typescript`（check と同じもの） | 同上 | `tsconfig.json` が無ければ check と同じ既定を `proj/<id>/tsconfig.json`（`include: ["lsp"]`） |
| Go（go） | gopls | `go install`（sum.golang.org） | 同上 | `GOOS=linux GOARCH=arm64 CGO_ENABLED=0 GOFLAGS=-mod=mod`（ビルドと同じ対象） |
| Zig（zig・zon） | zls 0.17.0 | 配布物の SHA-256 | 同上 | build on save を切る |
| C / C++（c・h・cpp ほか） | clangd 23.1.0 | clangd/clangd の配布物の SHA-256、strip | 同上 | 無ければ `proj/<id>/compile_flags.txt`（`--target=aarch64-linux-gnu`。C++ のファイルがあれば `-xc++`） |
| Shell（sh・bash） | bash-language-server | npm（`/opt/tools`） | 同上 | shellcheck は check に任せる |
| Ruby（rb） | ruby-lsp（本体を直接起動） | gem を 1 つずつ SHA-256 で確かめて `--local` | 同上 | 空の `proj/<id>/ruby-lsp/Gemfile` を `BUNDLE_GEMFILE` で |
| MoonBit（mbt） | moon-lsp | MoonBit の配布物（`mooninfo` と core の wasm 版も残す） | 同上 | — |

- 表は `zc-buildd/src/lsp/servers.rs`（`Spec`）。橋渡し（`zc-buildd/src/lsp.rs`）はどの言語でも同じ:
  JSON-RPC・UTF-16 ⇔ UTF-8 の桁・didOpen / didChange・答えの訳し方・CPU の数え方・同時に 1 つ
- **言語は問い合わせのパスの拡張子で決める**。zc-wire・IPC・Worker の約束は変えていない（古い zc-remote とも
  話せる）。パスの無い問い合わせ（名前を探す）は、同じプロジェクトの今のサーバー、無ければ目録の最初の受け持ちの
  ファイル。同じプロジェクトでも別の言語のファイルに移れば、前のサーバーを止めて替える（同時に 1 つ）
- 実機側の対象は `zc_remote_ipc::RemoteLang::has_lsp`（`match` を網羅にして、言語を足すとここで決めることになる）。
  serve は `has_lsp` の言語なら、その言語の check と同じ決め方で根を決めて頼む。npm の依存の無い JS / TS
  （check は実機の tsc）は、型の情報だけそのファイル 1 つで頼む（`scan::single_file_project`）
- hover はサーバーに Markdown を先に頼み、受付が地の文にする（囲み・区切り線・見出し・インラインのコード・
  リンク・太字を外し、同じ段落は 1 度）。tsc は plaintext だとシグネチャと説明を区切らずにつなぐので Markdown にした
- 同じ場所を 2 度返すサーバー（ruby-lsp）があるので、場所の一覧は（中・外・行・桁・名前）で重複を除く
- 補完の小窓の札は、言語によらず「ls」（前は Rust だけで「rs」）
- 受付は問い合わせのたびに lsp の木を目録にそろえる。サーバーが木の根に作るもの（moon-lsp の `_build`・zls の
  `.zig-cache`・clangd の `.cache`・ruby-lsp の `.ruby-lsp`）は `Spec::keep` に書いて消さない。**消すと moon-lsp は
  別のパッケージの型を読めず、何も答えなくなった**（2026-11-18、実機のサンプル `moonbit/cmd/main` で見つけた）
- サーバーのエラーの知らせ（`window/showMessage` のエラー・警告、`window/logMessage` のエラー、標準エラーのエラーらしい行）は、
  状態（`lsp_state`）の `message` として実機へ流す（状態は変えない）。Cloudflare のコンテナの中は外から見えないので、答えが空の
  ときの手がかりにする

### 9.1.1 問い合わせ中の小窓（2026-11-18、ユーザーの決定）

- 応答待ちで操作は止まらないが、Ctrl+H などを押してから答えまで画面に何も出ず、冷えたコンテナでは最大 30 秒近く
  「押せたのか分からない」時間があった。ステータスバーではなく**視線の先**に出す、という方針で 4 案をモックで比べ
  （Artifact「zcode 問い合わせ中のスピナー」）、**案 C（答えが出る場所 = カーソルの下に、空の小窓を先に出す）**に決めた
- Ctrl+H・Ctrl+D・Ctrl+R で、250ms たっても答えが無ければ小窓を出し、7×7 の点の輪を 120ms ごとに回す（描き直すのは
  小窓の行だけ）。文は「型と説明を問い合わせ中」など。答えが来たら消して答えに替える
- **どの打鍵でも消す**（ユーザーの決定。動いた・書いた後の答えはもともと捨てる。答えの小窓も次の打鍵で閉じるのとそろえる）
- 失敗・時間切れは 1 秒だけ赤い × と「問い合わせできなかった」にしてから消す（理由はステータスバー）。「準備中」はすぐ
  返るので × にしない
- 部品は `zc_ui::CompletionPopup` に印（`PopupMark`）と「案内の行なし」を足しただけ。正解画像 `lsp-waiting.png`・
  `lsp-waiting-failed.png`

### 9.2 新しい言語の足し方

1. `zc-buildd/src/lsp/servers.rs` に `Spec` を 1 つ書いて `SPECS` に入れる（拡張子が重ならないことは単体試験が見る）
2. ビルド係のイメージにサーバーを入れる（版と SHA-256 を ARG で固定）。大きさを測って `zcode/AGENTS.md` に書く
3. `zc_remote_ipc::RemoteLang::has_lsp` を `true` にする（言語そのものが新しければ、`RemoteLang`・`cli::lang_of`・
   `scan::find_project` も）
4. `zc-buildd/tests/lsp_langs.rs` の `CASES` に「関数を定義して別の所で使う」小さなプロジェクトを足し、イメージの
   中で回す（hover と定義へは必ず通す）

### 9.3 本物のサーバーで確かめたこと（`tests/lsp_langs.rs`、PC のイメージ、2026-11-18）

10 言語すべてで、使っている所での hover（名前と説明が出る）と定義へ（定義の行へ）が通った。Python は補完も。

| 言語 | 準備まで | 使っている所 | 名前を探す | 気づいたこと |
|---|---|---|---|---|
| Python | 0.1 秒 | 3 | 1 | |
| TS / JS | 0.02 秒 | 3 / 2 | 1 | 型定義（`.d.ts`）の場所は npm の `<パッケージ>/…` で見せる |
| Go | 0.02 秒 | 2 | 1 | |
| Zig | 0.002 秒 | 2 | 0 | zls は workspace/symbol に答えない |
| C / C++ | 0.003 秒 | 1 / 2 | 2 | イメージに x86_64 の C++ の見出しが無いので、clangd は aarch64（クロスの gcc の見出し）で見る |
| Shell | 0.15 秒 | 2 | 1 | 開いた直後の hover は空のことがある（2 回目で出る）。シバンの行を説明に含める |
| Ruby | 0.26 秒 | 2 | 1 | 開いた直後の hover は空のことがある。**トップレベルの `def` の呼び出しは追えない**（クラス・モジュールの中は追える）。`tmp` という名前のフォルダの下は索引しない |
| MoonBit | 0.004 秒 | 0 | 0 | moon-lsp は中で既定のターゲット（wasm）の `moon check` を回すので、`mooninfo` と core の wasm 版が要る。名前を探す・使っている所は開いた直後は 0（索引の後なら出る） |

- 準備までの時間は PC（wslc、制限なし）。Cloudflare の 1 vCPU では未計測
- イメージの大きさ（展開後）: deploy 済みの版 2.38GB → **2.68GB（+300MB）**。clangd を Debian の apt で入れると
  libllvm19・libclang-cpp19・z3・grpc などを引き込んで +270MB になったので、clangd/clangd の静的リンクの配布物を
  strip して入れた（本体 62MB + 見出し 17MB）。ほか gopls 31MB・moon-lsp 23MB・zls 20MB・mooninfo 13MB・
  core の wasm 版 20MB・ruby-lsp の gem 28MB・bash-language-server と依存 23MB
- zc-remote は 0.3.0（serve が Rust 以外のファイルも頼む）。**zc-remote を先に入れ替え、`zc-remote stop` してから
  zcode を入れる**（古い serve は Rust 以外を「対象ではない」で返すだけなので、壊れはしない）

### 9.4 未確認

- **deploy が要る**（イメージ）。deploy しないと、受付が Rust 以外のサーバーを知らず「型の情報を出せない種類のファイル」で返す
- Cloudflare の 1 vCPU での各サーバーの準備の時間とメモリ（rust-analyzer 以外は測っていない。どれも rust-analyzer より
  ずっと軽い見込み）、冷えた起動（イメージが +300MB 大きくなった分の取得）
- 実機での表示（hover の小窓の折り返し、clangd の「Parameters:」のような長い説明）
- 補完は Python 以外の候補の中身を確かめていない（受付の絞り込みはどの言語でも同じ）
