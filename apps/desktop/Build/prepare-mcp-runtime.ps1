[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$BundleRoot,
    [ValidateSet('win-x64')][string]$RuntimeIdentifier = 'win-x64',
    [string]$DownloadSourcesPath = $env:KYNXA_DOWNLOAD_SOURCES,
    [string]$NpmRegistry,
    [string]$PlaywrightDownloadHost,
    [switch]$Offline
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'RuntimeDownloads.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'ResourceConfigCompatibility.psm1') -Force
Add-Type -AssemblyName System.IO.Compression.FileSystem
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
$bundleRootPath = [IO.Path]::GetFullPath($BundleRoot)
$cacheRoot = Join-Path $repositoryRoot ('artifacts/runtime/mcp/' + $RuntimeIdentifier)
$sourceRoot = Join-Path $PSScriptRoot 'native-assets/mcp'
$manifestPath = Join-Path $PSScriptRoot 'native-assets/mcp-runtime.json'
$utf8 = [Text.UTF8Encoding]::new($false)
$downloadSettings = Get-KynxaDownloadSettings -Path $DownloadSourcesPath
if (-not $NpmRegistry) { $NpmRegistry = $env:KYNXA_NPM_REGISTRY }
if (-not $NpmRegistry -and $downloadSettings.Contains('npmRegistry')) { $NpmRegistry = [string]$downloadSettings['npmRegistry'] }
if (-not $NpmRegistry) { $NpmRegistry = 'https://registry.npmjs.org/' }
if (-not $PlaywrightDownloadHost) { $PlaywrightDownloadHost = $env:KYNXA_PLAYWRIGHT_DOWNLOAD_HOST }
if (-not $PlaywrightDownloadHost -and $downloadSettings.Contains('playwrightDownloadHost')) { $PlaywrightDownloadHost = [string]$downloadSettings['playwrightDownloadHost'] }
foreach ($configuredSource in @($NpmRegistry, $PlaywrightDownloadHost)) {
    if ($configuredSource -and ([Uri]$configuredSource).Scheme -ne 'https') { throw 'MCP download sources must use HTTPS.' }
}

function Read-McpJson([string]$Path) {
    $raw = [IO.File]::ReadAllText($Path, $utf8)
    if ($PSVersionTable.PSVersion.Major -ge 6) { return ($raw | ConvertFrom-Json -AsHashtable) }
    Add-Type -AssemblyName System.Web.Extensions
    $reader = [Web.Script.Serialization.JavaScriptSerializer]::new()
    $reader.MaxJsonLength = 32 * 1024 * 1024
    return $reader.DeserializeObject($raw)
}
function Assert-McpPath([string]$Root, [string]$Path) {
    $resolvedRoot = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
    $resolved = [IO.Path]::GetFullPath($Path)
    if ($resolved -ne $resolvedRoot -and -not $resolved.StartsWith($resolvedRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'MCP build path escaped its explicitly selected root.'
    }
    $ancestor = $resolved
    while ($ancestor) {
        if (([IO.File]::Exists($ancestor) -or [IO.Directory]::Exists($ancestor)) -and
            ([IO.File]::GetAttributes($ancestor) -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'MCP payload and cache paths cannot contain reparse points.'
        }
        $ancestor = [IO.Path]::GetDirectoryName($ancestor)
    }
    return $resolved
}
function Get-McpHash([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($hasher.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $hasher.Dispose(); $stream.Dispose() }
}
function Write-McpJson([string]$Path, $Value) {
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path)) | Out-Null
    [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 20), $utf8)
}

$cacheRoot = Assert-McpPath $repositoryRoot $cacheRoot
$payloadRoot = Assert-McpPath $bundleRootPath (Join-Path $bundleRootPath 'runtime/mcp')
[IO.Directory]::CreateDirectory($cacheRoot) | Out-Null
[IO.Directory]::CreateDirectory($payloadRoot) | Out-Null
$manifest = Read-McpJson $manifestPath
if ($manifest.schemaVersion -ne 1 -or $manifest.runtimeIdentifier -ne $RuntimeIdentifier) { throw 'Unsupported private MCP runtime manifest.' }
$sourceHash = Get-McpHash $manifestPath
$lockHash = Get-McpHash (Join-Path $sourceRoot 'package-lock.json')
$packageHash = Get-McpHash (Join-Path $sourceRoot 'package.json')
$outputManifest = Join-Path $payloadRoot 'manifest.json'
$integrityPath = Join-Path $payloadRoot 'payload-integrity.json'
$fileList = Join-Path $bundleRootPath 'mcp-files.txt'

# The build lock protects one immutable vendor payload; user extensions are never installed here.
# 构建锁保护固定的第三方随包资源；用户扩展不在此处安装，也不共享用户 npm 配置。
$cacheLock = $null
$lockDeadline = [DateTime]::UtcNow.AddMinutes(2)
while (-not $cacheLock) {
    try { $cacheLock = [IO.File]::Open((Join-Path $cacheRoot '.prepare.lock'), 'OpenOrCreate', 'ReadWrite', 'None') }
    catch [IO.IOException] {
        if ([DateTime]::UtcNow -ge $lockDeadline) { throw 'MCP payload preparation lock timed out.' }
        Start-Sleep -Milliseconds 200
    }
}
try {
    if ([IO.File]::Exists($outputManifest) -and [IO.File]::Exists($integrityPath)) {
        $previous = Read-McpJson $outputManifest
        $isCurrent = (Test-KynxaConfigHash -Path $manifestPath -ExpectedHash $previous.sourceHash) -and
            (Test-KynxaConfigHash -Path (Join-Path $sourceRoot 'package-lock.json') -ExpectedHash $previous.lockSha256) -and
            (Test-KynxaConfigHash -Path (Join-Path $sourceRoot 'package.json') -ExpectedHash $previous.packageSha256)
        $verified = [Collections.Generic.List[string]]::new()
        if ($isCurrent) {
            foreach ($directory in Get-ChildItem -LiteralPath $payloadRoot -Directory -Recurse -Force) {
                if ($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Cached MCP payload directories cannot contain links.' }
            }
            foreach ($file in (Read-McpJson $integrityPath).files) {
                if ($file.path.StartsWith('runtime/mcp/services/.playwright-mcp/', [StringComparison]::OrdinalIgnoreCase)) { $isCurrent = $false; break }
                $path = [IO.Path]::GetFullPath((Join-Path $bundleRootPath $file.path))
                if (-not $path.StartsWith($payloadRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Cached MCP file escaped the verified payload tree.' }
                if (-not [IO.File]::Exists($path) -or ([IO.File]::GetAttributes($path) -band [IO.FileAttributes]::ReparsePoint) -or
                    ([IO.FileInfo]::new($path)).Length -ne $file.bytes -or (Get-McpHash $path) -ne $file.sha256) { $isCurrent = $false; break }
                $verified.Add($path)
            }
        }
        if ($isCurrent) {
            & (Join-Path $PSScriptRoot 'prepare-browser-sandbox.ps1') -BundleRoot $bundleRootPath
            $verified.Add($outputManifest); $verified.Add($integrityPath)
            [IO.File]::WriteAllLines($fileList, [string[]]$verified, $utf8)
            Write-Output "Private MCP payload reused: $($verified.Count) verified files; no package installation."
            return
        }
    }
    $node = $manifest.node
    if ($node.version -notmatch '^\d+\.\d+\.\d+$' -or $node.archiveSha256 -notmatch '^[0-9a-f]{64}$' -or $node.executableSha256 -notmatch '^[0-9a-f]{64}$') { throw 'Invalid fixed Node identity.' }
    $downloads = Assert-McpPath $cacheRoot (Join-Path $cacheRoot 'downloads')
    [IO.Directory]::CreateDirectory($downloads) | Out-Null
    $archivePath = Assert-McpPath $cacheRoot (Join-Path $downloads $node.archive)
    $archiveBytes = if ($node.Keys -contains 'archiveBytes') { [long]$node.archiveBytes } else { [long]0 }
    Save-KynxaPinnedDownload -Url $node.url -Destination $archivePath -Sha256 $node.archiveSha256 -ExpectedBytes $archiveBytes -Offline:$Offline -DownloadSourcesPath $DownloadSourcesPath
    $nodeCache = Assert-McpPath $cacheRoot (Join-Path $cacheRoot ('node-' + $node.version))
    [IO.Directory]::CreateDirectory($nodeCache) | Out-Null
    $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $prefix = 'node-v' + $node.version + '-' + $RuntimeIdentifier + '/'
        foreach ($entry in $archive.Entries) {
            if (-not $entry.Name -or -not $entry.FullName.StartsWith($prefix, [StringComparison]::Ordinal)) { continue }
            $relative = $entry.FullName.Substring($prefix.Length)
            if ($relative -notin @('node.exe', 'LICENSE') -and -not $relative.StartsWith('node_modules/npm/', [StringComparison]::Ordinal)) { continue }
            $path = Assert-McpPath $nodeCache (Join-Path $nodeCache $relative)
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
            $input = $entry.Open(); $output = [IO.File]::Create($path)
            try { $input.CopyTo($output) } finally { $output.Dispose(); $input.Dispose() }
        }
    }
    finally { $archive.Dispose() }
    $nodeExecutable = Join-Path $nodeCache 'node.exe'
    if ((Get-McpHash $nodeExecutable) -ne $node.executableSha256) { throw 'Private Node executable SHA-256 mismatch.' }
    $npmCli = Join-Path $nodeCache 'node_modules/npm/bin/npm-cli.js'
    $servicesRoot = Assert-McpPath $payloadRoot (Join-Path $payloadRoot 'services')
    [IO.Directory]::CreateDirectory($servicesRoot) | Out-Null
    foreach ($name in @('package.json', 'package-lock.json')) { Copy-Item -LiteralPath (Join-Path $sourceRoot $name) -Destination (Join-Path $servicesRoot $name) -Force }
    # A private configuration keeps source overrides local while npm verifies locked package integrity.
    # 私有配置只影响随包构建源，npm 继续验证锁文件中的固定依赖完整性。
    $npmArguments = @($npmCli, 'ci', '--prefix', $servicesRoot, '--ignore-scripts', '--omit=dev', '--no-audit', '--no-fund', '--cache', (Join-Path $cacheRoot 'npm-cache'), '--userconfig', (Join-Path $cacheRoot 'npmrc'), '--registry', $NpmRegistry, '--fetch-retries=3', '--fetch-retry-mintimeout=2000', '--fetch-retry-maxtimeout=15000', '--fetch-timeout=120000', '--loglevel=notice', '--progress=true')
    [IO.File]::WriteAllText((Join-Path $cacheRoot 'npmrc'), 'registry=' + $NpmRegistry.TrimEnd('/') + '/' + [Environment]::NewLine, $utf8)
    if ($Offline) { $npmArguments += '--offline' }
    Write-Host "[MCP] Preparing locked npm dependencies (registry: $(([Uri]$NpmRegistry).Host), offline: $([bool]$Offline))."
    & $nodeExecutable @npmArguments
    if ($LASTEXITCODE -ne 0) { throw 'Pinned MCP dependency installation failed; no runtime download fallback will be shipped.' }
    if ((Get-McpHash (Join-Path $servicesRoot 'package-lock.json')) -ne $lockHash) { throw 'npm changed the pinned dependency lock.' }
    $nodeOutput = Assert-McpPath $payloadRoot (Join-Path $payloadRoot 'node')
    [IO.Directory]::CreateDirectory($nodeOutput) | Out-Null
    foreach ($name in @('node.exe', 'LICENSE')) { Copy-Item -LiteralPath (Join-Path $nodeCache $name) -Destination (Join-Path $nodeOutput $name) -Force }

    $services = @()
    foreach ($service in $manifest.services) {
        $entryPath = Assert-McpPath $bundleRootPath (Join-Path $bundleRootPath $service.entry)
        $packagePath = Join-Path $servicesRoot ('node_modules/' + $service.package + '/package.json')
        $package = Read-McpJson $packagePath
        if (-not [IO.File]::Exists($entryPath) -or $package.version -ne $service.version) { throw "Pinned MCP entry missing: $($service.id)" }
        $services += @{id=$service.id;package=$service.package;version=$service.version;entry=$service.entry;sha256=(Get-McpHash $entryPath)}
    }
    # Browser binaries are prepared once on the build machine; users never invoke Playwright install.
    # 浏览器二进制只在构建机准备一次；用户运行时不调用 Playwright install。
    $browserRoot = Assert-McpPath $payloadRoot (Join-Path $payloadRoot 'browsers')
    [IO.Directory]::CreateDirectory($browserRoot) | Out-Null
    $previousBrowsersPath = $env:PLAYWRIGHT_BROWSERS_PATH
    $previousDownloadHost = $env:PLAYWRIGHT_DOWNLOAD_HOST
    $previousDownloadTimeout = $env:PLAYWRIGHT_DOWNLOAD_CONNECTION_TIMEOUT
    try {
        $env:PLAYWRIGHT_BROWSERS_PATH = $browserRoot
        if ($PlaywrightDownloadHost) { $env:PLAYWRIGHT_DOWNLOAD_HOST = $PlaywrightDownloadHost }
        if (-not $env:PLAYWRIGHT_DOWNLOAD_CONNECTION_TIMEOUT) { $env:PLAYWRIGHT_DOWNLOAD_CONNECTION_TIMEOUT = '120000' }
        $browserMetadata = Read-McpJson (Join-Path $servicesRoot 'node_modules/playwright-core/browsers.json')
        $chromium = @($browserMetadata.browsers | Where-Object { $_.name -eq 'chromium' })[0]
        if ([string]$chromium.revision -ne $manifest.browser.revision -or $chromium.browserVersion -ne $manifest.browser.version) { throw 'Playwright browser identity differs from the pinned payload manifest.' }
        $chromiumDirectory = Join-Path $browserRoot ('chromium-' + $chromium.revision)
        $browserExecutable = @('chrome-win64/chrome.exe', 'chrome-win/chrome.exe') | ForEach-Object { Join-Path $chromiumDirectory $_ } | Where-Object { [IO.File]::Exists($_) } | Select-Object -First 1
        if (-not $browserExecutable) {
            if ($Offline) { throw 'The fixed Playwright Chromium revision is unavailable offline.' }
            Write-Host "[MCP] Preparing Chromium $($chromium.browserVersion) (revision $($chromium.revision)); Playwright prints download progress and retries failed sources."
            & $nodeExecutable (Join-Path $servicesRoot 'node_modules/playwright/cli.js') install chromium --no-shell
            if ($LASTEXITCODE -ne 0) { throw 'The fixed Playwright browser could not be prepared.' }
            $browserExecutable = @('chrome-win64/chrome.exe', 'chrome-win/chrome.exe') | ForEach-Object { Join-Path $chromiumDirectory $_ } | Where-Object { [IO.File]::Exists($_) } | Select-Object -First 1
        }
        if (-not $browserExecutable) { throw 'The prepared browser executable is missing.' }
        if ((Get-McpHash $browserExecutable) -ne $manifest.browser.executableSha256) { throw 'The fixed Playwright Chromium executable SHA-256 does not match.' }
    }
    finally {
        $env:PLAYWRIGHT_BROWSERS_PATH = $previousBrowsersPath
        $env:PLAYWRIGHT_DOWNLOAD_HOST = $previousDownloadHost
        $env:PLAYWRIGHT_DOWNLOAD_CONNECTION_TIMEOUT = $previousDownloadTimeout
    }
    $files = [Collections.Generic.List[object]]::new()
    foreach ($directory in Get-ChildItem -LiteralPath $payloadRoot -Directory -Recurse -Force) {
        if ($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'MCP payload directories cannot contain links.' }
    }
    $totalBytes = [long]0
    foreach ($file in Get-ChildItem -LiteralPath $payloadRoot -File -Recurse -Force | Sort-Object FullName) {
        if ($file.FullName -in @($outputManifest, $integrityPath)) { continue }
        if ($file.FullName.StartsWith((Join-Path $browserRoot '.links') + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { continue }
        if ($file.FullName.StartsWith((Join-Path $servicesRoot '.playwright-mcp') + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { continue }
        if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint -or -not $file.FullName.StartsWith($payloadRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'MCP payload file identity escaped the checked tree.' }
        $totalBytes += $file.Length
        $files.Add(@{path=$file.FullName.Substring($bundleRootPath.Length+1).Replace('\','/');bytes=$file.Length;sha256=(Get-McpHash $file.FullName)})
    }
    $browserRelative = $browserExecutable.Substring($bundleRootPath.Length+1).Replace('\','/')
    $output = @{schemaVersion=1;runtimeIdentifier=$RuntimeIdentifier;sourceHash=$sourceHash;lockSha256=$lockHash;packageSha256=$packageHash;
        node=@{version=$node.version;path=$node.path;sha256=$node.executableSha256;license='runtime/mcp/node/LICENSE'};services=$services;
        browser=@{name='chromium';revision=$chromium.revision;version=$chromium.browserVersion;path=$browserRelative;sha256=(Get-McpHash $browserExecutable)};
        fileCount=$files.Count;totalBytes=$totalBytes;downloadsAtRuntime=$false;coreDependsOnNode=$false}
    Write-McpJson $outputManifest $output
    Write-McpJson $integrityPath @{schemaVersion=1;files=$files}
    & (Join-Path $PSScriptRoot 'prepare-browser-sandbox.ps1') -BundleRoot $bundleRootPath
    $paths = @($files | ForEach-Object { Join-Path $bundleRootPath $_.path }) + @($outputManifest, $integrityPath)
    [IO.File]::WriteAllLines($fileList, [string[]]$paths, $utf8)
    Write-Output "Private Node $($node.version) and $($services.Count) pinned MCP services ready ($($files.Count) files, $($output.totalBytes) bytes)."
}
finally { $cacheLock.Dispose() }
