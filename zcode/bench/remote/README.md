# remotebench（外部ビルドの実機側の計測）

`docs/stage3-design.md` §15 の「見込み（測っていない）」を実機で確かめるための小さなベンチ
（課題 M1）。`zc-remote` と同じ crate・同じ版（`zcode/remote/Cargo.lock` に合わせた `Cargo.lock`）・
同じプロファイル（`opt-level = "s"`・thin LTO）で作る。zcode 本体・`remote/` のワークスペースには
入れない（単独でビルドする）。

| 区分 | 測るもの | 設計のどこに効くか |
|---|---|---|
| `sha256 [MiB] [回数]` | SHA-256 の速さ。sha2（`zc-remote` が使う）と ring。大きな 1 塊と、4KiB ごとに別のハッシュ（小さなソースが多い木） | §7.2 走査とハッシュ・§7.7 成果物の確認。見込みは 30〜60MB/s |
| `zstd [x.zst] [回数]` | ruzstd の展開の速さ（64KiB ずつ読む・SHA-256 を同時に計算する場合も）と、展開中のヒープの最大 | §4.5・§7.7（窓 2MiB の効き目、受け取りの時間） |
| `scan [フォルダ] [--files N] [回数]` | 木の `lstat` 走査 + RAM の表との照合（2 回目以降の依頼の手間）と、全部を読んでハッシュ（再起動後の初回） | §7.2 |
| `tls [回数]` | TLS 1.3 のハンドシェイクの CPU（クライアント側。完全と再開）。同じプロセスの中でクライアントとサーバを直結。`zc_remote::net::tls_config` と同じ絞り方（X25519・ChaCha20-Poly1305）、証明書は ECDSA P-256 の 3 段（根 → 中間 → 末端。検証は 3 回で、実物の Cloudflare と同じ） | §7.3。見込みは 600MHz で 15〜30ms |
| `crypto [MiB]` | AEAD の速さ（ChaCha20-Poly1305・AES-128/256-GCM、16KiB のレコード）、X25519 の鍵作り + 合意 1 回、ECDSA P-256 の検証 1 回 | §7.3（AES の命令が無いので ChaCha を選んだ根拠） |
| `all` | 上の全部を既定の大きさで | |

出力は 1 行 1 項目（`名前: 値`）。始めと終わりに cpu0 のクロック（`scaling_cur_freq`）、最後に
`VmRSS` / `VmHWM` を出す。ondemand ガバナは短い処理では上がりきらないので、どの区分も
「1 回目（first）」と「最良（best / min）・中央値（median）」を分けて出す。

## ビルド

```powershell
cd zcode\bench\remote
..\..\..\container\dev.ps1 ./build.sh          # → out/remotebench（aarch64）
```

`build.sh` は、A53 に無い命令（LSE / RCpc）が無条件に使われていないかも数える（`zc-remote` と同じ
12 個前後なら可）。開発コンテナの x86_64 でも `cargo run --release -- all` で動く（数字は参考にならない）。

## 実機での動かし方

実機には `/tmp` にだけ置き、終わったら消す。**必ず `timeout` を付ける**（液晶とキーには触らないが、
決まりどおりに）。

```bash
scp out/remotebench <実機>:/tmp/
ssh <実機> 'chmod +x /tmp/remotebench && cd /tmp && timeout -k 5 300 ./remotebench all; rm -f /tmp/remotebench'
```

区分ごとに（それぞれ数秒〜数十秒）:

```bash
timeout -k 5 60  /tmp/remotebench sha256 8 5
timeout -k 5 60  /tmp/remotebench zstd               # 自分の実行ファイルを ruzstd の Fastest で圧縮して使う
timeout -k 5 120 /tmp/remotebench scan --files 2000  # /tmp（tmpfs）に作って測り、消す
timeout -k 5 120 /tmp/remotebench scan ~/code 5      # 実物の木（SD）。読むだけで書かない
timeout -k 5 120 /tmp/remotebench tls 50
timeout -k 5 60  /tmp/remotebench crypto 4
```

