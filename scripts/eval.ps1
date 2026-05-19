#requires -Version 5.1
<#
.SYNOPSIS
  Build researchwiki and launch it with live log streaming.

.DESCRIPTION
  Wraps `cargo build` + a foreground run of the resulting exe with
  RUST_LOG tuned for ResearchWiki, tee-ing tracing output to both the
  console and a timestamped file under .eval-logs\.

  Defaults are tuned for "I want to see what the app is doing while I
  click around" - tracing at info for researchwiki itself, warn for
  noisy deps. Override via -RustLog.

.PARAMETER Release
  Build with --release. Default is debug for faster turnaround.

.PARAMETER RustLog
  Overrides the RUST_LOG env var passed to the child. Defaults to
  "researchwiki=debug,eframe=info,wgpu=warn,naga=warn".

.PARAMETER NoBuild
  Skip cargo build; only run the existing binary. Useful for re-runs
  while iterating on prompts/data rather than code.

.PARAMETER BuildOnly
  Build, then exit. No launch.

.PARAMETER LogDir
  Directory to write per-run log files. Defaults to .eval-logs\ in
  the repo root.

.EXAMPLE
  .\scripts\eval.ps1
  # debug build, info-level researchwiki logs, GUI launches

.EXAMPLE
  .\scripts\eval.ps1 -Release -RustLog "researchwiki=trace"
  # release build with maximally verbose app logs

.EXAMPLE
  $env:LLM_BASE_URL = "https://api.openai.com/v1"
  $env:LLM_API_KEY  = "sk-..."
  $env:LLM_MODEL    = "gpt-4o-mini"
  .\scripts\eval.ps1
  # supply LLM config via env (skips first-run modal)
#>
[CmdletBinding()]
param(
    [switch]$Release,
    [string]$RustLog = "researchwiki=debug,eframe=info,wgpu=warn,naga=warn",
    [switch]$NoBuild,
    [switch]$BuildOnly,
    [string]$LogDir
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

if (-not $LogDir) { $LogDir = Join-Path $repoRoot ".eval-logs" }
if (-not (Test-Path $LogDir)) { New-Item -ItemType Directory -Path $LogDir | Out-Null }

# Make sure rustup-installed cargo is on PATH even when the script is
# launched from a shell that hasn't picked up the user PATH yet.
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if ((Test-Path $cargoBin) -and ($env:Path -notlike "*$cargoBin*")) {
    $env:Path = "$cargoBin;$env:Path"
}

$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) {
    throw "cargo not found on PATH. Install Rust via rustup (https://rustup.rs) and re-run."
}

# Import the MSVC environment (cl.exe, link.exe, INCLUDE, LIB, ...) into
# this PowerShell session. rusqlite's `bundled` feature compiles SQLite
# C sources via the cc crate, so cargo needs to find a working linker.
# rustup picks up MSVC if it's already on PATH; if not, we run vcvarsall
# in a one-shot cmd and re-import everything it set.
function Import-MsvcEnvironment {
    if (Get-Command link.exe -ErrorAction SilentlyContinue) { return $true }

    $vswhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return $false }

    # Wrap native invocations so transient stderr (e.g. vswhere not on
    # PATH inside vcvars64.bat) doesn't trip $ErrorActionPreference=Stop.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $vsRoot = & $vswhere -latest -products * `
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
            -property installationPath
    } finally {
        $ErrorActionPreference = $prev
    }
    if (-not $vsRoot) { return $false }

    $vcvars = Join-Path $vsRoot 'VC\Auxiliary\Build\vcvars64.bat'
    if (-not (Test-Path $vcvars)) { return $false }

    # `set` after sourcing dumps the resulting env one var per line —
    # KEY=value. Redirect stderr to NUL inside cmd so any noise from
    # vcvars64.bat (e.g. its own vswhere probe failing) never reaches
    # PowerShell's pipeline.
    $dump = cmd /c "`"$vcvars`" >NUL 2>NUL && set"
    foreach ($line in $dump) {
        if ($line -match '^([^=]+)=(.*)$') {
            [System.Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process')
        }
    }
    return [bool](Get-Command link.exe -ErrorAction SilentlyContinue)
}

if (-not (Import-MsvcEnvironment)) {
    throw "MSVC linker not found. Install VS Build Tools 2022 with the 'Desktop development with C++' workload."
}

$buildFlags = @("build")
$profileDir = "debug"
if ($Release) {
    $buildFlags += "--release"
    $profileDir = "release"
}

if (-not $NoBuild) {
    Write-Host ">>> cargo $($buildFlags -join ' ')" -ForegroundColor Cyan
    & $cargo @buildFlags
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed (exit $LASTEXITCODE)"
    }
}

if ($BuildOnly) {
    Write-Host ">>> -BuildOnly set; not launching." -ForegroundColor Yellow
    return
}

$exe = Join-Path $repoRoot "target\$profileDir\researchwiki.exe"
if (-not (Test-Path $exe)) {
    throw "expected binary not found at $exe - did the build succeed?"
}

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$logFile = Join-Path $LogDir "researchwiki-$stamp.log"

Write-Host ">>> RUST_LOG = $RustLog" -ForegroundColor Cyan
Write-Host ">>> log file = $logFile" -ForegroundColor Cyan
Write-Host ">>> launching $exe (Ctrl+C in this window will kill it)" -ForegroundColor Cyan
Write-Host ""

$env:RUST_LOG = $RustLog

# tracing-subscriber's fmt layer writes to stdout by default, so we just
# tee the child's output. Merge stderr so panics/anyhow chains land in
# the log too. PowerShell's pipeline keeps streaming as the child runs.
& $exe 2>&1 | Tee-Object -FilePath $logFile

$exitCode = $LASTEXITCODE
Write-Host ""
Write-Host ">>> researchwiki exited with code $exitCode (log: $logFile)" -ForegroundColor Cyan
exit $exitCode
