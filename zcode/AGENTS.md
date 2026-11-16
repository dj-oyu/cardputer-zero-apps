# zcode

VS Code 風のコードエディタ（APPLaunch アプリ）。**Rust** で書く。段階 1（土台）まで
実装済みで、エディタ本体は段階 2 から（`docs/design.md` §11）。

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
  行列の位置）。イベントには MSC_SCAN（行列の位置）も付く。Sym レイヤーの刻印は未確定

## 次にやること

- **キー確認をユーザーと行う**（`docs/stage1-notes.md` §6）: Sym レイヤーの刻印の対応表、
  Shift / Sym / Fn の送られ方、読み取りと fsync までの遅延。結果を
  `keymap/cardputer-zero.toml` と `docs/measurements.md` に入れる
- フォント比較画面をユーザーに見てもらい、文字の大きさと行の高さを決める
  （`crates/zc-ui/src/layout.rs` の `CODE_PX` / `CODE_LINE_H`）
- APPLaunch からアプリとして起動したときに、時計・電池の再描画が止まるか確かめる
- `docs/stage1-notes.md` §3 の提案を design.md に取り込むか決めてから、段階 2 へ
