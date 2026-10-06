[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RuntimeIdentifier,
    [Parameter(Mandatory = $true)][string]$CacheDirectory,
    [string]$ManifestPath,
    [switch]$Offline
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.IO.Compression.FileSystem
if ([string]::IsNullOrWhiteSpace($ManifestPath)) { $ManifestPath = Join-Path $PSScriptRoot 'node-runtime.json' }

# The pinned digests come from the official release's SHASUMS256.txt. The complete
# verified ZIP supplies npm/npx and their licenses as well as the unmodified node.exe.
# 固定摘要来自官方发行的 SHASUMS256.txt；完整校验 ZIP 中的 npm/npx、许可及未修改的 node.exe 一并随包提供。
$runtimeManifestText = Get-Content -LiteralPath $ManifestPath -Raw -Encoding UTF8
$runtimeManifest = $runtimeManifestText | ConvertFrom-Json
$runtimePlatform = $runtimeManifest.platforms.PSObject.Properties[$RuntimeIdentifier]
if ($runtimeManifest.schemaVersion -ne 1 -or -not $runtimePlatform) { throw "Unsupported bundled Node runtime: $RuntimeIdentifier" }
$runtimeSpec = $runtimePlatform.Value
$runtimeVersion = [string]$runtimeSpec.version
if ($runtimeVersion -notmatch '^\d+\.\d+\.\d+$' -or $runtimeSpec.archive -ne "node-v$runtimeVersion-$RuntimeIdentifier.zip" -or
    $runtimeSpec.sha256 -notmatch '^[0-9a-f]{64}$' -or $runtimeSpec.nodeSha256 -notmatch '^[0-9a-f]{64}$') { throw 'Invalid pinned Node runtime manifest.' }
$runtimeCache = [IO.Path]::GetFullPath($CacheDirectory)
if ($runtimeCache.TrimEnd('\', '/') -eq [IO.Path]::GetPathRoot($runtimeCache).TrimEnd('\', '/')) { throw 'A filesystem root cannot be used as the Node build cache.' }
$runtimeVersionCache = Join-Path $runtimeCache $runtimeVersion
$runtimeBundle = Join-Path $runtimeVersionCache 'runtime'
$runtimeArchivePath = Join-Path $runtimeVersionCache $runtimeSpec.archive

function Assert-RuntimeNoReparse([string]$Path) {
    $current = [IO.Path]::GetFullPath($Path)
    while ($current -and $current.StartsWith($runtimeCache, [StringComparison]::OrdinalIgnoreCase)) {
        if ((Test-Path -LiteralPath $current) -and ([IO.File]::GetAttributes($current) -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'The Node runtime build cache cannot contain reparse points.'
        }
        $current = [IO.Path]::GetDirectoryName($current)
    }
}
Assert-RuntimeNoReparse $runtimeVersionCache
[IO.Directory]::CreateDirectory($runtimeVersionCache) | Out-Null
function Get-RuntimeDigest([string]$Path) {
    Assert-RuntimeNoReparse $Path
    $stream = [IO.File]::OpenRead($Path)
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($hasher.ComputeHash($stream))).Replace('-', '').ToLowerInvariant() }
    finally { $hasher.Dispose(); $stream.Dispose() }
}
function Get-RuntimeOutputPath([string]$RelativePath) {
    $outputPath = [IO.Path]::GetFullPath((Join-Path $runtimeBundle $RelativePath))
    if (-not $outputPath.StartsWith($runtimeBundle.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'The official runtime archive contains an invalid output path.'
    }
    Assert-RuntimeNoReparse $outputPath
    return $outputPath
}

# Concurrent IDE/build invocations share one cache without reading partial files.
# IDE 与构建并发调用共用一个缓存，通过锁避免读取尚未写完的文件。
$runtimeLock = $null
$runtimeLockDeadline = [DateTime]::UtcNow.AddMinutes(2)
while (-not $runtimeLock) {
    try { $runtimeLock = [IO.File]::Open((Join-Path $runtimeVersionCache '.prepare.lock'), [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None) }
    catch [IO.IOException] {
        if ([DateTime]::UtcNow -ge $runtimeLockDeadline) { throw 'Timed out waiting for the Node runtime build cache.' }
        Start-Sleep -Milliseconds 200
    }
}
try {
    if (-not (Test-Path -LiteralPath $runtimeArchivePath -PathType Leaf)) {
        if ($Offline) { throw 'The verified Node runtime archive is not cached. An online build is required once.' }
        $runtimeDownload = $runtimeArchivePath + '.download-' + [Guid]::NewGuid().ToString('N')
        try {
            [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
            Invoke-WebRequest -Uri "https://nodejs.org/dist/v$runtimeVersion/$($runtimeSpec.archive)" -UseBasicParsing -OutFile $runtimeDownload -TimeoutSec 120
            if ((Get-RuntimeDigest $runtimeDownload) -ne $runtimeSpec.sha256) { throw 'Official Node runtime download failed SHA-256 verification.' }
            Move-Item -LiteralPath $runtimeDownload -Destination $runtimeArchivePath
        }
        finally { if (Test-Path -LiteralPath $runtimeDownload -PathType Leaf) { Remove-Item -LiteralPath $runtimeDownload } }
    }
    if ((Get-RuntimeDigest $runtimeArchivePath) -ne $runtimeSpec.sha256) { throw 'Cached Node runtime archive failed SHA-256 verification. Remove that archive and rebuild.' }
    [IO.Directory]::CreateDirectory($runtimeBundle) | Out-Null
    $runtimeFiles = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $runtimeArchive = [IO.Compression.ZipFile]::OpenRead($runtimeArchivePath)
    try {
        $runtimePrefix = "node-v$runtimeVersion-$RuntimeIdentifier/"
        foreach ($entry in $runtimeArchive.Entries) {
            if (-not $entry.FullName.StartsWith($runtimePrefix, [StringComparison]::Ordinal) -or -not $entry.Name) { continue }
            $relativePath = $entry.FullName.Substring($runtimePrefix.Length)
            if ($relativePath -notin @('node.exe', 'LICENSE', 'npm', 'npm.cmd', 'npm.ps1', 'npx', 'npx.cmd', 'npx.ps1') -and
                -not $relativePath.StartsWith('node_modules/npm/', [StringComparison]::Ordinal)) { continue }
            if (-not $runtimeFiles.Add($relativePath)) { throw 'The runtime archive contains duplicate output paths.' }
            $outputPath = Get-RuntimeOutputPath $relativePath
            $sourceStream = $entry.Open()
            $hasher = [Security.Cryptography.SHA256]::Create()
            try { $expectedDigest = ([BitConverter]::ToString($hasher.ComputeHash($sourceStream))).Replace('-', '').ToLowerInvariant() }
            finally { $hasher.Dispose(); $sourceStream.Dispose() }
            if ($relativePath -eq 'node.exe' -and $expectedDigest -ne $runtimeSpec.nodeSha256) { throw 'Node executable does not match its official SHA-256 pin.' }
            if ((Test-Path -LiteralPath $outputPath -PathType Leaf) -and (Get-RuntimeDigest $outputPath) -eq $expectedDigest) { continue }
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($outputPath)) | Out-Null
            $pendingOutputPath = $outputPath + '.extract-' + [Guid]::NewGuid().ToString('N')
            try {
                $sourceStream = $entry.Open()
                $destinationStream = [IO.File]::Create($pendingOutputPath)
                try { $sourceStream.CopyTo($destinationStream) }
                finally { $destinationStream.Dispose(); $sourceStream.Dispose() }
                if ((Get-RuntimeDigest $pendingOutputPath) -ne $expectedDigest) { throw 'Extracted Node runtime file failed verification.' }
                Move-Item -LiteralPath $pendingOutputPath -Destination $outputPath -Force
            }
            finally { if (Test-Path -LiteralPath $pendingOutputPath -PathType Leaf) { Remove-Item -LiteralPath $pendingOutputPath } }
        }
    }
    finally { $runtimeArchive.Dispose() }
    foreach ($required in @('node.exe', 'LICENSE', 'npm.cmd', 'npx.cmd', 'node_modules/npm/bin/npm-cli.js', 'node_modules/npm/bin/npx-cli.js')) {
        if (-not $runtimeFiles.Contains($required)) { throw "Official Node runtime archive is missing $required." }
    }
    $runtimeFiles.Add('node-runtime.json') | Out-Null
    if ((Get-Content -LiteralPath $ManifestPath -Raw -Encoding UTF8) -ne $runtimeManifestText) { throw 'The Node runtime manifest changed during preparation. Rebuild using the new pin.' }
    [IO.File]::WriteAllText((Join-Path $runtimeBundle 'node-runtime.json'), $runtimeManifestText, [Text.UTF8Encoding]::new($false))
    foreach ($file in Get-ChildItem -LiteralPath $runtimeBundle -File -Recurse -Force) {
        $relativePath = $file.FullName.Substring($runtimeBundle.Length + 1).Replace('\', '/')
        if (-not $runtimeFiles.Contains($relativePath)) { throw "Unexpected file in the Node runtime cache: $relativePath" }
    }
    $runtimeTotalBytes = (Get-ChildItem -LiteralPath $runtimeBundle -File -Recurse | Measure-Object Length -Sum).Sum
    $runtimeMarker = Join-Path $runtimeCache 'runtime-path.txt'
    Assert-RuntimeNoReparse $runtimeMarker
    [IO.File]::WriteAllText($runtimeMarker, $runtimeBundle, [Text.UTF8Encoding]::new($false))
    Write-Output "Verified bundled Node $runtimeVersion $RuntimeIdentifier ($($runtimeFiles.Count) files, $runtimeTotalBytes bytes)."
}
finally { $runtimeLock.Dispose() }
