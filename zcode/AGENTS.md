# zcode

VS Code 風のコードエディタ（APPLaunch アプリ）。**Rust** で書く。段階 6（ショートカット一覧・
控え・.deb）まで実装済み（`docs/design.md` §11）。

## 文書

| ファイル | 中身 |
|---|---|
| `docs/design.md` | 設計・crate 構成・スレッド構成・遅延の目標・ハイライトの拡張方式・SD 前提のデータの持ち方・LSP の方針・段階とモデルの担当。**実装担当は自分の節と「共通の約束」（§3）を先に読む** |
| `docs/measurements.md` | 設計の根拠になる実測値（測り方・条件・出典つき）。数字を変えるときはここを先に直す |
| `docs/stage1-notes.md` | 段階 1 の作業メモ: API の決定と理由、design.md への提案、実機の計測、未解決の点、**キー確認の進め方** |
| `docs/ui-mock.html` | 画面のモック（320×170 実寸を拡大表示）。画面 1〜11 とショートカットの全項目 |
| `docs/setup-remote-build.md` | **人向け**の導入手順: Cloudflare（プラン・Zero Trust・サービストークン・Access・deploy・秘密の値）、Tailscale と PC（ACL・ファイアウォール）、実機ごとの設定、トークンを失くしたとき |
| `docs/remote-build.html` | 外でビルド・試験・型検査する仕組みの設計（Cloudflare Containers / PC、認証、送る形・返す形、実機側の別プロセス `zc-remote`）。却下した案の理由つき。未実装。ユーザーの決定（2026-11-16）: Cloudflare が前提、`zc-remote` の TLS の暗号は ring（「pure Rust だけ」の例外。zcode 本体には入れない）、最後に成功した実行ファイルをプロジェクトごとに 1 つ SD（`~/.cache/zcode/bin/`）に残す（「再計算できるキャッシュは SD に書かない」の例外。オフラインでは作り直せないため） |
| `docs/stage3-design.md` | 段階 3 の残りの設計: 外部ビルド（受付 `zc-buildd`・実機側 `zc-remote`・Cloudflare の Worker / DO / コンテナ・約束）、zcode へのつなぎ込み、診断の持ち方、JS / TS の F5、人がやる準備、**課題の表（持ち物・順番・担当）**。課題は C4（スナップショット。最初の deploy の後）以外実装済み（2026-11-17）。`remote-build.html` と食い違うところはこちらが優先 |
| `bench/` | 実測に使ったベンチマーク（`bench/README.md`）。測り直すときはこれを使う |

## 守ること（設計の要点）

- **メインスレッドで待たない**: `fsync(fb0)` は flush スレッド、フォントと SKK 辞書の
  mmap に触るのは assist スレッド、SD への書き込みは storage スレッド。
  描画処理そのもの（文字合成・行差分）は並列化しない（全画面でも 0.7ms で、
  並列化しても縮まない）
- **液晶**: 書いたら fsync（しないと最大 56ms 待つ）。転送は 1 つの行範囲に丸められ、
  開始までの待ちも行数に比例するので、離れた変更は範囲ごとに分けて送る。
  続けて fsync すると最大 7ms 止まる（だから flush スレッドで呼ぶ）
- **ハイライト**: syntect は使わない（1 行 1.8ms・初回 200ms 停止・+11MB）。独自の
  規則（テキスト）をビルド時に DFA に変換して埋め込む（1 行 0.004ms）。言語の追加は
  `languages/<言語>/` に `language.toml`（設定と規則）と見本を置くだけ。規則には
  後方参照・先読み・Unicode 文字クラスを使わない
- **SD**: キャッシュを SD に書かない。控えは RAM（tmpfs）と SD の 2 段。保存は
  一時ファイル → fsync → rename
- **LSP**: 常駐させない（jedi で初回 9〜11 秒・60MB）。最初の版は単語補完・保存時の
  構文チェック（`ast.parse`、約 0.4 秒）・アウトライン
- 担当（Fable / Opus / Sonnet）は API（`zc-core` の型と各 crate の公開関数）を
  勝手に変えない。変えたいときは統合側（メインのセッション）に戻す

## 決めたこと（2026-11-16）

- 用途: 実機でコードを書いて、そのまま実行する
- 最初の版の機能: シンタックスハイライト、ファイルツリー、SKK 日本語入力、
  Markdown ライブプレビュー、ショートカット一覧、統合ターミナル／F5 実行、
  単語補完・保存時の構文チェック・アウトライン。クイックオープン・LSP は後の版
- 描画は fb0 へ直接（Python の zero_ui は使わず、Rust で同等のものを持つ）。
  配色は VS Code の Dark+、等幅フォントは実機の DejaVu Sans Mono
  （日本語は Noto Sans Mono CJK にフォールバック）
- エディタの文字は DejaVu Sans Mono 10px・行の高さ 12px（ユーザーが実機で決定。`zc-ui` の
  `CODE_PX` / `CODE_LINE_H`）
