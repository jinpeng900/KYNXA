[CmdletBinding()]
param(
    [ValidateSet('Build', 'Run', 'Publish')][string]$Action = 'Build',
    [ValidateSet('Debug', 'Release')][string]$Configuration = 'Debug',
    [ValidateSet('Full', 'Core')][string]$Profile = 'Full',
    [switch]$Offline,
    [string]$DownloadSourcesPath,
    [string]$ResourcePackRoot,
    [string]$NuGetSource,
    [string]$NuGetConfigPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
$desktopProject = Join-Path $repositoryRoot 'apps/desktop/KYNXA.Desktop.csproj'
$toolHostProject = Join-Path $repositoryRoot 'apps/tool-host/KYNXA.ToolHost.csproj'

# A runtime pack supplies application resources, not the SDK or dependency-manager caches.
# 资源包只提供应用运行资源；离线源码构建仍需要 SDK 和已还原的开发依赖。
$restoreArguments = @('--runtime', 'win-x64', '-p:NuGetAudit=false')
if ($NuGetSource) {
    if ($Offline -and $NuGetSource -match '^https?://') { throw 'Offline NuGetSource must be a local package feed.' }
    if ($NuGetSource -notmatch '^[a-zA-Z][a-zA-Z0-9+.-]*://') { $NuGetSource = [IO.Path]::GetFullPath($NuGetSource) }
    $restoreArguments += @('--source', $NuGetSource)
}
if ($NuGetConfigPath) {
    $NuGetConfigPath = [IO.Path]::GetFullPath($NuGetConfigPath)
    if ($Offline -and -not $NuGetSource) {
        [xml]$nuGetConfiguration = Get-Content -LiteralPath $NuGetConfigPath -Raw
        foreach ($source in $nuGetConfiguration.SelectNodes('/configuration/packageSources/add')) {
            if ($source.value -match '^https?://') { throw 'Offline NuGet configuration contains a network feed; use an explicit local -NuGetSource.' }
        }
    }
    $restoreArguments += @('--configfile', $NuGetConfigPath)
}
if ($NuGetSource -or $NuGetConfigPath) {
    & dotnet restore $desktopProject @restoreArguments '-p:Platform=x64'
    if ($LASTEXITCODE -ne 0) { throw 'Desktop NuGet restore failed.' }
    & dotnet restore $toolHostProject @restoreArguments '-p:SelfContained=true'
    if ($LASTEXITCODE -ne 0) { throw 'ToolHost NuGet restore failed.' }
}
if ($Offline) {
    foreach ($project in @($desktopProject, $toolHostProject)) {
        $assetsPath = Join-Path (Split-Path -Parent $project) 'obj/project.assets.json'
        if (-not [IO.File]::Exists($assetsPath)) { throw "NuGet restore is missing: $assetsPath. Restore once online or supply a local package feed." }
    }
}
$arguments = @($Action.ToLowerInvariant(), '--project', $desktopProject)
if ($Action -ne 'Run') { $arguments = @($Action.ToLowerInvariant(), $desktopProject) }
$arguments += @('--configuration', $Configuration, '-p:Platform=x64', "-p:KynxaRuntimeProfile=$Profile")
if ($Offline -or $NuGetSource -or $NuGetConfigPath) { $arguments += '--no-restore' }
if ($Offline) { $arguments += '-p:KynxaOffline=true' }
if ($DownloadSourcesPath) { $arguments += "-p:KynxaDownloadSourcesPath=$([IO.Path]::GetFullPath($DownloadSourcesPath))" }
if ($ResourcePackRoot) { $arguments += "-p:KynxaResourcePackRoot=$([IO.Path]::GetFullPath($ResourcePackRoot))" }
if ($NuGetSource) { $arguments += "-p:RestoreSources=$NuGetSource" }
if ($NuGetConfigPath) { $arguments += "-p:RestoreConfigFile=$NuGetConfigPath" }
& dotnet @arguments
if ($LASTEXITCODE -ne 0) { throw "Desktop $Action failed with exit code $LASTEXITCODE." }
