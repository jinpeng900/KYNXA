[CmdletBinding(DefaultParameterSetName = 'Configure')]
param(
    [Parameter(Mandatory = $true)][string]$ApplicationRoot,
    [Parameter(Mandatory = $true, ParameterSetName = 'Configure')][string]$DataRoot,
    [Parameter(Mandatory = $true, ParameterSetName = 'Configure')][string]$ExtensionRoot,
    [ValidateSet('zh-CN', 'en')][string]$Language = 'zh-CN',
    [string]$ModelId,
    [ValidateSet('auto', 'official', 'mirror', 'github')][string]$DownloadSource,
    [string]$SourceCatalogPath,
    [Parameter(Mandatory = $true, ParameterSetName = 'Read')][switch]$ReadConfiguration,
    [Parameter(Mandatory = $true, ParameterSetName = 'Read')][string]$OutputFile
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function ConvertTo-InstallationPath([string]$Path, [string]$BaseDirectory) {
    $absolute = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    $base = [IO.Path]::GetFullPath($BaseDirectory).TrimEnd('\')
    if ($absolute.Equals($base, [StringComparison]::OrdinalIgnoreCase)) { return '.' }
    if ($absolute.StartsWith($base + '\', [StringComparison]::OrdinalIgnoreCase)) {
        return $absolute.Substring($base.Length + 1)
    }
    return $absolute
}

function Set-InstallerJson([string]$Path, [object]$Value) {
    $temporary = $Path + '.install.tmp'
    [IO.File]::WriteAllText($temporary, ($Value | ConvertTo-Json -Depth 12), [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporary -Destination $Path -Force
}

function Get-InstallerSourceSettings([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return @{} }
    $document = Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($document.schemaVersion -ne 1) { throw 'Unsupported KYNXA download source configuration.' }
    $settings = @{}
    foreach ($property in $document.PSObject.Properties) { $settings[$property.Name] = $property.Value }
    return $settings
}

function Merge-InstallerSourceSettings([hashtable]$Template, [hashtable]$Existing, [string]$Preferred) {
    # Upgrade only missing mappings; an existing custom mirror or asset list remains authoritative.
    # 升级只补缺失映射，保留已有自定义镜像和发布附件列表，不改动下载片段。
    $merged = @{}
    foreach ($key in $Existing.Keys) { $merged[$key] = $Existing[$key] }
    $merged['schemaVersion'] = 1
    foreach ($section in @('mirrors', 'assets')) {
        $entries = @{}
        foreach ($settings in @($Template, $Existing)) {
            if (-not $settings.ContainsKey($section) -or $null -eq $settings[$section]) { continue }
            foreach ($property in $settings[$section].PSObject.Properties) { $entries[$property.Name] = $property.Value }
        }
        $merged[$section] = $entries
    }
    $merged['preferred'] = if (-not [string]::IsNullOrWhiteSpace($Preferred)) { $Preferred }
        elseif ($Existing.ContainsKey('preferred') -and $Existing['preferred'] -in @('auto', 'official', 'mirror', 'github')) { $Existing['preferred'] }
        else { 'auto' }
    return $merged
}

function Test-InstallerGithubSources([hashtable]$Settings) {
    if (-not $Settings.ContainsKey('assets') -or $null -eq $Settings['assets']) { return $false }
    $assets = $Settings['assets']
    $values = if ($assets -is [Collections.IDictionary]) { @($assets.Values) }
        else { @($assets.PSObject.Properties | ForEach-Object { $_.Value }) }
    foreach ($urls in $values) {
        foreach ($url in @($urls)) {
            $uri = $null
            if ([Uri]::TryCreate([string]$url, [UriKind]::Absolute, [ref]$uri) -and
                $uri.Scheme -eq 'https' -and $uri.Host -eq 'github.com' -and
                $uri.AbsolutePath.Contains('/releases/download/')) { return $true }
        }
    }
    return $false
}

$ApplicationRoot = [IO.Path]::GetFullPath($ApplicationRoot).TrimEnd('\')
if ($ReadConfiguration) {
    $values = [ordered]@{}
    $configurationPath = Join-Path $ApplicationRoot 'kynxa-installation.json'
    $configuredData = Join-Path $ApplicationRoot 'Data'
    if (Test-Path -LiteralPath $configurationPath -PathType Leaf) {
        $configuration = Get-Content -LiteralPath $configurationPath -Raw -Encoding UTF8 | ConvertFrom-Json
        if ($configuration.version -ne 1) { throw 'Unsupported KYNXA installation configuration.' }
        $configuredData = [IO.Path]::GetFullPath([IO.Path]::Combine($ApplicationRoot, $configuration.dataRoot))
        $configuredExtensions = [IO.Path]::GetFullPath([IO.Path]::Combine($ApplicationRoot, $configuration.extensionRoot))
        $configuredPointers = [IO.Path]::GetFullPath([IO.Path]::Combine($ApplicationRoot, $configuration.pointerDirectory))
        $dataPointer = Join-Path $configuredPointers 'storage.json'
        $toolsPointer = Join-Path $configuredPointers 'extensions.json'
        if (Test-Path -LiteralPath $dataPointer -PathType Leaf) {
            $storedData = Get-Content -LiteralPath $dataPointer -Raw -Encoding UTF8 | ConvertFrom-Json
            if ($storedData.version -ne 1) { throw 'Unsupported KYNXA storage pointer.' }
            $configuredData = [IO.Path]::GetFullPath([IO.Path]::Combine($configuredPointers, $storedData.dataRoot))
        }
        if (Test-Path -LiteralPath $toolsPointer -PathType Leaf) {
            $storedTools = Get-Content -LiteralPath $toolsPointer -Raw -Encoding UTF8 | ConvertFrom-Json
            if ($storedTools.version -ne 1) { throw 'Unsupported KYNXA tools pointer.' }
            $configuredExtensions = [IO.Path]::GetFullPath([IO.Path]::Combine($configuredPointers, $storedTools.extensionRoot))
        }
        $values['DataRoot'] = $configuredData
        $values['ExtensionRoot'] = $configuredExtensions
        $values['Language'] = $configuration.language
    }
    $existingSources = Get-InstallerSourceSettings (Join-Path $configuredData 'Models/download-sources.json')
    $templateSources = if ($SourceCatalogPath) { Get-InstallerSourceSettings $SourceCatalogPath } else { @{} }
    $sourceSettings = Merge-InstallerSourceSettings $templateSources $existingSources ''
    $values['DownloadSource'] = $sourceSettings['preferred']
    $values['HasGithubSources'] = if (Test-InstallerGithubSources $sourceSettings) { '1' } else { '0' }
    $installerIni = "[Kynxa]`r`n"
    foreach ($key in $values.Keys) { $installerIni += "$key=$($values[$key])`r`n" }
    [IO.File]::WriteAllText([IO.Path]::GetFullPath($OutputFile), $installerIni, [Text.Encoding]::Unicode)
    return
}
$configurationLogDirectory = Join-Path $ApplicationRoot 'installer'
[IO.Directory]::CreateDirectory($configurationLogDirectory) | Out-Null
Start-Transcript -Path (Join-Path $configurationLogDirectory 'configuration.log') -Append | Out-Null
$DataRoot = [IO.Path]::GetFullPath($DataRoot).TrimEnd('\')
$ExtensionRoot = [IO.Path]::GetFullPath($ExtensionRoot).TrimEnd('\')
if (-not (Test-Path -LiteralPath (Join-Path $ApplicationRoot 'KYNXA.Desktop.exe') -PathType Leaf)) {
    throw 'KYNXA.Desktop.exe is missing from the selected installation directory.'
}
foreach ($directory in @($DataRoot, $ExtensionRoot)) {
    if ($directory -eq [IO.Path]::GetPathRoot($directory).TrimEnd('\')) {
        throw 'Choose a dedicated data or tools folder instead of a drive root.'
    }
    [IO.Directory]::CreateDirectory($directory) | Out-Null
}
$installerDirectory = Join-Path $DataRoot 'Installer'
[IO.Directory]::CreateDirectory($installerDirectory) | Out-Null
[IO.Directory]::CreateDirectory((Join-Path $DataRoot 'Desktop')) | Out-Null
[IO.Directory]::CreateDirectory((Join-Path $DataRoot 'Models')) | Out-Null

# Keep editable pointers outside a protected application directory; never overwrite a migrated user's pointers.
# 可修改指针位于用户选择的数据目录，不写入受保护的软件目录，也不覆盖用户已有迁移结果。
$storagePointer = Join-Path $installerDirectory 'storage.json'
if (-not (Test-Path -LiteralPath $storagePointer)) {
    Set-InstallerJson $storagePointer @{ version = 1; dataRoot = '..' }
}
$extensionPointer = Join-Path $installerDirectory 'extensions.json'
if (-not (Test-Path -LiteralPath $extensionPointer)) {
    $relativeExtensions = if ($ExtensionRoot.StartsWith($ApplicationRoot + '\', [StringComparison]::OrdinalIgnoreCase) -and
        $DataRoot.StartsWith($ApplicationRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
        $baseUri = [Uri]::new($installerDirectory.TrimEnd('\') + '\')
        [Uri]::UnescapeDataString($baseUri.MakeRelativeUri([Uri]::new($ExtensionRoot + '\')).ToString()).Replace('/', '\').TrimEnd('\')
    } else { $ExtensionRoot }
    Set-InstallerJson $extensionPointer @{ version = 1; extensionRoot = $relativeExtensions }
}
Set-InstallerJson (Join-Path $ApplicationRoot 'kynxa-installation.json') @{
    version = 1
    dataRoot = ConvertTo-InstallationPath $DataRoot $ApplicationRoot
    extensionRoot = ConvertTo-InstallationPath $ExtensionRoot $ApplicationRoot
    pointerDirectory = ConvertTo-InstallationPath $installerDirectory $ApplicationRoot
    language = $Language
}

# Initial language is an installer preference; upgrades retain the user's complete layout and language.
# 安装语言仅初始化新用户布局，升级保留已有完整布局及用户语言。
$activeDataPointer = Get-Content -LiteralPath $storagePointer -Raw -Encoding UTF8 | ConvertFrom-Json
if ($activeDataPointer.version -ne 1) { throw 'Unsupported KYNXA storage pointer.' }
$effectiveDataRoot = [IO.Path]::GetFullPath([IO.Path]::Combine($installerDirectory, $activeDataPointer.dataRoot))
[IO.Directory]::CreateDirectory((Join-Path $effectiveDataRoot 'Desktop')) | Out-Null
# The installed application's ModelHome follows the effective data pointer, including user migration.
# 安装版 ModelHome 跟随有效数据指针，数据迁移后仍写入实际 Models 目录，不继承开发机环境变量。
$effectiveModelHome = Join-Path $effectiveDataRoot 'Models'
[IO.Directory]::CreateDirectory($effectiveModelHome) | Out-Null
$downloadSourcesPath = Join-Path $effectiveModelHome 'download-sources.json'
$existingSources = Get-InstallerSourceSettings $downloadSourcesPath
if (-not $SourceCatalogPath) { $SourceCatalogPath = Join-Path $ApplicationRoot 'installer/model-download-sources.json' }
$templateSources = Get-InstallerSourceSettings $SourceCatalogPath
$sourceSettings = Merge-InstallerSourceSettings $templateSources $existingSources $DownloadSource
if ($sourceSettings['preferred'] -eq 'github' -and -not (Test-InstallerGithubSources $sourceSettings)) {
    $sourceSettings['preferred'] = 'auto'
}
Set-InstallerJson $downloadSourcesPath $sourceSettings
$layoutPath = Join-Path $effectiveDataRoot 'Desktop/layout.json'
if (-not (Test-Path -LiteralPath $layoutPath)) {
    Set-InstallerJson $layoutPath @{ LayoutVersion = 4; InterfaceLanguage = $Language }
}
if (-not [string]::IsNullOrWhiteSpace($ModelId)) {
    if ($ModelId -notmatch '^[a-z0-9-]{1,28}$') { throw 'The selected local model identifier is invalid.' }
    $modelRequestDirectory = Join-Path $effectiveDataRoot 'Installer'
    [IO.Directory]::CreateDirectory($modelRequestDirectory) | Out-Null
    Set-InstallerJson (Join-Path $modelRequestDirectory 'local-model-request.json') @{
        version = 1; modelId = $ModelId; status = 'pending'; enableAfterInstall = $true
        requestedAt = [DateTimeOffset]::UtcNow.ToString('O')
    }
}
Write-Output 'KYNXA installation paths and initial preferences are ready.'
Stop-Transcript | Out-Null