- 作業フォルダの既定は `~/code`。終了は Ctrl+Q（Esc は終了に使わない）
- 日本語入力: **SKK を zcode に組み込む**。uim-skk は GTK / Qt / uim-fep 経由でしか
  効かず fb0 アプリに届かないため。辞書は実機の `/usr/share/skk/SKK-JISYO.L` と
  個人辞書 `~/.skk-uim-jisyo`（どちらも EUC-JP）をそのまま読み書きし、uim-skk と
  学習を共有する。システム辞書は全部読み込まず mmap + 二分探索（okuri-ari は降順、
  okuri-nasi は昇順で並んでいる）。入力モードはステータスバー右端（緑「かな」/
  黄「カナ」/「A」）、起動時は英数。▽ 読み入力 → Space で ▼ と候補一覧
  （asdfjkl で選択）→ 候補が無ければステータスバー上の登録欄で辞書登録。
  ターミナルパネルにも確定文字を送る
- Markdown（モック 10・11）: .md はライブプレビューで開き、装飾を付けたまま編集する。
  記号はカーソル行だけ薄く見せる。Ctrl+Shift+V でソース表示に切替。本文は
  プロポーショナル（Noto Sans CJK）、コードは等幅なので、描画は **行ごとに高さと
  字幅が変わる**前提で作る。CJK には斜体の字形が無いので、斜体は字形を傾けて合成する
- **よく使う操作は 2 つ押しまでで済ませる**（2026-11-17 ユーザーの方針）。Sym の記号（` / など）や
  Fn の矢印・F キーは、それだけで 2 つ押しになることに注意する。Ctrl+Shift+`（Ctrl・Shift・Sym・W の
  4 つ押し）は実機で押せなかったので、ターミナルの開閉は F12（Fn+P）、全画面は F11（Fn+O）にした
- ショートカット一覧画面: F1（Fn+1）で開く。文字入力で絞り込み、↑↓ で選んで Enter で
  その操作を実行（コマンドパレットを兼ねる）。一覧と実際のキー割り当ては
  **同じ 1 つの表** から作る

## 作り方・試し方

ワークスペースは `zcode/Cargo.toml`（`crates/*`。`bench/` は別）。Windows では開発
コンテナで回す（`cd zcode` してから）:

```powershell
..\container\dev.ps1 cargo test --workspace       # 単体テストと render-test
..\container\dev.ps1 ./build.sh                   # 実機用 (aarch64) → out/zcode
..\container\dev.ps1 cargo run -p zcode -- --fonttest --page 2 --render-test out/x.png
```

- render-test の正解画像は `tests/golden/`。描画を変えたら
  `ZCODE_UPDATE_GOLDEN=1 cargo test -p zcode --test render` で更新し、**画像を目で
  見てから**コミットする。不一致のときは隣に `*.actual.png` が出る（git 管理外）。
  実機で描いた PNG は開発コンテナの正解と 1 バイトも違わない（2026-11-16 確認）ので、
  描画の確認はコンテナだけで足りる
- 段階 1 で動く画面: `--keytest [--guide]`（キー確認）、`--fonttest`（フォント比較）、
  `--bench <line|line-status|scroll|full>`（描画の計測）。どれも `--stats` で終了時に
  計測値（起動の内訳、フレームの所要、fsync、RSS）を標準エラーに出す。`--help` 参照
- crate を足すときは `crates/<名前>/` に置けば入る。ほかの crate とやり取りする型は
  `zc-core` に置く（横同士は依存しない）

## 実機で動かすときの決まり

- 実機には `/tmp` にだけ置き、終わったら消す。**必ず `timeout` を付ける**
  （例: `timeout -s INT -k 5 30 /tmp/zcode --fonttest --auto 3000 --stats`）。zcode は
  キーボードを独占（`EVIOCGRAB`）し、液晶を上書きするので、固まると実機を操作できなくなる
- zcode は、ふつうの終了・SIGINT / SIGTERM / SIGHUP・panic・メインが固まった場合の
  どれでも、独占を放して起動前の画面に戻す（`crates/zcode/src/crash.rs`。実機で確認済み）。
  新しい終了経路を足したら、`--bench panic` / `--bench hang` と同じやり方で確かめる
- キー確認モードの終了は Esc を続けて 5 回（無操作 120 秒でも終わる）

## 段階 1 で分かったこと（2026-11-16、詳しくは `docs/stage1-notes.md`）

- 打鍵 1 回ぶんのこちらの持ち時間（`poll()` から戻る → 描画 → 行差分 → flush スレッドが
  `fsync` を呼ぶまで）は p50 0.2ms / p95 0.35ms。スクロールは 0.5 / 0.8ms。fsync →
  転送開始の 4.6ms（measurements.md）が支配的
