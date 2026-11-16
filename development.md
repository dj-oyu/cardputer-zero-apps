# Development Notes

`cardputer-zero-apps` の開発記録。設計判断と実測で分かったことを
残すための文書で、個人の環境に依存する情報（アドレス類、
アカウント、端末名など）は含まない。

## 対象ハードウェア

M5Stack CardputerZero。Raspberry Pi Compute Module 0
（RP3A0、クアッドコア Cortex-A53 @1.0GHz、512MB LPDDR2）を
載せた Linux マシンで、Raspberry Pi OS (Debian 13, aarch64) が
microSD から起動する。マイコンではなく SBC なので、開発は
「Linux のライブラリの上にアプリを組む」形になる。

- 本体液晶: 1.9" 320×170、/dev/fb0、RGB565
- カメラ: IMX219（3280×2464、固定フォーカス、遠方合焦）
- アプリの配布形態: APPLaunch（本体のランチャー）向けの
  .deb 単体完結パッケージ

実機側の制約として効いてくるのは主に2つ。

- **CMA が 64MB しかなく、起動直後でも空きは約 9MB**。
  カメラの高解像度バッファ確保が平気で失敗する
- メモリ総量 512MB。重い処理はプロセス分離とメモリ量の
  ガードが前提になる

## QR Reader

### 課題

クレデンシャルや API キーの受け渡しに使える QR リーダーを
本体カメラ＋液晶で完結させる。固定フォーカスが遠方合焦の
ため、通常の持ち方（25〜40cm）では小さなコードが
1〜2 px/module 程度にしか写らず、素のデコードでは読めない
場面が多い、というのが中心の課題だった。

### アーキテクチャ

- デコードエンジンは zxing-cpp、撮像は picamera2 / libcamera。
  重い処理は C++ 側に寄せ、Python は配管に徹する
- ライブプレビューは ISP の lores ストリーム（Y 平面のみ）
  を使い、メイン解像度は必要な場面だけ使う
- 重い走査・デコードは別プロセス（ワーカー）に分離する。
  理由は後述の GIL 問題

### 実測で決めたこと

- **バースト積算の床は実効 約1.5 px/module**（16枚平均、
  ノイズ支配の場面）。較正実験で累積解像度を決め、実装は
  コードの大きさに応じて累積サイズを適応的に選ぶ
- **ボケ支配の場面ではソフトウェア復元は不可能**。Wiener、
  Richardson–Lucy、アンシャープ、学習型超解像デコーダまで
  試して全滅だった。シャープなら ≳2 px/module が目安
- ピッチ駆動の前処理（ファインダ間隔から画素ピッチを推定し
  デコンボリューションする案）も検証したが、読めるコードを
  強い前処理が壊す事例を確認したため、前処理は「素の画像に
  並列で追加試行する」場合にのみ安全、という結論
- **静止画シャッター**：動画（1640幅）では物理的に届かない
  帯は、rpicam-still の全解像度静止画（3280×2464）で読む。
  実測でも動画で不可能なフレーミングが静止画では読めた
- **距離ガイダンス**：ファインダパターンはデータモジュール
  よりボケに強いので、メイン解像度でファインダを走査して
  実測ピッチから「読める大きさ／あと少し／もっと近づいて」
  を案内できる
- **GIL の教訓**：純 Python のファインダ走査はノイズ画面で
  数十秒 CPU を握り、アプリ全体を固めた（ハードウェア
  ウォッチドッグによる再起動まで起きた）。重い走査は
  ワーカープロセスへ逃がすのが正解だった
- **センサの向き**：Rotation=180 で取り付けられているため
  補正は hflip+vflip の両軸。片軸だけの補正は回転を鏡像に
  変えてしまう。検証は「写っている文字を読む」ことで行う
  （設定同士の比較では誤判定した）
- 結果表示の規約：読み取り結果は文字列を無編集でそのまま
  表示し、クリップボードへのコピーは Enter の明示操作の
  ときだけ（自動コピーしない）。複数コードがある場面で
  「どれが読まれたか」が分かるよう、静止画ヒット時は
  元画像のコード周辺をクロップして背景に表示し、読まれた
  コードへ緑枠を付ける

### バージョンの流れ

- v2.x: バースト積算＋関心マップ（網掛け表現）の可視化、
  メモリ起因のフリーズ修正
- v3.0: lores を 320×240 に、累積の適応解像度化
- v3.1–3.2: 結果パネル（ガラス調、緑アクセント）、
  左右反転の修正
- v3.4: 静止画シャッターと距離ガイダンス（重処理の
  ワーカー分離を含む）
- v3.5: 静止画結果のクロップ表示とフローナビゲーション
- v3.5.1: monorepo 化に伴い LCD 土台を common/zero_ui.py
  へ分離（振る舞いの変更なし）

