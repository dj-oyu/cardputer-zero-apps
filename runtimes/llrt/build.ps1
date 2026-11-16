<#
LLRT を CardputerZero（Cortex-A53）向けにビルドする（Windows から。中身は build.sh）。

  .\build.ps1                         # v0.10.0-beta の no-sdk
  .\build.ps1 v0.11.0-beta            # 版を上げる
  .\build.ps1 v0.10.0-beta std        # AWS SDK 入り（no-sdk / std / full-sdk）

開発コンテナのイメージ（cz-dev:latest）を使い捨てで動かす。ソースとツールチェーンは
名前付きボリューム llrt-build に残る（2 回目からは速い）。できた実行ファイルは out/ に出る。
ボリュームを消すときは: wslc volume rm llrt-build
#>
param(
    [string]$Tag = 'v0.10.0-beta',
    [ValidateSet('no-sdk', 'std', 'full-sdk')]
    [string]$Variant = 'no-sdk',
    [string]$Image = 'cz-dev:latest'
)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
New-Item -ItemType Directory -Force (Join-Path $here 'out') | Out-Null
wslc run --rm -v "${here}:/s" -v "$(Join-Path $here 'out'):/out" -v 'llrt-build:/b' $Image `
    bash -c "sed 's/\r$//' /s/build.sh > /tmp/build.sh && bash /tmp/build.sh '$Tag' '$Variant'"
exit $LASTEXITCODE