- テキスト 1 行（11 画素行）を変えると、SPI には約 12.9KB（3 ページ ≒ 19 行ぶん）出る
- 起動 → 最初のフレームは 10〜12ms、RSS は約 6MB、スレッドは 3 本
- **APPLaunch は、ランチャー表示中は右上の時計と電池（行 2〜18）を描き続ける。** SSH から
  起動した zcode の画面は、そこだけ上書きされる（SPI は 17.9kB/秒のまま）。APPLaunch から
  アプリとして起動したときに止まるかは未確認
- `/dev/input/event*` を全部開いて名前を調べると 60〜85ms かかる。キーボードは sysfs
  （`/sys/class/input/event*/device/name`）で探して、目当てのものだけ開く
- 初めての字が多い画面で、字形を 1 字ずつ最大 2ms 待つと 1 フレームが 145ms になった。
  1 フレームの合計にも上限（6ms）を入れてある（`zc_font::FRAME_WAIT_FACTOR`）
- **実機の実行ファイルに `toml` を入れない。** fat LTO の aarch64 ビルドで `toml` 0.8 の
  パーサが異常終了した（原因は未特定。x86_64・thin LTO・LTO なしでは起きない）。
  キー配列は `zc-input/build.rs` がビルド時に Rust の表に変換する。ユーザーの設定を
  実行時に TOML で読む段階になったら、先にこれを片づける
- キーボードのデバイスツリーの表は `keymap/cardputer-zero.toml` に写してある（キーコードと
  行列の位置）。イベントには MSC_SCAN（行列の位置）も付く。Sym レイヤーの記号は
  ランチャーと同じ表で確定済み（実行時はドライバの付属ファイルで上書き。
  `zc_input::Keymap::load_driver_map`）。キーを 1 つずつ押してもらう手順は要らなくなった

## 段階 2 で分かったこと（ハイライトと書き込み層、2026-11-16）

- 言語の足し方・`language.toml` の形・正規表現の制限は `languages/README.md`。規則の確認は
  `cargo run -p zcode-langc -- check|show`、`.sublime-syntax` の取り込みは `... -- import`
- **キーワードは `[words]` に書く**（正規表現の選択で書くと DFA が数百 KB になる）。
  文脈の最後の `include` は DFA を共有する。1 言語 100KB を超えるとビルドが警告し、
  `cargo test` が落ちる
- 見本の正解は `tests/golden/highlight/<言語>.txt`。言語のフォルダを足すと自動でテストされる
- 書き込みは `zc-store`（原子的な保存、CRC 付きの独自形式と `.bak`、2 段の控え、SKK 個人辞書の
  書き戻し）。storage ワーカーには `zc_store::submit` で依頼する

## 段階 2 の統合で決めたこと（2026-11-16）

- 保存は `zc-store` 経由（`save.rs` の `StoreSaver`）。内容が前回と同じでディスクも
  変わっていなければ書かない（「変更なし」）。開いた後にほかのプログラムが変えた
  ファイルを上書きしたら、ステータスバーで知らせる
- 保存できたら、`language.toml` の `check` を **`check` ワーカー**で使い捨てで走らせる（`intel.rs`。段階 3 で batch から移した）。
  `remote` のある言語はまずビルド係で check し、届かなければ手元の `check` に戻す。
  エラーは赤い波線・赤いタブ名・ステータスバーの件数 `⊗ n` と右のメッセージ（モック 13）。
  波線は編集に合わせて行をずらす
- 単語補完（モック 12）: 単語を 2 文字打つと自動で出す。Tab で確定（↑↓ で選ぶ、Esc で
  閉じる）。Enter は改行のまま。文字列・コメントの中、単語の途中では自動で出さない
  （Ctrl+Space なら出す）。確定は `TextDoc::replace_range` で 1 回の取り消しで戻せる
- アウトライン（モック 14）: Ctrl+O。`language.toml` の `[outline]` から作る。絞り込みで
  一致した項目の親は薄く残す（選べない）
- 小窓（補完）を閉じる・動かすときは、下にあった行だけ描き直す
  （`EditorView::invalidate_rows`）。全部を描き直すと SPI の転送が全画面ぶんになる
- エディタの試験は `SimpleDoc` と本物の `zc_buffer::Buffer` の両方で回す（`both!`）。
  一時フォルダは試験ごとに別にする（同じ名前だと並列に走ったときに消し合う）
- Shell / C の `check`・`run`、Rust の `run` の書き方は実機で未確認（`bash -n` / `gcc` の
  出力の形、実機に gcc・rustc があるか）

## TypeScript / JavaScript の方針（ユーザーが 2026-11-16 に決定）

- 言語は `languages/javascript/`（js / mjs / cjs）と `languages/typescript/`（ts / mts / cts）。
  JSX / TSX はタグの対応が書けないので対象外（限界は各 `language.toml` の先頭）
- 型チェックは**公式の TypeScript 7（Go 版。npm `@typescript/typescript-linux-arm64`）の
  `tsc`** を使う。tsc-rs は実機で 1.3〜2.2 倍速く軽かったが、`ulimit -v` が要る・終了コードが
  違う・作者が読んでいない AI 移植、という理由で採らない（サプライチェーンの信頼と継続的な
  更新を優先）。`tsc` が無い間は「構文チェックできない」と出る。実機にはまだ入れていない
