# bt-tether

iPhone のインターネット共有へ Bluetooth PAN (NAP) でつなぐ操作パネル
（パッケージ名 `bt-tether-lcd`）。使い方と実装メモは `README.md`。

## ファイル

- `bt_tether.py` — BlueZ を D-Bus で直接操作（探索・ペアリング・
  PAN 接続、DHCP 起動）
- `bt_tether_lcd.py` — 液晶アプリ本体。`--render-test <dir>` で描画確認
- `packaging/bt-tether.sudoers` — `/etc/sudoers.d/bt-tether`（mode 440）

## 触るときの注意

- NetworkManager は polkit で止まるので使わない。BlueZ を直接叩く
- ペアリングのエージェントは DisplayYesNo。GLib メインループは
  別スレッドで回す
- IPv4 は NetworkManager が bnep を管理しないため来ない →
  `sudo -n dhcpcd --metric 2048 bnep*` で取得、切断時 `dhcpcd -k`
- **sudoers の許可範囲を広げない**。dhcpcd の 2 形式だけに限る。
  コマンドを変えるときは sudoers とコードを同時に直す
- メトリック 2048 は自宅 Wi-Fi のデフォルトルートを奪わないため
- アプリを閉じても PAN 接続は bluetoothd 側に残る（仕様）
- USB テザリングは不可（カーネルに ipheth が無い）
- 依存を足したら `packaging/control` の `Depends:` も更新する