- **zstd は実物に近い圧縮でも測る。** 受付は `zstd -19`・窓 2MiB で圧縮する（§4.5）。ruzstd の
  Fastest（≒ `-1`）とは中身の形が違う。実機か PC に `zstd` コマンドがあれば:
  ```bash
  zstd -19 --zstd=wlog=21 -o /tmp/rb.zst /tmp/remotebench     # 実機の zc-remote や zcode でもよい
  timeout -k 5 60 /tmp/remotebench zstd /tmp/rb.zst 5; rm -f /tmp/rb.zst
  ```
  開発コンテナには `zstd` が無い（ビルド係のイメージには入る。§6.4）
- **scan の冷えた状態**（再起動直後・dentry と inode のキャッシュが空）は、`/proc/sys/vm/drop_caches`
  に書く必要があり sudo が要る。やるならユーザーに頼む（`sync; echo 3 | sudo tee /proc/sys/vm/drop_caches`
  のあと `scan ~/code 1`）。tmpfs に作った木は常に温かい
- `scan` の生成先は `$TMPDIR`（無ければ `/tmp`。Debian 13 では tmpfs）。SD には書かない。
  `target/`・`.git/` などの除外（§7.2）は、試しの木にも少し作って、中に入らないことを確かめている
- `tls` のサーバ側の時間は参考（実物のサーバは Cloudflare）。効くのはクライアント側の `tls_full_client_ms`。
  ネットワークの往復は入らない（往復の時間は `remote-build.html` §12 の curl で別に測る）
- できればガバナが上がる前後の両方を見る: 何もしていない状態から 1 回目をそのまま測る（600MHz 寄り）のと、
  `all` を 2 回続けて 2 回目を見る（1GHz 寄り）。始めと終わりのクロックの行を一緒に残す

## `docs/measurements.md` に残すもの

新しい節（例: 「12. 外部ビルドの実機側（remotebench）」）を作り、共通の条件（機体の号機・日付・
温度・`vcgencmd get_throttled`・ランチャーとデスクトップの常駐の有無）と、次の表を書く。出典の列は
`bench/remote` の区分と引数。§15 の「見込み」の行は、測れたら「実測」へ移す。

| 項目 | 記録する値 |
|---|---|
| SHA-256（sha2 / ring） | `sha256_*_bulk` の first・best（MB/s）、`sha256_*_4k` の best（MB/s と us/ファイル） |
| zstd の展開 | 入力（`zstd -19 wlog=21` か Fastest か）、圧縮前後の大きさ、`zstd_decode` と `zstd_decode_sha256` の best（ms・MB/s）、`zstd_decode_heap_peak`（kB） |
| 木の走査 | ファイル数・合計、`scan_stat` の first・best（ms と us/ファイル）、`scan_read_hash` の first・best。tmpfs の試しの木と `~/code`（SD）を別の行に。冷えた状態を測れたらそれも |
| TLS のハンドシェイク（クライアントの CPU） | `tls_full_client_ms` と `tls_resumed_client_ms` の first・median・p90、`tls_full_heap_peak_client_plus_server` |
| 暗号の部品 | `aead_*_seal` の best（MB/s。ChaCha と AES の比）、`x25519_keygen_agree_ms`・`ecdsa_p256_verify_ms` の median |
| 全体 | 始めと終わりのクロック、`mem_end` の VmHWM |

この表の数字で決まること: 走査の 2 秒の決まりと再ハッシュの費用（§7.2）、成果物の窓 2MiB と
tmpfs の上限（§4.5・§7.7）、ハンドシェイクの時間切れ 5 秒の余裕（§7.3）、`zc-remote` のメモリの
見込み（2〜4MB）。

## 実装のメモ

- ヒープの最大は、自前の `GlobalAlloc`（`System` を包んで数える）で測る。展開の前後で RSS を見ても、
  解放済みのページを使い回すので増えない（開発コンテナで 0 kB と出た）
- `zstd` の既定の入力（自分の実行ファイル）では、展開した結果が元と同じか（長さと SHA-256）を確かめる
- `tls` は、選ばれた暗号が ChaCha20-Poly1305 であること、再開のときに本当に再開したこと
  （`tls_resumed_resumed: 50/50`）を確かめる
