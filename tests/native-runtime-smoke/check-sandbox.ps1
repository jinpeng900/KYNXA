[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$ToolHostPath, [string]$ResultPath)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$toolHost = [IO.Path]::GetFullPath($ToolHostPath)
if (-not [IO.File]::Exists($toolHost)) { throw 'Build the native ToolHost before running this check.' }
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('kynxa-native-sandbox-' + [Guid]::NewGuid().ToString('N'))
$dataRoot = Join-Path $fixture 'Data with spaces'
$extensionsRoot = Join-Path $fixture 'Extensions'
$workspace = Join-Path $dataRoot ('Sandboxes/' + [Guid]::NewGuid().ToString())
$checks = [Collections.Generic.List[string]]::new()
$stages = [Collections.Generic.List[string]]::new()
$utf8 = [Text.UTF8Encoding]::new($false)

function Assert-Sandbox([bool]$Condition, [string]$Label) {
    if (-not $Condition) { throw $Label }
    $checks.Add($Label)
}
function Invoke-NativeSandbox($Request) {
    $start = [Diagnostics.ProcessStartInfo]::new($toolHost)
    $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true; $start.RedirectStandardInput = $true
    $start.StandardOutputEncoding = $utf8; $start.StandardErrorEncoding = $utf8
    $start.EnvironmentVariables['PATH'] = Join-Path $env:SystemRoot 'System32'
    $process = [Diagnostics.Process]::Start($start)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync(); $stderr = $process.StandardError.ReadToEndAsync()
        $bytes = $utf8.GetBytes(($Request | ConvertTo-Json -Compress -Depth 5) + "`n")
        $process.StandardInput.BaseStream.Write($bytes, 0, $bytes.Length)
        $process.StandardInput.BaseStream.Flush()
        # Keep stdin open until completion: EOF is the helper's explicit cancellation signal.
        # 执行完成前保留标准输入；EOF 是助手明确的取消信号。
        if (-not $process.WaitForExit(15000)) {
            & (Join-Path $env:SystemRoot 'System32/taskkill.exe') /PID $process.Id /T /F | Out-Null
            [void]$process.WaitForExit(5000)
            throw 'Owned native sandbox fixture timed out.'
        }
        $lines = @($stdout.Result -split '[\r\n]+' | Where-Object { $_.Trim().StartsWith('{') })
        if ($lines.Count -eq 0) { throw ('ToolHost returned no protocol receipt: ' + $stderr.Result) }
        return $lines[-1] | ConvertFrom-Json
    }
    finally { $process.Dispose() }
}
function New-SandboxRequest([string]$Path, [bool]$Trusted, [string]$ConfiguredHome) {
    return @{ operation = 'run'; workspaceRoot = $Path; command = 'cmd'; args = @('/d', '/c', 'type fixture.txt & echo staged>created.txt');
        timeoutMs = 8000; excludedRoots = @($dataRoot, $extensionsRoot); trustedManagedWorkspace = $Trusted; conversationWorkspaceHome = $ConfiguredHome }
}
function Assert-RejectedWorkspace($Request, [string]$Label) {
    $result = Invoke-NativeSandbox $Request
    Assert-Sandbox ($result.error.code -eq 'SANDBOX_INVALID_WORKSPACE') $Label
}

