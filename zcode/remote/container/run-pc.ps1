<#
.SYNOPSIS
  PC をビルド係にする: ビルド係のイメージ (cz-builder) を WSL コンテナー (wslc) で動かし、
  Tailscale のアドレスのポート 7411 で待つ (docs/stage3-design.md §6.7・§12.2)。

.DESCRIPTION
  - イメージ cz-builder:latest が無ければ作る (-Build で作り直す)。文脈は zcode/
  - 名前付きボリューム cz-buildd-data を /var/lib/zc-buildd に付ける。cargo の出力・crate と
    npm のキャッシュが残るので、2 回目からのビルドが速い
  - ポートは **Tailscale のインターフェースの IPv4 アドレスにだけ** 公開する
    (wslc run -p <アドレス>:7411:7411)。LAN やインターネットには開けない
  - 認証は Tailscale の ACL (実機のタグ → この PC の 7411 番だけ) と Windows の
    ファイアウォール。受付そのものは認証しない (docs/setup-remote-build.md §3)

  wslc 3.0.5 で確かめたこと (2026-11-17):
  - -p <ポート>:7411 (アドレスなし) は 127.0.0.1 だけで待つ (docker と違い 0.0.0.0 ではない)
  - -p <Tailscale の IPv4>:7411:7411 は、そのアドレスだけで待つ (127.0.0.1 からは届かない)
  - Windows 側で待つのは dllhost.exe (wslc のサービス)。ファイアウォールの規則はプログラムでは
    なくポートとインターフェースで書く
  - 名前付きボリュームは、初回にイメージの中身と持ち主 (uid 1000) で作られ、builder で書ける
  確かめていないこと: 実機 (tailnet の別の機体) から届くか (ACL とファイアウォールを入れてから)、
  Tailscale が止まる・つなぎ直したときに待ち受けがどうなるか (止まったら、このスクリプトを
  動かし直す)。

.PARAMETER Build
  イメージを作り直す (Dockerfile・zc-buildd を変えたとき)。作り直したら動いている受付は止めて
  起動し直す。

.PARAMETER Bind
  待つアドレス。既定は Tailscale のインターフェースの IPv4。手元だけで試すときは 127.0.0.1。

.PARAMETER Port
  PC 側のポート (既定 7411)。コンテナの中は常に 7411。

.PARAMETER Detach
  裏で動かす (止めるときは -Stop)。既定は前面で動かし、Ctrl+C で止める。

.PARAMETER Stop
  裏で動かしている受付を止める。

.EXAMPLE
  cd zcode; .\remote\container\run-pc.ps1                 # 前面で動かす (Ctrl+C で止める)
  cd zcode; .\remote\container\run-pc.ps1 -Build          # イメージを作り直してから動かす
  cd zcode; .\remote\container\run-pc.ps1 -Bind 127.0.0.1 # 手元だけで試す
  cd zcode; .\remote\container\run-pc.ps1 -Detach; .\remote\container\run-pc.ps1 -Stop

.NOTES
  後片付け: wslc volume remove cz-buildd-data (キャッシュが消えるだけ)、wslc rmi cz-builder:latest
#>
param(
    [switch]$Build,
    [string]$Bind,
    [ValidateRange(1, 65535)]
    [int]$Port = 7411,
    [switch]$Detach,
    [switch]$Stop
)
$ErrorActionPreference = 'Stop'

$Image = 'cz-builder:latest'
$Name = 'cz-buildd'
$Volume = 'cz-buildd-data'
$Zcode = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Dockerfile = Join-Path $Zcode 'remote\container\Dockerfile'

if (-not (Get-Command wslc -ErrorAction SilentlyContinue)) {
    throw 'wslc が見つからない。WSL 2.9.3 以降が必要 (wsl --update)。'
}

function Test-Running {
    $ids = wslc list --quiet --filter "name=$Name" 2>$null
    return [bool]($ids | Where-Object { $_ -and $_.Trim() })
}

if ($Stop) {
    if (Test-Running) {
        wslc stop $Name | Out-Null
        Write-Host "止めた: $Name"
    } else {
        Write-Host "動いていない: $Name"
    }
    exit 0
}

if (Test-Running) {
    throw "$Name はもう動いている。止めるには: .\remote\container\run-pc.ps1 -Stop"
}

# イメージ
wslc image inspect $Image *> $null
if ($Build -or $LASTEXITCODE -ne 0) {
    Write-Host "イメージを作る: $Image (初回は数分)"
    wslc build -f $Dockerfile -t $Image $Zcode
    if ($LASTEXITCODE -ne 0) { throw "イメージを作れなかった (終了コード $LASTEXITCODE)" }
}

# 待つアドレス: 既定は Tailscale のインターフェースの IPv4。見つからなければ止まる
# (0.0.0.0 や LAN に落とさない)
if (-not $Bind) {
    $addr = Get-NetIPAddress -InterfaceAlias 'Tailscale' -AddressFamily IPv4 -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if (-not $addr) {
        throw 'Tailscale のインターフェース (名前 "Tailscale") に IPv4 が無い。Tailscale を起動するか、-Bind で指定する。'
    }
    $Bind = $addr.IPAddress
}

$runArgs = @('run', '--rm', '--name', $Name,
             '-p', "${Bind}:${Port}:7411",
             '-v', "${Volume}:/var/lib/zc-buildd",
             '--stop-timeout', '20')
Write-Host "ビルド係を動かす: http://${Bind}:${Port}/healthz (置き場はボリューム $Volume)"

if ($Detach) {
    wslc @runArgs -d $Image | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "起動できなかった (終了コード $LASTEXITCODE)" }
    Write-Host "裏で動いている。ログ: wslc logs $Name / 止める: .\remote\container\run-pc.ps1 -Stop"
    exit 0
}

# 前面: Ctrl+C でスクリプトが止まっても、finally でコンテナを止める
# (zc-buildd は SIGTERM で走っているジョブを止めてから終わる)
try {
    wslc @runArgs $Image
} finally {
    if (Test-Running) { wslc stop $Name | Out-Null }
}
