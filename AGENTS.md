# cardputer-zero-apps

M5Stack CardputerZero（CM0 ベースの Linux マシン）向け APPLaunch
アプリの monorepo。**公開リポジトリ**
（github.com/dj-oyu/cardputer-zero-apps）。

このリポジトリはワークスペースで唯一 commit / push してよい
リポジトリ。ただし commit / push / タグ作成はユーザーが頼んだときに
行う。上位にワークスペースの `AGENTS.md` があれば併せて従う
（git ルールはそちらが最優先）。

**main には直接 push もマージもしない。必ず PR 経由。** 作業はブランチ
（またはエージェントの worktree のブランチ）で行い、ブランチを push して PR を
作る。PR のマージはユーザーの承認を得てから。GitHub のルールセット
「main: PR only」で PR 必須・強制 push 禁止・削除禁止になっている。

## このリポジトリに書いてはいけないもの

公開リポジトリなので、個人環境の情報（Tailscale のホスト名・IP、
アカウント、端末名など）はコード・ドキュメント・この AGENTS.md の
どこにも書かない。そういう知見はワークスペース側（リポジトリ外）に
置く。

## 構成

| ディレクトリ | 内容 |
|---|---|
| `qrreader/` | QR コードリーダー（カメラ＋液晶） |
| `bt-tether/` | iPhone との Bluetooth PAN テザリング |
| `common/` | 液晶アプリ共通の UI 土台 `zero_ui.py` |
| `zcode/` | VS Code 風コードエディタ（Rust。段階 1 の土台まで実装） |
| `container/` | 開発コンテナ（Dockerfile と Windows 用ラッパー `dev.ps1`） |

新しいアプリは `<app>/` を切り、既存アプリと同じ形
（`build.sh`、`packaging/`、`tools/make_icon.py`、`README.md`、
自前の `AGENTS.md`）にそろえる。

## アプリ共通の決まり

- 配布は APPLaunch 向けの **.deb 単体完結**。`../common/zero_ui.py` は
  `build.sh` で各 .deb の `lib/<app>/` へコピーして同梱する
- インストール先: `/usr/share/APPLaunch/{bin,lib/<app>,applications,share/images}`。
  `postinst` で `jimmy` の user サービス `APPLaunch.service` を
  再起動してランチャーに拾わせる
- ビルド: `./build.sh <version>` → `<pkg>_<version>_arm64.deb`。
  Windows では開発コンテナ（`container/`、WSL コンテナー `wslc`）で回す:
  アプリのフォルダで `..\container\dev.ps1 ./build.sh <version>`。
  `.deb` と生成アセット（アイコン等）は git 管理外
- 開発コンテナでできるのは .deb ビルド・`--render-test`・selftest まで。
  カメラ / fb0 / キー / BlueZ は実機で確認する（詳細は `container/README.md`）
- 改行は `.gitattributes` で LF 固定。CRLF の sh / postinst / sudoers は壊れる
- リリースはアプリごとのタグ（例: `qrreader-v3.5.1`）。`.deb` は
  そのタグの GitHub Release に添付
- コミットメッセージは `<app> v<version>: <要約>` の形
  （例: `bt-tether v0.2: IPv4 via dhcpcd`）

## 実機の制約（設計の前提）

- RAM 512MB、CMA 64MB（起動直後の空きは約 9MB）。重い処理は
  ライブラリ（zxing-cpp、libcamera、numpy）に寄せ、Python は配管に徹する
- 重い純 Python 処理は GIL でアプリ全体を固める（ハードウェア
  ウォッチドッグで再起動した実例あり）→ 別プロセスに分離する
- ストレージは SD カード（ext4、noatime）。書き込み寿命・fsync の遅延のばらつき・
  電池切れによる電源断を前提にする: 頻繁に変わるものは RAM（`/run/user/1000` の
  tmpfs）に置き、SD には間隔をあけてまとめて書く。内容が変わらなければ書かない。
  再計算できるキャッシュは SD に書かない。保存は一時ファイル → fsync → rename。
  ログは SD に書かない。スワップは zram（SD ではない）
- 実行時に重い変換が要るデータ（正規表現など）は、ビルド時に実行できる形へ
  変換して埋め込む（例: zcode のハイライト。実行時コンパイルは A53 で数百 ms）
- 液晶は 320×170 RGB565 の `/dev/fb0`（stride 640、リトルエンディアン）、
  キーは tca8418c（`/dev/input/event3`）。Python アプリの描画・入力は
  `common/zero_ui.py` を使う。fb0 は video、event3 は input グループで
  `jimmy` から読み書きできる
