[CmdletBinding()]
param(
    [string]$RuntimeIdentifier = 'win-x64',
    [ValidateSet('Debug', 'Release')][string]$Configuration = 'Debug',
    [ValidateSet('Full', 'Core')][string]$Profile = 'Full',
    [string]$DownloadSourcesPath = $env:KYNXA_DOWNLOAD_SOURCES,
    [string]$ResourcePackRoot = $env:KYNXA_RESOURCE_PACK_ROOT,
    [switch]$AssetsOnly,
    [switch]$Offline
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'RuntimeDownloads.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'ResourceConfigCompatibility.psm1') -Force
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
$runtimeRoot = Join-Path $repositoryRoot 'artifacts/runtime/native-desktop/win-x64'
$utf8 = [Text.UTF8Encoding]::new($false)
if ($RuntimeIdentifier -ne 'win-x64' -or -not [Environment]::Is64BitProcess) {
    throw 'The native gateway payload is currently verified only on Windows x64.'
}
[IO.Directory]::CreateDirectory($runtimeRoot) | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem

function Get-NativeFileHash([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($hasher.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $hasher.Dispose(); $stream.Dispose() }
}
function Get-NativeOutputPath([string]$RelativePath) {
    if ($RelativePath -match '(^|[\\/])\.\.([\\/]|$)' -or [IO.Path]::IsPathRooted($RelativePath)) { throw 'Invalid native bundle path.' }
    $path = [IO.Path]::GetFullPath((Join-Path $runtimeRoot $RelativePath))
    if (-not $path.StartsWith($runtimeRoot.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'The native bundle path escaped its cache.' }
    $ancestor = $path
    while ($ancestor -and $ancestor.StartsWith($runtimeRoot, [StringComparison]::OrdinalIgnoreCase)) {
        if ([IO.File]::Exists($ancestor) -or [IO.Directory]::Exists($ancestor)) {
            if ([IO.File]::GetAttributes($ancestor) -band [IO.FileAttributes]::ReparsePoint) { throw 'Reparse points are not allowed in the native bundle cache.' }
        }
        $ancestor = [IO.Path]::GetDirectoryName($ancestor)
    }
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
    return $path
}
function Test-NativeFile([string]$Path, $Specification) {
    if (-not [IO.File]::Exists($Path)) { return $false }
    $file = Get-Item -LiteralPath $Path
    return -not ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) -and $file.Length -eq $Specification.bytes -and (Get-NativeFileHash $Path) -eq $Specification.sha256
}
function Save-NativePinnedFile([string]$Url, [string]$Destination, $Specification) {
    if (Test-NativeFile $Destination $Specification) { return }
    Save-KynxaPinnedDownload -Url $Url -Destination $Destination -Sha256 $Specification.sha256 -ExpectedBytes $Specification.bytes -Offline:$Offline -DownloadSourcesPath $DownloadSourcesPath
}
function Copy-NativeFile([string]$Source, [string]$RelativePath, [switch]$ConfigurationText) {
    $destination = Get-NativeOutputPath $RelativePath
    $matches = [IO.File]::Exists($destination) -and (Get-NativeFileHash $Source) -eq (Get-NativeFileHash $destination)
    if (-not $matches -and $ConfigurationText -and [IO.File]::Exists($destination)) {
        $matches = Test-KynxaConfigHash -Path $Source -ExpectedHash (Get-NativeFileHash $destination)
    }
    if (-not $matches) {
        Copy-Item -LiteralPath $Source -Destination $destination -Force
    }
    [void]$bundle.Add($RelativePath.Replace('\', '/'))
}

# Debug and Release share this publication root, so hold one lock through inventory publication.
# Debug 与 Release 共用发布缓存；从准备到清单发布都持有同一把锁，避免交叉覆盖。
$publicationLock = $null
$publicationDeadline = [DateTime]::UtcNow.AddMinutes(15)
while (-not $publicationLock) {
    try { $publicationLock = [IO.File]::Open((Join-Path $runtimeRoot '.prepare.lock'), 'OpenOrCreate', 'ReadWrite', 'None') }
    catch [IO.IOException] {
        if ([DateTime]::UtcNow -ge $publicationDeadline) { throw 'Native payload preparation lock timed out; another build still owns this checkout cache.' }
        Start-Sleep -Milliseconds 200
    }
}
try {
# Whitelist native assets; external MCP packages are prepared separately without restoring a Node gateway.
# 原生资产逐项白名单校验；外部 MCP 包单独准备，不恢复 Node 网关。
$bundle = [Collections.Generic.List[string]]::new()
$nativeManifestPath = Join-Path $PSScriptRoot 'native-assets/native-runtime.json'
$nativeManifest = Get-Content -LiteralPath $nativeManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($ResourcePackRoot) {
    # Import a versioned local pack first; partial downloads and compiler caches are not runtime resources.
    # 优先导入版本匹配的本地资源包；下载断点与编译器缓存不属于随包运行资源。
    & (Join-Path $PSScriptRoot 'prepare-resource-pack.ps1') -Action Import -ArchivePath $ResourcePackRoot -DestinationRoot $runtimeRoot
}
Write-Host "[Runtime] Preparing $Profile resources (offline: $([bool]$Offline))."
foreach ($package in $nativeManifest.packages) {
    if ($Profile -eq 'Core' -and -not @($package.files | Where-Object { $_.path -eq 'runtime/native/vec0.dll' }).Count) { continue }
    $missingAssets = @($package.files | Where-Object { -not (Test-NativeFile (Get-NativeOutputPath $_.path) $_) })
    if ($missingAssets.Count -eq 0) {
        foreach ($asset in $package.files) { [void]$bundle.Add($asset.path) }
        continue
    }
    $archivePath = Get-NativeOutputPath ('downloads/' + $package.name)
    $sharedArchive = Join-Path $repositoryRoot ('artifacts/runtime/native/win-x64/downloads/' + $package.name)
    if (Test-NativeFile $sharedArchive $package) { $archivePath = $sharedArchive }
    Save-NativePinnedFile $package.url $archivePath $package
    $zip = $null
    try {
        if ($package.format -eq 'zip') { $zip = [IO.Compression.ZipFile]::OpenRead($archivePath) }
        foreach ($asset in $package.files) {
            $destination = Get-NativeOutputPath $asset.path
            if (-not (Test-NativeFile $destination $asset)) {
                $pending = $destination + '.extract-' + [Guid]::NewGuid().ToString('N')
                try {
                    if ($zip) {
                        $entry = $zip.GetEntry($asset.entry)
                        if (-not $entry -or $entry.Length -ne $asset.bytes) { throw 'Pinned native archive entry missing or different.' }
                        $input = $entry.Open(); $output = [IO.File]::Create($pending)
                        try { $input.CopyTo($output) } finally { $input.Dispose(); $output.Dispose() }
                    }
                    else {
                        # Extract a single safe known member; no archive-wide path traversal or executable hooks.
                        # 只提取一个已知安全成员；不展开任意路径，也不运行归档中的安装钩子。
                        if ($asset.entry -notmatch '^[A-Za-z0-9_./-]+$' -or $asset.entry.Contains('..')) { throw 'Invalid pinned archive member.' }
                        $extractRoot = Get-NativeOutputPath ('extract/' + [Guid]::NewGuid().ToString('N') + '/marker')
                        $extractRoot = [IO.Path]::GetDirectoryName($extractRoot)
                        & tar.exe -xzf $archivePath -C $extractRoot $asset.entry
                        if ($LASTEXITCODE -ne 0) { throw 'Pinned native archive extraction failed.' }
                        Move-Item -LiteralPath (Join-Path $extractRoot $asset.entry) -Destination $pending
                    }
                    if (-not (Test-NativeFile $pending $asset)) { throw 'Extracted native DLL or license failed integrity verification.' }
                    Move-Item -LiteralPath $pending -Destination $destination -Force
                }
                finally { if ([IO.File]::Exists($pending)) { Remove-Item -LiteralPath $pending } }
            }
            [void]$bundle.Add($asset.path)
        }
    }
    finally { if ($zip) { $zip.Dispose() } }
}
foreach ($asset in $nativeManifest.localFiles) {
    $source = Join-Path (Split-Path -Parent $nativeManifestPath) $asset.source
    if (-not (Test-NativeFile $source $asset)) { throw "Bundled upstream license failed integrity verification: $($asset.source)" }
    Copy-NativeFile $source $asset.path
}
Copy-NativeFile $nativeManifestPath 'runtime/native/licenses/native-runtime-manifest.json' -ConfigurationText
Copy-NativeFile (Join-Path $PSScriptRoot 'native-assets/licenses/sources.json') 'runtime/native/licenses/upstream-license-sources.json'

# A private fixed Node serves optional MCP/skill execution only; npm runs on the build machine, never startup.
# 私有固定 Node 仅供 MCP/技能执行；npm 只在构建机运行，软件启动不下载依赖。
if ($Profile -eq 'Full') {
    # Include the current source notice before MCP records its payload integrity, avoiding a second npm preparation.
    # 在 MCP 记录资源完整性前放入当前来源清单，避免随后修改载荷导致二次 npm 准备。
    Copy-NativeFile (Join-Path $PSScriptRoot 'native-assets/mcp-runtime.json') 'runtime/mcp/licenses/source-manifest.json' -ConfigurationText
    & (Join-Path $PSScriptRoot 'prepare-mcp-runtime.ps1') -BundleRoot $runtimeRoot -RuntimeIdentifier $RuntimeIdentifier -Offline:$Offline -DownloadSourcesPath $DownloadSourcesPath
    foreach ($mcpFile in [IO.File]::ReadAllLines((Join-Path $runtimeRoot 'mcp-files.txt'), $utf8)) {
        if (-not $mcpFile.StartsWith($runtimeRoot.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'MCP bundle file escaped its selected root.' }
        [void]$bundle.Add($mcpFile.Substring($runtimeRoot.Length + 1).Replace('\', '/'))
    }

    foreach ($model in @(@{ Manifest = 'embedding.json'; Cache = 'embedding/builtin-multilingual'; Output = 'runtime/embedding/builtin-multilingual' },
                           @{ Manifest = 'reranker.json'; Cache = 'rerank/builtin-multilingual-reranker'; Output = 'runtime/rerank/builtin-multilingual-reranker' })) {
        $manifestPath = Join-Path $PSScriptRoot ('native-assets/' + $model.Manifest)
        $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
        foreach ($asset in $manifest.files) {
            $destination = Get-NativeOutputPath ($model.Output + '/' + $asset.path)
            $cached = Join-Path $repositoryRoot ('artifacts/runtime/' + $model.Cache + '/' + $asset.path)
            if (-not (Test-NativeFile $destination $asset) -and (Test-NativeFile $cached $asset)) {
                Copy-Item -LiteralPath $cached -Destination $destination -Force
            }
            Save-NativePinnedFile $asset.url $destination $asset
            [void]$bundle.Add($model.Output + '/' + $asset.path)
        }
        Copy-NativeFile $manifestPath ($model.Output + '/manifest.json') -ConfigurationText
    }
}
else { Write-Host '[Runtime] Core development profile skips optional models and MCP/browser preparation; release packages use Full.' }

function Get-NativeSourceHash([string]$Directory) {
    $hasher = [Security.Cryptography.SHA256]::Create()
    $stream = [IO.MemoryStream]::new()
    try {
        $sourceFiles = @(Get-ChildItem -LiteralPath (Join-Path $Directory 'src') -File -Recurse) + @(
            Get-Item -LiteralPath (Join-Path $Directory 'Cargo.toml'), (Join-Path $Directory 'Cargo.lock'), $PSCommandPath)
        foreach ($file in $sourceFiles | Sort-Object FullName) {
            $relative = $file.FullName.Substring($repositoryRoot.Length).Replace('\', '/')
            $name = $utf8.GetBytes($relative + "`n"); $stream.Write($name, 0, $name.Length)
            $bytes = [IO.File]::ReadAllBytes($file.FullName); $stream.Write($bytes, 0, $bytes.Length)
        }
        $flags = $utf8.GetBytes("$Configuration|$RuntimeIdentifier|$env:RUSTFLAGS")
        $stream.Write($flags, 0, $flags.Length); $stream.Position = 0
        return [BitConverter]::ToString($hasher.ComputeHash($stream)).Replace('-', '').ToLowerInvariant()
    }
    finally { $stream.Dispose(); $hasher.Dispose() }
}
function Initialize-NativeBuildEnvironment {
    if ((Get-Command cl.exe -ErrorAction SilentlyContinue) -and
        (Get-Command link.exe -ErrorAction SilentlyContinue) -and $env:INCLUDE -and $env:LIB -and
        $env:VSCMD_ARG_TGT_ARCH -eq 'x64') { return }
    # Rust's compiler discovery may predate a new VS release; use the installed VS developer shell.
    # Rust 的编译器发现可能早于新版 VS；通过已安装的 VS 开发环境加载 C++ 头文件、库及工具。
    $vswherePath = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    if (-not [IO.File]::Exists($vswherePath)) { throw 'Visual Studio C++ build tools are required to compile the native Windows payload.' }
    $installationPaths = @(& $vswherePath -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
    if ($LASTEXITCODE -ne 0 -or $installationPaths.Count -eq 0) { throw 'Install the Visual Studio MSVC x64/x86 build tools component before preparing the native payload.' }
    $developerCommandPath = Join-Path $installationPaths[0] 'Common7/Tools/VsDevCmd.bat'
    if (-not [IO.File]::Exists($developerCommandPath)) { throw 'The installed Visual Studio developer command script is missing.' }
    # Explicit UTF-8 avoids the developer shell's ANSI round trip corrupting Chinese TEMP/user paths.
    # 显式使用 UTF-8，避免开发环境的 ANSI 往返转换损坏中文 TEMP 或用户目录。
    $environmentStart = [Diagnostics.ProcessStartInfo]::new((Join-Path $env:SystemRoot 'System32/cmd.exe'))
    $environmentStart.UseShellExecute = $false
    $environmentStart.CreateNoWindow = $true
    $environmentStart.RedirectStandardOutput = $true
    $environmentStart.RedirectStandardError = $true
    $environmentStart.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $environmentStart.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    $environmentStart.Arguments = '/d /s /c "chcp 65001 >nul && call "' + $developerCommandPath + '" -no_logo -arch=x64 -host_arch=x64 >nul && set"'
    $environmentProcess = [Diagnostics.Process]::Start($environmentStart)
    try {
        $environmentOutput = $environmentProcess.StandardOutput.ReadToEndAsync()
        $environmentErrors = $environmentProcess.StandardError.ReadToEndAsync()
        if (-not $environmentProcess.WaitForExit(30000)) {
            & (Join-Path $env:SystemRoot 'System32/taskkill.exe') /PID $environmentProcess.Id /T /F | Out-Null
            [void]$environmentProcess.WaitForExit(5000)
            throw 'Visual Studio C++ environment initialization timed out.'
        }
        # A descendant can inherit a redirected pipe after cmd exits; bound the final drain too.
        # 后代进程可能在 cmd 退出后仍持有重定向管道，因此最终读取也必须有超时。
        if (-not [Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]@($environmentOutput, $environmentErrors), 5000)) {
            throw 'Visual Studio C++ environment output capture timed out.'
        }
        if ($environmentProcess.ExitCode -ne 0) { throw 'Visual Studio C++ environment initialization failed.' }
        # Consume environment values only in memory; never emit credentials or a full environment dump.
        # 环境值只在内存中使用，不输出凭据或完整环境转储。
        foreach ($line in ($environmentOutput.Result -split '\r?\n')) {
            $separatorIndex = $line.IndexOf('=')
            if ($separatorIndex -gt 0) {
                $name = $line.Substring(0, $separatorIndex)
                if ($name -in @('PATH', 'INCLUDE', 'LIB', 'LIBPATH', 'VCToolsInstallDir', 'VCToolsVersion',
                    'VCINSTALLDIR', 'VSINSTALLDIR', 'WindowsSdkDir', 'WindowsSDKVersion', 'WindowsSDKLibVersion',
                    'UniversalCRTSdkDir', 'UCRTVersion', 'VSCMD_ARG_TGT_ARCH', 'VSCMD_ARG_HOST_ARCH')) {
                    [Environment]::SetEnvironmentVariable($name, $line.Substring($separatorIndex + 1), 'Process')
                }
            }
        }
    }
    finally { $environmentProcess.Dispose() }
    if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue) -or
        -not (Get-Command link.exe -ErrorAction SilentlyContinue) -or -not $env:INCLUDE -or -not $env:LIB -or
        $env:VSCMD_ARG_TGT_ARCH -ne 'x64') {
        throw 'The Visual Studio developer shell did not provide a complete Windows x64 C++ build environment.'
    }
}

function Build-NativeExecutable([string]$DirectoryName, [string]$ExecutableName, [string]$BundleDirectory) {
    $source = Join-Path $repositoryRoot ('apps/' + $DirectoryName)
    $hash = Get-NativeSourceHash $source
    $destination = Get-NativeOutputPath ($BundleDirectory + '/' + $ExecutableName + '.exe')
    $buildRecord = Get-NativeOutputPath ($BundleDirectory + '/build.json')
    $existing = if ([IO.File]::Exists($buildRecord)) { Get-Content -LiteralPath $buildRecord -Raw -Encoding UTF8 | ConvertFrom-Json } else { $null }
    if (-not $existing -or $existing.sourceHash -ne $hash -or -not [IO.File]::Exists($destination) -or $existing.sha256 -ne (Get-NativeFileHash $destination)) {
        $arguments = @('build', '--locked', '--manifest-path', (Join-Path $source 'Cargo.toml'), '--bin', $ExecutableName)
        $arguments += $script:cargoNetworkArguments
        if ($Configuration -eq 'Release') { $arguments += '--release' }
        & $script:cargoCommand @arguments
        if ($LASTEXITCODE -ne 0) { throw "Native executable compilation failed: $ExecutableName" }
        if ((Get-NativeSourceHash $source) -ne $hash) { throw 'Native source changed during compilation; refusing a mixed bundle.' }
        $targetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $source 'target' }
        $profile = if ($Configuration -eq 'Release') { 'release' } else { 'debug' }
        Copy-Item -LiteralPath (Join-Path $targetRoot ($profile + '/' + $ExecutableName + '.exe')) -Destination $destination -Force
        [IO.File]::WriteAllText($buildRecord, (@{schemaVersion=1;sourceHash=$hash;sha256=(Get-NativeFileHash $destination);configuration=$Configuration} | ConvertTo-Json), $utf8)
    }
    [void]$bundle.Add($BundleDirectory + '/' + $ExecutableName + '.exe')
    [void]$bundle.Add($BundleDirectory + '/build.json')
    # Read Cargo's UTF-8 directly; PowerShell 5's console code page can corrupt Chinese source paths.
    # 直接按 UTF-8 读取 Cargo 输出；PowerShell 5 控制台代码页可能损坏中文源码路径。
    $metadataStart = [Diagnostics.ProcessStartInfo]::new()
    $metadataStart.FileName = $script:cargoCommand
    $metadataArguments = @('metadata', '--locked', '--format-version', '1', '--manifest-path', (Join-Path $source 'Cargo.toml')) + $script:cargoNetworkArguments
    $metadataStart.Arguments = ($metadataArguments | ForEach-Object { '"' + $_.Replace('"', '\"') + '"' }) -join ' '
    $metadataStart.UseShellExecute = $false
    $metadataStart.CreateNoWindow = $true
    $metadataStart.RedirectStandardOutput = $true
    $metadataStart.RedirectStandardError = $true
    $metadataStart.StandardOutputEncoding = $utf8
    $metadataStart.StandardErrorEncoding = $utf8
    $metadataProcess = [Diagnostics.Process]::Start($metadataStart)
    try {
        $metadataOutput = $metadataProcess.StandardOutput.ReadToEndAsync()
        $metadataErrors = $metadataProcess.StandardError.ReadToEndAsync()
        if (-not $metadataProcess.WaitForExit(120000)) {
            # Kill only this owned metadata process; no user Cargo or compiler process is selected by name.
            # 仅终止本次持有的元数据进程，不按名称处理用户其他 Cargo 或编译器进程。
            & (Join-Path $env:SystemRoot 'System32/taskkill.exe') /PID $metadataProcess.Id /T /F | Out-Null
            [void]$metadataProcess.WaitForExit(5000)
            throw 'Native dependency metadata timed out after 120 seconds.'
        }
        if ($metadataProcess.ExitCode -ne 0) { throw ('Native dependency metadata could not be verified: ' + $metadataErrors.Result) }
        $metadataJson = $metadataOutput.Result
    }
    finally { $metadataProcess.Dispose() }
    # Cargo feature names are case-sensitive (for example USB and usb); Windows PowerShell objects are not.
    # Cargo 特征名区分大小写（例如 USB 与 usb）；Windows PowerShell 对象不能安全承载这些键。
    if ($PSVersionTable.PSVersion.Major -ge 6) {
        $metadata = ($metadataJson -join [Environment]::NewLine) | ConvertFrom-Json -AsHashtable
    }
    else {
        Add-Type -AssemblyName System.Web.Extensions
        $metadataReader = [Web.Script.Serialization.JavaScriptSerializer]::new()
        $metadataReader.MaxJsonLength = 64 * 1024 * 1024
        $metadata = $metadataReader.DeserializeObject(($metadataJson -join [Environment]::NewLine))
    }
    $notices = [Collections.Generic.List[string]]::new()
    foreach ($package in $metadata.packages | Sort-Object name, version) {
        if (-not $package.source) { continue }
        $notices.Add("$($package.name) $($package.version) | $($package.license) | $($package.repository)")
        $packageRoot = Split-Path -Parent $package.manifest_path
        foreach ($license in Get-ChildItem -LiteralPath $packageRoot -File | Where-Object { $_.Name -match '^(LICEN[CS]E|COPYING|NOTICE)([._-].*)?$' }) {
            Copy-NativeFile $license.FullName ($BundleDirectory + '/licenses/' + $package.name + '-' + $package.version + '/' + $license.Name)
        }
        if ($package.license_file) {
            Copy-NativeFile (Join-Path $packageRoot $package.license_file) ($BundleDirectory + '/licenses/' + $package.name + '-' + $package.version + '/LICENSE-UPSTREAM')
        }
    }
    $noticesPath = Get-NativeOutputPath ($BundleDirectory + '/licenses/THIRD-PARTY-NOTICES.txt')
    [IO.File]::WriteAllLines($noticesPath, $notices, $utf8)
    [void]$bundle.Add($BundleDirectory + '/licenses/THIRD-PARTY-NOTICES.txt')
    return $hash
}

# Reuse Cargo's actual local target directory; do not rebuild all dependencies under another target triple.
# 复用 Cargo 现有 target 缓存；不为了目录形式更换 target triple 而全量重编依赖。
if (-not $AssetsOnly) {
    $previousBuildEnvironment = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in Get-ChildItem Env:) { $previousBuildEnvironment[$entry.Name] = $entry.Value }
    try {
        Initialize-NativeBuildEnvironment
        $script:cargoNetworkArguments = @()
        if ($Offline) { $script:cargoNetworkArguments += '--offline' }
        $settings = Get-KynxaDownloadSettings -Path $DownloadSourcesPath
        $registryIndex = $env:KYNXA_CARGO_REGISTRY_INDEX
        if (-not $registryIndex -and $settings.Contains('cargoRegistryIndex')) { $registryIndex = [string]$settings['cargoRegistryIndex'] }
        if ($registryIndex) {
            if ($registryIndex -notmatch '^(sparse\+)?https://[^\s\x22\x27]+$') { throw 'Cargo mirror index must be a HTTPS registry URL.' }
            $script:cargoNetworkArguments += @('--config', "source.crates-io.replace-with='kynxa-build-mirror'", '--config', "source.kynxa-build-mirror.registry='$registryIndex'")
        }
        $cargo = Get-Command cargo -ErrorAction SilentlyContinue
        if ($cargo) { $script:cargoCommand = $cargo.Source }
        else {
            $compilerBin = Join-Path $repositoryRoot 'artifacts/development/rust-1.90.0/toolchain/bin'
            $script:cargoCommand = Join-Path $compilerBin 'cargo.exe'
            if (-not [IO.File]::Exists($script:cargoCommand)) { throw 'Rust is needed only on the build machine. Install stable Rust >=1.88 to prepare this Windows payload.' }
            $env:PATH = $compilerBin + ';' + $env:PATH
            if (-not $env:CARGO_HOME) { $env:CARGO_HOME = Join-Path $repositoryRoot 'artifacts/development/rust-1.90.0/cargo-home' }
            $env:RUSTC = Join-Path $compilerBin 'rustc.exe'
        }
        $agentHash = Build-NativeExecutable 'agent-core' 'kynxa-gateway' 'runtime/agent'
        [void](Build-NativeExecutable 'resource-service' 'kynxa-resource-service' 'runtime/resource')
        $record = @{schemaVersion=1;sourceHash=$agentHash;executable='kynxa-gateway.exe';runtimeIdentifier=$RuntimeIdentifier;nodeRuntime=$false}
        $recordPath = Get-NativeOutputPath 'runtime/agent/runtime.json'
        [IO.File]::WriteAllText($recordPath, ($record | ConvertTo-Json), $utf8)
        [void]$bundle.Add('runtime/agent/runtime.json')
    }
    finally {
        # A manual invocation must not leave compiler or SDK overrides in the caller's process.
        # 手动调用结束后不把编译器或 SDK 覆盖留在调用方进程中。
        $currentBuildEnvironment = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($entry in Get-ChildItem Env:) {
            $currentBuildEnvironment[$entry.Name] = $entry.Value
            if (-not $previousBuildEnvironment.ContainsKey($entry.Name)) { [Environment]::SetEnvironmentVariable($entry.Name, [NullString]::Value, 'Process') }
        }
        # Leave unchanged empty values intact; .NET Framework would delete them when assigned again.
        # 保留未变化的空值；.NET Framework 再次赋空字符串时会删除对应环境项。
        foreach ($name in $previousBuildEnvironment.Keys) {
            if (-not $currentBuildEnvironment.ContainsKey($name) -or $currentBuildEnvironment[$name] -cne $previousBuildEnvironment[$name]) {
                [Environment]::SetEnvironmentVariable($name, $previousBuildEnvironment[$name], 'Process')
            }
        }
    }
}
$bundleFiles = @($bundle | Sort-Object -Unique | ForEach-Object { Get-NativeOutputPath $_ })
[IO.File]::WriteAllLines((Join-Path $runtimeRoot $(if ($AssetsOnly) {'asset-files.txt'} else {'bundle-files.txt'})), [string[]]$bundleFiles, $utf8)
Write-Output "Native Windows payload ready ($Profile): $($bundleFiles.Count) verified files."
}
finally { $publicationLock.Dispose() }
