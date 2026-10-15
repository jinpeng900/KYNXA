[CmdletBinding()]
param(
    [string]$BundleRoot,
    [string]$DownloadSourcesPath = $env:KYNXA_DOWNLOAD_SOURCES,
    [switch]$Offline
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'RuntimeDownloads.psm1') -Force
$specification = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'office-preview-runtime.json') -Raw -Encoding UTF8 | ConvertFrom-Json
if (-not $BundleRoot) { $BundleRoot = Join-Path $PSScriptRoot '../../../artifacts/runtime/native-desktop/win-x64' }
$BundleRoot = [IO.Path]::GetFullPath($BundleRoot)
$officeRoot = Join-Path $BundleRoot 'runtime/office'
$recordPath = Join-Path $officeRoot 'runtime.json'
$executablePath = Join-Path $officeRoot $specification.executable
$utf8 = [Text.UTF8Encoding]::new($false)
$ready = $false
if ([IO.File]::Exists($recordPath) -and [IO.File]::Exists($executablePath)) {
    $record = Get-Content -LiteralPath $recordPath -Raw -Encoding UTF8 | ConvertFrom-Json
    $ready = $record.version -eq $specification.version -and $record.archiveSha256 -eq $specification.sha256
}
if (-not $ready) {
    $archive = Join-Path $BundleRoot ('downloads/LibreOffice_' + $specification.version + '_Win_x86-64.msi')
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($archive)) | Out-Null
    Save-KynxaPinnedDownload -Url $specification.url -Destination $archive -Sha256 $specification.sha256 -ExpectedBytes $specification.bytes -Offline:$Offline -DownloadSourcesPath $DownloadSourcesPath
    $staging = [IO.Path]::GetFullPath((Join-Path $BundleRoot ('office-extract-' + [Guid]::NewGuid().ToString('N'))))
    [IO.Directory]::CreateDirectory($staging) | Out-Null
    $log = Join-Path $BundleRoot 'office-preview-extract.log'
    try {
        # Administrative extraction prepares a private payload; no system installation or PATH change.
        # 管理提取只准备应用私有文件，不安装到系统，也不修改 PATH。
        Write-Host ('[Office preview] Extracting LibreOffice ' + $specification.version + ' into the application runtime.')
        $arguments = '/a "' + $archive + '" /qn TARGETDIR="' + $staging + '" /L*v "' + $log + '"'
        $process = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32/msiexec.exe') -ArgumentList $arguments -WindowStyle Hidden -Wait -PassThru
        if ($process.ExitCode -notin @(0, 3010)) { throw "Office preview extraction failed: $($process.ExitCode). See $log" }
        $entry = Get-ChildItem -LiteralPath $staging -Filter 'soffice.exe' -File -Recurse | Where-Object { $_.Directory.Name -eq 'program' } | Select-Object -First 1
        if (-not $entry) { throw 'The Office preview archive did not contain program/soffice.exe.' }
        $sourceRoot = $entry.Directory.Parent.FullName
        [IO.Directory]::CreateDirectory($officeRoot) | Out-Null
        Get-ChildItem -LiteralPath $sourceRoot -Force | ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $officeRoot -Recurse -Force }
        $record = @{version=$specification.version;archiveSha256=$specification.sha256;source=$specification.url;executable=$specification.executable}
        [IO.File]::WriteAllText($recordPath, ($record | ConvertTo-Json), $utf8)
    }
    finally {
        $cachePrefix = $BundleRoot.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
        if ($staging.StartsWith($cachePrefix, [StringComparison]::OrdinalIgnoreCase) -and [IO.Directory]::Exists($staging)) {
            Remove-Item -LiteralPath $staging -Recurse -Force
        }
    }
}
$files = @(Get-ChildItem -LiteralPath $officeRoot -File -Recurse | ForEach-Object { $_.FullName })
[IO.File]::WriteAllLines((Join-Path $BundleRoot 'office-preview-files.txt'), [string[]]$files, $utf8)
Write-Host ('[Office preview] Private converter ready: ' + $specification.version + ', ' + $files.Count + ' files.')
