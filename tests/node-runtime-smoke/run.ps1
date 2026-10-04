[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$BundleRoot)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
$runtimeSource = [IO.Path]::GetFullPath($BundleRoot)
$runtimeTestRoot = Join-Path ([IO.Path]::GetTempPath()) ('kynxa-portable-runtime-' + [Guid]::NewGuid().ToString('N'))
$runtimePackage = Join-Path $runtimeTestRoot 'package with spaces'
[IO.Directory]::CreateDirectory($runtimePackage) | Out-Null
$checks = 0
function Assert-Runtime([bool]$Condition, [string]$Description) {
    if (-not $Condition) { throw $Description }
    $script:checks++
}
function New-IsolatedStart([string]$Executable, [string]$Arguments) {
    $start = [Diagnostics.ProcessStartInfo]::new($Executable, $Arguments)
    $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    $start.WorkingDirectory = $runtimePackage
    $start.EnvironmentVariables.Clear()
    $start.EnvironmentVariables['SystemRoot'] = $env:SystemRoot
    $start.EnvironmentVariables['WINDIR'] = $env:SystemRoot
    $start.EnvironmentVariables['COMSPEC'] = Join-Path $env:SystemRoot 'System32/cmd.exe'
    foreach ($name in @('USERPROFILE', 'LOCALAPPDATA', 'APPDATA', 'TEMP', 'TMP')) { $start.EnvironmentVariables[$name] = $runtimeTestRoot }
    $start.EnvironmentVariables['PATH'] = ''
    $start.EnvironmentVariables['DOTNET_ROOT'] = Join-Path $runtimeTestRoot 'absent-dotnet'
    $start.EnvironmentVariables['DOTNET_ROOT(x86)'] = Join-Path $runtimeTestRoot 'absent-dotnet'
    $start.EnvironmentVariables['DOTNET_MULTILEVEL_LOOKUP'] = '0'
    $start.EnvironmentVariables['KYNXA_DATA_HOME'] = Join-Path $runtimeTestRoot 'Data'
    $start.EnvironmentVariables['KYNXA_EXTENSION_HOME'] = Join-Path $runtimeTestRoot 'UserTools'
    $start.EnvironmentVariables['NPM_CONFIG_USERCONFIG'] = Join-Path $runtimeTestRoot 'empty.npmrc'
    $start.EnvironmentVariables['NPM_CONFIG_CACHE'] = Join-Path $runtimeTestRoot 'npm-cache'
    $start.EnvironmentVariables['NPM_CONFIG_UPDATE_NOTIFIER'] = 'false'
    return $start
}
function Invoke-IsolatedRuntime([string]$Executable, [string]$Arguments, [string]$InputText = '') {
    $start = New-IsolatedStart $Executable $Arguments
    $start.RedirectStandardInput = $true
    $process = [Diagnostics.Process]::Start($start)
    try {
        $output = $process.StandardOutput.ReadToEndAsync(); $errorOutput = $process.StandardError.ReadToEndAsync()
        if ($InputText) { $process.StandardInput.WriteLine($InputText) }
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(15000)) { $process.Kill(); throw 'The copied runtime process timed out.' }
        if ($process.ExitCode -ne 0) { throw ("Copied runtime failed ($([IO.Path]::GetFileName($Executable)), exit $($process.ExitCode)): " + $errorOutput.Result + $output.Result) }
        return $output.Result.Trim()
    }
    finally { $process.Dispose() }
}

