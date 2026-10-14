[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateSet('Export', 'Import')][string]$Action,
    [Parameter(Mandatory = $true)][string]$ArchivePath,
    [ValidateSet('Full', 'Core')][string]$Profile = 'Full',
    [string]$SourceRoot,
    [string]$FileListPath,
    [string]$VisualCppCacheDirectory,
    [string]$DestinationRoot
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'ResourceConfigCompatibility.psm1') -Force
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
$ArchivePath = [IO.Path]::GetFullPath($ArchivePath)
$utf8 = [Text.UTF8Encoding]::new($false)
$versionPaths = @('native-assets/native-runtime.json', 'native-assets/embedding.json',
    'native-assets/reranker.json', 'native-assets/mcp-runtime.json', 'native-assets/mcp/package-lock.json', 'vc-runtime.json')

function Get-ResourcePackHash([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}
function Get-ResourcePackPath([string]$Root, [string]$RelativePath) {
    if ([IO.Path]::IsPathRooted($RelativePath) -or $RelativePath -match '(^|[\\/])\.\.([\\/]|$)' -or $RelativePath.Contains(':')) {
        throw 'Resource pack paths must be relative and remain inside the selected directory.'
    }
    $absolutePath = [IO.Path]::GetFullPath((Join-Path $Root $RelativePath))
    if (-not $absolutePath.StartsWith($Root.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Resource pack path escaped the selected directory.'
    }
    $ancestor = $absolutePath
    while ($ancestor -and $ancestor.StartsWith($Root, [StringComparison]::OrdinalIgnoreCase)) {
        if (([IO.File]::Exists($ancestor) -or [IO.Directory]::Exists($ancestor)) -and
            ([IO.File]::GetAttributes($ancestor) -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'Resource packs cannot follow reparse points.'
        }
        $ancestor = [IO.Path]::GetDirectoryName($ancestor)
    }
    return $absolutePath
}
function Get-ResourcePackVersions {
    return @($versionPaths | ForEach-Object {
        @{ path = $_; sha256 = Get-ResourcePackHash (Join-Path $PSScriptRoot $_) }
    })
}

if ($Action -eq 'Export') {
    if (-not $SourceRoot) { $SourceRoot = Join-Path $repositoryRoot 'artifacts/runtime/native-desktop/win-x64' }
    $SourceRoot = [IO.Path]::GetFullPath($SourceRoot)
    if (-not $FileListPath) {
        $FileListPath = Join-Path $SourceRoot 'asset-files.txt'
        if (-not [IO.File]::Exists($FileListPath)) { $FileListPath = Join-Path $SourceRoot 'bundle-files.txt' }
    }
    $payload = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($sourcePath in [IO.File]::ReadAllLines([IO.Path]::GetFullPath($FileListPath), $utf8)) {
        if ([string]::IsNullOrWhiteSpace($sourcePath)) { continue }
        $sourcePath = [IO.Path]::GetFullPath($sourcePath)
        if (-not $sourcePath.StartsWith($SourceRoot.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'The verified file list contains a file outside SourceRoot.'
        }
        $relativePath = $sourcePath.Substring($SourceRoot.TrimEnd('\', '/').Length + 1).Replace('\', '/')
        if (-not $relativePath.StartsWith('runtime/', [StringComparison]::Ordinal)) { throw 'Only runtime payload files belong in a resource pack.' }
        $payload[$relativePath] = Get-ResourcePackPath $SourceRoot $relativePath
    }
    if ($Profile -eq 'Full') {
        foreach ($required in @('runtime/native/vec0.dll', 'runtime/mcp/manifest.json',
            'runtime/embedding/builtin-multilingual/manifest.json', 'runtime/rerank/builtin-multilingual-reranker/manifest.json')) {
            if (-not $payload.ContainsKey($required)) { throw "Full resource pack is missing $required. Prepare the Full assets once before exporting." }
        }
        if (-not $VisualCppCacheDirectory) { $VisualCppCacheDirectory = Join-Path $repositoryRoot 'artifacts/runtime/vc/win-x64' }
        foreach ($listName in @('runtime-files.txt', 'runtime-notices.txt')) {
            foreach ($sourcePath in [IO.File]::ReadAllLines((Join-Path $VisualCppCacheDirectory $listName), $utf8)) {
                $relativePath = 'runtime/vc/' + $(if ($listName -eq 'runtime-notices.txt') { 'licenses/' } else { '' }) + [IO.Path]::GetFileName($sourcePath)
                $payload[$relativePath] = $sourcePath
            }
        }
    }
    if ($payload.Count -eq 0) { throw 'The resource pack file list is empty.' }
    $records = [Collections.Generic.List[object]]::new()
    foreach ($relativePath in $payload.Keys | Sort-Object) {
        $file = Get-Item -LiteralPath $payload[$relativePath]
        if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Resource pack input cannot be a reparse point.' }
        $records.Add(@{ path = $relativePath; bytes = $file.Length; sha256 = Get-ResourcePackHash $file.FullName })
    }
    $manifest = @{ schemaVersion = 1; runtimeIdentifier = 'win-x64'; profile = $Profile;
        versions = Get-ResourcePackVersions; files = @($records.ToArray()) }
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($ArchivePath)) | Out-Null
    if ([IO.File]::Exists($ArchivePath)) { throw 'The export archive already exists; choose a new ArchivePath.' }
    $archive = [IO.Compression.ZipFile]::Open($ArchivePath, [IO.Compression.ZipArchiveMode]::Create)
    try {
        $entry = $archive.CreateEntry('resource-pack.json')
        $writer = [IO.StreamWriter]::new($entry.Open(), $utf8)
        try { $writer.Write(($manifest | ConvertTo-Json -Depth 8)) } finally { $writer.Dispose() }
        $position = 0
        foreach ($record in $records) {
            $position++
            Write-Progress -Activity 'Export fixed runtime resources' -Status "$position/$($records.Count) $($record.path)" -PercentComplete (100 * $position / $records.Count)
            [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $payload[$record.path], $record.path, [IO.Compression.CompressionLevel]::Fastest) | Out-Null
        }
    }
    finally { $archive.Dispose(); Write-Progress -Activity 'Export fixed runtime resources' -Completed }
    $archiveHash = Get-ResourcePackHash $ArchivePath
    [IO.File]::WriteAllText($ArchivePath + '.sha256', "$archiveHash  $([IO.Path]::GetFileName($ArchivePath))`n", $utf8)
    Write-Output "Exported $($records.Count) fixed resource files: $ArchivePath"
}
else {
    if (-not $DestinationRoot) { throw 'Import requires an explicit DestinationRoot.' }
    $DestinationRoot = [IO.Path]::GetFullPath($DestinationRoot)
    $archive = $null
    $packDirectory = if ([IO.Directory]::Exists($ArchivePath)) { $ArchivePath } else { $null }
    if (-not $packDirectory) { $archive = [IO.Compression.ZipFile]::OpenRead($ArchivePath) }
    try {
        if ($packDirectory) {
            $manifest = [IO.File]::ReadAllText((Get-ResourcePackPath $packDirectory 'resource-pack.json'), $utf8) | ConvertFrom-Json
        }
        else {
            $entry = $archive.GetEntry('resource-pack.json')
            if (-not $entry) { throw 'resource-pack.json is missing.' }
            $reader = [IO.StreamReader]::new($entry.Open(), $utf8)
            try { $manifest = $reader.ReadToEnd() | ConvertFrom-Json } finally { $reader.Dispose() }
        }
        if ($manifest.schemaVersion -ne 1 -or $manifest.runtimeIdentifier -ne 'win-x64' -or $manifest.profile -notin @('Full', 'Core')) { throw 'Unsupported resource pack manifest.' }
        $currentVersions = Get-ResourcePackVersions
        if (@($manifest.versions).Count -ne $currentVersions.Count) { throw 'Resource pack version list differs from this checkout.' }
        foreach ($current in $currentVersions) {
            $version = @($manifest.versions | Where-Object { $_.path -eq $current.path })
            if ($version.Count -ne 1 -or -not (Test-KynxaConfigHash -Path (Join-Path $PSScriptRoot $current.path) -ExpectedHash $version[0].sha256)) {
                throw "Resource pack version differs: $($current.path)."
            }
        }
        # Extract only listed files and commit each file after its size and digest pass.
        # 只提取白名单文件；每份文件完成大小与摘要校验后才替换目标，既有有效文件保持可用。
        $fileCount = @($manifest.files).Count
        Write-Host "[Resources] Importing $fileCount fixed files from the $($manifest.profile) pack."
        $position = 0
        foreach ($record in $manifest.files) {
            $position++
            if ($position -eq 1 -or $position % 1000 -eq 0 -or $position -eq $fileCount) {
                Write-Host "[Resources] $position/${fileCount}: $($record.path)"
            }
            if (-not $record.path.StartsWith('runtime/', [StringComparison]::Ordinal) -or $record.sha256 -notmatch '^[0-9a-f]{64}$') { throw 'Invalid resource pack file record.' }
            if ($packDirectory) {
                $sourceFile = Get-Item -LiteralPath (Get-ResourcePackPath $packDirectory $record.path)
                if ($sourceFile.Length -ne $record.bytes) { throw "Resource pack file has a different size: $($record.path)." }
            }
            else {
                $entry = $archive.GetEntry($record.path)
                if (-not $entry -or $entry.Length -ne $record.bytes) { throw "Resource pack file is missing or has a different size: $($record.path)." }
            }
            $destination = Get-ResourcePackPath $DestinationRoot $record.path
            if ([IO.File]::Exists($destination) -and ([IO.FileInfo]::new($destination)).Length -eq $record.bytes -and
                (Get-ResourcePackHash $destination) -eq $record.sha256) { continue }
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination)) | Out-Null
            $pendingPath = $destination + '.import-' + [Guid]::NewGuid().ToString('N')
            try {
                $input = if ($packDirectory) { [IO.File]::OpenRead($sourceFile.FullName) } else { $entry.Open() }
                $output = [IO.File]::Create($pendingPath)
                try { $input.CopyTo($output) } finally { $input.Dispose(); $output.Dispose() }
                if ((Get-ResourcePackHash $pendingPath) -ne $record.sha256) { throw "Resource pack file failed SHA-256: $($record.path)." }
                Move-Item -LiteralPath $pendingPath -Destination $destination -Force
            }
            finally { if ([IO.File]::Exists($pendingPath)) { Remove-Item -LiteralPath $pendingPath } }
        }
        [IO.File]::WriteAllText((Join-Path $DestinationRoot 'resource-pack.json'), ($manifest | ConvertTo-Json -Depth 8), $utf8)
        Write-Output "Imported $(@($manifest.files).Count) verified resource files: $DestinationRoot"
    }
    finally { if ($archive) { $archive.Dispose() } }
}
