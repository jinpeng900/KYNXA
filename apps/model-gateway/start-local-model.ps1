param(
    [string]$ServerPath,
    [string]$ModelPath,
    [string]$ModelId = 'qwen3-8b',
    [ValidateRange(1024, 65535)][int]$Port = 8080,
    [ValidateRange(512, 131072)][int]$ContextSize = 8192,
    [string]$GpuLayers = 'auto',
    [string]$BackendPath
)
$ErrorActionPreference = 'Stop'
$dataRoot = $env:KYNXA_DATA_HOME
$storagePointer = Join-Path ([Environment]::GetFolderPath('UserProfile')) '.kynxa\storage.json'
if (!$dataRoot -and (Test-Path -LiteralPath $storagePointer)) {
    $dataRoot = (Get-Content -LiteralPath $storagePointer -Raw -Encoding UTF8 | ConvertFrom-Json).dataRoot
    if (!$dataRoot) { throw 'KYNXA storage directory is empty.' }
}
$configDirectory = if ($env:KYNXA_MODEL_HOME) { $env:KYNXA_MODEL_HOME } elseif ($dataRoot) {
    Join-Path $dataRoot 'Models'
} else {
    Join-Path ([Environment]::GetFolderPath('UserProfile')) '.kynxa\models'
}
$configFile = Join-Path $configDirectory 'local-server.json'
if ((!$ServerPath -or !$ModelPath) -and (Test-Path -LiteralPath $configFile)) {
    $config = Get-Content -LiteralPath $configFile -Raw -Encoding UTF8 | ConvertFrom-Json
    if (!$ServerPath) { $ServerPath = $config.serverPath }
    if (!$ModelPath) { $ModelPath = $config.modelPath }
    if (!$PSBoundParameters.ContainsKey('ModelId') -and $config.modelId) { $ModelId = $config.modelId }
    if (!$BackendPath -and $config.backendPath) { $BackendPath = $config.backendPath }
}
if (!$ServerPath -or !$ModelPath) { throw 'Specify -ServerPath and -ModelPath, or create local-server.json in KYNXA_MODEL_HOME.' }
$serverExecutable = (Resolve-Path -LiteralPath $ServerPath).Path
$modelFilePath = (Resolve-Path -LiteralPath $ModelPath).Path
if ($ModelId -notmatch '^[a-zA-Z0-9._:/-]+$' -or $GpuLayers -notmatch '^(auto|all|[0-9]+)$') {
    throw 'Invalid model alias or GPU layer setting.'
}
if (Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue) {
    throw "Port $Port is already in use. Choose another port or use the existing service."
}
$logDirectory = if ($dataRoot) { Join-Path $dataRoot 'Logs\models' } else {
    Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'KYNXA\model-logs'
}
[void](New-Item -ItemType Directory -Path $logDirectory -Force)
$logTimestamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$arguments = @('-m', ('"' + $modelFilePath + '"'), '--alias', $ModelId, '--host', '127.0.0.1',
    '--port', "$Port", '-c', "$ContextSize", '-ngl', $GpuLayers, '--parallel', '1')

function Test-GpuBackend([string]$Candidate) {
    $processStartInfo = New-Object System.Diagnostics.ProcessStartInfo
    $processStartInfo.FileName = $serverExecutable
    $processStartInfo.Arguments = '--list-devices'
    $processStartInfo.UseShellExecute = $false
    $processStartInfo.CreateNoWindow = $true
    $processStartInfo.RedirectStandardOutput = $true
    $processStartInfo.RedirectStandardError = $true
    $processStartInfo.EnvironmentVariables['GGML_BACKEND_PATH'] = $Candidate
    $processStartInfo.EnvironmentVariables['PATH'] = (Split-Path $Candidate) + ';' + $env:PATH
    $backendProbeProcess = New-Object System.Diagnostics.Process
    $backendProbeProcess.StartInfo = $processStartInfo
    try {
        [void]$backendProbeProcess.Start()
        $standardOutputTask = $backendProbeProcess.StandardOutput.ReadToEndAsync()
        $standardErrorTask = $backendProbeProcess.StandardError.ReadToEndAsync()
        if (!$backendProbeProcess.WaitForExit(15000)) { $backendProbeProcess.Kill(); return $false }
        return $backendProbeProcess.ExitCode -eq 0 -and $standardOutputTask.Result -match '(CUDA\d+|Vulkan\d+|ROCm\d+):'
    } finally { $backendProbeProcess.Dispose() }
}

if ($BackendPath) {
    $BackendPath = (Resolve-Path -LiteralPath $BackendPath).Path
    if (!(Test-GpuBackend $BackendPath)) { throw 'The configured GPU backend could not initialize. Check the driver or choose another backend.' }
} elseif ($GpuLayers -ne '0') {
    # A GPU layer count alone does not load backends installed in subdirectories.
    # 仅设置 GPU 层数不会自动加载安装在子目录中的后端。
    foreach ($subdirectory in @('cuda_v12', 'cuda_v13')) {
        $candidate = Join-Path (Split-Path $serverExecutable) "$subdirectory\ggml-cuda.dll"
        if ((Test-Path -LiteralPath $candidate) -and (Test-GpuBackend $candidate)) { $BackendPath = $candidate; break }
    }
}
$previousPath = $env:PATH
$previousBackend = $env:GGML_BACKEND_PATH
try {
    if ($BackendPath) {
        $env:GGML_BACKEND_PATH = $BackendPath
        $env:PATH = (Split-Path $BackendPath) + ';' + $previousPath
    }
    $modelProcess = Start-Process -FilePath $serverExecutable -ArgumentList $arguments -WorkingDirectory (Split-Path $serverExecutable) `
        -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $logDirectory "$logTimestamp.out.log") `
        -RedirectStandardError (Join-Path $logDirectory "$logTimestamp.err.log")
} finally { $env:PATH = $previousPath; $env:GGML_BACKEND_PATH = $previousBackend }
[pscustomobject]@{ ProcessId = $modelProcess.Id; BaseUrl = "http://127.0.0.1:$Port/v1"; ModelId = $ModelId;
    Backend = $(if ($BackendPath) { $BackendPath } else { 'Runtime default; check log for selected device' }); LogDirectory = $logDirectory }
