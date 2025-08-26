Param(
  [switch]$Debug,
  [string]$Target,
  [switch]$MSVC
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Default to release unless -Debug is requested
$argsList = @('build')
if (-not $Debug) { $argsList += '--release' }

if ($MSVC -and -not $Target) {
  $Target = 'x86_64-pc-windows-msvc'
}

if (-not $Target) {
  $hostLine = (& rustc -vV | Select-String '^host:').ToString()
  $Target = $hostLine.Split(':')[1].Trim()
}

$targetDir = Join-Path (Get-Location) ("target\" + $Target)

$argsList += @('--target', $Target, '--target-dir', $targetDir)

Write-Host "cargo $($argsList -join ' ')"
cargo @argsList
