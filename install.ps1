# Install scope from the latest GitHub release.
#   irm https://raw.githubusercontent.com/nyfeblade/nyfe-scope/main/install.ps1 | iex
# Or read this file first and run the four commands yourself — it is short on
# purpose.
$ErrorActionPreference = 'Stop'

$repo = 'nyfeblade/nyfe-scope'
$dest = if ($env:SCOPE_INSTALL_DIR) { $env:SCOPE_INSTALL_DIR } else { "$env:LOCALAPPDATA\Programs\scope" }

$tag = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
if (-not $tag) { throw 'could not find the latest release' }

$name = "scope-$tag-x86_64-pc-windows-msvc"
$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP ([guid]::NewGuid()))
try {
    Write-Host "downloading scope $tag"
    $zip = Join-Path $tmp "$name.zip"
    Invoke-WebRequest "https://github.com/$repo/releases/download/$tag/$name.zip" -OutFile $zip
    Expand-Archive $zip -DestinationPath $tmp
    New-Item -ItemType Directory -Path $dest -Force | Out-Null
    Copy-Item (Join-Path $tmp "$name\scope.exe") $dest -Force
} finally {
    Remove-Item $tmp -Recurse -Force
}

Write-Host "installed $dest\scope.exe"
if (($env:PATH -split ';') -notcontains $dest) {
    Write-Host "note: $dest is not on your PATH. To add it for next time:"
    Write-Host "  [Environment]::SetEnvironmentVariable('PATH', `"`$env:PATH;$dest`", 'User')"
}
