# BT Tether

iPhone のインターネット共有へ Bluetooth PAN でつなぐ
APPLaunch アプリ。外出先でも CardputerZero をオンラインに
する（Tailscale の到達性も回復する）のが目的。

## 使い方

1. アプリを起動する。ペアリング済みデバイスが無い間は
   本体が発見可能になり「ペアリング待ち」になる
2. iPhone の 設定 > Bluetooth で本体（例: CardputerZero-2）を
   選ぶ。両方に同じ数字が出たら、本体の **Enter** で承認する
   （Esc で拒否）。あわせて iPhone の「インターネット共有」を
   ON にしておく
3. ペアリング後は **Enter** で接続 / 切断。接続中は
   インターフェース名と IP アドレス、経過時間が出る

アプリを終了しても PAN 接続は bluetoothd 側に残る
（このアプリは操作パネル）。切断もアプリから行う。

## 実装メモ（2026-11-16 実機調査に基づく）

- NetworkManager は polkit で認証止まりだったため使わず、
  BlueZ を D-Bus で直接叩く（`bt_tether.py`）。jimmy の権限で
  探索開始などの書き込みが通ることを実地確認済み
- ペアリングのエージェントは DisplayYesNo（数字照合）。
  GLib メインループを別スレッドで回して受け口にする
- IP アドレスは bnep インターフェースから ioctl で読む。
  IPv6 は iPhone の RA で自動的に付く。一方 IPv4 は、
  NetworkManager が外部から作られた bnep を自分のデバイス
  として認識せず DHCP が走らない（実地確認）。そこで接続を
  検知したら dhcpcd を起動して取りに行く。dhcpcd は root が
  要るので、deb が /etc/sudoers.d/bt-tether に「dhcpcd の
  bnep 向け2コマンドだけ」の限定許可を置き、アプリは
  sudo -n で呼ぶ。ルートのメトリックは 2048 にして、自宅で
  Wi-Fi のデフォルトルートを奪わないようにした
- USB テザリングは不可（カーネルに ipheth が無い）。
  BT PAN は 1〜2 Mbps 程度の見込みで、Wi-Fi ホットスポット
  より両機とも省電力

描画の土台は `../common/zero_ui.py`。ビルドは
`./build.sh 0.1`（bt-tether-lcd_0.1_arm64.deb を生成）。
描画確認: `python3 bt_tether_lcd.py --render-test out`。
