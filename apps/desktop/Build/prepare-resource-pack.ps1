[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateSet('Export', 'Import', 'Inspect')][string]$Action,
    [Parameter(Mandatory = $true)][string]$ArchivePath,
    [ValidateSet('Full', 'Core')][string]$Profile = 'Full',
    [string]$SourceRoot,
    [string]$FileListPath,
    [string]$VisualCppCacheDirectory,
    [string]$DestinationRoot,
    [switch]$VerifyResources
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# Nested MSBuild PowerShell invocations may not auto-load the inbox utility module.
# 嵌套 MSBuild PowerShell 调用可能不会自动加载系统工具模块。
Import-Module (Join-Path $PSHOME 'Modules/Microsoft.PowerShell.Utility/Microsoft.PowerShell.Utility.psd1') -Force
Import-Module (Join-Path $PSScriptRoot 'ResourceConfigCompatibility.psm1') -Force
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
$ArchivePath = [IO.Path]::GetFullPath($ArchivePath)
$utf8 = [Text.UTF8Encoding]::new($false)
$checkedDirectories = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
$versionPaths = @('native-assets/native-runtime.json', 'native-assets/embedding.json',
    'native-assets/mcp-runtime.json', 'native-assets/mcp/package-lock.json', 'vc-runtime.json')
