# Side-by-side download benchmark: Eid Download (eid.exe) vs Internet Download Manager (IDMan.exe),
# on the SAME Windows PC, the SAME file and the SAME server. Measures, never guesses.
#
# Default source: the loopback eid-benchserver (target\release\eid-benchserver.exe) serving a
# generated file, with an optional per-connection cap (-ThrottlePerConn, bytes/s) that simulates a
# server limiting each connection, the case where multi-connection download managers help.
# -Url uses a real internet file instead (then results also depend on the network at that moment;
#  run eid and IDM alternately, several times, and compare medians only).
#
# For every run: wall time from start until the complete file is on disk, MiB/s, SHA-256 of the
# result (must equal the server's/first run's hash, a mismatch fails the script), and the peak
# working set of eid.exe (IDM's memory is sampled from the long-lived IDMan.exe process).
#
# How IDM is driven: its documented command line `IDMan.exe /d URL /p DIR /f NAME /n`
# (/n = silent, no questions). IDM writes into its own temp folder and moves the file into DIR
# when it completes, so "the file exists with the expected size" = finished. Set IDM's connection
# count in IDM itself (Options > Connection > "Default max. conn. number") to the same -Connections
# value before running, and turn off IDM's speed limiter. Nothing here changes IDM's settings.
#
# Usage (PowerShell, repository root, after `cargo build --release -p eid-cli -p eid-testserver`):
#   scripts\windows\compare_idm.ps1                                   # eid + IDM, loopback, 256 MiB, 4 MiB/s per connection
#   scripts\windows\compare_idm.ps1 -SkipIdm                          # eid only (what CI runs)
#   scripts\windows\compare_idm.ps1 -Url https://example.com/big.iso -Runs 3
# Output: a table, plus $Out\results.json and $Out\results.csv.
param(
  [string]$Bin = "target\release",
  [string]$Out = "idm-compare",
  [long]$Size = 268435456,
  [long]$ThrottlePerConn = 4194304,
  [int[]]$Connections = @(8, 16),
  [int]$Runs = 3,
  [string]$Url = "",
  [string]$IdmPath = "",
  [switch]$SkipIdm,
  [int]$TimeoutSeconds = 900
)
$ErrorActionPreference = "Stop"
$Bin = (Resolve-Path $Bin).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$eid = Join-Path $Bin "eid.exe"
if (-not (Test-Path $eid)) { throw "missing $eid (cargo build --release -p eid-cli)" }

function Find-Idm {
  if ($IdmPath) { return (Resolve-Path $IdmPath).Path }
  foreach ($p in @("${env:ProgramFiles(x86)}\Internet Download Manager\IDMan.exe", "$env:ProgramFiles\Internet Download Manager\IDMan.exe")) {
    if ($p -and (Test-Path $p)) { return $p }
  }
  return $null
}

$server = $null
$expectedSha = $null
if (-not $Url) {
  $bs = Join-Path $Bin "eid-benchserver.exe"
  if (-not (Test-Path $bs)) { throw "missing $bs (cargo build --release -p eid-testserver)" }
  $psi = New-Object System.Diagnostics.ProcessStartInfo $bs
  $psi.Arguments = "--size $Size --throttle-per-conn $ThrottlePerConn"
  $psi.UseShellExecute = $false; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardInput = $true
  $server = [System.Diagnostics.Process]::Start($psi)
  $base = $null
  while (-not $expectedSha) {
    $line = $server.StandardOutput.ReadLine()
    if ($null -eq $line) { throw "eid-benchserver exited early" }
    if ($line -like "URL *") { $base = $line.Substring(4).Trim() }
    if ($line -like "SHA256 *") { $expectedSha = $line.Substring(7).Trim().ToLower() }
  }
  $path = if ($ThrottlePerConn -gt 0) { "throttled.bin" } else { "big.bin" }
  $Url = "$base/$path"
}
$name = [System.IO.Path]::GetFileName(([Uri]$Url).AbsolutePath)
if (-not $name) { $name = "download.bin" }
Write-Host "source: $Url"

function Sha([string]$f) { (Get-FileHash -Algorithm SHA256 $f).Hash.ToLower() }

function Measure-Eid([int]$conns, [int]$run) {
  $dir = Join-Path $Out "eid-$conns-$run"
  Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
  New-Item -ItemType Directory $dir | Out-Null
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $eid -ArgumentList @("get", $Url, "-d", $dir, "-c", "$conns", "-q") -NoNewWindow -PassThru
  $null = $p.Handle  # keep the handle so ExitCode is readable after exit
  $peak = 0
  while (-not $p.HasExited) {
    try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch {}
    if ($sw.Elapsed.TotalSeconds -gt $TimeoutSeconds) { $p.Kill(); throw "eid timed out" }
    Start-Sleep -Milliseconds 50
  }
  $sw.Stop()
  if ($p.ExitCode -ne 0) { throw "eid exited with $($p.ExitCode)" }
  $f = Get-ChildItem -File $dir | Sort-Object Length -Descending | Select-Object -First 1
  return [pscustomobject]@{ tool = "eid"; connections = $conns; run = $run; seconds = [math]::Round($sw.Elapsed.TotalSeconds, 3); bytes = $f.Length; sha256 = (Sha $f.FullName); peak_ws_mb = [math]::Round($peak / 1MB, 1) }
}

