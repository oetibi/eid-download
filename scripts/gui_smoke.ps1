# End-to-end smoke test of the real desktop app on Windows (CI and local):
#   EidDownload.exe opens its window -> `eid add` hands it links over the per-user endpoint ->
#   the app's engine downloads them from the local eid-testserver (one plain file, one whose
#   connection is cut every 32 KiB) -> SHA-256 of each saved file == the served content ->
#   a screenshot of the window is saved -> `eid stop` closes the app.
# Everything runs in a throw-away profile (USERPROFILE/APPDATA/LOCALAPPDATA) on its own endpoint,
# so a real Eid Download on the same account is never touched. No internet.
param(
  [string]$Bin = "target\release",
  [string]$Out = "gui-smoke"
)
$ErrorActionPreference = "Stop"
$Bin = (Resolve-Path $Bin).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$root = Join-Path ([IO.Path]::GetTempPath()) ("eid-gui-smoke-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
$profileDir = Join-Path $root "profile"
foreach ($d in "$profileDir", "$profileDir\AppData\Roaming", "$profileDir\AppData\Local", "$profileDir\Downloads") {
  New-Item -ItemType Directory -Force $d | Out-Null
}
$env:USERPROFILE = $profileDir
$env:APPDATA = "$profileDir\AppData\Roaming"
$env:LOCALAPPDATA = "$profileDir\AppData\Local"
$endpoint = "smoke" + [guid]::NewGuid().ToString("N").Substring(0, 8)
$eid = Join-Path $Bin "eid.exe"
$gui = Join-Path $Bin "EidDownload.exe"
$srv = Join-Path $Bin "eid-testserver.exe"
$fail = $null
$server = $null
$app = $null

function Eid([string[]]$a) {
  # an expected failure writes to stderr; that must not abort the script
  $ErrorActionPreference = "Continue"
  $o =& $eid @a --endpoint $endpoint 2>&1 | Out-String
  return @{ code = $LASTEXITCODE; text = $o }
}
function WaitFor([scriptblock]$cond, [int]$seconds, [string]$what) {
  $deadline = (Get-Date).AddSeconds($seconds)
  while ((Get-Date) -lt $deadline) {
    if (& $cond) { return }
    Start-Sleep -Milliseconds 250
  }
  throw "timed out after ${seconds}s waiting for: $what"
}

try {
  # 1. the bad test server (prints its base URL on stdout)
  $srvOut = Join-Path $root "server.txt"
  $server = Start-Process -FilePath $srv -RedirectStandardOutput $srvOut -PassThru -WindowStyle Hidden
  WaitFor { (Test-Path $srvOut) -and ((Get-Content $srvOut -Raw) -match "http://") } 30 "test server URL"
  $base = (Get-Content $srvOut -Raw).Trim()
  "test server: $base"
  $expected = (Get-FileHash -Algorithm SHA256 -InputStream ([IO.MemoryStream]::new((Invoke-WebRequest -UseBasicParsing "$base/demo.bin").Content))).Hash.ToLower()
  "expected SHA-256: $expected"

  # 2. nothing answers on our endpoint yet
  $r = Eid @("list")
  if ($r.code -eq 0) { throw "an app already answers on endpoint $endpoint" }

  # 3. the app: process up, endpoint up, window with its title
  $app = Start-Process -FilePath $gui -ArgumentList @("--endpoint", $endpoint) -PassThru
  WaitFor { (Eid @("list")).code -eq 0 } 60 "the app's endpoint"
  "endpoint answers"
  WaitFor { $app.Refresh(); $app.MainWindowHandle -ne 0 -and $app.MainWindowTitle -like "Eid Download*" } 60 "the app window"
  "window: '$($app.MainWindowTitle)' (pid $($app.Id))"

  # 4. a second launch must not open a second owner of the queue: it asks the open window to
  #    come forward and exits by itself (code 0, no dialog); the first one keeps running
  $second = Start-Process -FilePath $gui -ArgumentList @("--endpoint", $endpoint) -PassThru
  if (-not $second.WaitForExit(20000)) { Stop-Process -Id $second.Id -Force; throw "the second launch did not exit by itself (a dialog?)" }
  if ($second.ExitCode -ne 0) { throw "the second launch exited with $($second.ExitCode)" }
  if ($app.HasExited) { throw "the first window closed when a second one was started" }
  if ((Eid @("list")).code -ne 0) { throw "the endpoint stopped answering after a second launch" }
  "second launch: brought the open window forward and exited (code 0); first instance kept"

  # 5. downloads through the app's engine
  foreach ($p in "demo.bin", "cuts.bin") {
    $r = Eid @("add", "$base/$p", "--name", "smoke-$p")
    if ($r.code -ne 0) { throw "eid add $p failed: $($r.text)" }
    $r.text.Trim()
  }
  WaitFor { ((Eid @("list")).text -split "`n" | Where-Object { $_ -match "/(demo|cuts)\.bin" -and $_ -match " done " }).Count -eq 2 } 120 "both downloads done"
  (Eid @("list")).text.Trim()
  foreach ($p in "demo.bin", "cuts.bin") {
    $f = Join-Path "$profileDir\Downloads" "smoke-$p"
    $h = (Get-FileHash -Algorithm SHA256 $f).Hash.ToLower()
    if ($h -ne $expected) { throw "$p SHA-256 $h != $expected" }
    "$p SHA-256 OK"
  }

  # screenshots of the real window (the page polls the list every 0.7 s)
  Add-Type -AssemblyName System.Drawing
  Add-Type @"
using System; using System.Runtime.InteropServices;
public static class W {
  [StructLayout(LayoutKind.Sequential)] public struct R { public int L, T, Rt, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
  [W]::SetProcessDPIAware() | Out-Null
  function Shot([string]$name) {
    $app.Refresh()
    $h = $app.MainWindowHandle
    [W]::SetForegroundWindow($h) | Out-Null
    $rect = New-Object W+R
    [W]::GetWindowRect($h, [ref]$rect) | Out-Null
    $w = $rect.Rt - $rect.L; $hh = $rect.B - $rect.T
    $bmp = New-Object System.Drawing.Bitmap $w, $hh
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $dc = $g.GetHdc()
    [W]::PrintWindow($h, $dc, 2) | Out-Null   # PW_RENDERFULLCONTENT (WebView2 content)
    $g.ReleaseHdc($dc); $g.Dispose()
    $shot = Join-Path $Out $name
    $bmp.Save($shot, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
    "screenshot: $shot (${w}x${hh})"
  }
  function Status([string]$id) {
    $line = (Eid @("list")).text -split "`n" | Where-Object { $_ -match "^#$id " }
    if ($line -match "^#$id (\w+)") { return $Matches[1] }
    return ""
  }

  # 6. a throttled download: progress and speed on screen, pause, resume from where it stopped
  $r = Eid @("add", "$base/slow.bin", "--name", "smoke-slow.bin")
  if ($r.code -ne 0) { throw "eid add slow.bin failed: $($r.text)" }
  $slow = [regex]::Match($r.text, "#(\d+)").Groups[1].Value
  "slow download is #$slow"
  WaitFor { (Status $slow) -eq "running" } 30 "#$slow running"
  Start-Sleep -Seconds 6   # the engine writes in 128 KiB blocks: the bar moves after the first one
  Shot "gui-running.png"
  $r = Eid @("pause", $slow)
  if ($r.code -ne 0) { throw "eid pause failed: $($r.text)" }
  WaitFor { (Status $slow) -eq "paused" } 30 "#$slow paused"
  "paused"
  Start-Sleep -Milliseconds 1500
  Shot "gui-paused.png"
  $r = Eid @("resume", $slow)
  if ($r.code -ne 0) { throw "eid resume failed: $($r.text)" }
  WaitFor { (Status $slow) -eq "done" } 180 "#$slow done after resume"
  $h = (Get-FileHash -Algorithm SHA256 (Join-Path "$profileDir\Downloads" "smoke-slow.bin")).Hash.ToLower()
  if ($h -ne $expected) { throw "slow.bin SHA-256 $h != $expected" }
  "slow.bin paused, resumed, SHA-256 OK"
  Start-Sleep -Seconds 2
  Shot "gui-window.png"

  # 7. closing the window in the middle of a download, then opening the app again: it resumes
  $r = Eid @("add", "$base/slow.bin", "--name", "smoke-close.bin")
  if ($r.code -ne 0) { throw "eid add (close test) failed: $($r.text)" }
  $closeId = [regex]::Match($r.text, "#(\d+)").Groups[1].Value
  WaitFor { (Status $closeId) -eq "running" } 30 "#$closeId running"
  Start-Sleep -Seconds 4
  if (-not $app.CloseMainWindow()) { throw "could not send close to the window" }
  if (-not $app.WaitForExit(20000)) { throw "the app did not exit after its window was closed" }
  "window closed mid-download, app exited with code $($app.ExitCode)"
  $app = Start-Process -FilePath $gui -ArgumentList @("--endpoint", $endpoint) -PassThru
  WaitFor { (Eid @("list")).code -eq 0 } 60 "the app's endpoint after restart"
  WaitFor { (Status $closeId) -eq "done" } 180 "#$closeId done after restart"
  $h = (Get-FileHash -Algorithm SHA256 (Join-Path "$profileDir\Downloads" "smoke-close.bin")).Hash.ToLower()
  if ($h -ne $expected) { throw "close-test SHA-256 $h != $expected" }
  "resumed after closing the window and reopening, SHA-256 OK"
  WaitFor { $app.Refresh(); $app.MainWindowHandle -ne 0 } 60 "the app window after restart"

  # 8. `eid stop` closes the app cleanly
  $r = Eid @("stop")
  if ($r.code -ne 0) { throw "eid stop failed: $($r.text)" }
  if (-not $app.WaitForExit(20000)) { throw "the app did not exit after eid stop" }
  "app exited with code $($app.ExitCode)"
  $q = Get-Content "$profileDir\AppData\Roaming\EidDownload\queues.json" -Raw
  if ($q -notmatch "smoke-cuts.bin") { throw "queue file lost the items" }
  "queue file kept the items"

  # 9. without a usable WebView2 the app must not vanish silently: WebView2's own notice, then
  #    ours (title "Eid Download"), then a clean exit with code 1 and the queue file intact
  $env:WEBVIEW2_BROWSER_EXECUTABLE_FOLDER = Join-Path $root "no-webview2"
  $bad = Start-Process -FilePath $gui -ArgumentList @("--endpoint", $endpoint) -PassThru
  Remove-Item Env:WEBVIEW2_BROWSER_EXECUTABLE_FOLDER
  $titles = @()
  for ($k = 0; $k -lt 6 -and -not $bad.HasExited; $k++) {
    WaitFor { $bad.Refresh(); $bad.HasExited -or $bad.MainWindowHandle -ne 0 } 30 "a notice from the app without WebView2"
    if ($bad.HasExited) { break }
    $titles += $bad.MainWindowTitle
    $bad.CloseMainWindow() | Out-Null
    $null = $bad.WaitForExit(3000)
  }
  if (-not $bad.HasExited) { Stop-Process -Id $bad.Id -Force; throw "the app without WebView2 did not exit (notices: $($titles -join ', '))" }
  "without WebView2: notices '$($titles -join "', '")', exit code $($bad.ExitCode)"
  if ($titles -notcontains "Eid Download") { throw "no Eid Download notice without WebView2" }
  if ($bad.ExitCode -ne 1) { throw "exit code $($bad.ExitCode) without WebView2, expected 1" }
  $q = Get-Content "$profileDir\AppData\Roaming\EidDownload\queues.json" -Raw
  if ($q -notmatch "smoke-cuts.bin") { throw "queue file lost the items after the WebView2 failure" }
  "GUI SMOKE: PASSED"
} catch {
  $fail = $_
  "GUI SMOKE: FAILED - $fail"
} finally {
  if ($app -and -not $app.HasExited) { Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue }
  if ($server -and -not $server.HasExited) { Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue }
  Get-Process EidDownload -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Bin*" } | Stop-Process -Force -ErrorAction SilentlyContinue
  Start-Sleep -Seconds 1
  Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}
if ($fail) { exit 1 }
