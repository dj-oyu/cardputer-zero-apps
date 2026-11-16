<#
.SYNOPSIS
  CardputerZero 開発コンテナ (WSL コンテナー / wslc) のラッパー。

.DESCRIPTION
  リポジトリを /work にマウントし、今いるフォルダに対応する場所で
  コマンドを実行する。リポジトリ外から呼んだ場合は /work で実行する。

.EXAMPLE
  .\container\dev.ps1 -Build                       # イメージを作る (初回 / Dockerfile 変更時)
  .\container\dev.ps1                              # 対話シェル
  cd qrreader; ..\container\dev.ps1 ./build.sh 3.5.2
  cd qrreader; ..\container\dev.ps1 python3 qrreader_lcd.py --render-test out.png
#>
# param ブロックは使わない。使うと "bash -c ..." の -c が PowerShell の
# パラメータ名として解釈されてしまうため、$args をそのまま受ける。
$ErrorActionPreference = 'Stop'
$Build = ($args.Count -ge 1 -and $args[0] -eq '-Build')
$Command = if ($Build) { @() } else { @($args) }

$Image = 'cz-dev:latest'
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

if (-not (Get-Command wslc -ErrorAction SilentlyContinue)) {
    throw 'wslc が見つからない。WSL 2.9.3 以降が必要 (wsl --update)。'
}

if ($Build) {
    wslc build -t $Image (Join-Path $Repo 'container')
    exit $LASTEXITCODE
}

# 今いる場所をコンテナ内のパスに写す
$here = (Get-Location).Path
$workdir = '/work'
if ($here.StartsWith($Repo, [System.StringComparison]::OrdinalIgnoreCase)) {
    $rel = $here.Substring($Repo.Length).TrimStart('\').Replace('\', '/')
    if ($rel) { $workdir = "/work/$rel" }
}

# Rust の crate キャッシュとビルド成果物は名前付きボリュームに置く
# (Windows 側のマウント越しだと遅い。コンテナを消しても残る)
$runArgs = @('run', '--rm', '-v', "${Repo}:/work",
             '-v', 'cz-cargo-registry:/opt/cargo/registry',
             '-v', 'cz-cargo-target:/cargo-target',
             '-w', $workdir)
if ($Command) {
    # 引数は 1 本のシェルコマンドにして bash に渡す
    $quoted = $Command | ForEach-Object { "'" + ($_ -replace "'", "'\''") + "'" }
    wslc @runArgs $Image bash -c ($quoted -join ' ')
} else {
    wslc @runArgs -it $Image bash
}
exit $LASTEXITCODE
