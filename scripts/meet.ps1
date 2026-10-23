# Records a meeting with captions, audio, live ASR and the analyst, then transcribes, fuses and writes the minutes.
# Usage: .\scripts\meet.ps1 [-Source meet|teams-web|teams|zoom|slack] [-Name "Daily"] [-Build] [-Tisty] | -Config
param(
    [ValidateSet('meet', 'teams-web', 'teams', 'zoom', 'slack')]
    [string]$Source = 'meet',
    [string]$Name,
    [switch]$Build,
    [switch]$Tisty,
    [switch]$Config
)
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$built = Join-Path $repo 'target\release\eli5.exe'
$installed = Get-Command eli5.exe -ErrorAction SilentlyContinue
$mc = if ($Build -or -not $installed) { $built } else { $installed.Source }
$corpus = Join-Path $env:LOCALAPPDATA 'justeli5\corpus'
$configFile = Join-Path $env:APPDATA 'justeli5\config.json'

if ($Config) {
    if (-not (Test-Path $configFile)) {
        New-Item -ItemType Directory -Force (Split-Path $configFile) | Out-Null
        Copy-Item (Join-Path $PSScriptRoot 'config.example.json') $configFile
    }
    Start-Process $configFile
    exit 0
}

if ($Build -or -not (Test-Path $mc)) {
    $env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
    cargo build --release -p mc-cli --features whisper --manifest-path (Join-Path $repo 'Cargo.toml')
    if ($LASTEXITCODE) { exit $LASTEXITCODE }
    $mc = $built
}

$started = Get-Date
$capture = @('tui', '--source', $Source, '--consent', '--audio', '--consent-audio', '--live-asr', '--analyst')
if ($Name) { $capture += @('--name', $Name) }
& $mc @capture

$session = Get-ChildItem $corpus -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.LastWriteTime -ge $started -and (Test-Path (Join-Path $_.FullName 'manifest.json')) } |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $session) {
    Write-Host 'No meeting was recorded.'
    exit 1
}
$dir = $session.FullName

& $mc transcribe $dir
& $mc fuse $dir
& $mc minutes $dir
& $mc export $dir --format md --save
if ($Tisty) { & $mc export $dir --tisty }
Write-Host "Session: $dir"
