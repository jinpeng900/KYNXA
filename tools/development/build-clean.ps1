[CmdletBinding()]
param(
    [string]$ResourcePackPath,
    [ValidateSet('Debug', 'Release')][string]$Configuration = 'Debug'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$cleanRoot = Join-Path $repositoryRoot 'artifacts/clean-build'
$cargoHome = Join-Path $cleanRoot 'cargo-home'
$rustupHome = Join-Path $cleanRoot 'rustup-home'
$nugetRoot = Join-Path $cleanRoot 'nuget/packages'
$rustVersion = '1.90.0'
$rustHost = 'x86_64-pc-windows-msvc'
if ([string]::IsNullOrWhiteSpace($ResourcePackPath)) {
    $ResourcePackPath = Join-Path $repositoryRoot 'kynxa-win-x64-resources-20261014.zip'
}
$ResourcePackPath = [IO.Path]::GetFullPath($ResourcePackPath)
if (-not [IO.File]::Exists($ResourcePackPath)) { throw "Resource pack is missing: $ResourcePackPath" }
$dotnetCommand = Get-Command dotnet -ErrorAction Stop
$vswherePath = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
if (-not [IO.File]::Exists($vswherePath)) { throw 'Install Visual Studio with the Windows SDK and MSVC build tools before building.' }
$visualStudioPath = @(& $vswherePath -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath) | Select-Object -First 1
if (-not $visualStudioPath) { throw 'Visual Studio MSVC x64 build tools were not found.' }
$developerShell = Join-Path $visualStudioPath 'Common7/Tools/VsDevCmd.bat'
if (-not [IO.File]::Exists($developerShell)) { throw "Visual Studio developer shell is missing: $developerShell" }
$toolsetVersionPath = Join-Path $visualStudioPath 'VC/Auxiliary/Build/Microsoft.VCToolsVersion.default.txt'
$toolsetVersion = [IO.File]::ReadAllText($toolsetVersionPath).Trim()
$toolsetRoot = Join-Path $visualStudioPath "VC/Tools/MSVC/$toolsetVersion"
$compilerBin = Join-Path $toolsetRoot 'bin/Hostx64/x64'
foreach ($compilerFile in @('cl.exe', 'link.exe')) {
    if (-not [IO.File]::Exists((Join-Path $compilerBin $compilerFile))) { throw "Installed MSVC tool is missing: $compilerBin/$compilerFile" }
}
$sdkRoot = (Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots' -Name KitsRoot10).KitsRoot10
$sdkVersion = @(Get-ChildItem -LiteralPath (Join-Path $sdkRoot 'Include') -Directory |
    Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' -and
        [IO.File]::Exists((Join-Path $sdkRoot "Lib/$($_.Name)/ucrt/x64/ucrt.lib")) -and
        [IO.File]::Exists((Join-Path $sdkRoot "Lib/$($_.Name)/um/x64/kernel32.lib")) } |
    Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1).Name
if (-not $sdkVersion) { throw 'An installed Windows SDK with x64 UCRT and Windows libraries is required.' }
$compilerInclude = (Join-Path $toolsetRoot 'include') + ';' + (Join-Path $sdkRoot "Include/$sdkVersion/ucrt") + ';' +
    (Join-Path $sdkRoot "Include/$sdkVersion/shared") + ';' + (Join-Path $sdkRoot "Include/$sdkVersion/um")
$compilerLibraries = (Join-Path $toolsetRoot 'lib/x64') + ';' + (Join-Path $sdkRoot "Lib/$sdkVersion/ucrt/x64") + ';' +
    (Join-Path $sdkRoot "Lib/$sdkVersion/um/x64")

# Build dependencies live only in this extracted checkout; no other project's cache is read or copied.
# 构建依赖仅保存在当前解压源码目录，不读取或复制其他项目的缓存。
$environmentValues = @{
    CARGO_HOME = $cargoHome
    RUSTUP_HOME = $rustupHome
    RUSTUP_TOOLCHAIN = "$rustVersion-$rustHost"
    CARGO_TARGET_DIR = (Join-Path $cleanRoot 'cargo-target')
    RUSTC = (Join-Path $cargoHome 'bin/rustc.exe')
    RUSTDOC = (Join-Path $cargoHome 'bin/rustdoc.exe')
    NUGET_PACKAGES = $nugetRoot
    NUGET_HTTP_CACHE_PATH = (Join-Path $cleanRoot 'nuget/http-cache')
    NUGET_PLUGINS_CACHE_PATH = (Join-Path $cleanRoot 'nuget/plugins-cache')
    DOTNET_CLI_HOME = (Join-Path $cleanRoot 'dotnet-home')
    DOTNET_SKIP_FIRST_TIME_EXPERIENCE = '1'
    KYNXA_CLEAN_VSDEV_CMD = $developerShell
    KYNXA_CLEAN_COMPILER_BIN = $compilerBin
    KYNXA_CLEAN_COMPILER_INCLUDE = $compilerInclude
    KYNXA_CLEAN_COMPILER_LIB = $compilerLibraries
    KYNXA_CLEAN_SDK_BIN = (Join-Path $sdkRoot "bin/$sdkVersion/x64")
    KYNXA_CLEAN_DOTNET_EXE = $dotnetCommand.Source
    KYNXA_CLEAN_DESKTOP_PROJECT = (Join-Path $repositoryRoot 'apps/desktop/KYNXA.Desktop.csproj')
    KYNXA_CLEAN_CONFIGURATION = $Configuration
    KYNXA_CLEAN_RESOURCE_PACK = $ResourcePackPath
    KYNXA_CLEAN_NUGET_PACKAGES = $nugetRoot
    KYNXA_CLEAN_BUILD_LOG = (Join-Path $cleanRoot 'msbuild.log')
}
$previousEnvironment = @{}
$transcriptStarted = $false
try {
    [IO.Directory]::CreateDirectory($cleanRoot) | Out-Null
    foreach ($name in $environmentValues.Keys) {
        $previousEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        [Environment]::SetEnvironmentVariable($name, $environmentValues[$name], 'Process')
    }
    $previousEnvironment['PATH'] = [Environment]::GetEnvironmentVariable('PATH', 'Process')
    [Environment]::SetEnvironmentVariable('PATH', (Join-Path $cargoHome 'bin') + [IO.Path]::PathSeparator + $previousEnvironment['PATH'], 'Process')
    foreach ($directory in @($cargoHome, $rustupHome, $nugetRoot, $env:NUGET_HTTP_CACHE_PATH,
        $env:NUGET_PLUGINS_CACHE_PATH, $env:DOTNET_CLI_HOME)) {
        [IO.Directory]::CreateDirectory($directory) | Out-Null
    }
    Start-Transcript -LiteralPath (Join-Path $cleanRoot 'build.log') -Append | Out-Null
    $transcriptStarted = $true
    Write-Host "[Build] Independent dependency root: $cleanRoot"
    Write-Host "[Build] Full local resource pack: $ResourcePackPath"

    $toolchainCompiler = Join-Path $rustupHome "toolchains/$rustVersion-$rustHost/bin/rustc.exe"
    if (-not [IO.File]::Exists($toolchainCompiler)) {
        $rustupCommand = Join-Path $cargoHome 'bin/rustup.exe'
        if ([IO.File]::Exists($rustupCommand)) {
            Write-Host "[Rust] Preparing private toolchain $rustVersion."
            & $rustupCommand toolchain install "$rustVersion-$rustHost" --profile minimal
            if ($LASTEXITCODE -ne 0) { throw "Private Rust toolchain preparation failed: $LASTEXITCODE" }
        }
        else {
            $installerPath = Join-Path $cleanRoot 'rustup-init.exe'
            if (-not [IO.File]::Exists($installerPath)) {
                $installerUrl = "https://static.rust-lang.org/rustup/dist/$rustHost/rustup-init.exe"
                Write-Host "[Rust] Downloading the official installer: $installerUrl"
                $previousTls = [Net.ServicePointManager]::SecurityProtocol
                try {
                    [Net.ServicePointManager]::SecurityProtocol = $previousTls -bor [Net.SecurityProtocolType]::Tls12
                    Invoke-WebRequest -Uri $installerUrl -OutFile ($installerPath + '.download') -UseBasicParsing
                    Move-Item -LiteralPath ($installerPath + '.download') -Destination $installerPath -Force
                }
                finally { [Net.ServicePointManager]::SecurityProtocol = $previousTls }
            }
            Write-Host "[Rust] Installing $rustVersion into this checkout; the system PATH is unchanged."
            & $installerPath -y --no-modify-path --profile minimal --default-host $rustHost --default-toolchain $rustVersion
            if ($LASTEXITCODE -ne 0) { throw "Private Rust installation failed: $LASTEXITCODE" }
        }
    }
    Write-Host '[Build] Restoring NuGet dependencies and compiling the complete desktop and Rust runtime.'

    # Discover MSVC/SDK paths from the installed toolset and use them only in the build subprocess.
    # 从已安装工具集定位 MSVC/SDK，只在构建子进程使用，避免扩展脚本枚举失败；不修改注册表或系统环境。
    # An ASCII command file carries paths through process environment variables, including Unicode and spaces.
    # ASCII 命令文件通过进程环境传递中文及空格路径；开发环境只作用于此构建子进程。
    $commandPath = Join-Path $cleanRoot 'build.cmd'
    $commandText = @'
@echo off
chcp 65001 >nul
call "%KYNXA_CLEAN_VSDEV_CMD%" -arch=x64 -host_arch=x64 -no_ext
if errorlevel 1 echo [Build] Visual Studio reported a warning; checking the required compiler environment.
set "PATH=%KYNXA_CLEAN_COMPILER_BIN%;%KYNXA_CLEAN_SDK_BIN%;%PATH%"
set "INCLUDE=%KYNXA_CLEAN_COMPILER_INCLUDE%"
set "EXTERNAL_INCLUDE=%KYNXA_CLEAN_COMPILER_INCLUDE%"
set "LIB=%KYNXA_CLEAN_COMPILER_LIB%"
where cl.exe >nul 2>&1
if errorlevel 1 goto missing_compiler
where link.exe >nul 2>&1
if errorlevel 1 goto missing_compiler
"%KYNXA_CLEAN_DOTNET_EXE%" build "%KYNXA_CLEAN_DESKTOP_PROJECT%" --configuration "%KYNXA_CLEAN_CONFIGURATION%" --runtime win-x64 -p:Platform=x64 -p:KynxaRuntimeProfile=Full "-p:KynxaResourcePackRoot=%KYNXA_CLEAN_RESOURCE_PACK%" "-p:RestorePackagesPath=%KYNXA_CLEAN_NUGET_PACKAGES%" -p:RestoreNoCache=true -p:NuGetAudit=false "-flp:logfile=%KYNXA_CLEAN_BUILD_LOG%;verbosity=normal;encoding=UTF-8" 2>&1
exit /b %errorlevel%
:missing_compiler
echo [Build] MSVC cl.exe or link.exe is unavailable; Visual Studio initialization failed.
exit /b 1
'@
    [IO.File]::WriteAllText($commandPath, $commandText.Replace("`r`n", "`n").Replace("`n", "`r`n") + "`r`n", [Text.Encoding]::ASCII)
    & $env:ComSpec /d /s /c ('""' + $commandPath + '""')
    if ($LASTEXITCODE -ne 0) { throw "Complete build failed with exit code $LASTEXITCODE. See $cleanRoot/build.log and msbuild.log." }
    Write-Host "[Build] Complete. Output: $(Join-Path $repositoryRoot 'apps/desktop/bin/x64')"
    Write-Host "[Build] Logs: $cleanRoot/build.log and msbuild.log"
}
finally {
    if ($transcriptStarted) { Stop-Transcript | Out-Null }
    foreach ($name in $previousEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $previousEnvironment[$name], 'Process')
    }
}
