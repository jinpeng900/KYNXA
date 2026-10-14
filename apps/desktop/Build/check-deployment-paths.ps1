[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$LayoutDirectory,
    [Parameter(Mandatory = $true)][string]$PayloadRoot,
    [Parameter(Mandatory = $true)][string]$FileList
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$layoutRoot = [IO.Path]::GetFullPath($LayoutDirectory).TrimEnd('\', '/')
$sourceRoot = [IO.Path]::GetFullPath($PayloadRoot).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
$maximumPath = ''
foreach ($source in [IO.File]::ReadAllLines([IO.Path]::GetFullPath($FileList), [Text.Encoding]::UTF8)) {
    $sourcePath = [IO.Path]::GetFullPath($source)
    if (-not $sourcePath.StartsWith($sourceRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Native deployment file escaped its declared payload root.' }
    $targetPath = [IO.Path]::Combine($layoutRoot, $sourcePath.Substring($sourceRoot.Length))
    if ($targetPath.Length -gt $maximumPath.Length) { $maximumPath = $targetPath }
}
# VS deployment still uses legacy copy APIs on some hosts; diagnose before DEP1000 loses context.
# 部分 VS 部署主机仍使用传统复制 API；在 DEP1000 丢失上下文前给出明确诊断。
if ($maximumPath.Length -ge 260) {
    throw "Native deployment target is $($maximumPath.Length) characters (limit 259): $maximumPath. Use a shorter checkout path or set MSBuild LayoutDir to a short absolute directory, then rebuild. No directory was moved or deleted."
}
Write-Output "Native deployment path preflight: longest target $($maximumPath.Length) characters."
