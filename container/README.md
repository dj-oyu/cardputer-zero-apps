# 開発コンテナ

実機なしでできる作業（.deb のビルド、描画確認、自己テスト、Rust の
aarch64 クロスビルド）を、実機と同じ Debian 13 (trixie) / Python 3.13 で
行うためのコンテナ。
Windows では WSL コンテナー（`wslc`、WSL 2.9.3 以降）で動かす。
Dockerfile は Docker / Podman でもそのまま使える。

## できること / できないこと

| 作業 | コンテナ | 備考 |
|---|---|---|
| `./build.sh <ver>` で .deb を作る | ○ | x86_64 で作った .deb をそのまま実機に入れられる（中身は pure Python） |
| `--render-test` で描画を PNG に出す | ○ | Noto CJK フォント込み |
| `qrreader.py --selftest` | ○ | 実機と同じ結果（全項目 OK）。zxing-cpp 3.1.1 / PIL 11.1.0 / numpy 2.2.4 は実機と同じ版 |
| Rust アプリの aarch64 クロスビルド | ○ | `cargo build --release --target aarch64-unknown-linux-gnu`。成果物は `/cargo-target`（名前付きボリューム） |
| カメラ・液晶 (fb0)・キー・BlueZ | × | 実機で確認する |

## 使い方 (Windows / PowerShell)

```powershell
.\container\dev.ps1 -Build          # イメージ cz-dev:latest を作る（初回と Dockerfile 変更時）
.\container\dev.ps1                 # 対話シェル（リポジトリは /work）
cd qrreader
..\container\dev.ps1 ./build.sh 3.5.2
..\container\dev.ps1 python3 qrreader_lcd.py --render-test out.png
```

`dev.ps1` は今いるフォルダに対応するコンテナ内の場所でコマンドを
実行する。中身は次と同じ:

```powershell
wslc run --rm -v "<repo>:/work" `
  -v cz-cargo-registry:/opt/cargo/registry -v cz-cargo-target:/cargo-target `
  -w /work/qrreader cz-dev:latest bash -c "./build.sh 3.5.2"
```

`cz-cargo-registry`（crate のキャッシュ）と `cz-cargo-target`（Rust の
ビルド成果物）は名前付きボリューム。Windows のフォルダ越しにビルドすると
遅いので、成果物はリポジトリの外に置いている。消すときは
`wslc volume remove cz-cargo-target`。

## 注意

- リポジトリは `.gitattributes` で LF に固定している。Windows で
  CRLF に変わったシェルスクリプトや `packaging/` のファイルは、
  bash で動かず、.deb に入ると壊れる（postinst / sudoers）
- 実機へのデプロイは Windows 側から scp で行う（コンテナから
  Tailscale に出る前提にはしていない）
