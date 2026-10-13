[CmdletBinding()]
param([string]$BundleRoot, [string]$ToolHostDirectory)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
if (-not $BundleRoot) { $BundleRoot = Join-Path $repositoryRoot 'artifacts/runtime/native-desktop/win-x64' }
if (-not $ToolHostDirectory) { $ToolHostDirectory = Join-Path $repositoryRoot 'apps/tool-host/bin/Debug/net10.0-windows10.0.19041.0' }
$artifactRoot = Join-Path $repositoryRoot 'artifacts/development/history-mcp-repair-20261012'
$fixtureRoot = Join-Path $artifactRoot ('skill-' + [guid]::NewGuid().ToString('N'))
$utf8 = [Text.UTF8Encoding]::new($false)
$stages = [Collections.Generic.List[string]]::new()
$results = [Collections.Generic.List[object]]::new()

function Invoke-PrivateSkillHost([object]$Request) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = Join-Path $fixtureRoot 'ToolHost/KYNXA.ToolHost.exe'
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = $utf8
    $start.StandardErrorEncoding = $utf8
    # Prove that the helper does not discover Node through system PATH.
    # 验证宿主不通过系统 PATH 查找 Node；沙箱自身仍只收到隔离环境。
    $start.EnvironmentVariables['PATH'] = [Environment]::SystemDirectory
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    try {
        [void]$process.Start()
        $output = $process.StandardOutput.ReadToEndAsync()
        $errors = $process.StandardError.ReadToEndAsync()
        # Explicit UTF-8 bytes avoid Windows PowerShell's default stdin code page.
        # 显式 UTF-8 字节避免 Windows PowerShell 默认 stdin 代码页损坏中文路径。
        $encodedRequest = $utf8.GetBytes(($Request | ConvertTo-Json -Depth 12 -Compress) + "`n")
        $process.StandardInput.BaseStream.Write($encodedRequest, 0, $encodedRequest.Length)
        $process.StandardInput.BaseStream.Flush()
        if (-not $process.WaitForExit(20000)) { throw 'Isolated skill helper did not terminate.' }
        $lines = @($output.GetAwaiter().GetResult() -split "`r?`n" | Where-Object { $_.Trim() })
        $value = $lines[-1] | ConvertFrom-Json
        if ($value.PSObject.Properties['stagingDirectory']) { $stages.Add($value.stagingDirectory) }
        return $value
    }
    finally {
        if (-not $process.HasExited) { $process.Kill() }
        $process.Dispose()
    }
}

