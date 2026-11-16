# cardputer-zero-apps

M5Stack CardputerZero（CM0 ベースの Linux マシン）向け APPLaunch
アプリの monorepo。**公開リポジトリ**
（github.com/dj-oyu/cardputer-zero-apps）。

このリポジトリはワークスペースで唯一 commit / push してよい
リポジトリ。ただし commit / push / タグ作成はユーザーが頼んだときに
行う。上位にワークスペースの `AGENTS.md` があれば併せて従う
（git ルールはそちらが最優先）。

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
- 液晶は 320×170 RGB565 の `/dev/fb0`、キーは tca8418c
  （`/dev/input/event3`）。描画・入力は `common/zero_ui.py` を使う
- 文字は必ず CJK フォント（`zero_ui.font()`）。PIL のデフォルト
  フォントは日本語が豆腐になる

## 進め方

- UI の変更は、先に HTML のデザインモックで確認してから実装する
- 「効くはず」で配布しない。A/B で実測してから入れる
- 不具合の報告があれば、新機能より先に直す
- 描画の確認は各アプリの `--render-test` で PNG に出して見る
- 設計判断と実測は `development.md`（横断）と各アプリの
  `README.md` / `docs/` に残す。エージェント向けの作業メモは
  該当階層の `AGENTS.md` に書く
