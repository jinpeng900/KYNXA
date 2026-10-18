[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
Import-Module (Join-Path $repositoryRoot 'apps/desktop/Build/DesktopBuild.psm1') -Force
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('kynxa-build-entry-' + [Guid]::NewGuid().ToString('N'))
$checks = [Collections.Generic.List[string]]::new()
try {
    [IO.Directory]::CreateDirectory($fixture) | Out-Null
    $statePath = Join-Path $fixture 'state.json'
    $state = @{schemaVersion=1; steps=@{}; build=@{signature='previous-success'}}
    Save-KynxaBuildState $statePath $state
    $state = Read-KynxaBuildState $statePath
    if ($state.build.signature -ne 'previous-success') { throw 'Build state did not roundtrip.' }
    try { Invoke-KynxaBuildStage 5 'Expected failure' compile $state $statePath { throw 'expected failure' } }
    catch { if ($_.Exception.Message -ne 'expected failure') { throw } }
    $state = Read-KynxaBuildState $statePath
    if ($state.steps.compile.status -ne 'failed' -or $state.build.signature -ne 'previous-success') { throw 'A failed stage lost the previous build.' }
    Invoke-KynxaBuildStage 5 'Resume failed stage' compile $state $statePath { }
    if ((Read-KynxaBuildState $statePath).steps.compile.status -ne 'ready') { throw 'A failed stage could not resume.' }
    $checks.Add('Checkpoint, failure and retry preserve the last successful build.')
    $source = Join-Path $fixture 'source'
    [IO.Directory]::CreateDirectory($source) | Out-Null
    $file = Join-Path $source 'sample.cs'
    [IO.File]::WriteAllText($file, 'first')
    $before = Get-KynxaBuildSignature $fixture @('source') @() @('Debug','Full')
    if ($before -ne (Get-KynxaBuildSignature $fixture @('source') @() @('Debug','Full'))) { throw 'Unchanged sources did not reuse the signature.' }
    if ($before -eq (Get-KynxaBuildSignature $fixture @('source') @() @('Release','Full'))) { throw 'Build configurations were mixed.' }
    if ($before -eq (Get-KynxaBuildSignature $fixture @('source') @() @('Debug','Core'))) { throw 'Runtime profiles were mixed.' }
    [IO.File]::WriteAllText($file, 'changed source')
    if ($before -eq (Get-KynxaBuildSignature $fixture @('source') @() @('Debug','Full'))) { throw 'Changed source was not detected.' }
    [IO.File]::Delete($file)
    if ($before -eq (Get-KynxaBuildSignature $fixture @('source') @() @('Debug','Full'))) { throw 'Deleted source was not detected.' }
    $checks.Add('Unchanged sources reuse state; changes, deletions, configuration and profile invalidate it.')
    # Use a fake local package, never installed frameworks or a network restore.
    # 仅使用本地伪包，不改已安装框架，也不发起网络还原。
    $packages = Join-Path $fixture 'packages'
    $package = Join-Path $packages 'sample/1.0.0'
    [IO.Directory]::CreateDirectory($package) | Out-Null
    [IO.File]::WriteAllText((Join-Path $package '.nupkg.metadata'), '{}')
    $assetsPath = Join-Path $fixture 'project.assets.json'
    $assets = @{packageFolders=@{}; libraries=@{'sample/1.0.0'=@{type='package';path='sample/1.0.0'}};targets=@{'net10.0/win-x64'=@{'sample/1.0.0'=@{compile=@{'sample.dll'=@{}}}}}}
    $assets.packageFolders[$packages] = @{}
    [IO.File]::WriteAllText($assetsPath, ($assets | ConvertTo-Json -Depth 12))
    if (Test-KynxaNuGetAssets $assetsPath) { throw 'A package with a missing compiler asset was accepted.' }
    [IO.File]::WriteAllText((Join-Path $package 'sample.dll'), 'fixture only')
    if (-not (Test-KynxaNuGetAssets $assetsPath)) { throw 'A complete local package could not be reused.' }
    $checks.Add('NuGet reuse checks selected assets, rather than package directory existence alone.')
    $directory = Join-Path $fixture '中文 空格'
    [IO.Directory]::CreateDirectory($directory) | Out-Null
    $echo = Join-Path $directory 'echo.ps1'
    [IO.File]::WriteAllText($echo, 'param([string]$Value) if($Value -ne ''中文 参数''){exit 9}; Write-Output ''argument roundtrip passed''', [Text.UTF8Encoding]::new($true))
    Invoke-KynxaBuildCommand (Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe') @('-NoProfile','-File',$echo,'-Value','中文 参数') $fixture (Join-Path $fixture 'commands.log') 'Path arguments'
    $checks.Add('Chinese and spaced executable arguments survive the real child-process boundary.')
    @{passed=$checks.Count;failed=0;checks=@($checks)} | ConvertTo-Json
}
finally {
    $temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    $resolved = [IO.Path]::GetFullPath($fixture)
    if (-not $resolved.StartsWith($temporaryRoot,[StringComparison]::OrdinalIgnoreCase) -or (Split-Path -Leaf $resolved) -notmatch '^kynxa-build-entry-[0-9a-f]{32}$') { throw 'Unexpected build test directory was preserved.' }
    if ([IO.Directory]::Exists($resolved)) { Remove-Item -LiteralPath $resolved -Recurse -Force }
}
exit 0
