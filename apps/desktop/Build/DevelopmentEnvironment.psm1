Set-StrictMode -Version Latest

$script:minimumRustVersion = [version]'1.88.0'
$script:managedRustVersion = '1.90.0'
$script:managedRustHost = 'x86_64-pc-windows-msvc'
$script:managedRustSha256 = 'a2db311d0ac032a97717c9280d85fc31d6ccc34b330f8a095b63a1bc042744f6'

function Invoke-KynxaEnvironmentTool {
    param([string]$Path, [string[]]$Arguments, [string]$WorkingDirectory)
    $start = [Diagnostics.ProcessStartInfo]::new($Path)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    $start.WorkingDirectory = $WorkingDirectory
    $start.Arguments = ($Arguments | ForEach-Object { '"' + $_.Replace('"', '\"') + '"' }) -join ' '
    # Discovery must not make a rustup shim install a toolchain or change user configuration.
    # 只读发现不能触发 rustup 自动安装或修改用户配置。
    $start.EnvironmentVariables['RUSTUP_AUTO_INSTALL'] = '0'
    $start.EnvironmentVariables['DOTNET_CLI_UI_LANGUAGE'] = 'en-US'
    $process = [Diagnostics.Process]::Start($start)
    try {
        $output = $process.StandardOutput.ReadToEndAsync()
        $errors = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(10000)) {
            $process.Kill()
            $process.WaitForExit()
            return $null
        }
        if ($process.ExitCode -ne 0) { return $null }
        return $output.GetAwaiter().GetResult()
    }
    finally { $process.Dispose() }
}

function Get-KynxaRegistryValues {
    param([string]$KeyPath, [string]$Name)
    foreach ($view in @([Microsoft.Win32.RegistryView]::Registry64, [Microsoft.Win32.RegistryView]::Registry32)) {
        $baseKey = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $view)
        $key = $null
        try {
            $key = $baseKey.OpenSubKey($KeyPath)
            if (-not $key) { continue }
            if ($Name) {
                $value = $key.GetValue($Name)
                if ($value) { [string]$value }
            }
            else {
                foreach ($valueName in $key.GetValueNames()) {
                    $value = $key.GetValue($valueName)
                    if ($value -is [string] -and $value) { $value }
                }
            }
        }
        finally { if ($key) { $key.Dispose() }; $baseKey.Dispose() }
    }
}

function Sort-KynxaVisualStudioInstallations {
    param([object[]]$Installations)
    return $Installations | Sort-Object @{ Expression = { [bool]$_.isPrerelease } }, @{ Expression = { [version]$_.installationVersion }; Descending = $true }
}

function Get-KynxaStableDotNetSdkVersion {
    param([string]$SdkList)
    return $SdkList -split '\r?\n' | ForEach-Object {
        if ($_ -match '^(10\.0\.\d+) \[') { [version]$Matches[1] }
    } | Sort-Object -Descending | Select-Object -First 1
}

