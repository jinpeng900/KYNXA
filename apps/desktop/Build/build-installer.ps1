[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [string]$PackageVersion = '1.8.6',
    [string]$CompilerPath,
    [string]$DownloadSourcesPath = $env:KYNXA_DOWNLOAD_SOURCES,
    [string]$ModelDownloadSourcesPath,
    [switch]$SkipPublish,
    [switch]$Offline
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
$installerSources = Join-Path $PSScriptRoot 'Installer'
Import-Module (Join-Path $PSScriptRoot 'RuntimeDownloads.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'DesktopBuild.psm1') -Force
$assetManifest = Get-Content -LiteralPath (Join-Path $installerSources 'installer-assets.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$downloadSettings = Get-KynxaDownloadSettings -Path $DownloadSourcesPath
if (-not $ModelDownloadSourcesPath) { $ModelDownloadSourcesPath = Join-Path $installerSources 'model-download-sources.json' }

function Save-InstallerAsset {
    param([string]$Id, [string]$Destination)
    $asset = $assetManifest.assets.PSObject.Properties[$Id].Value
    $sourceUrls = @($asset.sources | ForEach-Object { [string]$_.url })
    if ($downloadSettings.Contains('installerAssets')) {
        $override = $downloadSettings['installerAssets'].PSObject.Properties[$Id]
        if ($override -and $override.Value.PSObject.Properties['sources']) {
            $sourceUrls += @($override.Value.sources | ForEach-Object { [string]$_.url })
        }
    }
    try {
        Save-KynxaPinnedDownload -Url $sourceUrls[0] -SourceUrls $sourceUrls -Destination $Destination -Sha256 $asset.sha256 `
            -ExpectedBytes $asset.bytes -DownloadSourcesPath $DownloadSourcesPath -Offline:$Offline
    }
    catch {
        throw "Installer prerequisite '$Id': $($_.Exception.Message) If the official Evergreen asset changed, verify its publisher and update installer-assets.json as a maintainer."
    }
}
$payloadRoot = Join-Path $repositoryRoot 'artifacts/installer/win-x64/Release/app'
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repositoryRoot 'artifacts/installer/output' }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
[IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null
$toolRoot = Join-Path $repositoryRoot 'artifacts/development/installer-tools'
if (-not $CompilerPath) { $CompilerPath = Join-Path $toolRoot 'InnoSetup/tools/ISCC.exe' }
if (-not (Test-Path -LiteralPath $CompilerPath -PathType Leaf)) {
    [IO.Directory]::CreateDirectory($toolRoot) | Out-Null
    $archive = Join-Path $toolRoot 'tools.innosetup.6.7.3.nupkg'
    Save-InstallerAsset -Id 'inno-setup-6.7.3' -Destination $archive
    # Extract a build tool locally; do not install software, register file associations, or change PATH.
    # 编译工具只解压到项目缓存，不安装系统软件、不注册文件关联、不修改 PATH。
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::ExtractToDirectory($archive, (Join-Path $toolRoot 'InnoSetup'))
}
$signature = Get-AuthenticodeSignature -LiteralPath $CompilerPath
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notlike '*Pyrsys*') {
    throw 'The installer compiler does not have its expected publisher signature.'
}
if (-not $SkipPublish) {
    Import-Module (Join-Path $PSScriptRoot 'DevelopmentEnvironment.psm1') -Force
    $environment = Initialize-KynxaDevelopmentEnvironment -RepositoryRoot $repositoryRoot -Offline:$Offline -DownloadSourcesPath $DownloadSourcesPath
    [IO.Directory]::CreateDirectory($payloadRoot) | Out-Null
    $arguments = @('publish', (Join-Path $repositoryRoot 'apps/desktop/KYNXA.Desktop.csproj'),
        '--configuration', 'Release', '--runtime', 'win-x64', '--no-restore', '--output', $payloadRoot,
        '-p:Platform=x64', '-p:WindowsPackageType=None', '-p:EnableMsixTooling=false',
        '-p:GenerateAppxPackageOnBuild=false', '-p:EnableWinAppRunSupport=false',
        '-p:SelfContained=true', '-p:WindowsAppSDKSelfContained=true', '-p:PublishTrimmed=false',
        '-p:KynxaRuntimeProfile=Full', '-p:KynxaDependenciesPrepared=true')
    if ($Offline) { $arguments += '-p:KynxaOffline=true' }
    if ($DownloadSourcesPath) { $arguments += '-p:KynxaDownloadSourcesPath=' + [IO.Path]::GetFullPath($DownloadSourcesPath) }
    Write-Host '[1/3] Publishing the self-contained Windows x64 application.'
    $publishLogPath = Join-Path $repositoryRoot 'artifacts/development/build/win-x64/Release/Full/installer-publish.log'
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($publishLogPath)) | Out-Null
    Invoke-KynxaBuildCommand $environment.DotNet.Path $arguments $repositoryRoot $publishLogPath '发布完整安装载荷'
}
$required = @('KYNXA.Desktop.exe', 'KYNXA.Desktop.dll', 'coreclr.dll', 'Microsoft.UI.Xaml.dll',
    'KYNXA.Desktop.pri', 'App.xbf', 'MainWindow.xbf', 'Views/ShellPage.xbf',
    'Styles/Colors.xbf', 'Styles/Dimensions.xbf', 'Styles/Icons.xbf', 'Styles/Controls.xbf',
    'Assets/AppIcon.ico', 'Resources/UI/Brand/kynxa-logo.png',
    'runtime/agent/kynxa-gateway.exe', 'runtime/resource/kynxa-resource-service.exe',
    'runtime/embedding/builtin-multilingual/onnx/model_quantized.onnx')
foreach ($relative in $required) {
    if (-not (Test-Path -LiteralPath (Join-Path $payloadRoot $relative) -PathType Leaf)) {
        throw "The release payload is incomplete: $relative"
    }
}
$prerequisiteSource = Join-Path $repositoryRoot 'artifacts/development/installer-prerequisites/MicrosoftEdgeWebView2RuntimeInstallerX64.exe'
Save-InstallerAsset -Id 'webview2-offline-x64' -Destination $prerequisiteSource
$webviewSignature = Get-AuthenticodeSignature -LiteralPath $prerequisiteSource
if ($webviewSignature.Status -ne 'Valid' -or $webviewSignature.SignerCertificate.Subject -notlike '*Microsoft Corporation*') {
    throw 'The WebView2 prerequisite does not have its expected Microsoft signature.'
}
Write-Host '[2/3] Preparing installation configuration and the offline prerequisite.'
$helperRoot = Join-Path $payloadRoot 'installer'
[IO.Directory]::CreateDirectory($helperRoot) | Out-Null
foreach ($name in @('Configure-Kynxa.ps1', 'Launch-Kynxa.ps1', 'Remove-Kynxa.ps1')) {
    Copy-Item -LiteralPath (Join-Path $installerSources $name) -Destination (Join-Path $helperRoot $name) -Force
}
Copy-Item -LiteralPath $ModelDownloadSourcesPath -Destination (Join-Path $helperRoot 'model-download-sources.json') -Force
$prerequisiteRoot = Join-Path $payloadRoot 'prerequisites'
[IO.Directory]::CreateDirectory($prerequisiteRoot) | Out-Null
Copy-Item -LiteralPath $prerequisiteSource -Destination (Join-Path $prerequisiteRoot 'MicrosoftEdgeWebView2RuntimeInstallerX64.exe') -Force
# The catalog is documentation for installed choices, not preinstalled generation weights.
# 模型目录仅说明可安装选项，不把对话模型权重或本机数据混入安装包。
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'apps/agent-core/src/models/local_model_catalog.json') -Destination (Join-Path $helperRoot 'local-model-catalog.json') -Force
Write-Host '[3/3] Compiling the EXE installer without launching it.'
$compilerArguments = @('/Qp', "/DPayloadRoot=$payloadRoot", "/DPackageOutputDir=$OutputDirectory",
    "/DPackageVersion=$PackageVersion", '/DLauncherFile=KYNXA.Desktop.exe',
    ('/DIconFile=' + (Join-Path $repositoryRoot 'apps/desktop/Assets/AppIcon.ico')),
    ('/DBrandImageFile=' + (Join-Path $repositoryRoot 'apps/desktop/Resources/UI/Brand/kynxa-logo.png')),
    (Join-Path $installerSources 'KYNXA.iss'))
& $CompilerPath @compilerArguments
if ($LASTEXITCODE -ne 0) { throw "Installer compilation failed: $LASTEXITCODE" }
$installer = Join-Path $OutputDirectory "KYNXA-$PackageVersion-windows-x64-setup.exe"
$installerInfo = Get-Item -LiteralPath $installer
$releaseSources = @()
if ($downloadSettings.Contains('installerReleaseSources')) {
    foreach ($source in @($downloadSettings['installerReleaseSources'])) {
        if (-not $source.PSObject.Properties['id'] -or -not $source.PSObject.Properties['url'] -or -not $source.PSObject.Properties['status'] -or
            $source.status -notin @('available', 'pending')) {
            throw 'installerReleaseSources entries require id, url and status (available or pending).'
        }
        [void](Get-KynxaDownloadUrls -Url ([string]$source.url) -DownloadSourcesPath '')
        $releaseSources += @{ id=[string]$source.id; url=[string]$source.url; status=[string]$source.status }
    }
}
$origin = (& git -C $repositoryRoot remote get-url origin 2>$null)
if ($LASTEXITCODE -ne 0) { $origin = '' }
$githubRepository = ''
if ($origin -match 'github\.com[:/]([^/]+/[^/]+?)(?:\.git)?$') { $githubRepository = $Matches[1] }
if ($githubRepository -and -not @($releaseSources | Where-Object { $_.id -eq 'github' }).Count) {
    $releaseSources += @{ id='github'; url="https://github.com/$githubRepository/releases/download/v$PackageVersion/$($installerInfo.Name)"; status='pending' }
}
$release = [ordered]@{
    schemaVersion=1; version=$PackageVersion; fileName=$installerInfo.Name; bytes=$installerInfo.Length
    sha256=(Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
    createdUtc=[DateTime]::UtcNow.ToString('o'); installationMode='offline-full'; publicationStatus='not-published'
    sources=@($releaseSources)
}
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'installer-release.json'), ($release | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'RuntimeDownloads.psm1') -Destination (Join-Path $OutputDirectory 'RuntimeDownloads.psm1') -Force
Copy-Item -LiteralPath (Join-Path $installerSources 'Get-KynxaInstaller.ps1') -Destination (Join-Path $OutputDirectory 'Get-KynxaInstaller.ps1') -Force
Get-Item -LiteralPath $installer | Select-Object FullName, Length, LastWriteTime
Write-Host 'Installer created; it has not been executed. Production code signing is not configured.'
