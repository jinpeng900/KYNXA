[CmdletBinding()]
param([string]$ApplicationRoot = (Split-Path -Parent $PSScriptRoot))
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$ApplicationRoot = [IO.Path]::GetFullPath($ApplicationRoot)
$executable = Join-Path $ApplicationRoot 'KYNXA.Desktop.exe'
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'KYNXA.Desktop.exe was not found.' }

# Installed startup reads its own path configuration; remove inherited development-only overrides for this child.
# 安装软件自行读取本目录配置；仅为此子进程清除继承的开发环境覆盖，不修改用户或系统环境。
$startInfo = New-Object Diagnostics.ProcessStartInfo
$startInfo.FileName = $executable
$startInfo.WorkingDirectory = $ApplicationRoot
$startInfo.UseShellExecute = $false
foreach ($name in @('KYNXA_APPLICATION_ROOT', 'KYNXA_DATA_HOME', 'KYNXA_MODEL_HOME', 'KYNXA_EXTENSION_HOME',
    'KYNXA_STORAGE_POINTER', 'KYNXA_EXTENSION_POINTER', 'KYNXA_STORAGE_MIGRATION_LOCK', 'KYNXA_LEGACY_DESKTOP_HOME',
    'KYNXA_MODEL_API_URL', 'KYNXA_MODEL_API_PORT', 'WEBVIEW2_USER_DATA_FOLDER')) {
    $startInfo.EnvironmentVariables.Remove($name)
}
[Diagnostics.Process]::Start($startInfo) | Out-Null
