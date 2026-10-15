[CmdletBinding()]
param([string]$ResultPath)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('kynxa-mcp-cache-' + [Guid]::NewGuid().ToString('N'))
$buildRoot = Join-Path $fixture 'apps/desktop/Build'
$bundleRoot = Join-Path $fixture 'bundle'
$payloadRoot = Join-Path $bundleRoot 'runtime/mcp'
$utf8 = [Text.UTF8Encoding]::new($false)
$checks = [Collections.Generic.List[string]]::new()

function Write-CacheJson([string]$Path, $Value) {
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path)) | Out-Null
    [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 20), $utf8)
}
function Get-CacheHash([string]$Path) { return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }
function Invoke-CachePreparation {
    $ErrorActionPreference = 'Continue'
    $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $buildRoot 'prepare-mcp-runtime.ps1') -BundleRoot $bundleRoot -Offline 2>&1
    return @{exitCode=$LASTEXITCODE;output=($output | Out-String)}
}

try {
    [IO.Directory]::CreateDirectory((Join-Path $buildRoot 'native-assets/mcp')) | Out-Null
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'apps/desktop/Build/prepare-mcp-runtime.ps1') -Destination $buildRoot
    Copy-Item -LiteralPath (Join-Path $repositoryRoot 'apps/desktop/Build/prepare-browser-sandbox.ps1') -Destination $buildRoot
    foreach ($moduleName in @('RuntimeDownloads.psm1', 'ResourceConfigCompatibility.psm1')) {
        Copy-Item -LiteralPath (Join-Path $repositoryRoot ('apps/desktop/Build/' + $moduleName)) -Destination $buildRoot
    }
    foreach ($name in @('package.json', 'package-lock.json')) {
        Copy-Item -LiteralPath (Join-Path $repositoryRoot ('apps/desktop/Build/native-assets/mcp/' + $name)) -Destination (Join-Path $buildRoot 'native-assets/mcp')
    }
    $source = Get-Content -LiteralPath (Join-Path $repositoryRoot 'apps/desktop/Build/native-assets/mcp-runtime.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    # Harmless placeholder bytes test cache completeness only; no executable, service or browser is run.
    # 无害占位字节仅验证缓存完整性；不运行任何可执行文件、服务或浏览器。
    $requiredPaths = @($source.node.path, $source.browser.path, 'runtime/mcp/node/LICENSE',
        'runtime/mcp/services/package.json', 'runtime/mcp/services/package-lock.json',
        'runtime/mcp/services/node_modules/playwright-core/browsers.json')
    foreach ($service in $source.services) {
        $requiredPaths += $service.entry
        $requiredPaths += 'runtime/mcp/services/node_modules/' + $service.package + '/package.json'
    }
    foreach ($relativePath in $requiredPaths) {
        $path = Join-Path $bundleRoot $relativePath
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
        [IO.File]::WriteAllText($path, 'cache fixture only; never executed: ' + $relativePath, $utf8)
    }
    foreach ($name in @('package.json', 'package-lock.json')) {
        Copy-Item -LiteralPath (Join-Path $buildRoot ('native-assets/mcp/' + $name)) -Destination (Join-Path $payloadRoot ('services/' + $name)) -Force
    }
    $source.node.executableSha256 = Get-CacheHash (Join-Path $bundleRoot $source.node.path)
    $source.browser.executableSha256 = Get-CacheHash (Join-Path $bundleRoot $source.browser.path)
    $sourceManifest = Join-Path $buildRoot 'native-assets/mcp-runtime.json'
    Write-CacheJson $sourceManifest $source
    $files = @($requiredPaths | ForEach-Object {
        $path = Join-Path $bundleRoot $_
        @{path=$_;bytes=([IO.FileInfo]::new($path)).Length;sha256=(Get-CacheHash $path)}
    })
    $services = @($source.services | ForEach-Object {
        @{id=$_.id;package=$_.package;version=$_.version;entry=$_.entry;sha256=(Get-CacheHash (Join-Path $bundleRoot $_.entry))}
    })
    $previous = @{schemaVersion=1;runtimeIdentifier='win-x64';sourceHash=(Get-CacheHash $sourceManifest);
        lockSha256=(Get-CacheHash (Join-Path $buildRoot 'native-assets/mcp/package-lock.json'));
        packageSha256=(Get-CacheHash (Join-Path $buildRoot 'native-assets/mcp/package.json'));
        node=@{version=$source.node.version;path=$source.node.path;sha256=$source.node.executableSha256;license='runtime/mcp/node/LICENSE'};
        browser=@{path=$source.browser.path;revision=$source.browser.revision;version=$source.browser.version;sha256=$source.browser.executableSha256};
        services=$services;fileCount=$files.Count;totalBytes=($files | ForEach-Object { $_.bytes } | Measure-Object -Sum).Sum;
        coreDependsOnNode=$false;downloadsAtRuntime=$false}
    $manifestPath = Join-Path $payloadRoot 'manifest.json'
    $integrityPath = Join-Path $payloadRoot 'payload-integrity.json'
    Write-CacheJson $manifestPath $previous
    Write-CacheJson $integrityPath @{schemaVersion=1;files=$files}
    $positive = Invoke-CachePreparation
    if ($positive.exitCode -ne 0 -or $positive.output -notmatch 'Private MCP payload reused' -or
        @([IO.File]::ReadAllLines((Join-Path $bundleRoot 'mcp-files.txt'))).Count -ne ($files.Count + 2)) {
        throw ('Complete cache fixture failed: ' + $positive.output)
    }
    $checks.Add('A complete declared cache produces the exact packaging file list without executing its placeholder files.')
    $sourceBytes = [IO.File]::ReadAllBytes($sourceManifest)
    $equivalentSource = [IO.File]::ReadAllText($sourceManifest, $utf8).Replace("`r`n", "`n")
    [IO.File]::WriteAllText($sourceManifest, $equivalentSource, [Text.UTF8Encoding]::new($true))
    $compatible = Invoke-CachePreparation
    [IO.File]::WriteAllBytes($sourceManifest, $sourceBytes)
    if ($compatible.exitCode -ne 0 -or $compatible.output -notmatch 'Private MCP payload reused') { throw $compatible.output }
    $checks.Add('Equivalent source-manifest LF/BOM variants preserve upstream cache identity compatibility.')
    $cases = @(
        @{name='empty inventory';change={param($saved,$inventory) $inventory.files=@()}},
        @{name='partial inventory';change={param($saved,$inventory) $inventory.files=@($inventory.files | Select-Object -Skip 1)}},
        @{name='duplicate paths';change={param($saved,$inventory) $inventory.files[1]=$inventory.files[0]}},
        @{name='wrong schema';change={param($saved,$inventory) $inventory.schemaVersion=99}},
        @{name='wrong count';change={param($saved,$inventory) $saved.fileCount++}},
        @{name='wrong byte total';change={param($saved,$inventory) $saved.totalBytes++}},
        @{name='unsafe relative path';change={param($saved,$inventory) $inventory.files[0].path='runtime/mcp/../outside.txt'}},
        @{name='wrong pinned Node identity';change={param($saved,$inventory) $saved.node.sha256='0' * 64}},
        @{name='wrong listed hash';change={param($saved,$inventory) $inventory.files[0].sha256='0' * 64}},
        @{name='missing required browser with consistent totals';change={param($saved,$inventory)
            $inventory.files=@($inventory.files | Where-Object { $_.path -notlike '*/chrome.exe' })
            $saved.fileCount=$inventory.files.Count; $saved.totalBytes=($inventory.files | Measure-Object -Property bytes -Sum).Sum}},
        @{name='missing required service with consistent totals';change={param($saved,$inventory)
            $inventory.files=@($inventory.files | Where-Object { $_.path -ne $saved.services[0].entry })
            $saved.fileCount=$inventory.files.Count; $saved.totalBytes=($inventory.files | Measure-Object -Property bytes -Sum).Sum}}
    )
    foreach ($case in $cases) {
        $saved = ($previous | ConvertTo-Json -Depth 20) | ConvertFrom-Json
        $inventory = (@{schemaVersion=1;files=$files} | ConvertTo-Json -Depth 20) | ConvertFrom-Json
        & $case.change $saved $inventory
        Write-CacheJson $manifestPath $saved
        Write-CacheJson $integrityPath $inventory
        Remove-Item -LiteralPath (Join-Path $bundleRoot 'mcp-files.txt')
        $result = Invoke-CachePreparation
        if ($result.exitCode -eq 0 -or $result.output -match 'Private MCP payload reused' -or
            [IO.File]::Exists((Join-Path $bundleRoot 'mcp-files.txt'))) {
            throw ('Invalid cache was accepted: ' + $case.name + ' / ' + $result.output)
        }
        $checks.Add('Offline preparation rejects ' + $case.name + ' without publishing a false complete file list.')
        # The next iteration removes a known owned list, even when the failed branch published none.
        # 下一轮仅清理已知自有列表；失败分支本轮没有发布列表也不扩大清理范围。
        [IO.File]::WriteAllText((Join-Path $bundleRoot 'mcp-files.txt'), '', $utf8)
    }
    $report = @{passed=$checks.Count;failed=0;checks=@($checks);scope='exact preparation script in an isolated offline repository; placeholder files are never executed'}
    if ($ResultPath) { Write-CacheJson ([IO.Path]::GetFullPath($ResultPath)) $report }
    $report | ConvertTo-Json -Depth 5
}
finally {
    $temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    $resolvedFixture = [IO.Path]::GetFullPath($fixture)
    if (-not $resolvedFixture.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path -Leaf $resolvedFixture) -notmatch '^kynxa-mcp-cache-[0-9a-f]{32}$') { throw 'Unexpected fixture root was preserved.' }
    if ([IO.Directory]::Exists($resolvedFixture)) { Remove-Item -LiteralPath $resolvedFixture -Recurse -Force }
}

# Expected child failures were asserted above; publish success only after checks and cleanup complete.
# 子进程的预期失败已在上方断言；仅在检查与清理全部完成后返回成功，避免 CI 沿用其失败退出码。
exit 0
