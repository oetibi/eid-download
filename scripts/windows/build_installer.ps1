# Build the NSIS installer (ADR-0005) from binaries that are ALREADY built and tested
# (target\release\EidDownload.exe, eid.exe, eid-native-host.exe): `cargo tauri bundle`, not
# `cargo tauri build`, so nothing is recompiled, and `--no-binary-patching`, so the installed
# EidDownload.exe is byte-identical to the one the GUI smoke ran (verify_installer.ps1 checks
# the SHA-256 of every installed binary against target\release).
#
# Needs: the Tauri CLI (`cargo tauri`, pinned in ci-windows.yml), python, and
# THIRD-PARTY-LICENSES.html at the repository root (cargo about, same as the ZIP).
#   -Release   refuse placeholder extension IDs (tag builds)
# Output: dist\EidDownload-<version>-windows-x64-setup.exe (path printed on the last line).
param(
  [switch]$Release
)
$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
Set-Location $root
$conf = Get-Content crates\eid-gui\tauri.conf.json -Raw | ConvertFrom-Json
$version = $conf.version
if (-not $version) { throw "no version in tauri.conf.json" }

$stage = Join-Path $root "target\installer-stage"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null

foreach ($f in "EidDownload.exe", "eid.exe", "eid-native-host.exe") {
  if (-not (Test-Path "target\release\$f")) { throw "target\release\$f is missing (build it first)" }
}
Copy-Item target\release\eid.exe, target\release\eid-native-host.exe $stage
Copy-Item crates\eid-gui\README-WINDOWS.txt (Join-Path $stage "README.txt")
Copy-Item LICENSE (Join-Path $stage "LICENSE.txt")
if (-not (Test-Path THIRD-PARTY-LICENSES.html)) { throw "THIRD-PARTY-LICENSES.html is missing (cargo about generate)" }
Copy-Item THIRD-PARTY-LICENSES.html $stage
# v0.2: the unpacked browser extension and its instructions (built by scripts/ext/build.mjs
# --out target\ext in ci-windows.yml)
Copy-Item crates\eid-gui\INSTALL-BROWSER-EXTENSION.txt $stage
$extDirs = [ordered]@{ chrome = "chrome-edge"; firefox = "firefox" }
foreach ($t in $extDirs.GetEnumerator()) {
  if (-not (Test-Path "target\ext\$($t.Key)\manifest.json")) { throw "target\ext\$($t.Key) is missing (node scripts/ext/build.mjs --out target\ext)" }
}

# Native Messaging manifests from the single ID file
$nmArgs = @("scripts\windows\native_host_manifests.py", "extension\native-host-ids.json", $stage)
if ($Release) { $nmArgs += "--release" }
& python @nmArgs
if ($LASTEXITCODE -ne 0) { throw "native_host_manifests.py failed" }

# Merge-config for the CLI only: `cargo build` never sees these resources (they exist only here).
# Resource paths are relative to crates\eid-gui (the CLI runs there); all go to the install root.
$res = [ordered]@{}
Get-ChildItem -File $stage | Sort-Object Name | ForEach-Object {
  $res["../../target/installer-stage/$($_.Name)"] = $_.Name
}
# every file of the extension, one by one (exact install paths, removed again by the uninstaller)
foreach ($t in $extDirs.GetEnumerator()) {
  $src = (Resolve-Path "target\ext\$($t.Key)").Path
  Get-ChildItem -File -Recurse $src | Sort-Object FullName | ForEach-Object {
    $rel = $_.FullName.Substring($src.Length + 1).Replace('\', '/')
    $res["../../target/ext/$($t.Key)/$rel"] = "browser-extension/$($t.Value)/$rel"
  }
}
$overlay = [ordered]@{ bundle = [ordered]@{ active = $true; resources = $res } }
$overlayPath = Join-Path $root "target\tauri.installer.conf.json"
[IO.File]::WriteAllText($overlayPath, ($overlay | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))  # no BOM
"installer resources:"; $res.Values | ForEach-Object { "  $_" }

Push-Location crates\eid-gui
try {
  & cargo tauri bundle --bundles nsis --no-binary-patching --config $overlayPath
  if ($LASTEXITCODE -ne 0) { throw "cargo tauri bundle failed ($LASTEXITCODE)" }
} finally { Pop-Location }

$built = @(Get-ChildItem "target\release\bundle\nsis\*_${version}_x64-setup.exe")
if ($built.Count -ne 1) { throw "expected one NSIS installer for $version, found $($built.Count): $($built.Name -join ', ')" }
New-Item -ItemType Directory -Force dist | Out-Null
$out = "dist\EidDownload-$version-windows-x64-setup.exe"
Copy-Item $built[0].FullName $out -Force
"installer: $out ($((Get-Item $out).Length) bytes)"