try {
    [IO.Directory]::CreateDirectory($workspace) | Out-Null
    [IO.Directory]::CreateDirectory($extensionsRoot) | Out-Null
    [IO.File]::WriteAllText((Join-Path $workspace 'fixture.txt'), 'native snapshot fixture', $utf8)
    [IO.File]::WriteAllText((Join-Path $workspace '.env'), 'FAKE_ONLY=not-a-secret', $utf8)
    $capabilities = Invoke-NativeSandbox @{ operation = 'capabilities' }
    Assert-Sandbox ($capabilities.available -and $capabilities.failClosed -and $capabilities.checksChildToken -and
        @($capabilities.commands).Count -eq 1 -and $capabilities.commands[0] -eq 'cmd' -and $null -eq $capabilities.skillExecution) 'Actual capabilities expose verified CMD only, without a Node/script execution claim.'

    $result = Invoke-NativeSandbox (New-SandboxRequest $workspace $true $dataRoot)
    if ($result.PSObject.Properties.Name -contains 'error') { throw ($result.error | ConvertTo-Json -Compress) }
    $stages.Add((Split-Path -Parent $result.stagingDirectory))
    Assert-Sandbox ($result.exitCode -eq 0 -and $result.tokenVerified -and $result.activeProcessesAfterExit -eq 0 -and
        $result.stdout.Contains('native snapshot fixture') -and $result.snapshot.files -eq 1 -and $result.snapshot.skipped -ge 1) 'Exact Data/Sandboxes/GUID workspace executes in a verified AppContainer; sensitive files are skipped and the process tree exits.'
    Assert-Sandbox (([IO.File]::ReadAllText((Join-Path $workspace 'fixture.txt'))) -eq 'native snapshot fixture' -and
        -not [IO.File]::Exists((Join-Path $workspace 'created.txt')) -and [IO.File]::Exists((Join-Path $result.stagingDirectory 'created.txt')) -and
        -not [IO.File]::Exists((Join-Path $result.stagingDirectory '.env'))) 'Snapshot writes remain staged; original files and protected input are preserved.'

    Assert-RejectedWorkspace (New-SandboxRequest $workspace $false $dataRoot) 'Managed Data workspace requires the broker-owned trust flag.'
    Assert-RejectedWorkspace (New-SandboxRequest $workspace $true $extensionsRoot) 'A different configured home cannot authorize the Data ancestor.'
    $invalid = Join-Path $dataRoot 'Sandboxes/not-a-guid'; [IO.Directory]::CreateDirectory($invalid) | Out-Null
    Assert-RejectedWorkspace (New-SandboxRequest $invalid $true $dataRoot) 'An invalid chat identifier cannot become a managed sandbox.'
    $formal = Join-Path $dataRoot ('Chats/' + [Guid]::NewGuid().ToString()); [IO.Directory]::CreateDirectory($formal) | Out-Null
    Assert-RejectedWorkspace (New-SandboxRequest $formal $true $dataRoot) 'Formal Chats data remains excluded even with the managed flag.'
    $unsupported = New-SandboxRequest $workspace $true $dataRoot; $unsupported.command = 'node'; $unsupported.args = @('-e', 'throw new Error("not executed")')
    $refusal = Invoke-NativeSandbox $unsupported
    Assert-Sandbox ($refusal.error.code -eq 'SANDBOX_COMMAND_UNSUPPORTED') 'Node commands are explicitly refused before staging or dispatch.'

    $report = @{ passed = $checks.Count; failed = 0; checks = @($checks); runtime = 'native ToolHost'; fixture = $fixture }
    if ($ResultPath) { [IO.File]::WriteAllText([IO.Path]::GetFullPath($ResultPath), ($report | ConvertTo-Json -Depth 5), $utf8) }
    $report | ConvertTo-Json -Depth 5
}
finally {
    # Delete only the checked temporary roots that this fixture created, after confirmed child cleanup.
    # 子进程清理确认后，仅删除本夹具创建且已核验的临时根目录。
    foreach ($stage in $stages) {
        $expected = [IO.Path]::GetFullPath((Join-Path ([IO.Path]::GetTempPath()) 'kynxa-tool-sandbox')).TrimEnd('\') + '\'
        $resolved = [IO.Path]::GetFullPath($stage)
        if (-not $resolved.StartsWith($expected, [StringComparison]::OrdinalIgnoreCase) -or
            -not [Guid]::TryParse((Split-Path -Leaf $resolved), [ref]([Guid]::Empty))) { throw 'Unexpected staging directory was preserved.' }
        if ([IO.Directory]::Exists($resolved)) { Remove-Item -LiteralPath $resolved -Recurse -Force }
    }
    $expected = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not [IO.Path]::GetFullPath($fixture).StartsWith($expected, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unexpected fixture directory was preserved.' }
    if ([IO.Directory]::Exists($fixture)) { Remove-Item -LiteralPath $fixture -Recurse -Force }
}