function Measure-Idm([string]$idm, [int]$conns, [int]$run, [long]$expectBytes) {
  $dir = Join-Path $Out "idm-$conns-$run"
  Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
  New-Item -ItemType Directory $dir | Out-Null
  $target = Join-Path $dir $name
  $sw = [Diagnostics.Stopwatch]::StartNew()
  Start-Process -FilePath $idm -ArgumentList @("/d", $Url, "/p", $dir, "/f", $name, "/n") | Out-Null
  $peak = 0
  while ($true) {
    if ((Test-Path $target) -and ((Get-Item $target).Length -eq $expectBytes)) {
      # IDM moves the finished file in place; wait until it can be opened exclusively
      try { $s = [IO.File]::Open($target, 'Open', 'Read', 'None'); $s.Close(); break } catch {}
    }
    $proc = Get-Process IDMan -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($proc -and $proc.WorkingSet64 -gt $peak) { $peak = $proc.WorkingSet64 }
    if ($sw.Elapsed.TotalSeconds -gt $TimeoutSeconds) { throw "IDM did not finish in $TimeoutSeconds s (is a dialog open in IDM?)" }
    Start-Sleep -Milliseconds 50
  }
  $sw.Stop()
  return [pscustomobject]@{ tool = "idm"; connections = $conns; run = $run; seconds = [math]::Round($sw.Elapsed.TotalSeconds, 3); bytes = (Get-Item $target).Length; sha256 = (Sha $target); peak_ws_mb = [math]::Round($peak / 1MB, 1) }
}

$idm = $null
if (-not $SkipIdm) {
  $idm = Find-Idm
  if (-not $idm) { throw "IDMan.exe not found; pass -IdmPath or use -SkipIdm" }
  Write-Host "IDM: $idm  (set its max connections to match -Connections and disable its speed limiter)"
}

$results = @()
try {
  foreach ($c in $Connections) {
    if ($idm -and $Connections.Count -gt 1) { Read-Host "Set IDM's default max. connections to $c, then press Enter" | Out-Null }
    for ($r = 1; $r -le $Runs; $r++) {
      # alternate the order so neither tool always gets the warmer cache / network moment
      $order = if ($r % 2 -eq 1) { @("eid", "idm") } else { @("idm", "eid") }
      foreach ($tool in $order) {
        if ($tool -eq "eid") { $res = Measure-Eid $c $r }
        elseif ($idm) { $expect = ($results | Where-Object { $_.tool -eq "eid" } | Select-Object -First 1).bytes; if (-not $expect) { $expect = (Measure-Eid $c 0).bytes }; $res = Measure-Idm $idm $c $r $expect }
        else { continue }
        if (-not $expectedSha) { $expectedSha = $res.sha256 }
        if ($res.sha256 -ne $expectedSha) { throw "$($res.tool) produced a different file (sha256 $($res.sha256), expected $expectedSha)" }
        $res | Add-Member mib_s ([math]::Round($res.bytes / 1MB / $res.seconds, 2))
        $results += $res
        Write-Host ("{0,-4} c={1,-3} run {2}: {3,8} s  {4,8} MiB/s  peak {5} MB  sha ok" -f $res.tool, $c, $r, $res.seconds, $res.mib_s, $res.peak_ws_mb)
        Remove-Item -Recurse -Force (Join-Path $Out "$($res.tool)-$c-$r") -ErrorAction SilentlyContinue
      }
    }
  }
} finally {
  if ($server) { try { $server.StandardInput.Close(); if (-not $server.WaitForExit(5000)) { $server.Kill() } } catch {} }
}

$summary = $results | Group-Object tool, connections | ForEach-Object {
  $s = @($_.Group | ForEach-Object { $_.seconds } | Sort-Object)
  $med = $s[[int][math]::Floor(($s.Count - 1) / 2)]
  [pscustomobject]@{ tool = $_.Group[0].tool; connections = $_.Group[0].connections; runs = $s.Count; median_s = $med; median_mib_s = [math]::Round($_.Group[0].bytes / 1MB / $med, 2) }
}
$summary | Format-Table -AutoSize | Out-String | Write-Host
@{ url = $Url; size = $Size; throttle_per_conn = $ThrottlePerConn; machine = $env:COMPUTERNAME; cpus = [Environment]::ProcessorCount; date = (Get-Date).ToString("o"); runs = $results; summary = $summary } |
  ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8 (Join-Path $Out "results.json")
$results | Export-Csv -NoTypeInformation -Encoding utf8 (Join-Path $Out "results.csv")
Write-Host "results: $Out\results.json"
