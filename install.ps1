# Installs the latest oso release into $env:INSTALL_DIR (default %LOCALAPPDATA%\oso\bin). Experimental.
$ErrorActionPreference = 'Stop'
$repo = 'seyadog/oso'
$dir = if ($env:INSTALL_DIR) { $env:INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'oso\bin' }
$tag = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
if (-not $tag) { throw 'Could not find the latest release' }
$pkg = "oso-$tag-windows-x86_64"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  Invoke-WebRequest "https://github.com/$repo/releases/download/$tag/$pkg.zip" -OutFile "$tmp\$pkg.zip"
  Expand-Archive "$tmp\$pkg.zip" -DestinationPath $tmp
  New-Item -ItemType Directory -Path $dir -Force | Out-Null
  Copy-Item "$tmp\$pkg\oso.exe" (Join-Path $dir 'oso.exe') -Force
} finally { Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue }
Write-Host "Installed oso $tag to $dir\oso.exe"
if (($env:PATH -split ';') -notcontains $dir) { Write-Host "Add $dir to your PATH to run 'oso' from anywhere." }