- 型チェックは必須。TS の F5 は `tsc` で型チェックして JS を出力し、それを実行する
  （型を剥がすだけの esbuild は型チェックしないので使わない）。`run` は未設定（F5 の設計は後）
- JS の実行系は Cortex-A53 向けにビルドした LLRT（`runtimes/llrt/`）
- `--lib es2022,dom` は実機の Go 版 tsc で約 3.8 秒・約 91MB（実測）。LLRT のグローバルだけの
  小さな ambient `.d.ts` にすれば減らせる。ランタイムを入れるときに見直す

## 段階 3 の zc-term で分かったこと（2026-11-17）

- 使い方（fd の poll・`closed` で poll から外す・`try_wait`・`take_events`・画面の約束）は
  `crates/zc-term/src/lib.rs` の先頭。試験は本物の pty で回る（`cargo test -p zc-term`）
- **F5 の包み方**: `bash -c 'trap : INT; <cmd>; printf "\e]777;zcode-exit;%d\a" $?; exec bash -i'`。
  `trap : INT` が無いと Ctrl+C で外側の bash も死に、終了コードもシェルも残らない
  （`trap '' INT` は無視が子に引き継がれて Ctrl+C が効かなくなるので不可）
- **`send_text` は貼り付けの扱い**: bash 5.2 / Python 3.13 の REPL は括弧付き貼り付けを
  有効にするので、改行入りの文字列は囲まれて「実行されない」（貼り付けの安全策どおり）。
  コマンドを実行させたいときは `send_text(cmd)` のあと `send_key(Enter)`
- ロケールが空（C）だと readline が UTF-8 の字幅を数え違えて日本語の行の折り返しが崩れる。
  子に `LANG` / `LC_CTYPE` / `LC_ALL` が無ければ `LC_CTYPE=C.UTF-8` を足す（APPLaunch から
  起動した zcode の環境にロケールがあるかは未確認）
- zcode は SIGPIPE を無視している。子では全シグナルを既定に戻し、マスクも空にしてから exec
  する（しないと `yes | head` が終わらない）
- 解析の速さは実機で 8.6〜12MB/s（`docs/measurements.md` §10）。2ms の予算で 16KB 前後

## 段階 3 の組み込みで決めたこと（2026-11-17、モック 3・4）

