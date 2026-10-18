[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$PlanPath)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$plan = Get-Content -LiteralPath $PlanPath -Raw -Encoding UTF8 | ConvertFrom-Json
try {
    $package = [Windows.ApplicationModel.Package, Windows.ApplicationModel, ContentType = WindowsRuntime]::Current
    if ($package.Id.Name -ne $plan.packageIdentity) { throw 'The desktop launcher received a different package identity.' }
    # Set the application root inside the package context; broker activation does not inherit the caller's overrides.
    # 在包身份上下文内设置应用根目录；不能依赖激活代理继承外部调用者的环境覆盖。
    $root = [IO.Path]::GetFullPath($plan.applicationRoot)
    foreach ($name in @('KYNXA_DATA_HOME','KYNXA_MODEL_HOME','KYNXA_EXTENSION_HOME','KYNXA_STORAGE_POINTER','KYNXA_EXTENSION_POINTER','KYNXA_LEGACY_DESKTOP_HOME')) {
        [Environment]::SetEnvironmentVariable($name, [NullString]::Value, 'Process')
    }
    [Environment]::SetEnvironmentVariable('KYNXA_APPLICATION_ROOT', $root, 'Process')
    Import-Module (Join-Path $PSScriptRoot '../../../tools/development/PortableStorage.psm1') -Force
    $data = Get-KynxaPortablePath -ApplicationRoot $root -PointerName storage.json -PropertyName dataRoot -DefaultPath Data
    $extensions = Get-KynxaPortablePath -ApplicationRoot $root -PointerName extensions.json -PropertyName extensionRoot -DefaultPath Extensions
    $port = 5218
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, $port)
    try { $listener.Start() }
    catch [Net.Sockets.SocketException] {
        $sameStorage = $false
        try {
            $health = Invoke-RestMethod -Uri "http://127.0.0.1:$port/health" -TimeoutSec 1
            $sameStorage = $health.service -eq 'kynxa-model-gateway' -and $health.modelDataHome -eq (Join-Path $data 'Models') -and $health.extensionRoot -eq $extensions
        }
        catch { }
        if (-not $sameStorage) {
            # Separate checkouts use separate ports instead of attaching to another workspace's gateway.
            # 独立项目副本改用空闲端口，不连接到其他工作区的网关。
            $free = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
            try { $free.Start(); $port = $free.LocalEndpoint.Port } finally { $free.Stop() }
        }
    }
    finally { $listener.Stop() }
    [Environment]::SetEnvironmentVariable('KYNXA_MODEL_API_PORT', [string]$port, 'Process')
    [Environment]::SetEnvironmentVariable('KYNXA_MODEL_API_URL', "http://127.0.0.1:$port", 'Process')
    [Environment]::SetEnvironmentVariable('WEBVIEW2_USER_DATA_FOLDER', (Join-Path $data 'Desktop/WebView2'), 'Process')
    $process = Start-Process -FilePath (Join-Path $root 'KYNXA.Desktop.exe') -WorkingDirectory $root -PassThru
    $receipt = @{ started = $true; processId = $process.Id; applicationRoot = $root; port = $port }
}
catch { $receipt = @{ started = $false; error = $_.Exception.Message } }
[IO.File]::WriteAllText($plan.receipt, ($receipt | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
