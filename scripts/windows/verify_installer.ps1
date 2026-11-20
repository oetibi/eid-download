# Verify the NSIS installer for real on Windows (CI runner, or a test account by hand):
#   silent per-user install (/S, no admin) -> files in %LOCALAPPDATA%\Eid Download, byte-identical
#   to target\release -> HKCU Native Messaging keys point at manifests that point at the installed
#   eid-native-host.exe -> start-menu shortcut + uninstall entry -> the installed EidDownload.exe
#   opens its window (throw-away profile, private endpoint) and the installed eid.exe stops it ->
#   the installed eid-native-host.exe starts and exits on EOF -> silent uninstall -> everything
#   gone, and a Native Messaging key that points elsewhere (another copy) is left alone.
# Refuses to run if Eid Download is already installed for this user (it would be overwritten).
param(
  [Parameter(Mandatory = $true)][string]$Installer,
  [string]$Bin = "target\release",
  [string]$IdsFile = "extension\native-host-ids.json"
)
$ErrorActionPreference = "Stop"
$Installer = (Resolve-Path $Installer).Path
$Bin = (Resolve-Path $Bin).Path
$ids = Get-Content $IdsFile -Raw | ConvertFrom-Json
$hostName = $ids.host_name
$inst = Join-Path $env:LOCALAPPDATA "Eid Download"
$uninstKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Eid Download"
$startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Eid Download\Eid Download.lnk"
$nm = [ordered]@{
  chrome  = "HKCU:\Software\Google\Chrome\NativeMessagingHosts\$hostName"
  edge    = "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\$hostName"
  firefox = "HKCU:\Software\Mozilla\NativeMessagingHosts\$hostName"
}
# which browsers the build registered: the same rule as native_host_manifests.py
$expect = @{
  chrome  = @($ids.chrome).Count -gt 0
  edge    = (@($ids.edge).Count + @($ids.chrome).Count) -gt 0
  firefox = @($ids.firefox).Count -gt 0
}
function Check([bool]$ok, [string]$what) {
  if (-not $ok) { throw "FAILED: $what" }
  "ok: $what"
}
function KeyValue([string]$k) {
  if (-not (Test-Path $k)) { return $null }
  return (Get-Item $k).GetValue("")
}
function Sha([string]$p) { (Get-FileHash $p -Algorithm SHA256).Hash.ToLower() }
function WaitFor([scriptblock]$cond, [int]$seconds, [string]$what) {
  $deadline = (Get-Date).AddSeconds($seconds)
  while ((Get-Date) -lt $deadline) {
    if (& $cond) { return }
    Start-Sleep -Milliseconds 250
  }
  throw "timed out after ${seconds}s waiting for: $what"
}

Check (-not (Test-Path $inst) -and -not (Test-Path $uninstKey)) "Eid Download is not installed for this user yet ($inst)"
foreach ($b in $nm.Keys) { Check (-not (Test-Path $nm[$b])) "no $b Native Messaging key before install" }