- **APPLaunch は、キー入力がしばらく無いとバックライトを 0 にして画面を眠らせ、
  キー入力で起こす。** キーボードを独占（`EVIOCGRAB`）するアプリは、打鍵が
  APPLaunch に届かないので、放っておくと画面が消えたまま戻らなくなる
  （2026-11-16、zcode のキー確認中に実機で起きた）。独占するアプリは、実行中は
  自分でバックライト（`/sys/class/backlight/backlight/brightness`、0〜100、
  video グループで書ける）を点けておく。zcode は `zc_display::backlight` で、
  起動時・打鍵ごと・1 秒ごとに 0 なら点け直している
- 液晶の実体は ST7789V（SPI、spi-max-frequency 60MHz）で、ドライバは
  panel-mipi-dbi-spi（DRM の fbdev エミュレーション、card1-SPI-1）。fb0 への
  書き込みは遅延 I/O でまとめて SPI 転送される（変更されたページを含む
  1 つの行範囲、横は全幅。1 ページ 4KB ≒ 6.4 行）。全画面 108,800 バイトは
  60MHz でも理論値で約 15ms
- 書いただけだと転送開始まで中央値 30ms・最大 56ms（約 50ms 周期）。書いた直後に
  **`fsync(fb0)` すると約 5ms（1 行）〜8ms（全画面）で転送が始まる**（2026-11-16
  実測。SPI の統計 `/sys/class/spi_master/spi0/statistics/bytes_tx` で計測）。
  打鍵に反応するアプリは描いたら fsync する
- APPLaunch はランチャー表示中も常に再描画している（SPI 約 35 万バイト/秒、CPU 約 9%）。
  labwc が使うのは HDMI 側の card0 で、液晶（card1）には触らない
- 2026-11-16 計測（デスクトップ常駐時）: OS から見えるメモリ 352MB のうち
  available は約 74MB。スワップは zram（約 352MB、すでに約 140MB 使用）。
  CPU は ondemand ガバナで 600MHz〜1GHz。温度は約 56℃、スロットリング無し
- fb0 を mmap するときは長さ（320*170*2）を明示する。文字デバイスは
  ファイルサイズが 0 と報告されるので、省略すると 0 バイトのマップになる
- 文字は必ず CJK フォント（`zero_ui.font()`）。PIL のデフォルト
  フォントは日本語が豆腐になる。等幅は DejaVu Sans Mono / Noto Sans Mono CJK
  （NotoSansCJK-*.ttc 内）が実機にある

## キーボード（tca8418c ドライバ、デバイスツリーから読んだもの）

- Shift / Sym / Fn は押しっぱなしでも、1 回タップ（次の 1 キーだけ）でも、
  2 回タップ（ロック）でも効く。ドライバが `linux,keymap`（通常）/
  `keymap1`（Sym）/ `keymap2`（Fn）を切り替えてキーコードを送る
- 通常レイヤー: 数字は 2〜0 が最上段、1 は最下段。Ctrl / Alt / Esc / Tab /
  Enter / BackSpace / Space あり
- Fn レイヤー: F1〜F12、Del、矢印（←↓→ と ↑）、Home / End / PgUp / PgDn / Ins、
  音量
- Sym レイヤー: `] ; ' \` \\ , . /` と `[` は通常のキーコード。残りは
  テンキー系（KP0〜9、KP*、KP-、KP+、KP.）や NumLock / ScrollLock、
  かな系（KEY_KATAKANA など）のコードで届く。**刻印との対応は実機で押して
  確かめるまで未確定**（KEY_MINUS / KEY_EQUAL はどのレイヤーにも無い）
- **キーリピートが無い**（デバイスの EV ビットに EV_REP が無い。EV=13）。
  長押しで連続入力したいアプリは、押下・離上を見て自前でリピートする
- Python の `zero_ui.KeyReader` は押下コードしか返さない（モディファイアの
  状態は持たない）

## Rust アプリ

- 開発コンテナに rustup の stable と aarch64 クロスリンカが入っている。
  `cargo build --release --target aarch64-unknown-linux-gnu` で作ったバイナリは
  実機（trixie の glibc）でそのまま動く（2026-11-16 確認）
- ビルド成果物は `CARGO_TARGET_DIR=/cargo-target`（名前付きボリューム
  `cz-cargo-target`）に出る。リポジトリの中には出ない。.deb は build.sh が
  そこから拾う
- 依存は C ライブラリを要求しない pure Rust の crate を優先する
  （クロスビルドで C の依存を足すと aarch64 用の開発パッケージが要る）

## 進め方

- UI の変更は、先に HTML のデザインモックで確認してから実装する
- 「効くはず」で配布しない。A/B で実測してから入れる
- 不具合の報告があれば、新機能より先に直す
- 描画の確認は各アプリの `--render-test` で PNG に出して見る
- 設計判断と実測は `development.md`（横断）と各アプリの
  `README.md` / `docs/` に残す。エージェント向けの作業メモは
  該当階層の `AGENTS.md` に書く
