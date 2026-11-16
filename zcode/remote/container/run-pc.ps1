<#
.SYNOPSIS
  PC をビルド係にする: ビルド係のイメージ (cz-builder) を WSL コンテナーで動かし、
  ポート 7411 で待つ (docs/stage3-design.md §6.7・§12.2)。

.DESCRIPTION
  雛形。課題 C3 が仕上げる。
  - イメージが無ければ作る: wslc build -f remote/container/Dockerfile -t cz-builder:latest <zcode/>
  - 名前付きボリューム cz-buildd-data を /var/lib/zc-buildd に付ける (cargo の出力と crate の
    キャッシュが残るので、2 回目からのビルドが速い)
  - ポート 7411 を公開する。**tailnet の側だけで待つ** (LAN やインターネットに開けない)。
    wslc のポートの公開のしかたと、Windows のファイアウォールの穴の開け方は未確認
    (docs/stage3-design.md §15)
  - 認証は Tailscale の ACL (実機のタグ → この PC の 7411 番だけ)。受付そのものは認証しない

.EXAMPLE
  .\remote\container\run-pc.ps1            # 前面で動かす (Ctrl+C で止める)
  .\remote\container\run-pc.ps1 -Build     # イメージを作り直す
#>
$ErrorActionPreference = 'Stop'
throw 'まだ実装していない (docs/stage3-design.md の課題 C3)'
