# Startup time and memory of EidDownload.exe (Windows). Each run: a fresh throw-away profile and
# private endpoint -> start the process -> time until its main window handle exists with the
# "Eid Download" title -> idle for -IdleSeconds -> sample memory -> `eid stop` -> wait for exit.
#
# Reported per run:
#   window_ms        Start-Process -> MainWindowHandle with title (window created; the page inside
#                    WebView2 may still be painting: this is NOT "first paint")
#   app_ws_mb        WorkingSet64 of EidDownload.exe alone
#   app_private_mb   PrivateMemorySize64 of EidDownload.exe alone
#   tree_ws_mb       WorkingSet64 summed over EidDownload.exe + all descendants (the WebView2
#   tree_private_mb  msedgewebview2.exe processes hold most of the memory), same for private
#   tree_procs       number of processes in that tree
# Run 1 is the coldest (WebView2 data folder created for the first time in that profile; OS file
# cache may be cold). Numbers are from whatever machine runs this; on CI they are GitHub runner
# numbers, not a user's PC.
param(
  [string]$Bin = "target\release",
  [string]$Out = "perf-windows",
  [int]$Runs = 5,
  [int]$IdleSeconds = 10
)
$ErrorActionPreference = "Stop"
$Bin = (Resolve-Path $Bin).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$gui = Join-Path $Bin "EidDownload.exe"
$eid = Join-Path $Bin "eid.exe"
$saved = @{}
foreach ($k in "USERPROFILE", "APPDATA", "LOCALAPPDATA") { $saved[$k] = [Environment]::GetEnvironmentVariable($k) }

function Tree([int]$rootPid) {
  $all = @(Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId)
  $ids = [Collections.Generic.List[int]]::new()
  $ids.Add($rootPid)
  for ($i = 0; $i -lt $ids.Count; $i++) {
    foreach ($c in $all) { if ($c.ParentProcessId -eq $ids[$i] -and -not $ids.Contains([int]$c.ProcessId)) { $ids.Add([int]$c.ProcessId) } }
  }
  return @($ids | ForEach-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue } | Where-Object { $_ })
}
function MB([long]$b) { [math]::Round($b / 1MB, 1) }

$rows = @()
try {
  for ($r = 1; $r -le $Runs; $r++) {
    $profileRoot = Join-Path ([IO.Path]::GetTempPath()) ("eid-perf-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
    foreach ($d in "$profileRoot", "$profileRoot\AppData\Roaming", "$profileRoot\AppData\Local", "$profileRoot\Downloads") {
      New-Item -ItemType Directory -Force $d | Out-Null
    }
    $env:USERPROFILE = $profileRoot
    $env:APPDATA = "$profileRoot\AppData\Roaming"
    $env:LOCALAPPDATA = "$profileRoot\AppData\Local"
    $endpoint = "perf" + [guid]::NewGuid().ToString("N").Substring(0, 8)
    $app = $null
    try {
      $sw = [Diagnostics.Stopwatch]::StartNew()
      $app = Start-Process -FilePath $gui -ArgumentList @("--endpoint", $endpoint) -PassThru
      while ($true) {
        $app.Refresh()
        if ($app.HasExited) { throw "run ${r}: the app exited with $($app.ExitCode) before showing its window" }
        if ($app.MainWindowHandle -ne 0 -and $app.MainWindowTitle -like "Eid Download*") { break }
        if ($sw.ElapsedMilliseconds -gt 60000) { throw "run ${r}: no window after 60 s" }
        Start-Sleep -Milliseconds 10
      }
      $windowMs = $sw.ElapsedMilliseconds
      Start-Sleep -Seconds $IdleSeconds
      $app.Refresh()
      $tree = Tree $app.Id
      $row = [pscustomobject]@{
        run             = $r
        window_ms       = $windowMs
        app_ws_mb       = MB $app.WorkingSet64
        app_private_mb  = MB $app.PrivateMemorySize64
        tree_ws_mb      = MB (($tree | Measure-Object WorkingSet64 -Sum).Sum)
        tree_private_mb = MB (($tree | Measure-Object PrivateMemorySize64 -Sum).Sum)
        tree_procs      = $tree.Count
      }
      $rows += $row
      $row | Format-List | Out-Host
      $ErrorActionPreference = "Continue"
      & $eid stop --endpoint $endpoint 2>&1 | Out-Host
      $ErrorActionPreference = "Stop"
      if (-not $app.WaitForExit(20000)) { throw "run ${r}: the app did not exit after eid stop" }
    } finally {
      if ($app -and -not $app.HasExited) { Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue }
      foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
      Start-Sleep -Milliseconds 500   # WebView2 helpers release the profile folder
      Remove-Item -Recurse -Force $profileRoot -ErrorAction SilentlyContinue
    }
  }
} finally {
  foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
}

function Median($xs) { $s = @($xs | Sort-Object); $n = $s.Count; if ($n % 2) { $s[($n - 1) / 2] } else { ($s[$n / 2 - 1] + $s[$n / 2]) / 2 } }
$os = Get-CimInstance Win32_OperatingSystem
$cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name
$sha = (Get-FileHash $gui -Algorithm SHA256).Hash.ToLower()
$md = @()
$md += "# EidDownload.exe startup and memory ($Runs runs, $IdleSeconds s idle before sampling)"
$md += ""
$md += "Machine: $($os.Caption) $($os.Version), $cpu, $env:NUMBER_OF_PROCESSORS logical CPUs, $([math]::Round($os.TotalVisibleMemorySize / 1MB, 1)) GB RAM."
if ($env:GITHUB_ACTIONS) { $md += "GitHub Actions runner ($env:RUNNER_NAME, run $env:GITHUB_RUN_ID, commit $env:GITHUB_SHA): runner numbers, not a user's PC." }
$md += "Binary: $gui (SHA-256 $sha). Command: scripts/windows/perf_startup.ps1 -Runs $Runs -IdleSeconds $IdleSeconds"
$md += ""
$md += "| run | window_ms | app_ws_mb | app_private_mb | tree_ws_mb | tree_private_mb | tree_procs |"
$md += "|---|---|---|---|---|---|---|"
foreach ($x in $rows) { $md += "| $($x.run) | $($x.window_ms) | $($x.app_ws_mb) | $($x.app_private_mb) | $($x.tree_ws_mb) | $($x.tree_private_mb) | $($x.tree_procs) |" }
$md += "| median | $(Median $rows.window_ms) | $(Median $rows.app_ws_mb) | $(Median $rows.app_private_mb) | $(Median $rows.tree_ws_mb) | $(Median $rows.tree_private_mb) | $(Median $rows.tree_procs) |"
$md += ""
$md += "window_ms = start -> main window handle with title (not first paint). tree = EidDownload.exe + all descendants (WebView2)."
$md | Set-Content -Encoding utf8 (Join-Path $Out "perf-windows.md")
$rows | Export-Csv -NoTypeInformation -Encoding utf8 (Join-Path $Out "perf-windows.csv")
$md | Out-Host
if ($env:GITHUB_STEP_SUMMARY) { $md | Add-Content -Encoding utf8 $env:GITHUB_STEP_SUMMARY }
