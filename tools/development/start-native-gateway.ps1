[CmdletBinding()]
param(
    [string]$BundleRoot,
    [string]$DataHome,
    [string]$ModelHome,
    [string]$ExtensionHome,
    [ValidateRange(1, 65535)][int]$Port = 5218,
    [switch]$Foreground
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
if (-not $BundleRoot) {
    [xml]$project = Get-Content -LiteralPath (Join-Path $repositoryRoot 'apps/desktop/KYNXA.Desktop.csproj') -Raw
    $framework = $project.SelectSingleNode('/Project/PropertyGroup/TargetFramework').InnerText
    $BundleRoot = Join-Path $repositoryRoot "apps/desktop/bin/x64/Debug/$framework/win-x64"
}
$applicationRoot = [IO.Path]::GetFullPath($BundleRoot)
$executable = Join-Path $applicationRoot 'runtime/agent/kynxa-gateway.exe'
foreach ($path in @($executable, (Join-Path $applicationRoot 'runtime/native/onnxruntime.dll'),
    (Join-Path $applicationRoot 'runtime/native/vec0.dll'),
    (Join-Path $applicationRoot 'runtime/resource/kynxa-resource-service.exe'),
    (Join-Path $applicationRoot 'ToolHost/KYNXA.ToolHost.exe'))) {
    if (-not [IO.File]::Exists($path)) { throw "Native bundle file missing: $path. Build the desktop project first." }
}

# Refuse a busy port instead of stopping a gateway or task that belongs to another session.
# 已占用端口直接报告，不停止其他会话的网关或正在运行的任务。
$probe = [Net.Sockets.TcpClient]::new()
$occupied = $false
try { $occupied = $probe.ConnectAsync('127.0.0.1', $Port).Wait(300) -and $probe.Connected }
catch [AggregateException] { }
finally { $probe.Dispose() }
if ($occupied) { throw "Port $Port is already in use; the existing process was preserved." }

$updates = @{
    KYNXA_APPLICATION_ROOT = $applicationRoot
    KYNXA_MODEL_API_PORT = [string]$Port
    PATH = (Join-Path $applicationRoot 'runtime/native') + ';' + $env:PATH
}
foreach ($option in @(@('KYNXA_DATA_HOME', $DataHome), @('KYNXA_MODEL_HOME', $ModelHome), @('KYNXA_EXTENSION_HOME', $ExtensionHome))) {
    if ($option[1]) { $updates[$option[0]] = [IO.Path]::GetFullPath($option[1]) }
}
$previous = @{}
try {
    foreach ($name in $updates.Keys) {
        $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        [Environment]::SetEnvironmentVariable($name, $updates[$name], 'Process')
    }
    if ($Foreground) {
        # Console mode is explicit; the regular development launch stays in the background.
        # 只有明确选择前台模式才占用控制台；日常开发启动默认在后台。
        & $executable
        if ($LASTEXITCODE -ne 0) { throw "Native gateway exited with code $LASTEXITCODE." }
    }
    else {
        $logDirectory = Join-Path $repositoryRoot 'artifacts/development/native-gateway'
        [IO.Directory]::CreateDirectory($logDirectory) | Out-Null
        $identity = Get-Date -Format 'yyyyMMdd-HHmmss-fff'
        $stdoutPath = Join-Path $logDirectory "$identity.stdout.log"
        $stderrPath = Join-Path $logDirectory "$identity.stderr.log"
        $process = Start-Process -FilePath $executable -WorkingDirectory $applicationRoot -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath
        [pscustomobject]@{ ProcessId = $process.Id; Url = "http://127.0.0.1:$Port"; StandardOutput = $stdoutPath; StandardError = $stderrPath }
    }
}
finally {
    # Child processes inherit the launch snapshot; do not leave changed storage or PATH in the caller.
    # 子进程继承启动时的环境快照；不把修改后的存储路径或 PATH 留给调用者。
    foreach ($name in $previous.Keys) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
}
