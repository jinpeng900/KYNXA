[CmdletBinding()]
param(
    [string]$BundleRoot,
    [string]$DownloadSourcesPath = $env:KYNXA_DOWNLOAD_SOURCES,
    [switch]$Offline,
    [switch]$VerifyResources
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSHOME 'Modules/Microsoft.PowerShell.Utility/Microsoft.PowerShell.Utility.psd1') -Force
Import-Module (Join-Path $PSScriptRoot 'RuntimeDownloads.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'ResourceConfigCompatibility.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'AtomicFiles.psm1') -Force
Add-Type -AssemblyName System.IO.Compression.FileSystem

if (-not $BundleRoot) { $BundleRoot = Join-Path $PSScriptRoot '../../../artifacts/runtime/native-desktop/win-x64' }
$BundleRoot = [IO.Path]::GetFullPath($BundleRoot).TrimEnd('\', '/')
$manifestPath = Join-Path $PSScriptRoot 'transcription-runtime.json'
$specification = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
$utf8 = [Text.UTF8Encoding]::new($false)
if ($specification.schema -ne 'kynxa-transcription-runtime/v1' -or $specification.runtimeIdentifier -ne 'win-x64') {
    throw 'Unsupported transcription runtime specification.'
}

function Get-TranscriptionPath([string]$Relative) {
    if ([IO.Path]::IsPathRooted($Relative) -or $Relative -match '(^|[\\/])\.\.([\\/]|$)|:') { throw 'Invalid transcription bundle path.' }
    $path = [IO.Path]::GetFullPath((Join-Path $BundleRoot $Relative))
    if (-not $path.StartsWith($BundleRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Transcription path escaped its selected bundle.'
    }
    $ancestor = $path
    while ($ancestor) {
        if (([IO.File]::Exists($ancestor) -or [IO.Directory]::Exists($ancestor)) -and
            ([IO.File]::GetAttributes($ancestor) -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'Transcription bundle and cache paths cannot contain links.'
        }
        $ancestor = [IO.Path]::GetDirectoryName($ancestor)
    }
    return $path
}
function Test-TranscriptionFile([string]$Path, $Asset) {
    return Test-KynxaPreparedFile -Root $BundleRoot -Path $Path -Bytes $Asset.bytes -Sha256 $Asset.sha256 -Cache $preparedFiles
}
function Publish-TranscriptionCopy([string]$Source, [string]$Destination, $Asset) {
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Destination)) | Out-Null
    $pending = Get-TranscriptionPath ($Destination.Substring($BundleRoot.Length + 1) + '.write-' + [Guid]::NewGuid().ToString('N'))
    try {
        [IO.File]::Copy($Source, $pending)
        if (-not (Test-TranscriptionFile $pending $Asset)) { throw 'Transcription source failed pinned integrity verification.' }
        Move-KynxaAtomicFile -Source $pending -Destination $Destination
    }
    finally { if ([IO.File]::Exists($pending)) { Remove-Item -LiteralPath $pending } }
}

# Reuse verified local bytes before considering a download; no test data or user model is needed.
$records = @($specification.files)
$names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($asset in $records) {
    if ($asset.name -notmatch '^[A-Za-z0-9_.-]+$' -or -not $names.Add($asset.name) -or
        $asset.path -cne ('runtime/transcription/' + $asset.name) -or $asset.bytes -le 0 -or $asset.sha256 -notmatch '^[0-9a-f]{64}$' -or
        $asset.kind -notin @('archive', 'model', 'local')) { throw 'Invalid pinned transcription file record.' }
    [void](Get-TranscriptionPath $asset.path)
}
$lockPath = Get-TranscriptionPath '.prepare-transcription.lock'
[IO.Directory]::CreateDirectory($BundleRoot) | Out-Null
$lock = $null
$deadline = [DateTime]::UtcNow.AddMinutes(2)
while (-not $lock) {
    try { $lock = [IO.File]::Open($lockPath, 'OpenOrCreate', 'ReadWrite', 'None') }
    catch [IO.IOException] {
        if ([DateTime]::UtcNow -ge $deadline) { throw 'Another build still owns transcription runtime preparation.' }
        Start-Sleep -Milliseconds 200
    }
}
try {
    $preparedFiles = if ($VerifyResources) { $null } else { Get-KynxaPreparedFileCache -Root $BundleRoot -Component 'transcription' }
    $archiveAssets = @($records | Where-Object { $_.kind -eq 'archive' -and -not (Test-TranscriptionFile (Get-TranscriptionPath $_.path) $_) })
    if ($archiveAssets.Count -gt 0) {
        $archivePath = Get-TranscriptionPath ('downloads/' + $specification.engine.archiveName)
        Save-KynxaPinnedDownload -Url $specification.engine.download -Destination $archivePath -Sha256 $specification.engine.archiveSha256 -ExpectedBytes $specification.engine.archiveBytes -Offline:$Offline -DownloadSourcesPath $DownloadSourcesPath
        $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)
        try {
            foreach ($asset in $archiveAssets) {
                # Extract only the declared member, then hash it before its atomic publication.
                if ($asset.entry -notmatch '^Release/[A-Za-z0-9_.-]+$') { throw 'Invalid transcription archive member.' }
                $entries = @($archive.Entries | Where-Object { $_.FullName -ceq $asset.entry })
                if ($entries.Count -ne 1 -or $entries[0].Length -ne $asset.bytes) { throw 'Pinned transcription archive member missing or different.' }
                $destination = Get-TranscriptionPath $asset.path
                [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination)) | Out-Null
                $pending = Get-TranscriptionPath ($asset.path + '.extract-' + [Guid]::NewGuid().ToString('N'))
                try {
                    $input = $entries[0].Open(); $output = [IO.File]::Create($pending)
                    try { $input.CopyTo($output) } finally { $input.Dispose(); $output.Dispose() }
                    if (-not (Test-TranscriptionFile $pending $asset)) { throw 'Extracted transcription file failed pinned integrity verification.' }
                    Move-KynxaAtomicFile -Source $pending -Destination $destination
                }
                finally { if ([IO.File]::Exists($pending)) { Remove-Item -LiteralPath $pending } }
            }
        }
        finally { $archive.Dispose() }
    }
    foreach ($asset in $records) {
        $destination = Get-TranscriptionPath $asset.path
        if (Test-TranscriptionFile $destination $asset) { continue }
        if ($asset.kind -eq 'model') {
            if ($asset.name -cne $specification.model.name -or $asset.sha256 -cne $specification.model.sha256 -or $asset.bytes -ne $specification.model.bytes) {
                throw 'Transcription model record disagrees with its pinned source.'
            }
            Save-KynxaPinnedDownload -Url $specification.model.download -Destination $destination -Sha256 $asset.sha256 -ExpectedBytes $asset.bytes -Offline:$Offline -DownloadSourcesPath $DownloadSourcesPath
        }
        elseif ($asset.kind -eq 'local') {
            if ($asset.source -notmatch '^native-assets/licenses/transcription/[A-Za-z0-9_.-]+$') { throw 'Invalid transcription local input.' }
            Publish-TranscriptionCopy (Join-Path $PSScriptRoot $asset.source) $destination $asset
        }
        else { throw 'Transcription executable was not prepared.' }
    }
    # Keep provenance beside the distributable files, and publish a list for normal MSBuild packaging.
    $outputManifest = Get-TranscriptionPath 'runtime/transcription/manifest.json'
    $manifestAsset = @{bytes=([IO.FileInfo]::new($manifestPath)).Length;sha256=(Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()}
    if (-not (Test-TranscriptionFile $outputManifest $manifestAsset)) { Publish-TranscriptionCopy $manifestPath $outputManifest $manifestAsset }
    foreach ($asset in $records) {
        if (-not (Test-TranscriptionFile (Get-TranscriptionPath $asset.path) $asset)) { throw 'Transcription runtime is incomplete or changed during preparation.' }
    }
    $preparedRecords = @($records | ForEach-Object { @{path=$_.path;bytes=$_.bytes;sha256=$_.sha256} }) + @(
        @{path='runtime/transcription/manifest.json';bytes=$manifestAsset.bytes;sha256=$manifestAsset.sha256})
    $componentInput = Get-KynxaResourceInputHash -Paths @($PSCommandPath, $manifestPath, (Join-Path $PSScriptRoot 'ResourceConfigCompatibility.psm1'),
        (Join-Path $PSScriptRoot 'RuntimeDownloads.psm1'), (Join-Path $PSScriptRoot 'AtomicFiles.psm1'))
    Save-KynxaPreparedComponent -Root $BundleRoot -Component 'transcription' -InputHash $componentInput -Files $preparedRecords -Scope 'runtime/transcription'
    $fileList = Get-TranscriptionPath 'transcription-files.txt'
    $pendingList = Get-TranscriptionPath ('transcription-files.txt.write-' + [Guid]::NewGuid().ToString('N'))
    try {
        [IO.File]::WriteAllLines($pendingList, [string[]]@($preparedRecords | ForEach-Object { Get-TranscriptionPath $_.path }), $utf8)
        Move-KynxaAtomicFile -Source $pendingList -Destination $fileList
    }
    finally { if ([IO.File]::Exists($pendingList)) { Remove-Item -LiteralPath $pendingList } }
    Write-Host ('[Transcription] Pinned whisper.cpp ' + $specification.engine.version + ' and tiny multilingual model ready: ' + $preparedRecords.Count + ' files.')
}
finally { $lock.Dispose() }