$profileRoot = $null
$app = $null
$saved = @{}
try {
  # ---- 1. silent per-user install
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $Installer -ArgumentList "/S" -PassThru -Wait
  "install: exit code $($p.ExitCode) in $($sw.ElapsedMilliseconds) ms"
  Check ($p.ExitCode -eq 0) "installer exit code 0"
  Get-Process EidDownload -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$inst*" } | ForEach-Object {
    throw "the silent install started the app (pid $($_.Id)); it must not without /R"
  }

  # ---- 2. files, byte-identical to the tested build
  foreach ($f in "EidDownload.exe", "eid.exe", "eid-native-host.exe") {
    $ip = Join-Path $inst $f
    Check (Test-Path $ip) "$f installed in $inst"
    Check ((Sha $ip) -eq (Sha (Join-Path $Bin $f))) "$f identical (SHA-256) to $Bin\$f"
  }
  foreach ($f in "uninstall.exe", "LICENSE.txt", "THIRD-PARTY-LICENSES.html", "README.txt", "INSTALL-BROWSER-EXTENSION.txt", "browser-extension\chrome-edge\manifest.json", "browser-extension\chrome-edge\background.js", "browser-extension\firefox\manifest.json", "browser-extension\firefox\background.js") {
    Check (Test-Path (Join-Path $inst $f)) "$f installed"
  }
  Check (Test-Path $startMenu) "start-menu shortcut $startMenu"
  Check (Test-Path (Join-Path ([Environment]::GetFolderPath("Desktop")) "Eid Download.lnk")) "desktop shortcut (silent install creates it)"
  $u = Get-Item $uninstKey
  Check ($u.GetValue("DisplayVersion") -eq (Get-Content crates\eid-gui\tauri.conf.json -Raw | ConvertFrom-Json).version) "uninstall entry DisplayVersion = tauri.conf.json version ($($u.GetValue('DisplayVersion')))"
  "uninstall entry: $($u.GetValue('DisplayName')) / $($u.GetValue('UninstallString'))"

  # ---- 3. Native Messaging registration
  foreach ($b in $nm.Keys) {
    $mf = Join-Path $inst "$hostName.$b.json"
    $v = KeyValue $nm[$b]
    if (-not $expect[$b]) {
      Check ($null -eq $v -and -not (Test-Path $mf)) "$b not registered (no extension id configured)"
      continue
    }
    Check ($v -eq $mf) "$b key $($nm[$b]) = $v"
    $m = Get-Content $mf -Raw | ConvertFrom-Json
    Check ($m.name -eq $hostName -and $m.type -eq "stdio") "$b manifest name/type"
    $exe = Join-Path (Split-Path $mf) $m.path   # relative to the manifest (Windows rule)
    Check ((Test-Path $exe) -and ((Resolve-Path $exe).Path -eq (Join-Path $inst "eid-native-host.exe"))) "$b manifest path resolves to the installed eid-native-host.exe"
    if ($b -eq "firefox") {
      Check (@($m.allowed_extensions).Count -gt 0 -and -not $m.allowed_origins) "firefox manifest has allowed_extensions only"
    } else {
      Check (@($m.allowed_origins).Count -gt 0 -and -not $m.allowed_extensions) "$b manifest has allowed_origins only"
      foreach ($o in $m.allowed_origins) { Check ($o -match '^chrome-extension://[a-p]{32}/$') "$b origin $o" }
    }
  }

  # ---- 4. the installed host starts (no missing DLL) and exits 0 on EOF
  $tmp = [IO.Path]::GetTempPath()
  $emptyIn = Join-Path $tmp "eid-nh-empty-stdin.txt"
  [IO.File]::WriteAllBytes($emptyIn, [byte[]]@())
  $hp = Start-Process -FilePath (Join-Path $inst "eid-native-host.exe") -RedirectStandardInput $emptyIn `
    -RedirectStandardOutput (Join-Path $tmp "eid-nh-out.txt") -RedirectStandardError (Join-Path $tmp "eid-nh-err.txt") -PassThru -WindowStyle Hidden
  if (-not $hp.WaitForExit(15000)) { Stop-Process -Id $hp.Id -Force; throw "FAILED: eid-native-host.exe did not exit on EOF" }
  Check ($hp.ExitCode -eq 0) "installed eid-native-host.exe exits 0 on EOF (got $($hp.ExitCode))"

  # ---- 5. the installed app opens its window; the installed eid.exe stops it
  #      (throw-away profile and private endpoint: no real queue is touched)
  $profileRoot = Join-Path ([IO.Path]::GetTempPath()) ("eid-inst-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
  foreach ($d in "$profileRoot", "$profileRoot\AppData\Roaming", "$profileRoot\AppData\Local", "$profileRoot\Downloads") {
    New-Item -ItemType Directory -Force $d | Out-Null
  }
  foreach ($k in "USERPROFILE", "APPDATA", "LOCALAPPDATA") { $saved[$k] = [Environment]::GetEnvironmentVariable($k) }
  $env:USERPROFILE = $profileRoot
  $env:APPDATA = "$profileRoot\AppData\Roaming"
  $env:LOCALAPPDATA = "$profileRoot\AppData\Local"
  $endpoint = "inst" + [guid]::NewGuid().ToString("N").Substring(0, 8)
  $eid = Join-Path $inst "eid.exe"
  $app = Start-Process -FilePath (Join-Path $inst "EidDownload.exe") -ArgumentList @("--endpoint", $endpoint) -PassThru
  WaitFor { $app.Refresh(); $app.HasExited -or ($app.MainWindowHandle -ne 0 -and $app.MainWindowTitle -like "Eid Download*") } 60 "the installed app's window"
  Check (-not $app.HasExited) "installed EidDownload.exe opened '$($app.MainWindowTitle)' (pid $($app.Id))"
  $ErrorActionPreference = "Continue"
  WaitFor { & $eid list --endpoint $endpoint 2>&1 | Out-Null; $LASTEXITCODE -eq 0 } 60 "the installed app's endpoint"
  & $eid stop --endpoint $endpoint 2>&1 | Out-Host
  $stopCode = $LASTEXITCODE
  $ErrorActionPreference = "Stop"
  Check ($stopCode -eq 0) "installed eid.exe stop"
  Check ($app.WaitForExit(20000)) "installed app exited after eid stop (code $($app.ExitCode))"
  foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
  $saved = @{}

  # ---- 6. a key that points at another copy must survive our uninstall
  $foreign = "C:\Elsewhere\$hostName.firefox.json"
  if ($expect.firefox) {
    Set-Item -Path $nm.firefox -Value $foreign
  }

  # ---- 7. silent uninstall (the NSIS uninstaller re-launches itself from %TEMP%: wait for the result)
  $un = Start-Process -FilePath (Join-Path $inst "uninstall.exe") -ArgumentList "/S" -PassThru -Wait
  "uninstall: exit code $($un.ExitCode)"
  Check ($un.ExitCode -eq 0) "uninstaller exit code 0"
  WaitFor { -not (Test-Path (Join-Path $inst "EidDownload.exe")) -and -not (Test-Path $uninstKey) } 120 "uninstall to finish"
  foreach ($f in "EidDownload.exe", "eid.exe", "eid-native-host.exe", "uninstall.exe", "LICENSE.txt", "THIRD-PARTY-LICENSES.html", "README.txt", "$hostName.chrome.json", "$hostName.edge.json", "$hostName.firefox.json") {
    Check (-not (Test-Path (Join-Path $inst $f))) "$f removed"
  }
  Check (-not (Test-Path $inst) -or @(Get-ChildItem -Force $inst).Count -eq 0) "install directory removed or empty"
  Check (-not (Test-Path $startMenu)) "start-menu shortcut removed"
  Check (-not (Test-Path (Join-Path ([Environment]::GetFolderPath("Desktop")) "Eid Download.lnk"))) "desktop shortcut removed"
  Check (-not (Test-Path $nm.chrome) -and -not (Test-Path $nm.edge)) "chrome and edge Native Messaging keys removed"
  if ($expect.firefox) {
    Check ((KeyValue $nm.firefox) -eq $foreign) "a firefox key pointing at another copy is left alone"
    Remove-Item -Path $nm.firefox
  }
  Check (-not (Test-Path $nm.firefox)) "no firefox key left"
  "INSTALLER VERIFIED"
} finally {
  foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
  if ($app -and -not $app.HasExited) { Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue }
  if ($profileRoot) { Remove-Item -Recurse -Force $profileRoot -ErrorAction SilentlyContinue }
}
