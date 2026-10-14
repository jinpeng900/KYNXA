[CmdletBinding()]
param([string]$ResultPath)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('kynxa-layout-check-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($fixture) | Out-Null
$payloadRoot = Join-Path $fixture 'payload'
$fileList = Join-Path $fixture 'files.txt'
$utf8 = [Text.UTF8Encoding]::new($false)
$checks = [Collections.Generic.List[string]]::new()
function Invoke-LayoutCheck([string]$LayoutDirectory) {
    $ErrorActionPreference = 'Continue'
    $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $repositoryRoot 'apps/desktop/Build/check-deployment-paths.ps1') -LayoutDirectory $LayoutDirectory -PayloadRoot $payloadRoot -FileList $fileList 2>&1
    return @{exitCode=$LASTEXITCODE;output=($output | Out-String)}
}
try {
    # The checker reads names only; long deployment targets are never created or deleted.
    # 检查器仅读取路径名，不创建或删除超长的部署目标。
    $relativePath = 'runtime\mcp\' + ('x' * 128) + '.js'
    [IO.File]::WriteAllLines($fileList, @((Join-Path $payloadRoot $relativePath)), $utf8)
    $short = Invoke-LayoutCheck (Join-Path $fixture 'layout')
    if ($short.exitCode -ne 0) { throw $short.output }
    $checks.Add('An ordinary owned payload path is accepted without creating a deployment layout.')
    $layoutPrefix = [IO.Path]::GetPathRoot($fixture) + 'qa-layout-'
    $boundaryLayout = $layoutPrefix + ('l' * (259 - 1 - $relativePath.Length - $layoutPrefix.Length))
    $boundary = Invoke-LayoutCheck $boundaryLayout
    if ($boundary.exitCode -ne 0 -or $boundary.output -notmatch '259 characters') { throw $boundary.output }
    $checks.Add('The 259-character deployment boundary remains accepted.')
    $tooLong = Invoke-LayoutCheck ($boundaryLayout + 'l')
    if ($tooLong.exitCode -eq 0 -or $tooLong.output -notmatch '260 characters' -or $tooLong.output -notmatch 'LayoutDir') { throw $tooLong.output }
    $checks.Add('A 260-character target fails with the exact path and a LayoutDir remedy before deployment.')
    [IO.File]::WriteAllLines($fileList, @((Join-Path $fixture 'outside.txt')), $utf8)
    $escape = Invoke-LayoutCheck (Join-Path $fixture 'layout')
    if ($escape.exitCode -eq 0 -or $escape.output -notmatch 'escaped its declared payload root') { throw $escape.output }
    $checks.Add('A source outside the declared payload root is rejected.')
    $report = @{passed=$checks.Count;failed=0;checks=@($checks)}
    if ($ResultPath) { [IO.File]::WriteAllText([IO.Path]::GetFullPath($ResultPath), ($report | ConvertTo-Json -Depth 5), $utf8) }
    $report | ConvertTo-Json -Depth 5
}
finally {
    $temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    $resolvedFixture = [IO.Path]::GetFullPath($fixture)
    if (-not $resolvedFixture.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path -Leaf $resolvedFixture) -notmatch '^kynxa-layout-check-[0-9a-f]{32}$') { throw 'Unexpected layout fixture was preserved.' }
    if ([IO.Directory]::Exists($resolvedFixture)) { Remove-Item -LiteralPath $resolvedFixture -Recurse -Force }
}