try {
    foreach ($directory in @('ToolHost','runtime/mcp/node','work','skill','private')) {
        [IO.Directory]::CreateDirectory((Join-Path $fixtureRoot $directory)) | Out-Null
    }
    Copy-Item -Path (Join-Path $ToolHostDirectory '*') -Destination (Join-Path $fixtureRoot 'ToolHost') -Recurse
    Copy-Item -LiteralPath (Join-Path $BundleRoot 'runtime/mcp/node/node.exe') -Destination (Join-Path $fixtureRoot 'runtime/mcp/node/node.exe')
    Copy-Item -LiteralPath (Join-Path $BundleRoot 'runtime/mcp/manifest.json') -Destination (Join-Path $fixtureRoot 'runtime/mcp/manifest.json')
    [IO.File]::WriteAllText((Join-Path $fixtureRoot 'work/source.txt'), 'unchanged source', $utf8)
    [IO.File]::WriteAllText((Join-Path $fixtureRoot 'private/forbidden.txt'), 'synthetic private fixture', $utf8)
    $script = @'
import fs from 'node:fs';
let privateDenied = false;
try { fs.readFileSync(process.argv[2], 'utf8'); } catch (error) { privateDenied = error.code === 'EACCES' || error.code === 'EPERM'; }
let packageDenied = false;
try { fs.writeFileSync(import.meta.filename, 'overwritten'); } catch (error) { packageDenied = error.code === 'EACCES' || error.code === 'EPERM'; }
fs.writeFileSync('result.txt', 'isolated output');
console.log(JSON.stringify({privateDenied, packageDenied, text:'私有 Node 技能'}));
if (!privateDenied || !packageDenied) process.exitCode = 1;
'@
    $scriptPath = Join-Path $fixtureRoot 'skill/run.mjs'
    [IO.File]::WriteAllText($scriptPath, $script, $utf8)
    $scriptHash = (Get-FileHash -LiteralPath $scriptPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $capabilities = Invoke-PrivateSkillHost @{operation='capabilities'}
    if (-not $capabilities.skillExecution.hashChecked -or -not ($capabilities.commands -contains 'node')) { throw 'Private skill runtime was not advertised.' }
    $request = @{operation='run';command='node';nodeExecutable=(Join-Path $fixtureRoot 'runtime/mcp/node/node.exe');
        workspaceRoot=(Join-Path $fixtureRoot 'work');excludedRoots=@((Join-Path $fixtureRoot 'private'));
        timeoutMs=10000;args=@((Join-Path $fixtureRoot 'private/forbidden.txt'));
        skill=@{root=(Join-Path $fixtureRoot 'skill');script='run.mjs';files=@(@{path='run.mjs';size=(Get-Item -LiteralPath $scriptPath).Length;sha256=$scriptHash})}}
    $executed = Invoke-PrivateSkillHost $request
    if (-not $executed.PSObject.Properties['exitCode']) { throw ('Skill helper refused the fixture: ' + ($executed | ConvertTo-Json -Depth 5 -Compress)) }
    if ($executed.exitCode -ne 0 -or -not $executed.tokenVerified -or $executed.activeProcessesAfterExit -ne 0) { throw ('Skill execution failed: ' + ($executed | ConvertTo-Json -Depth 5 -Compress)) }
    $body = $executed.stdout.Trim() | ConvertFrom-Json
    if (-not $body.privateDenied -or -not $body.packageDenied) { throw 'Skill isolation check failed.' }
    if ((Get-FileHash -LiteralPath $scriptPath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $scriptHash -or
        [IO.File]::ReadAllText((Join-Path $fixtureRoot 'work/source.txt')) -ne 'unchanged source' -or
        [IO.File]::Exists((Join-Path $fixtureRoot 'work/result.txt'))) { throw 'Original skill/work source changed.' }
    $results.Add(@{name='private-node-skill-appcontainer';passed=$true;elapsedMs=$executed.elapsedMs;privateDenied=$true;packageDenied=$true;orphanProcesses=0})
    $request.skill.files[0].sha256 = ('0' * 64)
    $changed = Invoke-PrivateSkillHost $request
    if ($changed.error.code -ne 'APP_SKILL_CHANGED') { throw 'Changed manifest was not rejected.' }
    $results.Add(@{name='changed-skill-manifest';passed=$true;code=$changed.error.code})
    $request.nodeExecutable = Join-Path $fixtureRoot 'other-node.exe'
    $foreign = Invoke-PrivateSkillHost $request
    if ($foreign.error.code -ne 'SANDBOX_COMMAND_UNSUPPORTED') { throw 'Arbitrary Node executable was not rejected.' }
    $results.Add(@{name='foreign-node-runtime';passed=$true;code=$foreign.error.code})
    $summary = @{passed=$true;cases=$results.ToArray();usesUserData=$false;usesModel=$false;systemNodeRequired=$false}
    [IO.File]::WriteAllText((Join-Path $artifactRoot 'skill-runtime-results.json'), ($summary | ConvertTo-Json -Depth 8), $utf8)
    $summary | ConvertTo-Json -Depth 8
}
finally {
    foreach ($stage in $stages) {
        $run = [IO.Path]::GetFullPath((Split-Path -Parent $stage))
        $ownedRuns = [IO.Path]::GetFullPath((Join-Path ([IO.Path]::GetTempPath()) 'kynxa-tool-sandbox')) + [IO.Path]::DirectorySeparatorChar
        if ($run.StartsWith($ownedRuns, [StringComparison]::OrdinalIgnoreCase)) { Remove-Item -LiteralPath $run -Recurse -Force }
    }
    $resolved = [IO.Path]::GetFullPath($fixtureRoot)
    if ($resolved.StartsWith(([IO.Path]::GetFullPath($artifactRoot) + [IO.Path]::DirectorySeparatorChar), [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