foreach ($directory in @('runtime', 'model-gateway', 'ToolHost')) {
    Assert-Runtime (Test-Path -LiteralPath (Join-Path $runtimeSource $directory) -PathType Container) "The app bundle is missing $directory."
    Copy-Item -LiteralPath (Join-Path $runtimeSource $directory) -Destination $runtimePackage -Recurse
}
$runtimeNode = Join-Path $runtimePackage 'runtime/node.exe'
$manifest = Get-Content -LiteralPath (Join-Path $runtimePackage 'runtime/node-runtime.json') -Raw | ConvertFrom-Json
$nodeInfo = Invoke-IsolatedRuntime $runtimeNode '-p "JSON.stringify({version:process.versions.node,arch:process.arch,execPath:process.execPath})"' | ConvertFrom-Json
Assert-Runtime ([IO.Path]::GetFullPath($nodeInfo.execPath) -eq $runtimeNode) 'The process executed the copied bundled node.exe, not an installed Node.'
$runtimeId = if ($nodeInfo.arch -eq 'ia32') { 'win-x86' } else { 'win-' + $nodeInfo.arch }
$runtimePin = $manifest.platforms.PSObject.Properties[$runtimeId].Value
Assert-Runtime ($nodeInfo.version -eq $runtimePin.version) 'Copied node.exe reports its platform-specific pinned official version.'
Assert-Runtime ((Get-FileHash -LiteralPath $runtimeNode -Algorithm SHA256).Hash.ToLowerInvariant() -eq $runtimePin.nodeSha256) 'The copied runtime matches the official executable checksum.'
Assert-Runtime ((Get-Item -LiteralPath (Join-Path $runtimePackage 'runtime/LICENSE')).Length -gt 100000) 'The complete Node and dependency licenses are present.'
foreach ($cli in @('npm', 'npx')) {
    $version = Invoke-IsolatedRuntime (Join-Path $env:SystemRoot 'System32/cmd.exe') ('/d /s /c ""' + (Join-Path $runtimePackage "runtime/$cli.cmd") + '" --version"')
    Assert-Runtime ($version -match '^\d+\.\d+\.\d+$') "Bundled $cli starts without installed Node/npm or a system PATH."
}
$runtimeHost = Join-Path $runtimePackage 'ToolHost/KYNXA.ToolHost.exe'
$hostCapabilities = Invoke-IsolatedRuntime $runtimeHost '' '{"operation":"desktop_capabilities"}' | ConvertFrom-Json
Assert-Runtime ($hostCapabilities.protocolVersion -eq 1 -and $hostCapabilities.boundary -eq 'host-desktop') 'Copied self-contained native ToolHost starts with unavailable DOTNET_ROOT and PATH.'
$desktopOperations = @('apps', 'windows', 'screenshot', 'read', 'launch', 'window', 'activate', 'move', 'click', 'scroll', 'drag', 'type', 'key')
Assert-Runtime ($hostCapabilities.operations.Count -eq $desktopOperations.Count) 'Copied ToolHost advertises the current desktop action contract.'
foreach ($operation in $desktopOperations) {
    Assert-Runtime ($hostCapabilities.operations -contains $operation) "Copied ToolHost includes $operation."
}
Assert-Runtime ($hostCapabilities.features.backgroundLaunch -eq 'best-effort-no-activate' -and
    $hostCapabilities.features.screenshotCrop -eq $true -and $hostCapabilities.features.boundedRead -eq $true) 'The copied helper retains background launch, original-pixel crop and bounded reads.'
foreach ($mode in @('resize', 'maximize', 'minimize', 'restore')) {
    Assert-Runtime ($hostCapabilities.features.windowModes -contains $mode) "The copied helper supports window $mode."
}
$hostTerminalCapabilities = Invoke-IsolatedRuntime $runtimeHost '' '{"operation":"host_terminal_capabilities"}' | ConvertFrom-Json
Assert-Runtime ($hostTerminalCapabilities.protocolVersion -eq 2 -and $hostTerminalCapabilities.boundary -eq 'host-terminal' -and
    $hostTerminalCapabilities.visibleTerminal -eq $true) 'The copied helper exposes current host terminal and real-console capability without external .NET.'

$runtimeListener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$runtimeListener.Start(); $runtimePort = $runtimeListener.LocalEndpoint.Port; $runtimeListener.Stop()
$start = New-IsolatedStart $runtimeNode ('"' + (Join-Path $runtimePackage 'model-gateway/server.mjs') + '"')
$start.EnvironmentVariables['KYNXA_MODEL_API_PORT'] = [string]$runtimePort
$gateway = [Diagnostics.Process]::Start($start)
$output = $gateway.StandardOutput.ReadToEndAsync(); $errorOutput = $gateway.StandardError.ReadToEndAsync()
try {
    $health = $null
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while ([DateTime]::UtcNow -lt $deadline -and -not $health) {
        if ($gateway.HasExited) { throw ('Bundled gateway exited early: ' + $errorOutput.Result) }
        try { $health = Invoke-RestMethod -Uri "http://127.0.0.1:$runtimePort/health" -TimeoutSec 1 }
        catch { Start-Sleep -Milliseconds 100 }
    }
    Assert-Runtime ($null -ne $health -and $health.status -eq 'ok') 'Copied bundled Node starts the real model gateway with an empty system PATH.'
    Assert-Runtime ($health.officialToolsProtocol -eq 2 -and $health.agentProtocol -eq 5 -and $health.hostTerminalProtocol -eq 3) 'The packaged gateway exposes current official-tool, agent and host-terminal protocols.'
    Assert-Runtime ($health.modelDataHome.StartsWith($runtimeTestRoot, [StringComparison]::OrdinalIgnoreCase) -and
        $health.extensionRoot -eq (Join-Path $runtimeTestRoot 'UserTools')) 'The packaged gateway uses only isolated temporary data and user-tools paths.'
    $config = Invoke-RestMethod -Uri "http://127.0.0.1:$runtimePort/api/agent/config" -TimeoutSec 3
    Assert-Runtime ($config.mcpServers.Count -eq 11 -and @($config.mcpServers | Where-Object enabled).Count -eq 0) 'Fresh official MCP presets remain disabled; no real MCP processes or model calls are triggered.'
}
finally {
    if (-not $gateway.HasExited) { $gateway.Kill(); $gateway.WaitForExit() }
    [IO.File]::WriteAllText((Join-Path $runtimeTestRoot 'gateway.stdout.txt'), $output.Result)
    [IO.File]::WriteAllText((Join-Path $runtimeTestRoot 'gateway.stderr.txt'), $errorOutput.Result)
    $gateway.Dispose()
}
Write-Output "PASS: $checks portable Node/npm/npx, isolated gateway and self-contained ToolHost checks."
Write-Output "Isolated copied package: $runtimeTestRoot"
