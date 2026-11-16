# qrreader

カメラ＋液晶で完結する QR リーダー（パッケージ名 `qrreader-lcd`）。
仕様とキー操作は `README.md`、設計の経緯と実測は
`docs/design-notes.md` を先に読む。

## ファイル

- `qrreader.py` — デコードエンジン（zxing-cpp）、バースト積算。
  `--selftest` で自己テスト
- `qrreader_lcd.py` — 液晶アプリ本体。`--render-test out.png` で描画確認
- `qrdecode_worker.py` / `qrguide_worker.py` — 重い走査・デコードを
  受け持つ別プロセス
- `tools/make_test_qr.py`（要 segno）/ `tools/make_icon.py` — 生成アセット

## 触るときの注意

- 重い処理をメインプロセスに戻さない（GIL でプレビューが固まり、
  ウォッチドッグ再起動まで起きた）
- センサは Rotation=180。補正は hflip+vflip の両軸。検証は
  「写っている文字が読めるか」で行う
- 強い前処理は読めるコードを壊すことがある。前処理は素の画像に
  **並列で追加試行**する形でだけ入れる
- 結果は無編集で表示し、クリップボードへのコピーは Enter の明示操作の
  ときだけ（自動コピーしない）
- 高解像度バッファは CMA 不足で確保に失敗しうる。静止画は
  `rpicam-still` に任せる
- テスト用 QR（`qrreader_test_qr.png`）は **330px（scale=10, border=2）** が
  前提。selftest のファインダ位置と較正レジームはこの画像で決めてある。
  border=4（370px）で作ると較正系の 2 項目（degraded single unreadable /
  adaptive regime found）が NG になる。monorepo 化のとき生成スクリプトが
  border=4 になっていたのを直した。正しい画像の md5 は
  `22ec70f37f34b3e2fcf1bc8c2421c34e`（実機の v3.5 同梱品と一致）
- 開発コンテナと実機で selftest は同じ結果（全項目 OK）になる。
  zxing-cpp 3.1.1・PIL 11.1.0・numpy 2.2.4 は実機と同じ
- バージョンを上げたら `packaging/control` の Description に
  変更点を追記する
