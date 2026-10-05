[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RuntimeIdentifier,
    [Parameter(Mandatory = $true)][string]$CacheDirectory,
    [string]$ManifestPath,
    [string]$RedistDirectory,
    [switch]$Offline
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
# MSBuild can inherit PowerShell 7 module paths while launching Windows PS 5.1.
# MSBuild 启动 Windows PS 5.1 时可能继承 PS 7 模块路径，显式加载当前宿主的签名校验模块。
Import-Module (Join-Path $PSHOME 'Modules/Microsoft.PowerShell.Security/Microsoft.PowerShell.Security.psd1')
if ([string]::IsNullOrWhiteSpace($ManifestPath)) { $ManifestPath = Join-Path $PSScriptRoot 'vc-runtime.json' }
$runtimeManifest = Get-Content -LiteralPath $ManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
$runtimePlatform = $runtimeManifest.platforms.PSObject.Properties[$RuntimeIdentifier]
if ($runtimeManifest.schemaVersion -ne 1 -or -not $runtimePlatform) { throw "No pinned app-local VC runtime for $RuntimeIdentifier." }
$runtimeSpec = $runtimePlatform.Value
$runtimeCache = [IO.Path]::GetFullPath($CacheDirectory)
if ($runtimeCache.TrimEnd('\', '/') -eq [IO.Path]::GetPathRoot($runtimeCache).TrimEnd('\', '/')) { throw 'A filesystem root cannot be used as the VC build cache.' }
$runtimeBundle = Join-Path $runtimeCache $runtimeManifest.version
[IO.Directory]::CreateDirectory($runtimeBundle) | Out-Null

function Assert-VisualRuntimePath([string]$Path) {
    $absolutePath = [IO.Path]::GetFullPath($Path)
    if (-not $absolutePath.StartsWith($runtimeCache.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'The VC runtime output escaped its build cache.'
    }
    if ((Test-Path -LiteralPath $absolutePath) -and ([IO.File]::GetAttributes($absolutePath) -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'The VC runtime build cache cannot contain reparse points.'
    }
}
function Test-VisualRuntimeFile([string]$Path, $Spec) {
    if (-not [IO.File]::Exists($Path)) { return $false }
    $file = Get-Item -LiteralPath $Path
    if (($file.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $file.Length -ne $Spec.bytes) { return $false }
    $stream = [IO.File]::OpenRead($Path)
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($hasher.ComputeHash($stream))).Replace('-', '').ToLowerInvariant() -eq $Spec.sha256 }
    finally { $hasher.Dispose(); $stream.Dispose() }
}
function Find-VisualRuntimeRedist {
    if (-not [string]::IsNullOrWhiteSpace($RedistDirectory)) { return [IO.Path]::GetFullPath($RedistDirectory) }
    $locator = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    if (-not [IO.File]::Exists($locator)) { throw 'The licensed MSVC Redist build source is unavailable. Supply -RedistDirectory from the official pinned toolset.' }
    $compilerInstall = & $locator -latest -products '*' -property installationPath
    if (-not $compilerInstall) { throw 'A stable Visual Studio build source is required for the pinned VC runtime.' }
    return Join-Path $compilerInstall ("VC/Redist/MSVC/$($runtimeManifest.toolsetDirectory)/$($runtimeSpec.redistDirectory)")
}

# Only the licensed official Redist directory is used; never copy System32 DLLs.
# 只使用有许可的官方 Redist 目录，绝不从 System32 随意复制运行库；逐文件检查固定摘要及 Microsoft 签名。
$runtimeLock = $null
$runtimeDeadline = [DateTime]::UtcNow.AddMinutes(2)
Assert-VisualRuntimePath $runtimeBundle
while (-not $runtimeLock) {
    try { $runtimeLock = [IO.File]::Open((Join-Path $runtimeBundle '.prepare.lock'), [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None) }
    catch [IO.IOException] {
        if ([DateTime]::UtcNow -ge $runtimeDeadline) { throw 'Timed out waiting for the VC runtime build cache.' }
        Start-Sleep -Milliseconds 200
    }
}
try {
    $sourceDirectory = $null
    foreach ($fileSpec in $runtimeSpec.files) {
        if ($fileSpec.name -notmatch '^[a-zA-Z0-9_]+\.dll$' -or $fileSpec.sha256 -notmatch '^[0-9a-f]{64}$') { throw 'Invalid pinned VC runtime manifest.' }
        $destination = Join-Path $runtimeBundle $fileSpec.name
        Assert-VisualRuntimePath $destination
        if (Test-VisualRuntimeFile $destination $fileSpec) { continue }
        if (-not $sourceDirectory) { $sourceDirectory = Find-VisualRuntimeRedist }
        $source = Join-Path $sourceDirectory $fileSpec.name
        if (-not (Test-VisualRuntimeFile $source $fileSpec)) { throw "Pinned official VC runtime source missing or different: $($fileSpec.name)." }
        $signature = Get-AuthenticodeSignature -LiteralPath $source
        if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
            throw 'The VC runtime source does not have a valid Microsoft signature.'
        }
        $pendingPath = $destination + '.copy-' + [Guid]::NewGuid().ToString('N')
        try {
            Copy-Item -LiteralPath $source -Destination $pendingPath
            if (-not (Test-VisualRuntimeFile $pendingPath $fileSpec)) { throw 'Copied VC runtime failed integrity verification.' }
            Move-Item -LiteralPath $pendingPath -Destination $destination -Force
        }
        finally { if ([IO.File]::Exists($pendingPath)) { Remove-Item -LiteralPath $pendingPath } }
    }
    $licenseDirectory = Join-Path $runtimeBundle 'licenses'
    [IO.Directory]::CreateDirectory($licenseDirectory) | Out-Null
    $licensePath = Join-Path $licenseDirectory 'Microsoft-Visual-Cpp-Runtime-2026.docx'
    Assert-VisualRuntimePath $licensePath
    if (-not (Test-VisualRuntimeFile $licensePath $runtimeManifest.license)) {
        if ($Offline) { throw 'The original VC runtime license is not cached. An online build is required once.' }
        $pendingLicense = $licensePath + '.download-' + [Guid]::NewGuid().ToString('N')
        try {
            [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
            Invoke-WebRequest -Uri $runtimeManifest.license.url -UseBasicParsing -OutFile $pendingLicense -TimeoutSec 60
            if (-not (Test-VisualRuntimeFile $pendingLicense $runtimeManifest.license)) { throw 'The original VC license failed integrity verification.' }
            Move-Item -LiteralPath $pendingLicense -Destination $licensePath -Force
        }
        finally { if ([IO.File]::Exists($pendingLicense)) { Remove-Item -LiteralPath $pendingLicense } }
    }
    Copy-Item -LiteralPath $ManifestPath -Destination (Join-Path $licenseDirectory 'vc-runtime-manifest.json') -Force
    [IO.File]::WriteAllText((Join-Path $runtimeCache 'runtime-path.txt'), $runtimeBundle, [Text.UTF8Encoding]::new($false))
    # Publish an explicit verified payload; unrelated cache files never become runtime DLLs.
    # 输出明确的已验证负载清单，不把无关缓存文件当作运行库或许可证随包发布。
    $runtimeFiles = @($runtimeSpec.files | ForEach-Object { Join-Path $runtimeBundle $_.name })
    [IO.File]::WriteAllLines((Join-Path $runtimeCache 'runtime-files.txt'), [string[]]$runtimeFiles, [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllLines((Join-Path $runtimeCache 'runtime-notices.txt'), [string[]]@($licensePath, (Join-Path $licenseDirectory 'vc-runtime-manifest.json')), [Text.UTF8Encoding]::new($false))
    Write-Output "Verified app-local Microsoft VC runtime $($runtimeManifest.version) $RuntimeIdentifier."
}
finally { $runtimeLock.Dispose() }
