# QR Reader for CardputerZero

CardputerZero 本体カメラと液晶で動く QR コードリーダー
（APPLaunch アプリ）。クレデンシャルや API キーの受け渡しに
使う想定で作った。読み取り結果は文字列を**無編集でそのまま**
表示し、クリップボードへのコピーは Enter を押したときだけ
行う（自動コピーしない）。

## 特徴

- **バースト積算**: 位置合わせ＋加算平均でノイズを落とし、
  実効 約1.5 px/module（ノイズ支配の場面）まで読む。
  累積解像度はコードの大きさに応じて適応的に選ぶ
- **静止画シャッター**: Enter / Space で動画を止め、
  rpicam-still で全解像度（3280×2464）を撮って読む。
  動画（1640幅）では物理的に不可能な約2 px/module 帯が射程
- **距離ガイダンス**: メイン解像度でファインダを走査し、
  実測ピッチから「読める大きさ / あと少し / もっと近づいて」
  を案内する
- **どれが読まれたか**: 静止画で読めたら元画像からコード
  周辺をクロップして背景に出し、読まれたコードに緑枠を
  付ける。複数件は ←/→ で切り替え
- **フローナビ**: 案内チップ→ステップ表示→フェーズカードで
  「Enter→撮影→解析→結果」の流れを画面で案内する
- 失敗した全走査が支配的にならないよう、重い処理は別
  プロセス（ワーカー）に分離し、本体はプレビューを止めない

設計の経緯と実測は `docs/design-notes.md` にまとめた。

## 動作環境

- CardputerZero（カメラ付き Full モデル）+ APPLaunch
- zxing-cpp（デコードエンジン）:
  `pip3 install --user --break-system-packages zxing-cpp`
- picamera2 / rpicam-still（Raspberry Pi OS 標準）

## インストール

Release から `qrreader-lcd_<version>_arm64.deb` を取り、
本体で `sudo dpkg -i qrreader-lcd_<version>_arm64.deb`。
APPLaunch に「QR Reader」が追加される。

## キー操作

| キー | 動作 |
|---|---|
| Enter / Space | 静止画シャッター（パネル表示中は Enter = コピー） |
| ← / → | 静止画の複数結果を切り替え |
| Esc | パネルを閉じる（コピーしない）/ 終了 |
| ` / BackSpace | 終了 |

## ビルド

```sh
./build.sh 3.5.1   # qrreader-lcd_3.5.1_arm64.deb を生成
```

テスト用 QR 画像とアイコンは生成物（git 管理外）で、
無ければビルド時に `tools/` のスクリプトが作る
（要: segno / Pillow）。

自己検証: `python3 qrreader.py --selftest`、
描画確認: `python3 qrreader_lcd.py --render-test out.png`。