- パネル（`zc-ui` の `TerminalPanel`）は下に 6 行、F11（Fn+O）で全画面（Ctrl+Shift+` は Sym も要る 4 つ押しで、実機で押せなかった）。中身が変わった行だけ
  描く（端末の dirty の印には頼らず、前回描いた行と比べる）。全角の右半分の空白は飛ばす
- キー: F12（Fn+P）は「閉じていれば開いて端末へ / 端末にいれば閉じてエディタへ / エディタに
  いれば端末へ」。端末にいる間は、F12 / F11 / F5 / Ctrl+Q 以外は全部端末へ送る
  （Ctrl+C も）。Shift+PgUp / PgDn でスクロールバック
- F5: 未保存なら保存が終わってから（`Saved` を待つ）、言語の `run` を新しい端末で実行する。
  作業フォルダはファイルのフォルダ、`{file}` はファイル名。前の端末は閉じる。`run` の無い
  言語は知らせるだけ。実行中はステータスバーが橙、終わったら終了コード（0 は緑、それ以外は赤）。
  終了コードは独自の OSC（`TermEvent::CommandExit`）で受け取り、そのままシェルになる
- イベントループは `Screen::poll_fd` の fd も待つ。打鍵を処理してから端末を読み、読み残しが
  あれば次の `poll()` を待たない。シェルが終わったら端末を捨てる（fd が poll から外れる）
- 正解画像はモック 3（実行中）だけ。モック 4（エラー終了）はシェルのプロンプトが環境で変わる
  ので、`cargo test -p zcode --test terminal -- --ignored` で `out/terminal-error.png` に描いて
  目で見る

## 段階 3 の残りの設計で決めたこと（2026-11-17、`docs/stage3-design.md`）

- 外部ビルドは **別のワークスペース `remote/`**（`zc-wire`・`zc-remote`・`zc-buildd`、Worker、
  イメージ）。ring・rustls・serde_json を zcode 本体の `Cargo.lock` に入れないため。回し方は
  `cd zcode\remote; ..\..\container\dev.ps1 cargo test --locked --workspace` と `./build.sh`
  （`out/zc-remote` = 実機用、`out/zc-buildd` = ビルド係用）
- zcode 本体に足したのは、診断の型（`zc_core::diag`。`zc-intel` からも同じ名前）、診断の置き場
  （`zc_intel::DiagStore`・`ShiftLog`）、UNIX ソケットの約束（`zc-remote-ipc`）、専用スレッドの
  口（`zcode/src/remote.rs`）、`AppEvent::Remote`、`check` ワーカー、`language.toml` の `remote`。
  2026-11-17 時点で C4 以外の課題（W1・W2・B1〜B4・D1〜D5・I1・Z1〜Z3・C1〜C3・H1・R1・M1）は実装済み・統合済み
  （`zcode-stage3-remote`）。zcode 側は `DiagStore`（ソースごとの置き換え・遅れた結果のずらし）、
  保存時のビルド係の check、F6（Fn+6）の「試験を実行」（モック 15）、起動時に `~/.local/bin` を PATH の先頭へ
- **雛形の公開の形（型・関数の名前と引数）は、課題の担当が勝手に変えない。** 課題は
  `docs/stage3-design.md` §13 の表のとおりに分け、課題どうしで同じファイルに触らない
- F5（Rust / C）は、ターミナルで `zc-remote run <file>` を動かす形にする（zcode の F5 の
  仕組みは変えない）。保存時の check と「試験を実行」は専用スレッド経由
- ring は開発コンテナのクロスの gcc だけで aarch64 にビルドできた（LSE / RCpc の命令は 12 個）

## ビルド係のイメージ・deploy・CI（課題 C3、2026-11-17）

- イメージ `remote/container/Dockerfile`（文脈は zcode/、`zcode/.dockerignore` で絞る）。版は ARG で固定:
  ベース `debian:trixie-20261104-slim@sha256:…`、rustup 1.29.3（rustup-init の SHA-256）、Rust 1.100.0、
  Node 26.13.1（SHASUMS256 の値）、esbuild 0.28.2・TypeScript 7.0.2（`remote/container/tools/package-lock.json`
  の integrity、`npm ci --ignore-scripts`）。git・Python・フォントは入れない。実行は uid 1000 の builder。
  `CARGO_HOME` と `npm_config_cache` は置き場（`/var/lib/zc-buildd`）の中（PC ではボリュームに残る）
- 大きさ: 展開後 1.52GB、送る形（gzip の層の合計）約 500MB（Rust 263・apt 140・Node 52・ベース 29・
  tsc / esbuild 15・受付 0.4MB）。`/opt/rustup` が 762MB で大半
- 版を上げるとき: ARG の値（と `tools/` の lock）を書き換え → `run-pc.ps1 -Build` → 中で試験:
  `wslc run --rm -v "<zcode>:/work:ro" -w /work/remote -e CARGO_TARGET_DIR=/var/lib/zc-buildd/t --entrypoint bash cz-builder:latest -c "cargo test --locked -p zc-buildd"`。
  `bash tools/test-install-runtimes.sh` もイメージの中で通る（60 件）
- イメージの中では zc-buildd の試験が 1 つも飛ばない（zstd・node・tsc・esbuild がある）。TypeScript 7 の tsc は
  `return /* 日本 */ "2";` の型の誤りを `return` の位置に出すので、Node の試験は誤りを CJK の後ろの引数に置く形に
  直した（2026-11-17）。`tests/jobs.rs` の、殺した直後のプロセスグループ（zombie）とフォルダの消え方を
  すぐに見る 3 つの試験も、消えるまで待つ形に直した（60 回続けて通る）
- Worker の依存は `remote/worker/package-lock.json`（wrangler 4.163.0・vitest 3.2.7・TypeScript 7.0.2・
  workers-types 5.20261116.1）。`.npmrc` で `ignore-scripts=true`（esbuild・workerd は OS ごとの
  optionalDependencies で動く）。試験は Node の上（`vitest.config.ts`、`node:sqlite` で Node 22.13 以上）。
  `@cloudflare/vitest-pool-workers` は使っていない
- `wrangler.jsonc` の項目名は wrangler 4.163.0 の `config-schema.json` と公式の文書で確かめた
  （`containers[].scheduling_policy: "durable_object"`・`images.<名前>.{dockerfile,build_context}`。
  この方式では `instance_type`・`max_instances` は書けず、大きさは DO が `start({instance})` で渡す →
  `vars.INSTANCE_TYPE`）。DO は `migrations` ではなく `exports`（公式の例の形。両方は書けない）。
  実物での deploy は未確認
- `wrangler deploy --dry-run` は、Dockerfile のイメージがあると **dry-run でも docker を呼ぶ**。
  `--containers-rollout=none` を付けるとイメージを飛ばす（CI はこれで設定と束ね方だけ確かめる）
- **deploy は Windows から `remote\container\deploy.ps1`（docker なし）**。wrangler の呼ぶ docker を
  `wrangler-docker/docker-shim.mjs` が wslc に読み替える。Windows の Node は `.cmd` / `.ps1` を shell なしで
  起動できない（EINVAL）ので、`WRANGLER_DOCKER_BIN` には `.mjs` のパスを入れ、`NODE_OPTIONS=--require
  spawn-hook.cjs` がその起動だけを `node docker-shim.mjs` に差し替える。wrangler 4.163.0 が deploy で呼ぶ
  docker: `info`・`build --load -t T --platform linux/amd64 --provenance=false -f - <文脈>`（Dockerfile は
  標準入力）・`image inspect T --format <Go の書式>`・`login --password-stdin`・`tag`・`push`・
  `manifest inspect -v`・`image rm`。確かめたこと: `deploy.ps1 -DryRun` が通る（wslc でイメージを作り、
  後で消す）、ローカルのレジストリ（`registry:3.0.0`）への tag・login・push・RepoDigests・manifest の読み替え。
  未確認: `registry.cloudflare.com` への login・push（最初の本番の deploy で）
- wslc（3.0.5）で分かったこと: `-p 7411:7411` は **127.0.0.1 だけ**で待つ（docker と違う）。
  `-p <アドレス>:7411:7411` はそのアドレスだけ。Windows 側で待つのは `dllhost.exe`。名前付きボリュームは
  初回にイメージの中身と持ち主で作られる。`.dockerignore` と `build -f -` は効く。`build` に
  `--platform`・`--load`・`--provenance` は無い。`image inspect --format` は json だけ（Go の書式は無い）。
  `manifest` コマンドは無い。push のあと RepoDigests は埋まる。`localhost:<公開したポート>` のレジストリへ
  push できる
- `run-pc.ps1`: Tailscale のインターフェースの IPv4 だけに公開（無ければ止まる）、`-Bind 127.0.0.1` で手元だけ、
  `-Detach` / `-Stop`。実機（tailnet の別の機体）から届くかは、ACL とファイアウォールを入れてから確かめる
- CI: `zcode-remote`（開発コンテナで remote の `cargo test`・`build.sh` の aarch64 と LSE の数・
  `test-install-runtimes.sh`）、`zcode-worker`（tsc・vitest・`wrangler deploy --dry-run --containers-rollout=none`）、
  `zcode-builder`（イメージを作って中で `cargo test -p zc-buildd` と `/healthz`。イメージか受付が変わったときだけ。
  層は GitHub Actions のキャッシュ scope=cz-builder）。zcode 本体の `zcode` ジョブは `zcode/remote/` だけの変更では回らない

## 段階 4 の zc-skk で分かったこと（2026-11-17）

- 使い方（打鍵 → `lookup` を assist で引く → `candidates_ready`、`take_changes` を zc-store の
  `DictChange::Learn` に、フォーカスが移ったら `reset`）とキーの表・本物の SKK との違いは
  `crates/zc-skk/src/lib.rs` の先頭。試験は辞書を EUC-JP で作って回る（`cargo test -p zc-skk`）
- **探索の結果を渡すまで、続く打鍵はエディタが取っておく**（「Space の直後の a」を正しく扱う
  ため）。取っておかなくても Skk が取っておくが、その打鍵で確定した文字列は次の打鍵まで遅れる
- ▼ では `asdfjkl` と `x` が候補の操作なので、`k` などは確定しない（SKK と同じ）。続けて
  打つなら Enter で確定する
- 実機の SKK-JISYO.L: 冷えた状態の `open` 約 19ms・1 回目の探索約 35ms、温まれば 1 回 0.01ms。
  全 175,786 項目が二分探索で見つかる（並びの前提は正しい）。`docs/measurements.md` §11

## 段階 4 の組み込みで決めたこと（2026-11-17、モック 7〜9）

- エディタとターミナルの打鍵は、まず `zc_skk::Skk` を通す（ツリー・入力欄・アウトラインでは通さない）。
  確定した文字は文書（ターミナルなら `send_text`）へ入れ、SKK が使わなかった打鍵はふつうに処理する。
  英数モードで SKK が使うのは Ctrl+J だけ
- 辞書引きは assist スレッド。システム辞書は最初に引くとき（またはかなモードに入ったとき、先読み
  つきで）開く。結果を待つ間の打鍵は取っておき、結果の後に流す（「Space の直後の a」）。
  試験では `ZCODE_SKK_DICT` で小さな辞書を指す
- 学習した語は 5 分ためて（`TimerId::SkkFlush`）と Ctrl+Q での終了時に、zc-store の
  `SkkSync`（uim-skk と同じロック → 読み直し → 重ねて書く）で書く。SD に毎回書かないため。
  **Esc の長押し（ランチャーの強制終了）や電源断では、ためていた分は書かれない**
- 個人辞書 `~/.skk-uim-jisyo` は起動時にメインで読む（数 KB）。2 号機には無い（最初の学習で作られる）
- 表示: 本文の未確定は `EditorView::set_preedit`（▽ は下線、▼ は青地、送り仮名は `*` の後ろに薄く）。
  候補一覧は ▼ の語の下（下に入らなければ上）、登録欄はステータスバーの上に 1 行で、その分エディタ・
  ツリー・ターミナルを縮める。登録中は本文に ▼ 読みを残す。ターミナルでの未確定はステータスバーの左に出す
- 正解画像: `skk-reading.png` / `skk-converting.png` / `skk-registration.png`（`cargo test -p zcode --test skk`）

## 段階 5 の組み込みで決めたこと（2026-11-17、モック 10・11）

- .md（言語名が Markdown）はライブプレビュー（`zc_ui::MdLayouter`）で開く。解析結果
  `zc_markdown::MdDocument` はタブ（`MdState`）とレイアウトで `Rc<RefCell<_>>` を共有する
- 編集のたびに `MdDocument::edited` で編集したブロックだけ直し、プレビュー中なら
  `EditorView::recheck_all`（見えている行を組み直し、変わった行だけ描く）。`edited` が `true`
  （境目が変わったかもしれない）なら全体を解析し直す: `MD_MAIN_PARSE_LINES` 行まではメインで、
  超えたら batch（版つき。古い結果は捨てて頼み直す）
- Ctrl+Shift+V でプレビュー ⇔ ソース（`set_layouter`。同じバッファ・同じカーソル）。ソース表示は
  コードと同じ等幅で、ハイライトは `languages/markdown`。ステータスバーの左に「プレビュー」/「ソース」
- Ctrl+Enter でカーソル行のチェックボックスを切り替え（1 回の取り消しで戻る）。Alt+B / Alt+I で
  選択を `**` / `*` で囲む（選択が無ければ記号の組を入れて間にカーソル）
- 正解画像: `md-preview.png` / `md-source.png`（`cargo test -p zcode --test markdown`）
- 実機の解析: 1 行約 4.5µs（`docs/measurements.md` §12）。`MD_MAIN_PARSE_LINES` = 400
- 見た目でモックから変えたところ（`zc-ui` の `mdlayout.rs`）: コードと記号は等幅 9px（DejaVu Sans Mono は
  8px 以下で `z` の斜線が消えて `=` に見える。8px 以下の等幅を使うところは同じ問題に注意）。カーソルの無い
  空の行は 6px、``` の行は 2px につぶす。引用は自分の行に `>` がある行だけ線を引く（CommonMark の
  遅延継続は引用として描かない）。番号付きリストの番号は青で残す
