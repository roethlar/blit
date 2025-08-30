Param(
  [switch]$Release,
  [switch]$Async
)

set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = (Resolve-Path ".").Path
$cfg = if ($Release) { 'release' } else { 'debug' }

Write-Host "Building ($cfg)..."
if ($Release) { cargo build --release } else { cargo build }

$binCli = Join-Path $root "target\$cfg\blit.exe"
$binD   = Join-Path $root "target\$cfg\blitd.exe"
if (-not (Test-Path $binCli)) { throw "CLI binary not found at $binCli" }
if (-not (Test-Path $binD))   { throw "Daemon binary not found at $binD" }

$tmp = Join-Path $env:TEMP ("blit-smoke-" + [Guid]::NewGuid().Guid)
$src = Join-Path $tmp 'src'
$dst = Join-Path $tmp 'dst'
$pull = Join-Path $tmp 'pull'
New-Item -ItemType Directory -Path $src,$dst,$pull | Out-Null

# Create a small dataset
Set-Content -LiteralPath (Join-Path $src 'a.txt') -Value 'hello world'
New-Item -ItemType Directory -Path (Join-Path $src 'sub') | Out-Null
Set-Content -LiteralPath (Join-Path $src 'sub' 'b.txt') -Value ('x' * 2048)

# Try to create a symlink; ignore failure if privileges missing
try {
  $link = Join-Path $src 'alink.txt'
  $target = 'a.txt'
  cmd /c "mklink `"$link`" `"$target`"" | Out-Null
} catch { Write-Host "Symlink creation skipped: $_" }

# Mark a.txt read-only and capture mtime for verification
($srcFile = Get-Item (Join-Path $src 'a.txt')).Attributes += 'ReadOnly'
$srcMtime = (Get-Item (Join-Path $src 'a.txt')).LastWriteTimeUtc

# Push test (client -> daemon)
$port = 9031
$p1 = Start-Process -FilePath $binD -ArgumentList @('--root', $dst, '--bind', "127.0.0.1:$port") -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 2
try {
  & $binCli $src ("blit://127.0.0.1:$port/") --mir | Out-Null
  if (-not (Test-Path (Join-Path $dst 'a.txt'))) { throw 'push: a.txt missing at destination' }
  if (-not (Test-Path (Join-Path $dst 'sub' 'b.txt'))) { throw 'push: sub/b.txt missing at destination' }
  # Verify read-only attribute mirrored and mtime within tolerance
  $dstFile = Get-Item (Join-Path $dst 'a.txt')
  if (-not ($dstFile.Attributes -band [IO.FileAttributes]::ReadOnly)) { throw 'push: a.txt not read-only at destination' }
  $dstMtime = $dstFile.LastWriteTimeUtc
  if ([math]::Abs((New-TimeSpan -Start $srcMtime -End $dstMtime).TotalSeconds) -gt 3) { throw 'push: a.txt mtime mismatch' }
} finally {
  if ($p1 -and -not $p1.HasExited) { $p1 | Stop-Process -Force }
}

# Pull test (daemon -> client)
$port2 = 9032
$p2 = Start-Process -FilePath $binD -ArgumentList @('--root', $src, '--bind', "127.0.0.1:$port2") -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 2
try {
  & $binCli ("blit://127.0.0.1:$port2/") $pull --mir | Out-Null
  if (-not (Test-Path (Join-Path $pull 'a.txt'))) { throw 'pull: a.txt missing at destination' }
  if (-not (Test-Path (Join-Path $pull 'sub' 'b.txt'))) { throw 'pull: sub/b.txt missing at destination' }
  # Verify read-only and mtime on pull
  $pullFile = Get-Item (Join-Path $pull 'a.txt')
  if (-not ($pullFile.Attributes -band [IO.FileAttributes]::ReadOnly)) { throw 'pull: a.txt not read-only at destination' }
  $pullMtime = $pullFile.LastWriteTimeUtc
  if ([math]::Abs((New-TimeSpan -Start $srcMtime -End $pullMtime).TotalSeconds) -gt 3) { throw 'pull: a.txt mtime mismatch' }
} finally {
  if ($p2 -and -not $p2.HasExited) { $p2 | Stop-Process -Force }
}

Write-Host 'Windows smoke tests OK (push and pull)'