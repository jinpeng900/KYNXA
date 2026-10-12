[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$BundleRoot, [string]$ResultPath)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
$bundle = [IO.Path]::GetFullPath($BundleRoot)
$manifest = Get-Content -LiteralPath (Join-Path $bundle 'runtime/mcp/manifest.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$node = Join-Path $bundle $manifest.node.path
$browser = Join-Path $bundle $manifest.browser.path
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('kynxa-private-mcp-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($fixture) | Out-Null
$utf8 = [Text.UTF8Encoding]::new($false)
$checks = [Collections.Generic.List[string]]::new()
$results = [Collections.Generic.List[object]]::new()

function Assert-PrivateMcp([bool]$Condition, [string]$Label) {
    if (-not $Condition) { throw $Label }
    $checks.Add($Label)
}
function Get-PrivateMcpHash([string]$Path) {
    $stream = [IO.File]::OpenRead($Path); $hash = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($hash.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $hash.Dispose(); $stream.Dispose() }
}
function Convert-ProcessArgument([string]$Value) {
    if ($Value.Contains([string][char]0)) { throw 'Process argument contains a null character.' }
    return '"' + [Regex]::Replace([Regex]::Replace($Value, '(\\*)"', '$1$1\"'), '(\\+)$', '$1$1') + '"'
}
function New-PrivateMcpStart([string[]]$Arguments, [string]$ServiceId) {
    $start = [Diagnostics.ProcessStartInfo]::new($node)
    $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true; $start.RedirectStandardInput = $true
    $start.StandardOutputEncoding = $utf8; $start.StandardErrorEncoding = $utf8
    $start.Arguments = ($Arguments | ForEach-Object { Convert-ProcessArgument $_ }) -join ' '
    $start.WorkingDirectory = Join-Path $bundle 'runtime/mcp/services'
    $start.EnvironmentVariables.Clear()
    $start.EnvironmentVariables['SystemRoot'] = $env:SystemRoot
    $start.EnvironmentVariables['WINDIR'] = $env:SystemRoot
    $start.EnvironmentVariables['PATH'] = Join-Path $env:SystemRoot 'System32'
    foreach ($name in @('TEMP', 'TMP', 'LOCALAPPDATA', 'APPDATA')) { $start.EnvironmentVariables[$name] = $fixture }
    $start.EnvironmentVariables['PLAYWRIGHT_BROWSERS_PATH'] = Join-Path $bundle 'runtime/mcp/browsers'
    $start.EnvironmentVariables['DO_NOT_TRACK'] = '1'
    if ($ServiceId -eq 'brave-search') { $start.EnvironmentVariables['BRAVE_API_KEY'] = 'fixture-not-a-real-key' }
    return $start
}
function Stop-PrivateMcp($Process) {
    if (-not $Process.HasExited) {
        # Only this live fixture process tree is terminated; existing user MCPs are untouched.
        # 仅终止本夹具持有的活动进程树；不按名称处理用户现有的 MCP。
        & (Join-Path $env:SystemRoot 'System32/taskkill.exe') /PID $Process.Id /T /F | Out-Null
        [void]$Process.WaitForExit(5000)
    }
}
function Send-PrivateMcp($Process, $Message) {
    $bytes = $utf8.GetBytes(($Message | ConvertTo-Json -Compress -Depth 12) + "`n")
    $Process.StandardInput.BaseStream.Write($bytes, 0, $bytes.Length)
    $Process.StandardInput.BaseStream.Flush()
}
function Read-PrivateMcp($Process, [int]$Id) {
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    for ($lineNumber = 0; $lineNumber -lt 512; $lineNumber++) {
        $lineTask = $Process.StandardOutput.ReadLineAsync()
        $remaining = [int][Math]::Max(1, ($deadline - [DateTime]::UtcNow).TotalMilliseconds)
        if (-not $lineTask.Wait($remaining)) { throw "MCP response $Id timed out." }
        if ($null -eq $lineTask.Result) { throw "MCP process exited before response $Id." }
        if ($lineTask.Result.Length -gt 512 * 1024) { throw 'MCP fixture response exceeded its bounded size.' }
        $response = $lineTask.Result | ConvertFrom-Json
        if ($response.PSObject.Properties['id'] -and $response.id -eq $Id) {
            if ($response.PSObject.Properties['error']) { throw "MCP response $Id returned an error: $($response.error | ConvertTo-Json -Compress)" }
            return $response.result
        }
    }
    throw 'MCP fixture produced too many unrelated protocol messages.'
}

Assert-PrivateMcp ($manifest.coreDependsOnNode -eq $false -and $manifest.downloadsAtRuntime -eq $false) 'Rust core remains independent and the private payload requires no runtime downloads.'
Assert-PrivateMcp ((Get-PrivateMcpHash $node) -eq '63c259c81e5d472b5f11c8d506070130cb04a1ecf84b80377a34ed6ec9048088') 'Private Node matches the pinned official executable.'
Assert-PrivateMcp ((Get-PrivateMcpHash $browser) -eq $manifest.browser.sha256) 'Bundled Chromium matches its fixed executable hash.'
Assert-PrivateMcp ([IO.File]::Exists((Join-Path $bundle $manifest.node.license))) 'Original Node license is included.'
$versionProcess = [Diagnostics.Process]::Start((New-PrivateMcpStart @('--version') 'node'))
try {
    $version = $versionProcess.StandardOutput.ReadToEndAsync(); $errorText = $versionProcess.StandardError.ReadToEndAsync()
    Assert-PrivateMcp ($versionProcess.WaitForExit(10000) -and $versionProcess.ExitCode -eq 0 -and $version.Result.Trim() -eq ('v' + $manifest.node.version)) 'Private Node runs with no system Node on PATH.'
}
finally { Stop-PrivateMcp $versionProcess; $versionProcess.Dispose() }

foreach ($service in $manifest.services) {
    $entry = Join-Path $bundle $service.entry
    Assert-PrivateMcp ((Get-PrivateMcpHash $entry) -eq $service.sha256) "Pinned service entry matches: $($service.id)"
    $arguments = @($entry)
    switch ($service.id) {
        'playwright' { $arguments += @('--headless', '--isolated', '--executable-path', $browser, '--output-dir', (Join-Path $fixture 'playwright-output')) }
        'chrome-devtools' { $arguments += @('--headless', '--isolated', '--no-usage-statistics', '--no-performance-crux', '--executablePath', $browser) }
        'brave-search' { $arguments += @('--transport', 'stdio') }
        'dbhub' { $arguments += @('--transport', 'stdio', '--demo') }
    }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $process = [Diagnostics.Process]::Start((New-PrivateMcpStart $arguments $service.id))
    try {
        $stderr = $process.StandardError.ReadToEndAsync()
        Send-PrivateMcp $process @{jsonrpc='2.0';id=1;method='initialize';params=@{protocolVersion='2025-11-25';capabilities=@{};clientInfo=@{name='kynxa-private-mcp-smoke';version='1.0.0'}}}
        $initialized = Read-PrivateMcp $process 1
        Assert-PrivateMcp ($initialized.protocolVersion -and $initialized.serverInfo.name) "Real offline MCP handshake succeeds: $($service.id)"
        Send-PrivateMcp $process @{jsonrpc='2.0';method='notifications/initialized';params=@{}}
        Send-PrivateMcp $process @{jsonrpc='2.0';id=2;method='tools/list';params=@{}}
        $listed = Read-PrivateMcp $process 2
        Assert-PrivateMcp (@($listed.tools).Count -gt 0) "Real installed tool definitions are available: $($service.id)"
        $handshakeMs = $timer.Elapsed.TotalMilliseconds
        if ($service.id -eq 'playwright') {
            Send-PrivateMcp $process @{jsonrpc='2.0';id=3;method='tools/call';params=@{name='browser_navigate';arguments=@{url='about:blank'}}}
            $navigated = Read-PrivateMcp $process 3
            $failed = $navigated.PSObject.Properties['isError'] -and $navigated.isError
            Assert-PrivateMcp (-not $failed) 'Private Playwright launches bundled headless Chromium without downloading or opening the user browser.'
        }
        $results.Add(@{id=$service.id;version=$service.version;tools=@($listed.tools).Count;handshakeAndListMs=[Math]::Round($handshakeMs,2)})
    }
    finally { Stop-PrivateMcp $process; $process.Dispose() }
}
$report = @{passed=$checks.Count;failed=0;checks=@($checks);services=@($results);nodeVersion=$manifest.node.version;payloadBytes=$manifest.totalBytes;fixture=$fixture;pageCacheState='not-cleared; not a machine-cold startup benchmark';scope='private runtime and real local handshake; no real API keys, user databases, remote tool calls or model requests'}
if ($ResultPath) {
    $path = [IO.Path]::GetFullPath($ResultPath)
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
    [IO.File]::WriteAllText($path, ($report | ConvertTo-Json -Depth 7), $utf8)
}
$report | ConvertTo-Json -Depth 7