- `zc-markdown` の `edited` はブロック単位で解析し直し、`false` のときは全体の解析と同じ結果になる
  （ランダムな編集の試験で確かめている）。`]:`（参照リンクの定義）がある文書は毎回 `true`
- レイアウトは解析結果を `try_borrow` で読む。`MdDocument` を `borrow_mut` したまま描画しない

## 段階 6 で決めたこと（2026-11-17、モック 5・6、§8.4）

- F1（Fn+1）でショートカット一覧（`zc_ui::ShortcutList`）。行は `commands::BINDINGS` から作る（キー割り当てと
  同じ表）。文字で絞り込み（操作名の部分一致、または「ctrl s」のようなキー名）、↑↓ で選び Enter で実行、
  Esc / F1 で閉じる。開いている間はタブバーに「ショートカット」のタブを足す。Fn レイヤーの物理キー
  （Fn+5 など）は `zc-ui` の `shortcuts::fn_physical`（矢印・Home/End などの位置は実機で未確認）
- 控え: 操作が 2 秒止まったら段 1（RAM）、60 秒止まったか Ctrl+Tab でタブを切り替えたら段 2（SD）を
  storage スレッドで書く（`BackupPolicy`、`TimerId::Backup`）。未保存の文書が無くなったら控えを消す。
  Ctrl+Q での終了は控えを消し、シグナル（ランチャーの Esc 長押しなど）での終了は `Screen::stop` で
  未保存の内容を両方の段にその場で書く
