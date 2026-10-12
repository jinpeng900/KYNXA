[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$BundleRoot)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
$bundle = [IO.Path]::GetFullPath($BundleRoot).TrimEnd('\', '/')
if (-not [IO.Directory]::Exists($bundle)) { return }
$allowed = @((Join-Path $repositoryRoot 'apps/desktop/bin'), (Join-Path $repositoryRoot 'artifacts/development'))
if (-not ($allowed | Where-Object { $bundle.StartsWith($_.TrimEnd('\', '/') + '\', [StringComparison]::OrdinalIgnoreCase) })) {
    throw 'Legacy payload cleanup is limited to the verified desktop build output or isolated development fixture.'
}
if (-not [IO.File]::Exists((Join-Path $bundle 'KYNXA.Desktop.dll'))) { throw 'Desktop assembly marker is absent; no files were removed.' }

function Assert-UnlinkedOutputPath([string]$Path) {
    if (-not $Path.StartsWith($bundle + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Obsolete payload escaped the desktop output.' }
    $ancestor = $Path
    while ($ancestor) {
        if ([IO.File]::Exists($ancestor) -or [IO.Directory]::Exists($ancestor)) {
            if ([IO.File]::GetAttributes($ancestor) -band [IO.FileAttributes]::ReparsePoint) { throw 'Linked output paths are preserved, not followed for cleanup.' }
        }
        $ancestor = [IO.Path]::GetDirectoryName($ancestor)
    }
}
$obsoleteDirectories = @('runtime/node', 'runtime/node_modules', 'model-gateway')
$obsoleteFiles = @('runtime/node.exe', 'runtime/node-runtime.json', 'runtime/npm', 'runtime/npm.cmd', 'runtime/npm.ps1',
    'runtime/npx', 'runtime/npx.cmd', 'runtime/npx.ps1', 'runtime/agent/kynxa-agent-core-native.node')
$payloadRoots = @($bundle)
$appx = [IO.Path]::GetFullPath((Join-Path $bundle 'AppX'))
if ([IO.Directory]::Exists($appx)) { $payloadRoots += $appx }
$legacyRevisions = [Collections.Generic.List[string]]::new()
foreach ($payloadRoot in $payloadRoots) {
    $revisionRoot = [IO.Path]::GetFullPath((Join-Path $payloadRoot 'runtime/agent/revisions'))
    if (-not [IO.Directory]::Exists($revisionRoot)) { continue }
    Assert-UnlinkedOutputPath $revisionRoot
    # A 64-hex revision and the exact old NAPI filename jointly identify this obsolete payload.
    # 仅由 64 位十六进制修订名和旧 NAPI 精确文件名共同识别旧载荷，不清空整个 revisions 目录。
    foreach ($entry in Get-ChildItem -LiteralPath $revisionRoot -Force) {
        if (-not $entry.PSIsContainer -or $entry.Name -notmatch '^[0-9a-fA-F]{64}$') { continue }
        Assert-UnlinkedOutputPath $entry.FullName
        if ([IO.File]::Exists((Join-Path $entry.FullName 'kynxa-agent-core.node'))) { $legacyRevisions.Add($entry.FullName) }
    }
}
$directoryTargets = @($payloadRoots | ForEach-Object {
    $payloadRoot = $_
    $obsoleteDirectories | ForEach-Object { [IO.Path]::GetFullPath((Join-Path $payloadRoot $_)) }
}) + @($legacyRevisions)
$targets = @($directoryTargets) + @($payloadRoots | ForEach-Object {
    $payloadRoot = $_
    $obsoleteFiles | ForEach-Object { [IO.Path]::GetFullPath((Join-Path $payloadRoot $_)) }
})
if (-not ($targets | Where-Object { [IO.File]::Exists($_) -or [IO.Directory]::Exists($_) })) { return }
if (-not [IO.File]::Exists((Join-Path $bundle 'runtime/agent/kynxa-gateway.exe'))) { throw 'Native replacement marker is absent; old payload was preserved.' }

# The parent output authenticates only its immediate AppX staging folder, never unrelated package layouts.
# 父输出的原生标记仅授权其紧邻 AppX 暂存目录；不会扫描其他包或整个 bin 目录。
foreach ($payloadRoot in $payloadRoots) {
    $hasObsoletePayload = $targets | Where-Object {
        $_.StartsWith($payloadRoot + '\', [StringComparison]::OrdinalIgnoreCase) -and
            ([IO.File]::Exists($_) -or [IO.Directory]::Exists($_))
    }
    if ($hasObsoletePayload -and -not [IO.File]::Exists((Join-Path $payloadRoot 'KYNXA.Desktop.dll'))) {
        throw 'Desktop assembly marker is absent in the package staging folder; no files were removed.'
    }
}

# Validate every absolute target and ancestor before touching any known obsolete file.
# 删除任何已知旧载荷之前，先核验全部绝对目标与父目录；不处理用户 Data 或未知输出位置。
foreach ($path in $targets) {
    Assert-UnlinkedOutputPath $path
}
$files = [Collections.Generic.List[string]]::new()
$directories = [Collections.Generic.List[string]]::new()
foreach ($root in $directoryTargets) {
    if (-not [IO.Directory]::Exists($root)) { continue }
    $isRevision = $legacyRevisions.Contains($root)
    $isGatewaySource = [IO.Path]::GetFileName($root) -eq 'model-gateway'
    $pending = [Collections.Generic.Stack[string]]::new(); $pending.Push($root)
    while ($pending.Count) {
        $directory = $pending.Pop(); $directories.Add($directory)
        foreach ($entry in Get-ChildItem -LiteralPath $directory -Force) {
            if (-not $entry.FullName.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase) -or
                ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Unexpected linked legacy payload was preserved.' }
            if ($isRevision) {
                $revisionRelative = $entry.FullName.Substring($root.Length + 1)
                $isKnownEntry = if ($entry.PSIsContainer) {
                    $revisionRelative -match '^licenses(?:\\[A-Za-z0-9_.+-]+)?$'
                } else {
                    $revisionRelative -match '^kynxa-agent-core\.(?:exe|node)$' -or
                        $revisionRelative -match '^licenses\\THIRD-PARTY-NOTICES\.txt$' -or
                        $revisionRelative -match '^licenses\\[A-Za-z0-9_.+-]+\\(?:COPYING|LICENSE(?:[-.][A-Za-z0-9.-]+)?|THIRD-PARTY-NOTICES\.txt)$'
                }
                if (-not $isKnownEntry) { throw 'Unknown content or possible formal data found in an obsolete NAPI revision; no files were removed.' }
            }
            if ($isGatewaySource -and -not $entry.FullName.Contains('\node_modules\') -and
                ($entry.Name -in @('Chats', 'Projects', 'Memory', 'Agent', 'catalog.json', 'storage.json', 'providers.json', 'model-providers.json') -or
                 $entry.Extension -in @('.sqlite', '.sqlite3', '.db', '.jsonl'))) {
                throw 'Possible formal data found in an obsolete source directory; no files were removed.'
            }
            if ($entry.PSIsContainer) { $pending.Push($entry.FullName) } else { $files.Add($entry.FullName) }
        }
    }
}
foreach ($payloadRoot in $payloadRoots) {
    foreach ($relative in $obsoleteFiles) {
        $path = [IO.Path]::GetFullPath((Join-Path $payloadRoot $relative))
        if ([IO.File]::Exists($path)) { $files.Add($path) }
    }
}

# Remove enumerated files, then empty directories; never perform a broad recursive output deletion.
# 先删除已枚举的旧载荷文件，再移除空目录；不对整个输出目录做递归清空。
foreach ($path in $files) { Remove-Item -LiteralPath $path -Force }
foreach ($path in ($directories | Sort-Object { $_.Length } -Descending)) { [IO.Directory]::Delete($path, $false) }
Write-Output "Native bundle: removed $($files.Count) obsolete backend files and $($directories.Count) empty payload directories."
