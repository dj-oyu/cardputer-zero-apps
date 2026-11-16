# cardputer-zero-apps

M5Stack **CardputerZero**（Raspberry Pi Compute Module 0 ベースの
Linux マシン）向けに自作した APPLaunch アプリの monorepo。

| ディレクトリ | 内容 |
|---|---|
| `qrreader/` | QRコードリーダー（本体液晶版）。バースト積算・静止画シャッター・距離ガイダンス搭載 |
| `bt-tether/` | iPhone との Bluetooth テザリングアプリ（計画中） |
| `common/` | 本体 LCD アプリ共通の UI 部品 `zero_ui.py`（fb0 描画、キー入力、パネル等） |

## 方針

- アプリは APPLaunch から起動する **.deb 単体で完結**させる
  （ストアの流儀に合わせる）。共通部品はビルド時に各 .deb へ
  コピーして同梱し、インストール先で共有ライブラリを要求しない。
- リリースはアプリごとのタグで切る（例: `qrreader-v3.5.1`）。
  .deb は各タグの GitHub Release に添付する。
- 本体は 512MB RAM / CMA 64MB と制約が強い。重い処理は
  ライブラリ（zxing-cpp、libcamera、numpy）に寄せ、Python は
  配管に徹する。各アプリの設計メモに実測と判断の経緯を残す。

## ライセンス

MIT License（`LICENSE` 参照）。