- 起動時: storage スレッドで控えを読み（`Saver::restore_job`）、ディスクと違う未保存の内容があれば
  ステータスバー（橙）で「Enter 戻す / Esc 捨てる」と尋ねる。答えるまでほかのキーは効かず、控えも
  上書きしない。戻した内容は「ディスクの中身を 1 回の編集で置き換えた」形（未保存の印が付き、1 回の
  取り消しでディスクの中身に戻る）。読み込み中のタブには読み終わってから入れる
- .deb: `./build.sh <版>` で `zcode_<版>_arm64.deb`。中身は `/usr/share/APPLaunch/{bin/zcode（起動
  スクリプト）,lib/zcode/zcode,applications/zcode.desktop,share/images/zcode.png}`。起動スクリプトは
  `~/code` を作ってそこを作業フォルダにし、標準エラーは `$XDG_RUNTIME_DIR/zcode.log`（RAM）へ。
  実行環境（llrt・tsc・zc-remote）は .deb に入れない（`tools/install-runtimes.sh` で別に入れる）。
  CI は `./build.sh 0.0.0-ci` で .deb の中身と権限まで確かめる

## 次にやること

- 段階 3 を実機の「zcode (dev)」で確かめる: F5・Ctrl+C・F12・F11、端末の出力が流れている間の
  打鍵の遅延、APPLaunch から起動したときのロケール（端末の日本語の折り返し）
