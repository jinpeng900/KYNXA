[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$VerifiedCacheDirectory)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
$runtimePrepare = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../apps/desktop/Build/prepare-node-runtime.ps1'))
$runtimeManifest = Get-Content -LiteralPath (Join-Path (Split-Path $runtimePrepare) 'node-runtime.json') -Raw | ConvertFrom-Json
$runtimeVersion = $runtimeManifest.platforms.'win-x64'.version
$runtimeCacheTest = Join-Path ([IO.Path]::GetTempPath()) ('kynxa-node-cache-' + [Guid]::NewGuid().ToString('N'))
$versionCache = Join-Path $runtimeCacheTest $runtimeVersion
[IO.Directory]::CreateDirectory($versionCache) | Out-Null
$runtimeArchive = Join-Path $VerifiedCacheDirectory ($runtimeVersion + '/' + $runtimeManifest.platforms.'win-x64'.archive)
Copy-Item -LiteralPath $runtimeArchive -Destination $versionCache
& $runtimePrepare -RuntimeIdentifier win-x64 -CacheDirectory $runtimeCacheTest -Offline
$runtimeExecutable = Join-Path $versionCache 'runtime/node.exe'
$expected = $runtimeManifest.platforms.'win-x64'.nodeSha256
if ((Get-FileHash -LiteralPath $runtimeExecutable).Hash.ToLowerInvariant() -ne $expected) { throw 'Initial offline extraction did not use the pinned Node.' }
# Corrupt only this fixture's copy, then prove rebuilding repairs it from the verified archive.
# 仅损坏此夹具的副本，验证重新构建会使用已校验的归档修复它。
[IO.File]::WriteAllText($runtimeExecutable, 'deliberately invalid fixture executable')
& $runtimePrepare -RuntimeIdentifier win-x64 -CacheDirectory $runtimeCacheTest -Offline
if ((Get-FileHash -LiteralPath $runtimeExecutable).Hash.ToLowerInvariant() -ne $expected) { throw 'A corrupted cached runtime was reused.' }
$archivePath = Join-Path $versionCache $runtimeManifest.platforms.'win-x64'.archive
[IO.File]::WriteAllText($archivePath, 'deliberately invalid fixture archive')
$rejected = $false
try { & $runtimePrepare -RuntimeIdentifier win-x64 -CacheDirectory $runtimeCacheTest -Offline }
catch { $rejected = $_.Exception.Message -match 'SHA-256 verification' }
if (-not $rejected) { throw 'A corrupt archive was accepted or fell back to installed Node.' }
$rejected = $false
try { & $runtimePrepare -RuntimeIdentifier win-x64 -CacheDirectory (Join-Path $runtimeCacheTest 'uncached') -Offline }
catch { $rejected = $_.Exception.Message -match 'not cached' }
if (-not $rejected) { throw 'An offline cache miss fell back to installed Node.' }
$rejected = $false
try { & $runtimePrepare -RuntimeIdentifier win-unsupported -CacheDirectory $runtimeCacheTest -Offline }
catch { $rejected = $_.Exception.Message -match 'Unsupported bundled Node runtime' }
if (-not $rejected) { throw 'An unsupported runtime architecture was silently substituted.' }
Write-Output 'PASS: verified offline extraction, corrupted executable repair, corrupted archive rejection, offline miss failure, unsupported architecture failure.'
