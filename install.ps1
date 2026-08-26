# Install the Sightline commands from the latest GitHub release.
#   irm https://raw.githubusercontent.com/nyfeblade/sightline/master/install.ps1 | iex
# Or read this file first and run the commands yourself — it is short on purpose.
#
# The desktop app is a separate download: an MSI built with WiX 3 (that is what
# Tauri 2 actually produces). This script installs `sightline.exe`. It has never
# contained `sightline-gui`; v0.4.1's zip was named `ironsight-*` and held only
# the CLI.
$ErrorActionPreference = 'Stop'

$repo = 'nyfeblade/sightline'
$dest = if ($env:SIGHTLINE_INSTALL_DIR) { $env:SIGHTLINE_INSTALL_DIR } else { "$env:LOCALAPPDATA\Programs\Sightline" }

$rel = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest"
if (-not $rel.tag_name) { throw 'could not find the latest release' }
$tag = $rel.tag_name

$msi = @($rel.assets | Where-Object { $_.name -like '*.msi' }) | Select-Object -First 1
$zip = @($rel.assets | Where-Object { $_.name -like '*-x86_64-pc-windows-msvc.zip' }) | Select-Object -First 1
if (-not $zip) {
    throw "no Windows CLI zip on $tag (looked for *-x86_64-pc-windows-msvc.zip). v0.4.1 shipped ironsight-v0.4.1-*; constructing sightline-$tag-*.zip 404s against that."
}

$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP ([guid]::NewGuid()))
try {
    Write-Host "downloading Sightline $tag ($($zip.name))"
    $zipPath = Join-Path $tmp $zip.name
    Invoke-WebRequest $zip.browser_download_url -OutFile $zipPath
    Expand-Archive $zipPath -DestinationPath $tmp
    $exe = Get-ChildItem -Path $tmp -Recurse -File -Include sightline.exe, ironsight.exe, scope.exe |
        Select-Object -First 1
    if (-not $exe) { throw "the zip $($zip.name) did not contain a Sightline executable" }
    New-Item -ItemType Directory -Path $dest -Force | Out-Null
    Copy-Item $exe.FullName (Join-Path $dest 'sightline.exe') -Force
} finally {
    Remove-Item $tmp -Recurse -Force
}

Write-Host "installed $dest\sightline.exe"
if ($msi) {
    Write-Host "the desktop app is a separate download:"
    Write-Host "  $($msi.browser_download_url)"
} else {
    Write-Host "the desktop app is an MSI, not this zip. Build it on Windows with WiX 3:"
    Write-Host "  cargo tauri build --features custom-protocol --bundles msi"
    Write-Host "  (from crates/gui; Tauri 2 MSI is WiX 3 — WiX 4 will not do)"
}
if (($env:PATH -split ';') -notcontains $dest) {
    Write-Host "note: $dest is not on your PATH. To add it for next time:"
    Write-Host "  [Environment]::SetEnvironmentVariable('PATH', `"`$env:PATH;$dest`", 'User')"
}
