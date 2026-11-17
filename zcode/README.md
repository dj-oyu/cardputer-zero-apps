# zcode

M5Stack CardputerZero 用の、VS Code 風のコードエディタ（APPLaunch アプリ）。液晶（fb0）に直接描き、
キーボード（evdev）を直接読む。Rust 製。

- シンタックスハイライト（Python・Shell・Markdown・JSON・TOML・Rust・C・JavaScript・TypeScript）
- ファイルツリー、タブ、検索、行へ移動
- SKK の日本語入力（Ctrl+J。辞書は `/usr/share/skk/SKK-JISYO.L`、個人辞書 `~/.skk-uim-jisyo` を uim-skk と共有）
- Markdown のライブプレビュー（Ctrl+Shift+V でソース表示）
- 統合ターミナル（F12 で開閉）と F5 で保存して実行
- 区画の移り方: Ctrl+1 ツリー・Ctrl+2 エディタ・Ctrl+3 ターミナル、Ctrl+B / F12 で開閉、F11 でいまの区画を最大化
  （ツリーではフォルダを開く画面）。ステータスバーの左端に、いまの区画と番号を出す
- 単語補完、保存時の構文チェック、アウトライン（Ctrl+O）
- ショートカット一覧（F1。絞り込んで Enter でその操作を実行）
- 未保存の内容の控え（異常終了・電源断のあと、次の起動で戻すか尋ねる）
- コードと端末の字は Cozette（英字、6×13）と PixelMplus12（日本語）。.deb に同梱（ライセンスは
  `/usr/share/doc/zcode/`）。`--no-bundled-font` で DejaVu Sans Mono に戻せる

## 入れ方

```sh
sudo dpkg -i zcode_<版>_arm64.deb
```

ランチャーに「zcode」が出る。作業フォルダは `~/code`（無ければ作る）。終了は Ctrl+Q。
SKK の辞書を使うなら `skkdic`、Python の構文チェックと実行には `python3` を入れておく。

## 作り方

開発コンテナで（Windows なら `cd zcode` してから）:

```powershell
..\container\dev.ps1 cargo test --workspace   # 単体テストと描画の正解画像
..\container\dev.ps1 ./build.sh 0.1.0         # 実機用のビルドと zcode_0.1.0_arm64.deb
```

設計は `docs/design.md`、画面のモックは `docs/ui-mock.html`、実測は `docs/measurements.md`、
作業メモは `AGENTS.md`。
