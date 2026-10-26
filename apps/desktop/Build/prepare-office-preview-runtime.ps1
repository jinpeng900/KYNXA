[CmdletBinding()]
param(
    [string]$BundleRoot,
    [string]$DownloadSourcesPath = $env:KYNXA_DOWNLOAD_SOURCES,
    [switch]$Offline,
    [switch]$VerifyResources
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# Nested MSBuild PowerShell invocations may not auto-load the inbox utility module.
# 嵌套 MSBuild PowerShell 调用可能不会自动加载系统工具模块。
Import-Module (Join-Path $PSHOME 'Modules/Microsoft.PowerShell.Utility/Microsoft.PowerShell.Utility.psd1') -Force
Import-Module (Join-Path $PSScriptRoot 'RuntimeDownloads.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'ResourceConfigCompatibility.psm1') -Force
$specification = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'office-preview-runtime.json') -Raw -Encoding UTF8 | ConvertFrom-Json
if (-not $BundleRoot) { $BundleRoot = Join-Path $PSScriptRoot '../../../artifacts/runtime/native-desktop/win-x64' }
$BundleRoot = [IO.Path]::GetFullPath($BundleRoot)
$officeRoot = Join-Path $BundleRoot 'runtime/office'
$recordPath = Join-Path $officeRoot 'runtime.json'
$executablePath = Join-Path $officeRoot $specification.executable
$utf8 = [Text.UTF8Encoding]::new($false)
$componentInput = Get-KynxaResourceInputHash -Paths @($PSCommandPath,
    (Join-Path $PSScriptRoot 'office-preview-runtime.json'), (Join-Path $PSScriptRoot 'ResourceConfigCompatibility.psm1'))
$preparedState = Read-KynxaPreparedComponent -Root $BundleRoot -Component 'office'
if (-not $VerifyResources -and (Test-KynxaPreparedComponent -Root $BundleRoot -Component 'office' -InputHash $componentInput -State $preparedState)) {
    $files = [string[]]@($preparedState['files'] | ForEach-Object { [IO.Path]::Combine($BundleRoot, $_['path']) })
    [IO.File]::WriteAllLines((Join-Path $BundleRoot 'office-preview-files.txt'), $files, $utf8)
    Write-Host ('[Office preview] Reused ' + $files.Count + ' unchanged files; no extraction or content hashing.')
    return
}
$preparedFiles = if ($VerifyResources) { $null } else { Get-KynxaPreparedFileCache -Root $BundleRoot -Component 'office' }
$ready = $false
if ([IO.File]::Exists($recordPath) -and [IO.File]::Exists($executablePath)) {
    $record = Get-Content -LiteralPath $recordPath -Raw -Encoding UTF8 | ConvertFrom-Json
    $ready = $record.version -eq $specification.version -and $record.archiveSha256 -eq $specification.sha256
}
if ($ready -and $preparedState) {
    foreach ($file in $preparedState['files']) {
        if (-not (Test-KynxaPreparedFile -Root $BundleRoot -Path ([IO.Path]::Combine($BundleRoot, $file['path'])) -Bytes $file['bytes'] -Sha256 $file['sha256'] -Cache $preparedFiles)) { $ready = $false; break }
    }
}
if (-not $ready) {
    $archive = Join-Path $BundleRoot ('downloads/LibreOffice_' + $specification.version + '_Win_x86-64.msi')
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($archive)) | Out-Null
    Save-KynxaPinnedDownload -Url $specification.url -Destination $archive -Sha256 $specification.sha256 -ExpectedBytes $specification.bytes -Offline:$Offline -DownloadSourcesPath $DownloadSourcesPath
    $staging = [IO.Path]::GetFullPath((Join-Path $BundleRoot ('office-extract/' + $specification.version)))
    [IO.Directory]::CreateDirectory($staging) | Out-Null
    $log = Join-Path $BundleRoot 'office-preview-extract.log'
    $sourceInput = Get-KynxaResourceInputHash -Values @($specification.version, $specification.sha256)
    $sourceState = Read-KynxaPreparedComponent -Root $staging -Component 'office-source'
    if (-not (Test-KynxaPreparedComponent -Root $staging -Component 'office-source' -InputHash $sourceInput -State $sourceState)) {
        # Administrative extraction prepares a private payload; no system installation or PATH change.
        # 管理提取只准备应用私有文件，不安装到系统，也不修改 PATH。
        Write-Host ('[Office preview] Extracting LibreOffice ' + $specification.version + ' into the application runtime.')
        $arguments = '/a "' + $archive + '" /qn TARGETDIR="' + $staging + '" /L*v "' + $log + '"'
        $process = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32/msiexec.exe') -ArgumentList $arguments -WindowStyle Hidden -Wait -PassThru
        if ($process.ExitCode -notin @(0, 3010)) { throw "Office preview extraction failed: $($process.ExitCode). See $log" }
        $entry = Get-ChildItem -LiteralPath $staging -Filter 'soffice.exe' -File -Recurse | Where-Object { $_.Directory.Name -eq 'program' } | Select-Object -First 1
        if (-not $entry) { throw 'The Office preview archive did not contain program/soffice.exe.' }
        $sourceRoot = $entry.Directory.Parent.FullName
        $sourceRecords = @(Get-ChildItem -LiteralPath $sourceRoot -File -Recurse | ForEach-Object {
            @{path=$_.FullName.Substring($staging.Length + 1).Replace('\','/');bytes=$_.Length;sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()}
        })
        Save-KynxaPreparedComponent -Root $staging -Component 'office-source' -InputHash $sourceInput -Files $sourceRecords
        $sourceState = Read-KynxaPreparedComponent -Root $staging -Component 'office-source'
    }
    # Administrative extraction may place program/ at the staging root or inside a product folder.
    # 管理提取可能将 program/ 直接放在暂存根目录，也可能放在产品子目录中。
    $relativeExecutable = $specification.executable.Replace('\','/')
    $sourceEntry = @($sourceState['files'] | Where-Object {
        $relativePath = $_['path'].Replace('\','/')
        $relativePath.Equals($relativeExecutable, [StringComparison]::OrdinalIgnoreCase) -or
            $relativePath.EndsWith('/' + $relativeExecutable, [StringComparison]::OrdinalIgnoreCase)
    })[0]
    $sourceRoot = [IO.Directory]::GetParent([IO.Path]::GetDirectoryName([IO.Path]::Combine($staging, $sourceEntry['path']))).FullName
    [IO.Directory]::CreateDirectory($officeRoot) | Out-Null
    foreach ($file in $sourceState['files']) {
        $sourcePath = [IO.Path]::Combine($staging, $file['path'])
        $relative = $sourcePath.Substring($sourceRoot.Length + 1)
        $destination = [IO.Path]::Combine($officeRoot, $relative)
        if (-not (Test-KynxaPreparedFile -Root $BundleRoot -Path $destination -Bytes $file['bytes'] -Sha256 $file['sha256'] -Cache $preparedFiles)) {
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination)) | Out-Null
            Copy-Item -LiteralPath $sourcePath -Destination $destination -Force
        }
    }
    $record = @{version=$specification.version;archiveSha256=$specification.sha256;source=$specification.url;executable=$specification.executable}
    [IO.File]::WriteAllText($recordPath, ($record | ConvertTo-Json), $utf8)
}
$records = @(Get-ChildItem -LiteralPath $officeRoot -File -Recurse | ForEach-Object {
    $relative = $_.FullName.Substring($BundleRoot.Length + 1).Replace('\','/')
    $hash = if ($preparedFiles -and $preparedFiles.ContainsKey($relative) -and
        (Test-KynxaPreparedFile -Root $BundleRoot -Path $_.FullName -Bytes $_.Length -Sha256 $preparedFiles[$relative]['sha256'] -Cache $preparedFiles)) {
        $preparedFiles[$relative]['sha256']
    } else { (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
    @{path=$relative;bytes=$_.Length;sha256=$hash}
})
$files = @($records | ForEach-Object { [IO.Path]::Combine($BundleRoot, $_.path) })
[IO.File]::WriteAllLines((Join-Path $BundleRoot 'office-preview-files.txt'), [string[]]$files, $utf8)
Save-KynxaPreparedComponent -Root $BundleRoot -Component 'office' -InputHash $componentInput -Files $records -Scope 'runtime/office'
Write-Host ('[Office preview] Private converter ready: ' + $specification.version + ', ' + $files.Count + ' files.')