## BT Tether

iPhone のインターネット共有へ Bluetooth PAN (NAP) で
つなぐ操作パネル。

### 方式選定

- NetworkManager 経由は polkit の認証で止まる構成だった
  ため、BlueZ を D-Bus で直接叩く方式にした。ペアリングの
  エージェント（数字照合、DisplayYesNo）は GLib メイン
  ループを別スレッドで回して受ける
- 接続そのものは bluetoothd が保持する。アプリは操作
  パネルであり、閉じても接続は残る

### IPv4 の問題と対策

- IPv6 は iPhone の RA で自動的に付く
- IPv4 は来なかった。原因は、NetworkManager が「外部の
  クライアントが作った bnep」を自分のデバイスとして認識
  せず、DHCP が走らないこと
- 対策として、PAN 接続を検知したら dhcpcd を起動して
  取りに行く形にした。dhcpcd は root が必要なため、.deb
  が /etc/sudoers.d に「dhcpcd の bnep 向け2コマンドだけ」
  の限定許可を置き、アプリは sudo -n で呼ぶ。付与される
  デフォルトルートのメトリックは 2048 にし、自宅では
  Wi-Fi 側を優先したままにする。切断時はリースを解放する

## 共通基盤 (common/zero_ui.py)

QR Reader の実装から、本体 LCD アプリの土台（fb0 描画、
キー入力、CJK フォント、結果パネル、ステップ表示などの
フロー UI 部品）を切り出した。切り出し時は、リファクタ
前後で描画テストの全出力がバイト単位で一致することを
確認してから置き換えた。各アプリの .deb にはビルド時に
コピーを同梱し、インストール先では共有ライブラリを
要求しない。

なお、ステータス帯のような小さな文字も含め、描画には
必ず CJK フォントを使う。PIL のデフォルトフォントは
日本語グリフを持たず、文字化け（豆腐）になる。

## 進め方で効いたこと

- UI の変更は、先に HTML のデザインモックを作って確認して
  から実装する（結果パネル、静止画結果表示、フローナビは
  この順序で決めた）
- 「効くはず」で配布せず、効果を A/B で実測してから入れる。
  原因の帰属は、構図を固定した対照実験ができるまで保留する
- 不具合の報告があったら、新機能より先に直してから出す

## 参考リンク

### 公式・ハードウェア

- [CardputerZero ドキュメント (M5Stack)](https://docs.m5stack.com/en/CardputerZero)
  — 本体仕様、Cap の着脱は電源 OFF で行うことなどの注意、
  回路図への入口
- [CardputerZero GitHub 組織](https://github.com/CardputerZero)
  — 公式アプリ（ランチャー、Cap 系アプリなど）と
  アプリ開発テンプレートのソース
- [Raspberry Pi Compute Module Zero データシート](https://pip-assets.raspberrypi.com/categories/1286-raspberry-pi-compute-module-zero/documents/RP-009251-DS-3-cm0-datasheet.pdf)
  — CM0 (RP3A0) の一次情報

### Cap のチップ

- [CC1101 データシート (TI)](https://www.ti.com/lit/ds/symlink/cc1101.pdf)
  — レジスタ表と SPI プロトコル。VERSION レジスタの読みが
  生存確認に使える
- [ST25R3916 製品ページ (ST)](https://www.st.com/en/nfc/st25r3916.html)
  — Cap CC1101 の NFC フロントエンド

### カメラ

- [Picamera2 マニュアル (Raspberry Pi)](https://pip-assets.raspberrypi.com/categories/810-raspberry-pi-global-shutter-camera/documents/RP-008156-DS-3-picamera2-manual.pdf)
  — ストリーム構成、センサモード、Transform の仕様
- [picamera2 ソース](https://github.com/raspberrypi/picamera2)
- [libcamera](https://libcamera.org/)
  — picamera2 の土台。CMA バッファ確保の挙動はここが決める

### QR デコード

- [zxing-cpp](https://github.com/zxing-cpp/zxing-cpp)
  — デコードエンジン本体。Python バインディング経由で使う

### Bluetooth / ネットワーク

- [BlueZ](https://github.com/bluez/bluez)
  — `doc/` に D-Bus API の定義（Network1、Agent1、
  NetworkServer1 など）。BT Tether の実装はここが正本
- [NetworkManager D-Bus API リファレンス](https://networkmanager.dev/docs/api/latest/)
  — NM が bnep をどう扱うかの確認に
- [dhcpcd](https://github.com/NetworkConfiguration/dhcpcd)
  — bnep の IPv4 取得に使う DHCP クライアント

### 音声

- [VOICEVOX エンジン](https://github.com/VOICEVOX/voicevox_engine)
  — HTTP API（`/audio_query`、`/streaming_synthesis`）の仕様。
  Zero へ音声を流す側の実装で参照する
