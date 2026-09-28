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
$server = (Resolve-Path -LiteralPath $ServerPath).Path
$model = (Resolve-Path -LiteralPath $ModelPath).Path
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
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$arguments = @('-m', ('"' + $model + '"'), '--alias', $ModelId, '--host', '127.0.0.1',
    '--port', "$Port", '-c', "$ContextSize", '-ngl', $GpuLayers, '--parallel', '1')

function Test-GpuBackend([string]$Candidate) {
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $server
    $info.Arguments = '--list-devices'
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.EnvironmentVariables['GGML_BACKEND_PATH'] = $Candidate
    $info.EnvironmentVariables['PATH'] = (Split-Path $Candidate) + ';' + $env:PATH
    $probe = New-Object System.Diagnostics.Process
    $probe.StartInfo = $info
    try {
        [void]$probe.Start()
        $output = $probe.StandardOutput.ReadToEndAsync()
        $errors = $probe.StandardError.ReadToEndAsync()
        if (!$probe.WaitForExit(15000)) { $probe.Kill(); return $false }
        return $probe.ExitCode -eq 0 -and $output.Result -match '(CUDA\d+|Vulkan\d+|ROCm\d+):'
    } finally { $probe.Dispose() }
}

if ($BackendPath) {
    $BackendPath = (Resolve-Path -LiteralPath $BackendPath).Path
    if (!(Test-GpuBackend $BackendPath)) { throw 'The configured GPU backend could not initialize. Check the driver or choose another backend.' }
} elseif ($GpuLayers -ne '0') {
    # A GPU layer count alone does not load backends installed in subdirectories.
    foreach ($subdirectory in @('cuda_v12', 'cuda_v13')) {
        $candidate = Join-Path (Split-Path $server) "$subdirectory\ggml-cuda.dll"
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
    $process = Start-Process -FilePath $server -ArgumentList $arguments -WorkingDirectory (Split-Path $server) `
        -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $logDirectory "$stamp.out.log") `
        -RedirectStandardError (Join-Path $logDirectory "$stamp.err.log")
} finally { $env:PATH = $previousPath; $env:GGML_BACKEND_PATH = $previousBackend }
[pscustomobject]@{ ProcessId = $process.Id; BaseUrl = "http://127.0.0.1:$Port/v1"; ModelId = $ModelId;
    Backend = $(if ($BackendPath) { $BackendPath } else { 'Runtime default; check log for selected device' }); LogDirectory = $logDirectory }
