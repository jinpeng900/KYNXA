[CmdletBinding()]
param(
    [ValidateSet('Setup', 'Build', 'Run', 'Rebuild', 'Publish')][string]$Action = 'Build',
    [ValidateSet('Debug', 'Release')][string]$Configuration = 'Debug',
    [ValidateSet('Full', 'Core')][string]$Profile = 'Full',
    [switch]$Offline,
    [switch]$VerifyResources,
    [string]$DownloadSourcesPath = $env:KYNXA_DOWNLOAD_SOURCES,
    [string]$ResourcePackRoot = $env:KYNXA_RESOURCE_PACK_ROOT,
    [string]$NuGetSource,
    [string]$NuGetConfigPath,
    [string]$NativeRuntimeRoot
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
Import-Module (Join-Path $PSScriptRoot 'DesktopBuild.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'DevelopmentEnvironment.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'ResourceConfigCompatibility.psm1') -Force
$stateDirectory = Join-Path $repositoryRoot "artifacts/development/build/win-x64/$Configuration/$Profile"
$statePath = Join-Path $stateDirectory 'state.json'
$logPath = Join-Path $stateDirectory ($Action.ToLowerInvariant() + '.log')
$lockPath = Join-Path $repositoryRoot 'artifacts/development/build/build.lock'
$projectLock = $null
$environment = $null
$transcribing = $false
$location = Get-Location
try {
    [IO.Directory]::CreateDirectory($stateDirectory) | Out-Null
    try { $projectLock = [IO.File]::Open($lockPath, 'OpenOrCreate', 'ReadWrite', 'None') }
    catch [IO.IOException] { throw "本项目已有构建或准备任务在进行。缓存已保留，请等待当前任务结束。日志目录：$stateDirectory" }
    Start-Transcript -LiteralPath $logPath -Append | Out-Null
    $transcribing = $true
    Set-Location -LiteralPath $repositoryRoot
    $state = Read-KynxaBuildState $statePath
    $desktopProject = Join-Path $repositoryRoot 'apps/desktop/KYNXA.Desktop.csproj'
    $toolHostProject = Join-Path $repositoryRoot 'apps/tool-host/KYNXA.ToolHost.csproj'
    # Use one selected resource root for preparation, reuse checks and the desktop build.
    # 资源准备、复用检查与桌面构建使用同一选定根目录，避免再次生成另一份长路径缓存。
    $runtimeRoot = if ($NativeRuntimeRoot) { [IO.Path]::GetFullPath([IO.Path]::Combine($NativeRuntimeRoot, '.')) } else { Join-Path $repositoryRoot 'artifacts/runtime/native-desktop/win-x64' }
    $windowsPowerShell = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
    $inputDirectories = @('apps/desktop','apps/shared','apps/tool-host','apps/agent-core/src','apps/resource-service/src','apps/official-tools','tools/development')
    $inputFiles = @('global.json','Directory.Build.props','Directory.Build.targets','NuGet.Config','apps/agent-core/Cargo.toml','apps/agent-core/Cargo.lock','apps/resource-service/Cargo.toml','apps/resource-service/Cargo.lock')
    foreach ($name in @('DownloadSourcesPath','NuGetConfigPath','ResourcePackRoot')) {
        $value = Get-Variable -Name $name -ValueOnly
        if ($value) { Set-Variable -Name $name -Value ([IO.Path]::GetFullPath($value)) }
    }
    if (-not $ResourcePackRoot) {
        $archives = @([IO.Directory]::EnumerateFiles($repositoryRoot,'kynxa-*-resources*.zip'))
        if ($archives.Count -eq 1) { $ResourcePackRoot = $archives[0] }
    }
    foreach ($path in @($DownloadSourcesPath,$NuGetConfigPath,$ResourcePackRoot)) { if ($path) { $inputFiles += $path } }
    $options = @($Configuration,$Profile,'win-x64',$DownloadSourcesPath,$ResourcePackRoot,$NuGetSource,$NuGetConfigPath,$runtimeRoot)
    $outputFiles = @('KYNXA.Desktop.exe','KYNXA.Desktop.dll','KYNXA.Desktop.deps.json','KYNXA.Desktop.runtimeconfig.json','resources.pri','AppxManifest.xml','runtime/agent/kynxa-gateway.exe')
    $signature = Get-KynxaBuildSignature $repositoryRoot $inputDirectories $inputFiles $options
    if ($state.ContainsKey('repositoryRoot') -and $state.repositoryRoot -ne $repositoryRoot -and $state.ContainsKey('build')) {
        foreach ($name in @('output','recipe','applicationRoot','intermediate')) {
            $oldPath = [string]$state.build.layout[$name]
            if ($oldPath.StartsWith($state.repositoryRoot.TrimEnd('\') + '\',[StringComparison]::OrdinalIgnoreCase)) {
                $state.build.layout[$name] = Join-Path $repositoryRoot $oldPath.Substring($state.repositoryRoot.TrimEnd('\').Length + 1)
            }
        }
    }
    $state.repositoryRoot = $repositoryRoot
    $built = $state.ContainsKey('build') -and $state.build.signature -eq $signature
    if ($built) {
        $built = $state.build.ContainsKey('outputs')
        foreach ($relative in $outputFiles) {
            if (-not $built) { break }
            $file = [IO.FileInfo]::new((Join-Path $state.build.layout.output $relative))
            if (-not $file.Exists -or -not $state.build.outputs.ContainsKey($relative) -or
                $file.Length -ne $state.build.outputs[$relative].bytes -or $file.LastWriteTimeUtc.Ticks -ne $state.build.outputs[$relative].modified) { $built = $false; break }
        }
    }
    if ($Action -eq 'Run' -and $built -and -not $VerifyResources) {
        $resourceInputs = Get-KynxaNativeAssetsInputHash -Profile $Profile -RuntimeIdentifier win-x64
        $built = Test-KynxaPreparedComponent -Root $runtimeRoot -Component native-assets -InputHash $resourceInputs
        if ($built) {
            Write-Host '[步骤 1/6] 源码与构建输入未变，复用成功产物；跳过下载、恢复、解压和编译。'
            $deployed = $true
            foreach ($relative in $outputFiles) {
                $file = [IO.FileInfo]::new((Join-Path $state.build.layout.applicationRoot $relative))
                if (-not $file.Exists -or ($relative -ne 'AppxManifest.xml' -and
                    ($file.Length -ne $state.build.outputs[$relative].bytes -or $file.LastWriteTimeUtc.Ticks -ne $state.build.outputs[$relative].modified))) {
                    $deployed = $false; break
                }
            }
            if (-not $deployed -or -not $state.build.ContainsKey('deployedSignature') -or $state.build.deployedSignature -ne $signature) {
                $state.build.layout = Update-KynxaDesktopLayout $repositoryRoot $state.build.layout
                $state.build.deployedSignature = $signature
                Save-KynxaBuildState $statePath $state
            }
            Start-KynxaDesktopBuild $repositoryRoot $state.build.layout $stateDirectory
            return
        }
    }
    $inspection = $null
    $discovery = Get-KynxaDevelopmentEnvironment -RepositoryRoot $repositoryRoot
    $issues = [Collections.Generic.List[string]]::new()
    foreach ($issue in $discovery.Issues) {
        if (-not $discovery.Rust -and $issue -match 'Rust') { continue }
        $issues.Add([string]$issue)
    }
    foreach ($name in @('DownloadSourcesPath','NuGetConfigPath','ResourcePackRoot')) {
        $value = Get-Variable -Name $name -ValueOnly
        if ($value) {
            $value = [IO.Path]::GetFullPath($value)
            Set-Variable -Name $name -Value $value
            if (-not [IO.File]::Exists($value)) { $issues.Add("$name 文件不存在：$value") }
        }
    }
    if ($ResourcePackRoot -and [IO.File]::Exists($ResourcePackRoot)) {
        try { $inspection = & (Join-Path $PSScriptRoot 'prepare-resource-pack.ps1') -Action Inspect -ArchivePath $ResourcePackRoot }
        catch { $issues.Add('资源包：' + $_.Exception.Message) }
    }
    if ($NuGetSource -and $NuGetSource -notmatch '^[a-zA-Z][a-zA-Z0-9+.-]*://') {
        $NuGetSource = [IO.Path]::GetFullPath($NuGetSource)
        if (-not [IO.Directory]::Exists($NuGetSource)) { $issues.Add("NuGet 本地源不存在：$NuGetSource") }
    }
    if ($Offline -and $NuGetSource -match '^https?://') { $issues.Add('离线模式不能使用远程 NuGetSource。') }
    if ($Offline -and $NuGetConfigPath -and -not $NuGetSource -and [IO.File]::Exists($NuGetConfigPath)) {
        [xml]$nuGetConfiguration = Get-Content -LiteralPath $NuGetConfigPath -Raw
        foreach ($source in $nuGetConfiguration.SelectNodes('/configuration/packageSources/add')) {
            if ($source.value -match '^https?://') { $issues.Add('离线 NuGet 配置含网络源，请指定本地 -NuGetSource。'); break }
        }
    }
    $drive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($repositoryRoot))
    Write-Host ("[步骤 1/6] 环境与资源预检；目标磁盘剩余 {0:N1} GiB，日志：{1}" -f ($drive.AvailableFreeSpace / 1GB),$logPath)
    if ($drive.AvailableFreeSpace -lt 512MB) { $issues.Add('目标磁盘剩余不足 512 MiB，请先释放空间。') }
    elseif ($drive.AvailableFreeSpace -lt 8GB) { Write-Warning '磁盘可用空间较少，首次准备完整资源及编译缓存可能需要更多空间；已有缓存会继续复用。' }
    if ($discovery.AutomaticActions.Count) { Write-Host ('[预检] 可自动补齐：' + ($discovery.AutomaticActions -join '；')) }
    if ($issues.Count) { throw ("环境预检发现以下问题，尚未开始下载或编译：`n- " + ($issues -join "`n- ")) }
    $environment = Invoke-KynxaBuildStage 2 '检查并复用工具链，缺少 Rust 时自动补齐' toolchain $state $statePath {
        Initialize-KynxaDevelopmentEnvironment -RepositoryRoot $repositoryRoot -InstallRust -Offline:$Offline -DownloadSourcesPath $DownloadSourcesPath
    }
    $prepareArguments = @('-NoProfile','-ExecutionPolicy','Bypass','-File',(Join-Path $PSScriptRoot 'prepare-native-runtime.ps1'),'-AssetsOnly','-Profile',$Profile,'-BundleRoot',$runtimeRoot)
    if ($Offline) { $prepareArguments += '-Offline' }
    if ($VerifyResources) { $prepareArguments += '-VerifyResources' }
    if ($DownloadSourcesPath) { $prepareArguments += @('-DownloadSourcesPath',$DownloadSourcesPath) }
    if ($ResourcePackRoot) { $prepareArguments += @('-ResourcePackRoot',$ResourcePackRoot) }
    Invoke-KynxaBuildStage 3 '准备资源，复用已就绪的组件' resources $state $statePath {
        Invoke-KynxaBuildCommand $windowsPowerShell $prepareArguments $repositoryRoot $logPath '准备运行资源'
        $vcArguments = @('-NoProfile','-ExecutionPolicy','Bypass','-File',(Join-Path $PSScriptRoot 'prepare-vc-runtime.ps1'),'-RuntimeIdentifier','win-x64','-CacheDirectory',(Join-Path $repositoryRoot 'artifacts/runtime/vc/win-x64'))
        if ($Offline) { $vcArguments += '-Offline' }
        if ($DownloadSourcesPath) { $vcArguments += @('-DownloadSourcesPath',$DownloadSourcesPath) }
        if ($ResourcePackRoot) { $vcArguments += @('-ResourcePackRoot',$ResourcePackRoot) }
        Invoke-KynxaBuildCommand $windowsPowerShell $vcArguments $repositoryRoot $logPath '准备 VC 运行库'
    } | Out-Null
    $restoreFiles = @('apps/desktop/KYNXA.Desktop.csproj','apps/tool-host/KYNXA.ToolHost.csproj','global.json','Directory.Build.props','Directory.Build.targets','NuGet.Config')
    if ($NuGetConfigPath) { $restoreFiles += $NuGetConfigPath }
    $restoreSignature = Get-KynxaBuildSignature $repositoryRoot @() $restoreFiles @('win-x64',$environment.DotNet.SdkVersion,$NuGetSource,$NuGetConfigPath)
    Invoke-KynxaBuildStage 4 '恢复必要依赖并预先检查许可证' dependencies $state $statePath {
        $assets = @((Join-Path $repositoryRoot 'apps/desktop/obj/project.assets.json'),(Join-Path $repositoryRoot 'apps/tool-host/obj/project.assets.json'))
        $reuse = $state.ContainsKey('restore') -and $state.restore.signature -eq $restoreSignature
        foreach ($path in $assets) { if (-not (Test-KynxaNuGetAssets $path)) { $reuse = $false } }
        if ($reuse) { Write-Host '[依赖] 清单与恢复结果有效，跳过 NuGet restore。' }
        else {
            $common = @('restore','--runtime','win-x64','-p:NuGetAudit=false','--verbosity','minimal')
            if ($NuGetSource) { $common += @('--source',$NuGetSource) }
            elseif ($Offline) {
                # Resolve changed projects from cache and local sources, without stale assets or network access.
                # 项目变化后从缓存和本地源重新还原，不复用过期资产或联网。
                $feed = Join-Path $stateDirectory 'nuget-feed'
                [IO.Directory]::CreateDirectory($feed) | Out-Null
                $common += @('--source',$feed)
            }
            if ($NuGetConfigPath) { $common += @('--configfile',$NuGetConfigPath) }
            Invoke-KynxaBuildCommand $environment.DotNet.Path ($common + @($desktopProject,'-p:Platform=x64')) $repositoryRoot $logPath '恢复桌面依赖'
            Invoke-KynxaBuildCommand $environment.DotNet.Path ($common + @($toolHostProject,'-p:SelfContained=true')) $repositoryRoot $logPath '恢复 ToolHost 依赖'
        }
        & (Join-Path $PSScriptRoot 'prepare-toolhost-notices.ps1') -AssetsPath $assets[1] -RuntimeIdentifier win-x64 -OutputDirectory (Join-Path $repositoryRoot 'apps/tool-host/obj/prepared-notices/win-x64') | Write-Host
        $state.restore = @{ signature = $restoreSignature; sdk = $environment.DotNet.SdkVersion }
    } | Out-Null
    if ($Action -eq 'Setup') { Write-Host '[步骤 5/6] 环境、资源与依赖已就绪，Setup 不编译项目。'; return }
    $arguments = @('build',$desktopProject,'--configuration',$Configuration,'--no-restore','-p:Platform=x64',"-p:KynxaRuntimeProfile=$Profile",'-p:KynxaDependenciesPrepared=true',"-p:KynxaNativeRuntimeRoot=$runtimeRoot")
    if ($Action -eq 'Rebuild') { $arguments += @('-t:Rebuild','-p:KynxaRebuildNative=true') }
    if ($Action -eq 'Publish') { $arguments[0] = 'publish' }
    if ($Offline) { $arguments += '-p:KynxaOffline=true' }
    if ($DownloadSourcesPath) { $arguments += "-p:KynxaDownloadSourcesPath=$DownloadSourcesPath" }
    Invoke-KynxaBuildStage 5 '增量构建桌面与 Rust 组件' build $state $statePath {
        Invoke-KynxaBuildCommand $environment.DotNet.Path $arguments $repositoryRoot $logPath '构建桌面'
    } | Out-Null
    $state.build = @{ signature = $signature; layout = (Get-KynxaDesktopLayout $repositoryRoot $Configuration); sdk = $environment.DotNet.SdkVersion }
    $state.build.outputs = @{}
    foreach ($relative in $outputFiles) {
        $file = [IO.FileInfo]::new((Join-Path $state.build.layout.output $relative))
        if (-not $file.Exists) { throw "构建结束但缺少产物：$($file.FullName)" }
        $state.build.outputs[$relative] = @{ bytes = $file.Length; modified = $file.LastWriteTimeUtc.Ticks }
    }
    Save-KynxaBuildState $statePath $state
    if ($Action -eq 'Run') {
        Invoke-KynxaBuildStage 6 '启动匹配本次构建的软件' launch $state $statePath {
            $state.build.layout = Update-KynxaDesktopLayout $repositoryRoot $state.build.layout
            $state.build.deployedSignature = $state.build.signature
            Start-KynxaDesktopBuild $repositoryRoot $state.build.layout $stateDirectory
        } | Out-Null
    }
    else { Write-Host '[步骤 6/6] 构建完成；再次使用 -Action Run 可直接启动。' }
}
catch {
    [Console]::Error.WriteLine($_.Exception.Message + [Environment]::NewLine + '日志：' + $logPath)
    exit 1
}
finally {
    if ($environment -and $environment.PSObject.Properties['PreviousEnvironment']) { Restore-KynxaDevelopmentEnvironment -Snapshot $environment.PreviousEnvironment }
    Set-Location -LiteralPath $location.Path
    if ($transcribing) { Stop-Transcript | Out-Null }
    if ($projectLock) { $projectLock.Dispose() }
}
