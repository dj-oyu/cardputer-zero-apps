# zcode 動作確認

言語ごとのサンプル。開いて **F5（実行）**・保存（Ctrl+S、ビルド係で検査）・**F6（試験）** を試す。
`errors/` の中は、わざと間違えてある（波線と `⊗ n` が出れば正しい）。

## 試すこと

- [ ] Ctrl+1 / 2 / 3 で区画を移る（ステータスバー左の番号と、上の辺の光）
- [ ] F11 で最大化、ツリーで F11 はフォルダを開く画面
- [ ] F12 でターミナル、PgUp / PgDn（Fn+L / Fn+M）でさかのぼる
- [ ] Ctrl+J でかな入力（**日本語**のコメントを書く）
- [ ] Ctrl+Shift+V でこのファイルのプレビュー ⇔ ソース

## 言語

| フォルダ | F5 | 保存時の検査 | F6 |
|---|---|---|---|
| `python/` | 実機の python3 | pyright | — |
| `shell/` | 実機の bash | shellcheck | — |
| `rs/` | ビルド係で aarch64 | cargo check（Ctrl+H 型） | cargo test |
| `c/` `cpp/` | ビルド係で aarch64 | gcc / g++ | — |
| `go/` | ビルド係で aarch64 | go vet | go test |
| `zig/` | ビルド係で aarch64 | zig ast-check | zig test |
| `moonbit/` | ビルド係で native | moon check | moon test |
| `ruby/` | 実機の ruby（未導入） | ruby -wc | — |
| `js/` `ts/` | 実機の LLRT | 実機の tsc | — |
| `node/` | ビルド係で束ねて LLRT | tsc（npm の依存あり） | — |

```python
print("コードブロックも等幅で")
```

> 引用の行。*斜体* と **太字** と `コード`。