function Get-KynxaVisualStudio {
    param([string]$RepositoryRoot)
    $installations = [Collections.Generic.List[object]]::new()
    $vswherePaths = @()
    $vswhereCommand = Get-Command vswhere.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($vswhereCommand) { $vswherePaths += $vswhereCommand.Source }
    foreach ($programFiles in @(${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
        if ($programFiles) { $vswherePaths += Join-Path $programFiles 'Microsoft Visual Studio/Installer/vswhere.exe' }
    }
    $vswherePath = $vswherePaths | Where-Object { [IO.File]::Exists($_) } | Select-Object -First 1
    if ($vswherePath) {
        # Include Insiders, but prefer a complete released installation whenever one is available.
        # 发现范围包含 Insiders；有完整正式安装时优先使用正式版。
        $json = Invoke-KynxaEnvironmentTool $vswherePath @('-products', '*', '-prerelease', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-format', 'json', '-utf8') $RepositoryRoot
        if ($json) {
            foreach ($installation in @(Sort-KynxaVisualStudioInstallations @($json | ConvertFrom-Json))) {
                $installations.Add([pscustomobject]@{ Root = $installation.installationPath; IsPrerelease = [bool]$installation.isPrerelease })
            }
        }
    }
    $fallbackRoots = @($env:VSINSTALLDIR)
    $fallbackRoots += @(Get-KynxaRegistryValues 'SOFTWARE\Microsoft\VisualStudio\SxS\VS7' '')
    foreach ($programFiles in @(${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
        if (-not $programFiles) { continue }
        $visualStudioRoot = Join-Path $programFiles 'Microsoft Visual Studio'
        if ([IO.Directory]::Exists($visualStudioRoot)) {
            foreach ($release in Get-ChildItem -LiteralPath $visualStudioRoot -Directory) {
                foreach ($edition in Get-ChildItem -LiteralPath $release.FullName -Directory) { $fallbackRoots += $edition.FullName }
            }
        }
    }
    foreach ($fallbackRoot in $fallbackRoots | Where-Object { $_ } | Select-Object -Unique) {
        $installations.Add([pscustomobject]@{ Root = $fallbackRoot; IsPrerelease = $fallbackRoot -match 'Preview|Insiders' })
    }
    foreach ($installation in $installations) {
        $toolsetsRoot = Join-Path $installation.Root 'VC/Tools/MSVC'
        $toolsets = @()
        $defaultFile = Join-Path $installation.Root 'VC/Auxiliary/Build/Microsoft.VCToolsVersion.default.txt'
        if ([IO.File]::Exists($defaultFile)) { $toolsets += Join-Path $toolsetsRoot ([IO.File]::ReadAllText($defaultFile).Trim()) }
        if ([IO.Directory]::Exists($toolsetsRoot)) {
            $toolsets += @(Get-ChildItem -LiteralPath $toolsetsRoot -Directory | Where-Object Name -Match '^\d+\.\d+\.\d+$' | Sort-Object { [version]$_.Name } -Descending | ForEach-Object FullName)
        }
        foreach ($toolset in $toolsets | Select-Object -Unique) {
            $required = @('bin/Hostx64/x64/cl.exe', 'bin/Hostx64/x64/link.exe', 'include/vcruntime.h', 'lib/x64/libcmt.lib')
            if (@($required | Where-Object { -not [IO.File]::Exists((Join-Path $toolset $_)) }).Count -eq 0) {
                return [pscustomobject]@{ InstallRoot = $installation.Root; MsvcRoot = $toolset; IsPrerelease = $installation.IsPrerelease }
            }
        }
    }
    return $null
}

function Get-KynxaWindowsSdk {
    param($VisualStudio)
    $roots = @($env:WindowsSdkDir, $env:UniversalCRTSdkDir)
    $roots += @(Get-KynxaRegistryValues 'SOFTWARE\Microsoft\Windows Kits\Installed Roots' 'KitsRoot10')
    $roots += @(Get-KynxaRegistryValues 'SOFTWARE\Microsoft\Microsoft SDKs\Windows\v10.0' 'InstallationFolder')
    if ($VisualStudio) { $roots += Join-Path $VisualStudio.InstallRoot 'Windows Kits/10' }
    foreach ($programFiles in @(${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
        if ($programFiles) { $roots += Join-Path $programFiles 'Windows Kits/10' }
    }
    $candidates = @()
    foreach ($root in $roots | Where-Object { $_ } | Select-Object -Unique) {
        $include = Join-Path $root 'Include'
        if (-not [IO.Directory]::Exists($include)) { continue }
        foreach ($version in Get-ChildItem -LiteralPath $include -Directory | Where-Object Name -Match '^10\.0\.\d+\.\d+$') {
            if ([version]$version.Name -lt [version]'10.0.26100.0') { continue }
            $number = $version.Name
            $required = @("Include/$number/um/windows.h", "Include/$number/ucrt/stdio.h", "Lib/$number/ucrt/x64/ucrt.lib", "Lib/$number/um/x64/kernel32.lib", "bin/$number/x64/rc.exe")
            if (@($required | Where-Object { -not [IO.File]::Exists((Join-Path $root $_)) }).Count -eq 0) {
                $candidates += [pscustomobject]@{ Root = $root; Version = $number }
            }
        }
    }
    return $candidates | Sort-Object { [version]$_.Version } -Descending | Select-Object -First 1
}

function Get-KynxaDotNet {
    param([string]$RepositoryRoot)
    $paths = @()
    $command = Get-Command dotnet.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($command) { $paths += $command.Source }
    foreach ($root in @($env:DOTNET_ROOT_X64, $env:DOTNET_ROOT, (Join-Path $env:ProgramFiles 'dotnet')) + @(Get-KynxaRegistryValues 'SOFTWARE\dotnet\Setup\InstalledVersions\x64' 'InstallLocation')) {
        if ($root) { $paths += Join-Path $root 'dotnet.exe' }
    }
    foreach ($path in $paths | Select-Object -Unique) {
        if (-not [IO.File]::Exists($path)) { continue }
        $output = Invoke-KynxaEnvironmentTool $path @('--list-sdks') $RepositoryRoot
        $version = Get-KynxaStableDotNetSdkVersion $output
        if ($version) {
            $information = Invoke-KynxaEnvironmentTool $path @('--info') $RepositoryRoot
            if ($information -match '(?m)^\s*Architecture:\s+x64\s*$') {
                return [pscustomobject]@{ Path = $path; SdkVersion = $version.ToString() }
            }
        }
    }
    return $null
}

function Get-KynxaRust {
    param([string]$RepositoryRoot)
    $candidates = @($env:RUSTC)
    foreach ($name in @('rustc.exe', 'cargo.exe')) {
        $command = Get-Command $name -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($command) { $candidates += Join-Path (Split-Path -Parent $command.Source) 'rustc.exe' }
    }
    $managedRoot = Join-Path $RepositoryRoot "artifacts/development/rust-$script:managedRustVersion"
    $candidates += Join-Path $managedRoot 'toolchain/bin/rustc.exe'
    if ($env:USERPROFILE) { $candidates += Join-Path $env:USERPROFILE '.cargo/bin/rustc.exe' }
    foreach ($compiler in $candidates | Where-Object { $_ } | Select-Object -Unique) {
        if (-not [IO.File]::Exists($compiler)) { continue }
        $compiler = [IO.Path]::GetFullPath($compiler)
        $cargo = Join-Path (Split-Path -Parent $compiler) 'cargo.exe'
        if (-not [IO.File]::Exists($cargo)) { continue }
        $output = Invoke-KynxaEnvironmentTool $compiler @('--version', '--verbose') $RepositoryRoot
        if ($output -notmatch '(?m)^release: (\d+\.\d+\.\d+)' -or [version]$Matches[1] -lt $script:minimumRustVersion) { continue }
        $version = $Matches[1]
        if ($output -notmatch '(?m)^host: x86_64-pc-windows-msvc\s*$') { continue }
        $cargoVersion = Invoke-KynxaEnvironmentTool $cargo @('--version') $RepositoryRoot
        if ($cargoVersion -notmatch '^cargo (\d+\.\d+\.\d+)' -or [version]$Matches[1] -lt $script:minimumRustVersion) { continue }
        $isManaged = $compiler.StartsWith($managedRoot.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)
        if ($isManaged) {
            $standardLibrary = Join-Path $managedRoot "toolchain/lib/rustlib/$script:managedRustHost/lib"
            if (-not [IO.Directory]::Exists($standardLibrary) -or [IO.Directory]::GetFiles($standardLibrary, 'libstd-*.rlib').Count -eq 0) { continue }
        }
        return [pscustomobject]@{ Cargo = $cargo; Rustc = $compiler; Version = $version; IsManaged = $isManaged }
    }
    return $null
}

function Get-KynxaDevelopmentEnvironment {
    [CmdletBinding()]
    param([string]$RepositoryRoot = ([IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))))
    $RepositoryRoot = [IO.Path]::GetFullPath($RepositoryRoot)
    $issues = [Collections.Generic.List[string]]::new()
    $automaticActions = [Collections.Generic.List[string]]::new()
    $isWindowsX64 = [Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT -and [Environment]::Is64BitProcess -and $env:PROCESSOR_ARCHITECTURE -eq 'AMD64'
    $visualStudio = $null; $sdk = $null; $dotnet = $null; $rust = $null
    if ($isWindowsX64) {
        if ([Environment]::OSVersion.Version -lt [version]'10.0.19041.0') {
            $issues.Add('Windows 10 build 19041 or newer is required. / 构建与运行要求 Windows 10 19041 或更新版本。')
        }
        $visualStudio = Get-KynxaVisualStudio $RepositoryRoot
        $sdk = Get-KynxaWindowsSdk $visualStudio
        $dotnet = Get-KynxaDotNet $RepositoryRoot
        $rust = Get-KynxaRust $RepositoryRoot
        if (-not $visualStudio) { $issues.Add('Install Visual Studio/Build Tools: Desktop development with C++ and MSVC x64/x86. / 请手动在 Visual Studio 安装器中添加“使用 C++ 的桌面开发”和 MSVC x64/x86；支持正式版及 Insiders。') }
        if (-not $sdk) { $issues.Add('Install Windows SDK >=10.0.26100.0 with headers, x64 libraries and rc.exe using Visual Studio Installer. / 请手动在 Visual Studio 安装器中补齐 Windows SDK >=10.0.26100.0 的头文件、x64 库与资源编译器。') }
        if (-not $dotnet) { $issues.Add('Install a stable .NET 10 x64 SDK, not only the runtime or .NET 11 preview. / 请手动安装正式版 .NET 10 x64 SDK，仅运行时或 .NET 11 预览版不满足构建要求。') }
        if (-not $rust) {
            $issues.Add('Rust >=1.88 with the Windows x64 MSVC host is missing. / 缺少 Rust >=1.88 Windows x64 MSVC 工具链。')
            $automaticActions.Add("Install private Rust $script:managedRustVersion under artifacts/development; no system PATH changes. / 可在项目内补齐私有 Rust，不修改系统 PATH。")
        }
    }
    else { $issues.Add('Run 64-bit PowerShell on Windows x64. / 请使用 Windows x64 的 64 位 PowerShell。') }
    return [pscustomobject]@{ RepositoryRoot = $RepositoryRoot; IsReady = $issues.Count -eq 0; Issues = $issues.ToArray(); AutomaticActions = $automaticActions.ToArray(); VisualStudio = $visualStudio; WindowsSdk = $sdk; DotNet = $dotnet; Rust = $rust }
}

function Install-KynxaDevelopmentRust {
    param([string]$RepositoryRoot, [switch]$Offline, [string]$DownloadSourcesPath)
    Import-Module (Join-Path $PSScriptRoot 'RuntimeDownloads.psm1') -Force
    $cacheRoot = Join-Path $RepositoryRoot "artifacts/development/rust-$script:managedRustVersion"
    $archive = Join-Path $cacheRoot 'rust.tar.xz'
    $distribution = "rust-$script:managedRustVersion-$script:managedRustHost"
    Save-KynxaPinnedDownload -Url "https://static.rust-lang.org/dist/$distribution.tar.xz" -Destination $archive -Sha256 $script:managedRustSha256 -ExpectedBytes 199904472 -Offline:$Offline -DownloadSourcesPath $DownloadSourcesPath
    $tar = Get-Command tar.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $tar) { throw 'Windows tar.exe is required to unpack the private Rust toolchain. / 缺少解压私有 Rust 所需的 Windows tar.exe。' }
    Write-Host "[Setup] Preparing private Rust $script:managedRustVersion in $cacheRoot"
    & $tar.Source -xf $archive -C $cacheRoot "$distribution/rustc" "$distribution/cargo" "$distribution/rust-std-$script:managedRustHost" "$distribution/LICENSE-APACHE" "$distribution/LICENSE-MIT" "$distribution/COPYRIGHT"
    if ($LASTEXITCODE -ne 0) { throw 'Private Rust archive extraction failed. / 私有 Rust 归档解压失败。' }
    $toolchainRoot = Join-Path $cacheRoot 'toolchain'
    # Install from official component manifests without shell installers or global rustup state.
    # 按官方组件清单安装，不调用 shell 安装器或全局 rustup。
    foreach ($component in @('rustc', "rust-std-$script:managedRustHost", 'cargo')) {
        $componentRoot = Join-Path $cacheRoot "$distribution/$component"
        foreach ($entry in [IO.File]::ReadAllLines((Join-Path $componentRoot 'manifest.in'))) {
            $kind, $relative = $entry.Split(':', 2)
            $source = Join-Path $componentRoot $relative
            $destination = Join-Path $toolchainRoot $relative
            if ($kind -eq 'dir') {
                [IO.Directory]::CreateDirectory($destination) | Out-Null
                foreach ($child in Get-ChildItem -LiteralPath $source -Force) {
                    Copy-Item -LiteralPath $child.FullName -Destination $destination -Recurse -Force
                }
            }
            elseif ($kind -eq 'file') {
                [IO.Directory]::CreateDirectory((Split-Path -Parent $destination)) | Out-Null
                Copy-Item -LiteralPath $source -Destination $destination -Force
            }
        }
    }
    foreach ($notice in @('LICENSE-APACHE', 'LICENSE-MIT', 'COPYRIGHT')) {
        Copy-Item -LiteralPath (Join-Path $cacheRoot "$distribution/$notice") -Destination $toolchainRoot -Force
    }
}

function Restore-KynxaDevelopmentEnvironment {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][Collections.IDictionary]$Snapshot)
    foreach ($name in $Snapshot.Keys) {
        $value = if ($null -eq $Snapshot[$name]) { [NullString]::Value } else { $Snapshot[$name] }
        [Environment]::SetEnvironmentVariable($name, $value, 'Process')
    }
}

function Initialize-KynxaDevelopmentEnvironment {
    [CmdletBinding()]
    param(
        [string]$RepositoryRoot = ([IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))),
        [switch]$InstallRust,
        [switch]$Offline,
        [string]$DownloadSourcesPath = $env:KYNXA_DOWNLOAD_SOURCES
    )
    $environment = Get-KynxaDevelopmentEnvironment $RepositoryRoot
    if (-not $environment.Rust -and $InstallRust -and [Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
        try { Install-KynxaDevelopmentRust $environment.RepositoryRoot -Offline:$Offline -DownloadSourcesPath $DownloadSourcesPath }
        catch { throw (($environment.Issues + $_.Exception.Message) -join [Environment]::NewLine) }
        $environment = Get-KynxaDevelopmentEnvironment $environment.RepositoryRoot
    }
    if (-not $environment.IsReady) { throw ($environment.Issues -join [Environment]::NewLine) }
    $msvc = $environment.VisualStudio.MsvcRoot
    $sdkRoot = $environment.WindowsSdk.Root
    $sdkVersion = $environment.WindowsSdk.Version
    $dotnetRoot = Split-Path -Parent $environment.DotNet.Path
    $rustBin = Split-Path -Parent $environment.Rust.Rustc
    $values = @{
        PATH = ($rustBin, $dotnetRoot, (Join-Path $msvc 'bin/Hostx64/x64'), (Join-Path $sdkRoot "bin/$sdkVersion/x64"), $env:PATH) -join ';'
        INCLUDE = ((Join-Path $msvc 'include'), (Join-Path $sdkRoot "Include/$sdkVersion/ucrt"), (Join-Path $sdkRoot "Include/$sdkVersion/shared"), (Join-Path $sdkRoot "Include/$sdkVersion/um"), (Join-Path $sdkRoot "Include/$sdkVersion/winrt")) -join ';'
        LIB = ((Join-Path $msvc 'lib/x64'), (Join-Path $sdkRoot "Lib/$sdkVersion/ucrt/x64"), (Join-Path $sdkRoot "Lib/$sdkVersion/um/x64")) -join ';'
        VCToolsInstallDir = $msvc.TrimEnd('\', '/') + '\'
        VCINSTALLDIR = (Join-Path $environment.VisualStudio.InstallRoot 'VC') + '\'
        VSINSTALLDIR = $environment.VisualStudio.InstallRoot.TrimEnd('\', '/') + '\'
        WindowsSdkDir = $sdkRoot.TrimEnd('\', '/') + '\'
        WindowsSDKVersion = $sdkVersion + '\'
        WindowsSDKLibVersion = $sdkVersion + '\'
        UniversalCRTSdkDir = $sdkRoot.TrimEnd('\', '/') + '\'
        UCRTVersion = $sdkVersion
        VSCMD_ARG_TGT_ARCH = 'x64'
        VSCMD_ARG_HOST_ARCH = 'x64'
        DOTNET_ROOT = $dotnetRoot
        DOTNET_ROOT_X64 = $dotnetRoot
        RUSTC = $environment.Rust.Rustc
        RUSTDOC = Join-Path $rustBin 'rustdoc.exe'
    }
    if ($environment.Rust.IsManaged -and -not $env:CARGO_HOME) { $values.CARGO_HOME = Join-Path $environment.RepositoryRoot "artifacts/development/rust-$script:managedRustVersion/cargo-home" }
    $snapshot = @{}
    foreach ($name in $values.Keys) { $snapshot[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
    try {
        foreach ($name in $values.Keys) { [Environment]::SetEnvironmentVariable($name, $values[$name], 'Process') }
    }
    catch { Restore-KynxaDevelopmentEnvironment $snapshot; throw }
    $environment | Add-Member -NotePropertyName PreviousEnvironment -NotePropertyValue $snapshot
    return $environment
}

Export-ModuleMember -Function Get-KynxaDevelopmentEnvironment, Initialize-KynxaDevelopmentEnvironment, Restore-KynxaDevelopmentEnvironment
