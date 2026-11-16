<#
.SYNOPSIS
  Worker・Durable Object・ビルド係のイメージを Cloudflare に deploy する (docker を使わず wslc で)。

.DESCRIPTION
  docs/setup-remote-build.md §2。この PC に docker は無いので、wrangler がイメージを作って送るのに
  呼ぶ docker を、WSL コンテナー (wslc) に直して渡す:
    - WRANGLER_DOCKER_BIN = wrangler-docker\docker-shim.mjs (docker の引数を wslc の引数に直す)
    - NODE_OPTIONS に --require wrangler-docker\spawn-hook.cjs (Windows の Node は .cmd / .ps1 を
      shell なしで起動できないので、shim の起動だけを node docker-shim.mjs に差し替える)
  wrangler は remote\worker\node_modules のもの (package-lock.json で版を固定)。無ければ npm ci する
  (インストールスクリプトは .npmrc で止めてある)。

  -DryRun はイメージを wslc で作り、Worker を束ねるところまで (Cloudflare にはつながない)。
  初めての deploy の前に、ログインせずに試せる。

  確かめたこと (2026-11-17、wrangler 4.163.0・wslc 3.0.5): -DryRun が通る (イメージを作る・
  Worker を束ねる)、shim の build・tag・image inspect・image rm、ローカルのレジストリへの
  login・push と、push 後の digest の読み取り。確かめていないこと: Cloudflare のレジストリ
  (registry.cloudflare.com) への login と push、そのあとのイメージの準備 (最初の deploy で見る)。

.PARAMETER DryRun
  wrangler deploy --dry-run (イメージは作るが、送らない・deploy しない)

.EXAMPLE
  cd zcode; .\remote\container\deploy.ps1 -DryRun     # 試す (ログイン不要)
  cd zcode/remote/worker; npx wrangler login           # 初回だけ
  cd zcode; .\remote\container\deploy.ps1             # 本番の deploy

.NOTES
  作ったイメージは wslc の中に残る (タグ zcode-build-buildsession-builder:wrangler-…)。
  片付けは wslc images で見て wslc rmi <名前>。
#>
param(
    [switch]$DryRun,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$WranglerArgs
)
$ErrorActionPreference = 'Stop'

$Worker = (Resolve-Path (Join-Path $PSScriptRoot '..\worker')).Path
$Shim = (Resolve-Path (Join-Path $PSScriptRoot 'wrangler-docker\docker-shim.mjs')).Path
$Hook = (Resolve-Path (Join-Path $PSScriptRoot 'wrangler-docker\spawn-hook.cjs')).Path

if (-not (Get-Command wslc -ErrorAction SilentlyContinue)) {
    throw 'wslc が見つからない。WSL 2.9.3 以降が必要 (wsl --update)。'
}
if (-not (Get-Command node -ErrorAction SilentlyContinue)) {
    throw 'node が見つからない。Node 22.13 以上を入れる。'
}
$nodeVersion = [version]((node --version).TrimStart('v'))
if ($nodeVersion -lt [version]'22.13.0') {
    throw "Node $nodeVersion は古い。22.13 以上が要る。"
}

$wrangler = Join-Path $Worker 'node_modules\wrangler\bin\wrangler.js'
if (-not (Test-Path $wrangler)) {
    Write-Host 'npm ci (remote\worker)'
    npm ci --prefix $Worker --ignore-scripts --no-audit --no-fund
    if ($LASTEXITCODE -ne 0) { throw "npm ci に失敗した (終了コード $LASTEXITCODE)" }
}

$state = Join-Path ([System.IO.Path]::GetTempPath()) ("zc-docker-shim-" + [guid]::NewGuid().ToString('N') + '.json')
$saved = @{
    WRANGLER_DOCKER_BIN = $env:WRANGLER_DOCKER_BIN
    NODE_OPTIONS = $env:NODE_OPTIONS
    ZC_DOCKER_SHIM_STATE = $env:ZC_DOCKER_SHIM_STATE
}
$env:WRANGLER_DOCKER_BIN = $Shim
# NODE_OPTIONS の "…" の中では \ がエスケープになるので / にする
$env:NODE_OPTIONS = ("--require `"$($Hook.Replace('\', '/'))`" " + $saved.NODE_OPTIONS).Trim()
$env:ZC_DOCKER_SHIM_STATE = $state

$cmd = @('deploy')
if ($DryRun) { $cmd += '--dry-run' }
if ($WranglerArgs) { $cmd += $WranglerArgs }

Push-Location $Worker
try {
    node $wrangler @cmd
    $code = $LASTEXITCODE
} finally {
    Pop-Location
    foreach ($k in $saved.Keys) { Set-Item -Path "env:$k" -Value $saved[$k] -ErrorAction SilentlyContinue }
    foreach ($k in $saved.Keys) { if ($null -eq $saved[$k]) { Remove-Item -Path "env:$k" -ErrorAction SilentlyContinue } }
    Remove-Item -LiteralPath $state -ErrorAction SilentlyContinue
}
exit $code
