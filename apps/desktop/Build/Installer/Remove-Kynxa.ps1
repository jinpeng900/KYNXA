[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$ApplicationRoot)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$ApplicationRoot = [IO.Path]::GetFullPath($ApplicationRoot).TrimEnd('\')
if ($ApplicationRoot -eq [IO.Path]::GetPathRoot($ApplicationRoot).TrimEnd('\') -or
    -not (Test-Path -LiteralPath (Join-Path $ApplicationRoot 'kynxa-installation.json') -PathType Leaf)) {
    throw 'The selected directory is not an installed KYNXA application.'
}

# Stop only executables supplied by this installation. Data, tools and referenced models are retained.
# 只停止本安装目录自带的进程；正式数据、用户工具和外部引用模型均保留。
$ownedProcesses = @(Get-CimInstance Win32_Process | Where-Object {
    $_.ExecutablePath -and $_.ExecutablePath.StartsWith($ApplicationRoot + '\', [StringComparison]::OrdinalIgnoreCase) -and
    $_.Name -notlike 'unins*.exe' -and
    ($_.Name -in @('KYNXA.Desktop.exe', 'kynxa-gateway.exe', 'kynxa-resource-service.exe', 'KYNXA.ToolHost.exe', 'llama-server.exe', 'chrome.exe', 'chrome-headless-shell.exe'))
})
foreach ($ownedProcess in $ownedProcesses) {
    Stop-Process -Id $ownedProcess.ProcessId -Force -ErrorAction SilentlyContinue
}
Write-Output 'KYNXA processes stopped. Personal data, tools and downloaded models were retained.'
