# zcode

VS Code 風のコードエディタ（APPLaunch アプリ）。**Rust** で書く。段階 2（エディタ）まで
実装済み（`docs/design.md` §11）。実機での段階 2 の確認はまだ。

## 文書

| ファイル | 中身 |
|---|---|
| `docs/design.md` | 設計・crate 構成・スレッド構成・遅延の目標・ハイライトの拡張方式・SD 前提のデータの持ち方・LSP の方針・段階とモデルの担当。**実装担当は自分の節と「共通の約束」（§3）を先に読む** |
| `docs/measurements.md` | 設計の根拠になる実測値（測り方・条件・出典つき）。数字を変えるときはここを先に直す |
| `docs/stage1-notes.md` | 段階 1 の作業メモ: API の決定と理由、design.md への提案、実機の計測、未解決の点、**キー確認の進め方** |
| `docs/ui-mock.html` | 画面のモック（320×170 実寸を拡大表示）。画面 1〜11 とショートカットの全項目 |
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
- 保存できたら、`language.toml` の `check` を batch で使い捨てで走らせる（`intel.rs`）。
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

## 次にやること

- 段階 3 を実機の「zcode (dev)」で確かめる: F5・Ctrl+C・F12・F11、端末の出力が流れている間の
  打鍵の遅延、APPLaunch から起動したときのロケール（端末の日本語の折り返し）
- ハイライトの実機の計測は済み（`docs/measurements.md` §6）
