[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$LayoutPath,
    [Parameter(Mandatory = $true)][string]$ExcludedLayoutPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Keep executable payloads out of WinUI resource classification.
# 可执行载荷不参与 WinUI 本地化资源分类。
$layoutFile = [IO.Path]::GetFullPath($LayoutPath)
$excludedFile = [IO.Path]::GetFullPath($ExcludedLayoutPath)
$layoutLines = [IO.File]::ReadAllLines($layoutFile, [Text.Encoding]::UTF8)
$uiResourcePaths = [Collections.Generic.List[string]]::new()
$runtimePayloadPaths = [Collections.Generic.List[string]]::new()
foreach ($line in $layoutLines) {
    $relativePath = $line.Replace('/', '\')
    if ($relativePath.StartsWith('runtime\', [StringComparison]::OrdinalIgnoreCase) -or
        $relativePath.StartsWith('ToolHost\', [StringComparison]::OrdinalIgnoreCase) -or
        $relativePath.StartsWith('model-gateway\node_modules\', [StringComparison]::OrdinalIgnoreCase)) {
        $runtimePayloadPaths.Add($line)
    }
    else { $uiResourcePaths.Add($line) }
}
if ($runtimePayloadPaths.Count -eq 0) { return }

$excludedResourcePaths = [Collections.Generic.List[string]]::new()
$knownExcludedPaths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
if ([IO.File]::Exists($excludedFile)) {
    foreach ($line in [IO.File]::ReadAllLines($excludedFile, [Text.Encoding]::UTF8)) {
        $excludedResourcePaths.Add($line)
        [void]$knownExcludedPaths.Add($line.Replace('/', '\'))
    }
}
foreach ($line in $runtimePayloadPaths) {
    if ($knownExcludedPaths.Add($line.Replace('/', '\'))) { $excludedResourcePaths.Add($line) }
}

# The exclusion list keeps these files in the main payload when resources are split.
# 排除列表确保资源拆包后，这些文件仍留在主载荷中。
# Keep the original layout, Content items and bundle files unchanged.
# 保留原布局、Content 项和实际捆绑文件。
$utf8 = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllLines($excludedFile, $excludedResourcePaths, $utf8)
[IO.File]::WriteAllLines($layoutFile, $uiResourcePaths, $utf8)
Write-Output "WinUI PRI: retained $($uiResourcePaths.Count) UI assets; $($runtimePayloadPaths.Count) runtime files remain plain package payloads."
