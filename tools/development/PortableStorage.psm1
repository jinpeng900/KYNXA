Set-StrictMode -Version Latest

# Resolve only this application's pointer; moving the folder rebases relative selections.
# 只读取本软件的路径配置；移动软件文件夹后，相对目录随当前位置重新解析。
function Get-KynxaPortablePath {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$ApplicationRoot,
        [Parameter(Mandatory = $true)][string]$PointerName,
        [Parameter(Mandatory = $true)][string]$PropertyName,
        [Parameter(Mandatory = $true)][string]$DefaultPath
    )
    $applicationDirectory = [IO.Path]::GetFullPath($ApplicationRoot)
    $pointerPath = Join-Path $applicationDirectory $PointerName
    $selectedPath = $DefaultPath
    if ([IO.File]::Exists($pointerPath)) {
        $pointer = Get-Content -LiteralPath $pointerPath -Raw -Encoding UTF8 | ConvertFrom-Json
        if ($pointer.version -ne 1 -or [string]::IsNullOrWhiteSpace([string]$pointer.$PropertyName)) {
            throw "Invalid storage configuration: $pointerPath"
        }
        $selectedPath = [string]$pointer.$PropertyName
    }
    if (-not [IO.Path]::IsPathRooted($selectedPath)) {
        $selectedPath = Join-Path $applicationDirectory $selectedPath
    }
    return [IO.Path]::GetFullPath($selectedPath)
}

Export-ModuleMember -Function Get-KynxaPortablePath
