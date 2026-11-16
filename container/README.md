# 開発コンテナ

実機なしでできる作業（.deb のビルド、描画確認、自己テスト）を、
実機と同じ Debian 13 (trixie) / Python 3.13 で行うためのコンテナ。
Windows では WSL コンテナー（`wslc`、WSL 2.9.3 以降）で動かす。
Dockerfile は Docker / Podman でもそのまま使える。

## できること / できないこと

| 作業 | コンテナ | 備考 |
|---|---|---|
| `./build.sh <ver>` で .deb を作る | ○ | x86_64 で作った .deb をそのまま実機に入れられる（中身は pure Python） |
| `--render-test` で描画を PNG に出す | ○ | Noto CJK フォント込み |
| `qrreader.py --selftest` | ○ | 実機と同じ結果（全項目 OK）。zxing-cpp 3.1.1 / PIL 11.1.0 / numpy 2.2.4 は実機と同じ版 |
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
wslc run --rm -v "<repo>:/work" -w /work/qrreader cz-dev:latest bash -c "./build.sh 3.5.2"
```

## 注意

- リポジトリは `.gitattributes` で LF に固定している。Windows で
  CRLF に変わったシェルスクリプトや `packaging/` のファイルは、
  bash で動かず、.deb に入ると壊れる（postinst / sudoers）
- 実機へのデプロイは Windows 側から scp で行う（コンテナから
  Tailscale に出る前提にはしていない）
