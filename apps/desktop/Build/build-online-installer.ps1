[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [string]$ReleaseManifestPath,
    [string]$PackageVersion = '1.8.8',
    [string]$CompilerPath,
    [switch]$AllowUnpublished
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
$installerSources = Join-Path $PSScriptRoot 'Installer'
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repositoryRoot 'artifacts/installer/output' }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (-not $ReleaseManifestPath) { $ReleaseManifestPath = Join-Path $OutputDirectory 'installer-release.json' }
$ReleaseManifestPath = [IO.Path]::GetFullPath($ReleaseManifestPath)
if (-not $CompilerPath) { $CompilerPath = Join-Path $repositoryRoot 'artifacts/development/installer-tools/InnoSetup/tools/ISCC.exe' }

# The bootstrapper consumes an existing pinned release; it never rebuilds or silently republishes the full application.
# 在线引导器只复用已生成的固定版本发布清单，不重编应用，也不隐式发布完整安装包。
$release = Get-Content -LiteralPath $ReleaseManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
$expectedFileName = "KYNXA-$PackageVersion-windows-x64-setup.exe"
if ($PackageVersion -notmatch '^\d+\.\d+\.\d+$' -or $release.schemaVersion -ne 1 -or
    $release.version -ne $PackageVersion -or $release.fileName -ne $expectedFileName -or
    $release.fileName -ne [IO.Path]::GetFileName($release.fileName) -or
    $release.sha256 -notmatch '^[0-9a-fA-F]{64}$' -or [long]$release.bytes -le 0 -or
    $release.installationMode -ne 'offline-full') {
    throw 'The release manifest must describe the matching Windows x64 full installer, with its fixed byte count and SHA-256.'
}
$sources = @($release.sources)
if ($sources.Count -eq 0) { throw 'The release manifest has no download sources.' }
$sourceIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($source in $sources) {
    if (-not $source.PSObject.Properties['id'] -or -not $source.PSObject.Properties['url'] -or
        -not $source.PSObject.Properties['status'] -or [string]::IsNullOrWhiteSpace([string]$source.id) -or
        -not $sourceIds.Add([string]$source.id) -or $source.status -notin @('available', 'pending')) {
        throw 'Every source requires a unique id, URL, and available or pending status.'
    }
    $sourceUri = $null
    if (-not [Uri]::TryCreate([string]$source.url, [UriKind]::Absolute, [ref]$sourceUri) -or
        $sourceUri.Scheme -ne 'https' -or $sourceUri.UserInfo -or $sourceUri.Fragment) {
        throw "The installer source '$($source.id)' requires an HTTPS URL without credentials or fragments."
    }
}
$availableSources = @($sources | Where-Object { $_.status -eq 'available' })
if ($availableSources.Count -eq 0 -and -not $AllowUnpublished) {
    throw 'No published installer source is available. Publish the full installer and update its release manifest before building the online installer. -AllowUnpublished is only for compiling an unpublished template.'
}
if ($availableSources.Count -eq 0) {
    Write-Warning 'Compiling an unpublished template. Its download sources are still pending; this is not a ready online release.'
}
if (-not (Test-Path -LiteralPath $CompilerPath -PathType Leaf)) {
    throw 'The portable Inno Setup compiler is missing. Provide -CompilerPath for an existing compiler.'
}
$signature = Get-AuthenticodeSignature -LiteralPath $CompilerPath
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notlike '*Pyrsys*') {
    throw 'The installer compiler does not have its expected publisher signature.'
}
$payloadRoot = Join-Path $repositoryRoot "artifacts/installer/win-x64/Online/$PackageVersion"
[IO.Directory]::CreateDirectory($payloadRoot) | Out-Null
[IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null
foreach ($name in @('Start-KynxaOnline.ps1', 'Get-KynxaInstaller.ps1')) {
    Copy-Item -LiteralPath (Join-Path $installerSources $name) -Destination (Join-Path $payloadRoot $name) -Force
}
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'RuntimeDownloads.psm1') -Destination (Join-Path $payloadRoot 'RuntimeDownloads.psm1') -Force
Copy-Item -LiteralPath $ReleaseManifestPath -Destination (Join-Path $payloadRoot 'installer-release.json') -Force
Write-Host "Compiling the online installer for $PackageVersion; $($availableSources.Count) published source(s)."
$compilerArguments = @('/Qp', "/DOnlinePayloadRoot=$payloadRoot", "/DPackageOutputDir=$OutputDirectory",
    "/DPackageVersion=$PackageVersion",
    ('/DIconFile=' + (Join-Path $repositoryRoot 'apps/desktop/Assets/AppIcon.ico')),
    ('/DBrandImageFile=' + (Join-Path $repositoryRoot 'apps/desktop/Resources/UI/Brand/kynxa-logo.png')),
    (Join-Path $installerSources 'KYNXA.Online.iss'))
& $CompilerPath @compilerArguments
if ($LASTEXITCODE -ne 0) { throw "Online installer compilation failed: $LASTEXITCODE" }
$installerPath = Join-Path $OutputDirectory "KYNXA-$PackageVersion-windows-x64-online-setup.exe"
$installer = Get-Item -LiteralPath $installerPath
$buildRecord = [ordered]@{
    schemaVersion = 1
    version = $PackageVersion
    fileName = $installer.Name
    bytes = $installer.Length
    downloadFileName = $release.fileName
    downloadBytes = [long]$release.bytes
    publishedSourceIds = @($availableSources | ForEach-Object { [string]$_.id })
    isDownloadReady = ($availableSources.Count -gt 0)
    createdUtc = [DateTime]::UtcNow.ToString('o')
}
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'online-installer-build.json'), ($buildRecord | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
Get-Item -LiteralPath $installerPath | Select-Object FullName, Length, LastWriteTime
Write-Host 'Online installer compiled without running it. Production code signing is not configured.'
