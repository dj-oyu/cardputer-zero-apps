# BT Tether（計画中）

iPhone のインターネット共有へ Bluetooth PAN でつなぐ
APPLaunch アプリ。外出先でも CardputerZero をオンラインに
する（Tailscale の到達性も回復する）のが目的。

## 分かっていること（2026-11-15 実機調査）

- Zero 側の受け入れ準備はできている: bnep モジュール、
  NetworkManager、bluetooth サービスが稼働中
- USB テザリングは不可（カーネルに iPhone 用ドライバ
  ipheth が無い）。Wi-Fi ホットスポットより BT PAN の方が
  両機とも省電力で、速度は 1〜2 Mbps 程度の見込み
- ペアリングと iPhone 側の「インターネット共有 ON」は、
  iPhone を手元で操作しながら行う必要がある

## アプリのスコープ（想定）

- ペアリング状態・接続状態・IP アドレスを本体液晶に表示
- キー操作で接続 / 切断
- UI は `../common/zero_ui.py` を使う