$removedRerankerPrefix = 'runtime/rerank/builtin-multilingual-reranker/'

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
        if ($checkedDirectories.Contains($ancestor)) { break }
        if (([IO.File]::Exists($ancestor) -or [IO.Directory]::Exists($ancestor)) -and
            ([IO.File]::GetAttributes($ancestor) -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'Resource packs cannot follow reparse points.'
        }
        if ([IO.Directory]::Exists($ancestor)) { [void]$checkedDirectories.Add($ancestor) }
        $ancestor = [IO.Path]::GetDirectoryName($ancestor)
    }
    return $absolutePath
}
function Get-ResourcePackVersions {
    param([switch]$IncludeSemantic)
    return @($versionPaths | ForEach-Object {
        $record = @{ path = $_; sha256 = Get-ResourcePackHash (Join-Path $PSScriptRoot $_) }
        if ($IncludeSemantic) { $record.semanticSha256 = Get-KynxaConfigSemanticHash -Path (Join-Path $PSScriptRoot $_) }
        $record
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
        # Old verified inventories may still mention the retired model; never re-export its weights.
        # 旧的已验证清单可能仍列出被移除模型；不再把其权重带入新资源包。
        if ($relativePath.StartsWith($removedRerankerPrefix, [StringComparison]::OrdinalIgnoreCase)) { continue }
        $payload[$relativePath] = Get-ResourcePackPath $SourceRoot $relativePath
    }
    if ($Profile -eq 'Full') {
        foreach ($required in @('runtime/native/vec0.dll', 'runtime/mcp/manifest.json',
            'runtime/embedding/builtin-multilingual/manifest.json')) {
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
        versions = Get-ResourcePackVersions -IncludeSemantic; files = @($records.ToArray()) }
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
    if ($Action -eq 'Import' -and -not $DestinationRoot) { throw 'Import requires an explicit DestinationRoot.' }
    if ($DestinationRoot) { $DestinationRoot = [IO.Path]::GetFullPath($DestinationRoot) }
    $archive = $null
    $packDirectory = if ([IO.Directory]::Exists($ArchivePath)) { $ArchivePath } else { $null }
    if (-not $packDirectory) { $archive = [IO.Compression.ZipFile]::OpenRead($ArchivePath) }
    try {
        if ($packDirectory) {
            $manifestText = [IO.File]::ReadAllText((Get-ResourcePackPath $packDirectory 'resource-pack.json'), $utf8)
        }
        else {
            $entry = $archive.GetEntry('resource-pack.json')
            if (-not $entry) { throw 'resource-pack.json is missing.' }
            $reader = [IO.StreamReader]::new($entry.Open(), $utf8)
            try { $manifestText = $reader.ReadToEnd() } finally { $reader.Dispose() }
        }
        $manifest = $manifestText | ConvertFrom-Json
        if ($manifest.schemaVersion -ne 1 -or $manifest.runtimeIdentifier -ne 'win-x64' -or $manifest.profile -notin @('Full', 'Core')) { throw 'Unsupported resource pack manifest.' }
        # Reuse the verified E5/runtime portion of older packs without extracting their retired BGE payload.
        # 旧资源包仍可复用已验证的 E5 与运行依赖，但不再提取已移除的 BGE 载荷。
        $manifest.versions = @($manifest.versions | Where-Object { $_.path -ne 'native-assets/reranker.json' })
        $manifest.files = @($manifest.files | Where-Object { -not $_.path.StartsWith($removedRerankerPrefix, [StringComparison]::OrdinalIgnoreCase) })
        $currentVersions = Get-ResourcePackVersions
        if (@($manifest.versions).Count -ne $currentVersions.Count) { throw 'Resource pack version list differs from this checkout.' }
        foreach ($current in $currentVersions) {
            $version = @($manifest.versions | Where-Object { $_.path -eq $current.path })
            $semanticHash = if ($version.Count -eq 1 -and $version[0].PSObject.Properties['semanticSha256']) { [string]$version[0].semanticSha256 } else { '' }
            if ($version.Count -eq 1 -and -not $semanticHash -and
                -not (Test-KynxaConfigHash -Path (Join-Path $PSScriptRoot $current.path) -ExpectedHash $version[0].sha256)) {
                # Verify older packs against their hash-bound source configuration, never a guessed version.
                # 用哈希绑定的原配置核验旧包，不猜测兼容版本。
                $configEntries = @{'native-assets/native-runtime.json'='runtime/native/licenses/native-runtime-manifest.json';
                    'native-assets/embedding.json'='runtime/embedding/builtin-multilingual/manifest.json';
                    'native-assets/mcp-runtime.json'='runtime/mcp/licenses/source-manifest.json';
                    'native-assets/mcp/package-lock.json'='runtime/mcp/services/package-lock.json'}
                if ($configEntries.ContainsKey($current.path)) {
                    $originalBytes = $null
                    $configEntry = $configEntries[$current.path]
                    if ($packDirectory -and [IO.File]::Exists((Get-ResourcePackPath $packDirectory $configEntry))) {
                        $originalBytes = [IO.File]::ReadAllBytes((Get-ResourcePackPath $packDirectory $configEntry))
                    }
                    elseif ($archive -and ($entry = $archive.GetEntry($configEntry))) {
                        $stream = $entry.Open(); $copy = [IO.MemoryStream]::new()
                        try { $stream.CopyTo($copy); $originalBytes = $copy.ToArray() } finally { $stream.Dispose(); $copy.Dispose() }
                    }
                    if ($originalBytes -and ((Get-KynxaConfigHashVariants -Bytes $originalBytes) -contains $version[0].sha256)) {
                        $semanticHash = Get-KynxaConfigSemanticHash -Text ($utf8.GetString($originalBytes).TrimStart([char]0xfeff))
                    }
                }
            }
            if ($version.Count -ne 1 -or -not (Test-KynxaConfigHash -Path (Join-Path $PSScriptRoot $current.path) -ExpectedHash $version[0].sha256 -SemanticHash $semanticHash)) {
                throw "Resource pack version differs: $($current.path)."
            }
        }
        $componentInput = Get-KynxaResourceInputHash -Paths @($PSCommandPath,
            (Join-Path $PSScriptRoot 'ResourceConfigCompatibility.psm1')) -Values @($manifestText)
        # Inspect and import share entry/schema checks; neither needs to decompress binary payloads here.
        # 预检与导入共用目录及结构检查，此阶段不解压二进制载荷。
        $resourceScopes = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        if ($Action -in @('Inspect', 'Import')) {
            $entries = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
            foreach ($record in $manifest.files) {
                if (-not $record.path.StartsWith('runtime/', [StringComparison]::Ordinal) -or
                    $record.path -match '(^|[\\/])\.\.([\\/]|$)|:' -or $record.sha256 -notmatch '^[0-9a-f]{64}$' -or
                    $record.bytes -lt 0 -or -not $entries.Add($record.path)) { throw 'Invalid or duplicate resource pack file record.' }
                [void]$resourceScopes.Add(($record.path -split '/', 3)[0..1] -join '/')
                if ($packDirectory) { $length = [IO.FileInfo]::new((Get-ResourcePackPath $packDirectory $record.path)).Length }
                else {
                    $declaredEntry = $archive.GetEntry($record.path)
                    if (-not $declaredEntry) { throw "Resource pack entry is missing: $($record.path)." }
                    $length = $declaredEntry.Length
                }
                if ($length -ne $record.bytes) { throw "Resource pack entry size differs: $($record.path)." }
            }
            if ($entries.Count -eq 0) { throw 'The resource pack file list is empty.' }
            $required = @('runtime/native/vec0.dll')
            if ($manifest.profile -eq 'Full') { $required += @('runtime/mcp/manifest.json',
                'runtime/embedding/builtin-multilingual/manifest.json') }
            foreach ($path in $required) { if (-not $entries.Contains($path)) { throw "Resource pack is missing $path." } }
            if ($Action -eq 'Inspect') {
                [pscustomobject]@{schemaVersion=1;runtimeIdentifier=$manifest.runtimeIdentifier;profile=$manifest.profile;
                    archivePath=$ArchivePath;fileCount=$entries.Count;compatible=$true;inputHash=$componentInput}
                return
            }
        }
        if (-not $VerifyResources -and (Test-KynxaPreparedComponent -Root $DestinationRoot -Component 'resource-pack' -InputHash $componentInput)) {
            & (Join-Path $PSScriptRoot 'prepare-browser-sandbox.ps1') -BundleRoot $DestinationRoot
            Write-Output "Resource pack reused: $(@($manifest.files).Count) unchanged files; no extraction or content hashing."
            return
        }
        $preparedFiles = if ($VerifyResources) { $null } else { Get-KynxaPreparedFileCache -Root $DestinationRoot -Component 'resource-pack' }
        if (-not $VerifyResources) {
            # A component already verified in this cache can also satisfy the pack's exact expected file identity.
            # 同一缓存中已验证的组件可复用，但仍须匹配本包要求的摘要、大小与修改时间。
            foreach ($component in @('native-assets', 'mcp', 'office')) {
                $componentFiles = Get-KynxaPreparedFileCache -Root $DestinationRoot -Component $component
                foreach ($key in $componentFiles.Keys) {
                    if (-not $preparedFiles.ContainsKey($key)) { $preparedFiles[$key] = $componentFiles[$key] }
                }
            }
        }
        # Validate old caches in one native batch; later runs compare metadata only.
        # 旧缓存先做一次原生批量校验，后续只比较元数据。
        if (Test-KynxaResourceFiles -Root $DestinationRoot -Files @($manifest.files) -Cache $preparedFiles) {
            [IO.File]::WriteAllText((Join-Path $DestinationRoot 'resource-pack.json'), ($manifest | ConvertTo-Json -Depth 8), $utf8)
            & (Join-Path $PSScriptRoot 'prepare-browser-sandbox.ps1') -BundleRoot $DestinationRoot
            Save-KynxaPreparedComponent -Root $DestinationRoot -Component 'resource-pack' -InputHash $componentInput -Files @($manifest.files) -Scope @($resourceScopes)
            Write-Output "Resource pack reused: $(@($manifest.files).Count) verified files; no extraction."
            return
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
            if (Test-KynxaPreparedFile -Root $DestinationRoot -Path $destination -Bytes $record.bytes -Sha256 $record.sha256 -Cache $preparedFiles) { continue }
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
        & (Join-Path $PSScriptRoot 'prepare-browser-sandbox.ps1') -BundleRoot $DestinationRoot
        Save-KynxaPreparedComponent -Root $DestinationRoot -Component 'resource-pack' -InputHash $componentInput -Files @($manifest.files) -Scope @($resourceScopes)
        Write-Output "Imported $(@($manifest.files).Count) verified resource files: $DestinationRoot"
    }
    finally { if ($archive) { $archive.Dispose() } }
}
