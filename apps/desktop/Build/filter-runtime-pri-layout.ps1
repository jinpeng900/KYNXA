[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$LayoutPath,
    [Parameter(Mandatory = $true)][string]$ExcludedLayoutPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# These are separate executable payloads. WinUI must not interpret their file
# names or satellite languages as localized resources of the desktop UI.
# 这些文件是独立的可执行负载；WinUI 不应将其文件名或卫星语言资源解释为桌面界面的本地化资源。
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

# The SDK reads the excluded list when constructing a main package file map,
# so these files remain ordinary payloads even when resource packs are split.
# Do not alter the unfiltered layout, Content items, or physical bundle files.
# SDK 构建主包文件映射时读取排除列表，拆分资源包后这些文件仍作为普通负载保留。
# 不修改未筛选的布局清单、Content 项或实际捆绑文件。
$utf8 = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllLines($excludedFile, $excludedResourcePaths, $utf8)
[IO.File]::WriteAllLines($layoutFile, $uiResourcePaths, $utf8)
Write-Output "WinUI PRI: retained $($uiResourcePaths.Count) UI assets; $($runtimePayloadPaths.Count) runtime files remain plain package payloads."
