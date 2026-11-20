# The extension in Brave, Opera and Vivaldi on a Windows runner: installs the browser, finds its
# real executable, then tries each candidate Native Messaging registry key ON ITS OWN (the
# stand-in host is registered under that key only) with check 2 (the menu asks the app through
# the host). The first key that works is then used for all six checks. Prints a table of what
# was tried; fails when no key works or a check fails. Nothing is guessed: a key counts only if
# the browser really reached the host through it.
param([Parameter(Mandatory)][ValidateSet('brave', 'opera', 'vivaldi')][string]$Browser, [string]$Out = 'browser-e2e', [switch]$Real)
$ErrorActionPreference = 'Stop'

$pkg = @{ brave = 'brave'; opera = 'opera'; vivaldi = 'vivaldi' }[$Browser]
choco install $pkg -y --no-progress --execution-timeout 600 | Out-Host
if ($LASTEXITCODE -ne 0) { throw "choco install $pkg failed ($LASTEXITCODE)" }

# The browser itself, not a launcher: Opera's top-level opera.exe starts a versioned copy and
# would break --remote-debugging-pipe, so a copy inside a version folder (1.2.3.4) is preferred.
$exe = @{ brave = 'brave.exe'; opera = 'opera.exe'; vivaldi = 'vivaldi.exe' }[$Browser]
$roots = @($env:ProgramFiles, ${env:ProgramFiles(x86)}, $env:LOCALAPPDATA) | Where-Object { $_ -and (Test-Path $_) }
$found = @($roots | ForEach-Object { Get-ChildItem -Path $_ -Recurse -Filter $exe -ErrorAction SilentlyContinue -Depth 6 } | Select-Object -ExpandProperty FullName)
Write-Host "found: $($found -join '; ')"
$bin = ($found | Where-Object { (Split-Path (Split-Path $_ -Parent) -Leaf) -match '^\d+(\.\d+)+$' } | Select-Object -First 1)
if (-not $bin) { $bin = $found | Select-Object -First 1 }
if (-not $bin) { throw "$exe not found after installing $pkg" }
$ver = (Get-Item $bin).VersionInfo.ProductVersion
Write-Host "$Browser $ver at $bin"

$candidates = @{
  brave   = @('HKCU\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts', 'HKCU\Software\Google\Chrome\NativeMessagingHosts', 'HKCU\Software\Chromium\NativeMessagingHosts')
  opera   = @('HKCU\Software\Opera Software\NativeMessagingHosts', 'HKCU\Software\Google\Chrome\NativeMessagingHosts', 'HKCU\Software\Chromium\NativeMessagingHosts')
  vivaldi = @('HKCU\Software\Vivaldi\NativeMessagingHosts', 'HKCU\Software\Google\Chrome\NativeMessagingHosts', 'HKCU\Software\Chromium\NativeMessagingHosts')
}[$Browser]

$table = @()
$works = $null
$mode = @()
# how the test drives the browser: the DevTools pipe (headless, then a normal window), then a
# DevTools port with --load-extension (a browser that does not serve the pipe)
$modes = @(@(), @('--headed'), @('--cdp-port'), @('--cdp-port', '--headed'))
foreach ($m in $modes) {
  foreach ($k in $candidates) {
    node scripts/ext/browser_e2e.mjs --browser $Browser --bin $bin --nm-key $k --checks 1,2 --max-minutes 3 --out "$Out\probe" @m | Out-Host
    $ok = ($LASTEXITCODE -eq 0)
    $table += [pscustomobject]@{ browser = "$Browser $ver"; mode = $(if ($m.Count) { $m -join ' ' } else { 'pipe headless' }); key = $k; reached_host = $ok }
    if ($ok -and -not $works) { $works = $k; $mode = $m }
  }
  if ($works) { break }
}
$table | Format-Table -AutoSize | Out-Host
New-Item -ItemType Directory -Force $Out | Out-Null
$table | ConvertTo-Json | Set-Content -Encoding utf8 "$Out\nm-keys-$Browser.json"
if (-not $works) { throw "$Browser $ver did not reach the native host through any candidate key" }

Write-Host "NM KEY ($Browser $ver): $works ($(if ($mode.Count) { $mode -join ' ' } else { 'pipe headless' }))"
node scripts/ext/browser_e2e.mjs --browser $Browser --bin $bin --nm-key $works --out $Out @mode | Out-Host
if ($LASTEXITCODE -ne 0) { exit 1 }
# -Real: then the whole chain with the real native host and app, and real files (see browser_e2e)
if ($Real) {
  node scripts/ext/browser_e2e.mjs --browser $Browser --bin $bin --nm-key $works --real --out "$Out\real" @mode | Out-Host
  if ($LASTEXITCODE -ne 0) { exit 1 }
}