- ハイライトの実機の計測は済み（`docs/measurements.md` §6）

## 段階 3 の `zc-remote`（D5）で分かったこと（2026-11-17）

- 端から端までの試験（本物の `zc-buildd` をループバックに立て、Rust / C を作って受け取る、日本語の行の
  型エラーの桁、途中で切れても attach で続く）は `zstd` が要る。開発コンテナ（`cz-dev`）には無いので、
  ビルド係のイメージで回す: `zcode` で
  `wslc run --rm -v "${PWD}:/src" -w /src/remote --entrypoint bash cz-builder:latest -c 'CARGO_TARGET_DIR=/tmp/t ZC_E2E_REQUIRE=1 cargo test --locked --workspace'`
  （`ZC_E2E_REQUIRE=1` だと、道具が無いときに飛ばさず落ちる）
- `zc-remote run` は `execv` で実行ファイルに置き換わる（std の exec は ENOEXEC で `/bin/sh` に
  落ちるので使わない）。実行できなければ 126
- `zc-remote` の版は 0.1.0 のまま中身が変わるので、`runtimes.lock` の SHA-256 は作り直すたびに
  書き換える。インストーラが「同じ版は入れ直さない」で古いものを残さないか、実機で確かめる
- 実行ファイル 1.77MB（aarch64）、待機中の RSS は x86-64 で約 3MB（実機は未計測）

## 外部ビルドを実機で通した（2026-11-17、Cloudflare）

- Worker を deploy し、workers.dev のホスト名に Zero Trust の self-hosted アプリ（Service Auth）を掛けた
  状態で、実機の `zc-remote run` が通った（Rust の hello を Cloudflare のコンテナでビルド →
  140KB を受け取って実機で実行）。手順は `docs/setup-remote-build.md`
- 実機での時間（Wi-Fi、`zc-remote` の CLI から計測）:

  | 場面 | 時間 |
  |---|---|
  | 冷えたコンテナからの初回の `run`（起動・ビルド・受け取り・実行） | 14.1 秒 |
  | 変更なしで `run`（結果の使い回し） | 0.31 秒 |
  | 1 行変えて `run`（温まったコンテナで差分のビルド） | 0.42 秒 |
  | 型エラーの `check` | 0.13 秒 |

- CLI は同じ診断を 2 回表示する（ログの行と診断のまとめの両方）。直すかは未定
- いろいろな形を実機（2 号機、Wi-Fi）で試した（2026-11-17、すべて温まったコンテナ）:

  | 形 | 結果 |
  |---|---|
  | Rust・crates.io の依存（itoa）あり、初回の `run` | 1.53 秒で通る（コンテナから crates.io に届く） |
  | Rust・依存の多い crate（regex）、初回の `run` | 8.6 秒（584KB を受け取る） |
  | Rust の `test`（1 つわざと落とす） | 失敗の場所（`src/lib.rs:9`）とメッセージ、通過 1 / 失敗 1。終了コード 1 |
  | Rust の `check`（警告だけ） | 0.20 秒、警告の位置付きで終了コード 0 |
  | C・2 ファイル + ヘッダの `run` / コンパイルエラー | 0.23 秒 / 0.11 秒（`util.c:2:32` の誤り） |
  | TS・npm の依存（nanoid）ありの `run` / 型エラーの `check` | 0.80 秒 / 0.21 秒（TS2345 の位置） |
  | TS・依存なし（実機の `tsc` → LLRT、LLRT の型定義） | 0.86 秒（`--lib dom` の 3.8 秒から大きく減った） |
  | ビルド中に Ctrl+C（SIGINT） | 「取り消した」、走っているジョブ 0、続けての `run` も通る |
  | `build` と `check` を同時に | 両方通る（3.3 秒） |
  | オフライン（行き先の名前が引けない） | すぐ（0.07 秒）「前の版がある。--offline-ok を付ける」で終了コード 3。`--offline-ok` で前の版を実行 |
  | `serve` のメモリ | RSS 4.2MB、最大 10MB（受け取り中）、Pss 2.6MB |

- **見つけて直した**: npm の依存がある **JS** が、ビルド係で `--checkJs` の型検査に掛けられて落ちた
  （LLRT の `process` が TS2591）。JS は `--allowJs` だけ（構文の誤りだけ）にした。deploy し直して、
  実機で通ることを確かめた（2026-11-17）
- **deploy し直しても、温まっているコンテナは古いイメージのまま動き続ける**（`durable_object` の
  方式では、走っているコンテナを差し替えない）。新しいイメージは、コンテナが眠った後（最後に使ってから
  10 分、`KEEP_WARM_S`）の次の起動から。新しいイメージでの最初の起動は 38 秒かかった（イメージを
  取ってくる分）。急ぐなら DO にコンテナを止めさせる口を足す（C4 のときに検討）
- 環境変数（`ZC_REMOTE_URL` など）は `serve` を起動するときに読まれる。動いている `serve` には効かないので、
  試すときは先に `zc-remote stop`

