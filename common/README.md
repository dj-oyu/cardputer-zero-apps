# common — 本体 LCD アプリの共通部品

## zero_ui.py

CardputerZero 本体液晶（320×170、/dev/fb0、RGB565）へ直接描く
APPLaunch アプリのための UI 土台。QR リーダーの実装から
切り出した。

- `FrameBuffer` — fb0 への描画
- `KeyReader` — 本体キーボード（tca8418c）のキー押下コード取得
- `font()` — Noto Sans CJK Bold（PIL のデフォルトフォントは
  日本語グリフを持たず豆腐になる。ステータス帯も含め必ずこれを使う）
- `draw_panel()` — 結果表示のガラスパネル。文字列は無編集で
  そのまま表示し、コピーは Enter の明示操作だけ、という表示
  規約を含む
- `draw_stepper()` / `draw_chip()` / `draw_card()` — 処理の
  流れを案内する要素（ステップ表示・案内チップ・フェーズカード）
- `finish_frame()` — 土台画像にステータス帯・フロー UI・
  緑枠・パネルを重ねて 1 フレームを仕上げる

アプリ側はプレビュー等の土台画像を用意し、`finish_frame` に
渡す。使い方の実例は `qrreader/qrreader_lcd.py` の
`render_frame` を参照。

各アプリの .deb には、ビルド時にこのファイルのコピーが
同梱される（インストール先で common を要求しない）。
