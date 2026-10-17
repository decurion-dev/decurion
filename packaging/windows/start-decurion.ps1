# Starts Decurion on Windows: makes sure Docker Desktop is running, prepares
# the agent workspace image when it is missing or out of date, starts the hub
# and the runner, and opens the board. Closing this window stops Decurion.
#
# Written for Windows PowerShell 5.1, which every Windows 10 and 11 PC has.

$ErrorActionPreference = 'Continue'
# Pick up programs installed since this window's parent started, such as
# helpers the installer just added with winget.
$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')
$Here = Split-Path -Parent $MyInvocation.MyCommand.Path
$Image = 'decurion/claude-worker:dev'
$Port = 7420
$Data = if ($env:DECURION_DATA) { $env:DECURION_DATA } else { Join-Path $env:LOCALAPPDATA 'decurion' }
$Logs = Join-Path $Data 'logs'
New-Item -ItemType Directory -Force -Path $Logs | Out-Null
$Host.UI.RawUI.WindowTitle = 'Decurion'

function Stop-WithMessage([string]$Message) {
    Write-Host ''
    Write-Host $Message -ForegroundColor Yellow
    Write-Host "Logs are in $Logs"
    Read-Host 'Press Enter to close this window'
    exit 1
}

# Reads a log file the hub or runner still has open for writing.
function Read-Shared([string]$Path) {
    if (-not (Test-Path $Path)) { return '' }
    $fs = [IO.File]::Open($Path, 'Open', 'Read', 'ReadWrite')
    try { (New-Object IO.StreamReader($fs)).ReadToEnd() } finally { $fs.Dispose() }
}

function Test-Docker { docker info *> $null; return ($LASTEXITCODE -eq 0) }

function Start-Logged([string]$Exe, [string]$Name) {
    $out = Join-Path $Logs "$Name.log"
    $err = Join-Path $Logs "$Name-errors.log"
    foreach ($f in @($out, $err)) {
        if (Test-Path $f) { Move-Item -Force $f ($f -replace '\.log$', '.previous.log') -ErrorAction SilentlyContinue }
    }
    # -NoNewWindow ties the process to this window, so closing it stops both.
    $p = Start-Process -FilePath (Join-Path $Here $Exe) -NoNewWindow -PassThru `
        -RedirectStandardOutput $out -RedirectStandardError $err
    # Touching Handle now keeps ExitCode readable after the process ends.
    $null = $p.Handle
    $p
}

Write-Host 'Starting Decurion...' -ForegroundColor Cyan

# Already running? Open the board instead of starting a second copy.
$probe = New-Object Net.Sockets.TcpClient
try {
    $probe.Connect('127.0.0.1', $Port)
    $probe.Close()
    Start-Process "http://decurion.localhost:$Port/"
    Write-Host 'Decurion is already running. Your board is open in your browser.'
    Start-Sleep -Seconds 4
    exit 0
} catch { }

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    Stop-WithMessage 'Docker Desktop is not installed. Install it from https://www.docker.com/products/docker-desktop/ and then open Decurion again.'
}

if (-not (Test-Docker)) {
    $desktop = Join-Path $env:ProgramFiles 'Docker\Docker\Docker Desktop.exe'
    if (Test-Path $desktop) {
        Write-Host 'Starting Docker Desktop. This can take a minute...'
        Start-Process $desktop
    }
    $deadline = (Get-Date).AddMinutes(3)
    while (-not (Test-Docker)) {
        if ((Get-Date) -gt $deadline) {
            Stop-WithMessage 'Docker Desktop did not start. Open Docker Desktop, wait until it says it is running, then open Decurion again.'
        }
        Start-Sleep -Seconds 3
    }
}

# The image is rebuilt when the Dockerfile shipped with this version changes.
$context = Join-Path $Here 'docker\claude-worker'
$want = (Get-FileHash -Algorithm SHA256 (Join-Path $context 'Dockerfile')).Hash.ToLower()
$have = docker image inspect -f '{{ index .Config.Labels "org.decurion.dockerfile" }}' $Image 2> $null
if ($LASTEXITCODE -ne 0 -or "$have".Trim() -ne $want) {
    Write-Host 'Getting the agent workspace ready. This happens once per version and takes a few minutes.'
    docker build --label "org.decurion.dockerfile=$want" -t $Image $context
    if ($LASTEXITCODE -ne 0) {
        Stop-WithMessage 'Could not prepare the agent workspace. Check that Docker Desktop is running and you are online, then try again.'
    }
}

# The gate is each agent's only way out to the web. Its image is rebuilt
# when any file of the gate's source shipped with this version changes.
$gateImage = 'decurion/gate:dev'
$gateContext = Join-Path $Here 'docker\gate'
$gateWant = (Get-ChildItem -Recurse -File $gateContext | Sort-Object FullName | ForEach-Object {
    $_.FullName.Substring($gateContext.Length) + ' ' + (Get-FileHash -Algorithm SHA256 $_.FullName).Hash
}) -join "`n"
$stream = [IO.MemoryStream]::new([Text.Encoding]::UTF8.GetBytes($gateWant))
$gateWant = (Get-FileHash -Algorithm SHA256 -InputStream $stream).Hash.ToLower()
$have = docker image inspect -f '{{ index .Config.Labels "org.decurion.source" }}' $gateImage 2> $null
if ($LASTEXITCODE -ne 0 -or "$have".Trim() -ne $gateWant) {
    Write-Host 'Getting the agents'' website gate ready. This happens once per version and takes a few minutes.'
    docker build --label "org.decurion.source=$gateWant" -t $gateImage $gateContext
    if ($LASTEXITCODE -ne 0) {
        Stop-WithMessage 'Could not prepare the agents'' website gate. Check that Docker Desktop is running and you are online, then try again.'
    }
}

$hub = Start-Logged 'decurion-hub.exe' 'hub'
$hubLog = Join-Path $Logs 'hub.log'
$url = $null
$deadline = (Get-Date).AddSeconds(30)
while (-not $url) {
    if ($hub.HasExited) { Stop-WithMessage 'Decurion could not start. The details are in hub-errors.log.' }
    if ((Get-Date) -gt $deadline) { Stop-WithMessage 'Decurion is taking too long to start.' }
    $m = [regex]::Match((Read-Shared $hubLog), 'Open the board: (\S+)')
    if ($m.Success) { $url = $m.Groups[1].Value } else { Start-Sleep -Milliseconds 300 }
}

$runner = Start-Logged 'decurion-runner.exe' 'runner'
Start-Process $url

Write-Host ''
Write-Host 'Decurion is running. Your board is open in your browser.' -ForegroundColor Green
Write-Host 'Keep this window open. Close it to stop Decurion.'
Write-Host "If the board did not open, paste this link into Edge or Chrome: $url"

$lastRestart = Get-Date
while (-not $hub.HasExited) {
    Start-Sleep -Seconds 2
    if ($runner.HasExited -and ((Get-Date) - $lastRestart).TotalSeconds -gt 10) {
        Write-Host 'The runner stopped. Restarting it.' -ForegroundColor Yellow
        $runner = Start-Logged 'decurion-runner.exe' 'runner'
        $lastRestart = Get-Date
    }
}

if (-not $runner.HasExited) { Stop-Process -Id $runner.Id -Force -ErrorAction SilentlyContinue }
if ($hub.ExitCode -ne 0) { Stop-WithMessage 'Decurion stopped because of a problem. The details are in hub-errors.log.' }
Write-Host 'Decurion stopped.'
