[CmdletBinding()]
param(
    [string]$ManifestPath = (Join-Path $PSScriptRoot 'installer-release.json'),
    [string]$DestinationDirectory = $PSScriptRoot,
    [string]$Source = 'auto',
    [string]$SourceUrl,
    [string]$DownloadSourcesPath,
    [switch]$Offline,
    [switch]$Install
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$downloadModule = Join-Path $PSScriptRoot 'RuntimeDownloads.psm1'
if (-not (Test-Path -LiteralPath $downloadModule -PathType Leaf)) { $downloadModule = Join-Path $PSScriptRoot '../RuntimeDownloads.psm1' }
Import-Module $downloadModule -Force
$manifest = Get-Content -LiteralPath $ManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($manifest.schemaVersion -ne 1 -or $manifest.fileName -ne [IO.Path]::GetFileName($manifest.fileName)) {
    throw 'The release manifest schema or installer file name is invalid.'
}
$destination = Join-Path ([IO.Path]::GetFullPath($DestinationDirectory)) $manifest.fileName
$sources = @($manifest.sources | Where-Object { $_.status -eq 'available' -and ($Source -eq 'auto' -or $_.id -eq $Source) })
$urls = @($sources | ForEach-Object { [string]$_.url })
if ($SourceUrl) { $urls = @($SourceUrl) + $urls }

# A local installer is reusable without any published download service. Never start it implicitly.
# 已有完整安装包无需远端已发布来源也能复用，只有显式 -Install 才启动安装。
$allSources = @($manifest.sources)
$canonicalUrl = if ($allSources.Count -gt 0) { [string]$allSources[0].url } elseif ($urls.Count -gt 0) { $urls[0] } else { 'https://localhost/installer-not-published' }
if (-not (Test-Path -LiteralPath $destination -PathType Leaf) -and $urls.Count -eq 0 -and -not $Offline) {
    throw 'No published source is configured. Place the EXE beside this script, or pass -SourceUrl with an actual release or mirror URL.'
}
Save-KynxaPinnedDownload -Url $canonicalUrl -SourceUrls $urls -Destination $destination -Sha256 $manifest.sha256 `
    -ExpectedBytes $manifest.bytes -DownloadSourcesPath $DownloadSourcesPath -Offline:($Offline -or $urls.Count -eq 0)
Write-Host "Installer ready: $destination"
if ($Install) { Start-Process -FilePath $destination -WorkingDirectory (Split-Path -Parent $destination) }
