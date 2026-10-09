[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$BundleRoot)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$runtimeSource = [IO.Path]::GetFullPath($BundleRoot)
$runtimeTestRoot = Join-Path ([IO.Path]::GetTempPath()) ('kynxa-portable-runtime-' + [Guid]::NewGuid().ToString('N'))
$runtimePackage = Join-Path $runtimeTestRoot 'package with spaces'
[IO.Directory]::CreateDirectory($runtimePackage) | Out-Null
Write-Output "Isolated copied package: $runtimeTestRoot"
$checks = 0
function Assert-Runtime([bool]$Condition, [string]$Description) {
    if (-not $Condition) { throw $Description }
    $script:checks++
}
function New-IsolatedStart([string]$Executable, [string]$Arguments) {
    $start = [Diagnostics.ProcessStartInfo]::new($Executable, $Arguments)
    $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    $start.WorkingDirectory = $runtimePackage
    $start.EnvironmentVariables.Clear()
    $start.EnvironmentVariables['SystemRoot'] = $env:SystemRoot
    $start.EnvironmentVariables['WINDIR'] = $env:SystemRoot
    $start.EnvironmentVariables['COMSPEC'] = Join-Path $env:SystemRoot 'System32/cmd.exe'
    foreach ($name in @('USERPROFILE', 'LOCALAPPDATA', 'APPDATA', 'TEMP', 'TMP')) { $start.EnvironmentVariables[$name] = $runtimeTestRoot }
    $start.EnvironmentVariables['PATH'] = ''
    $start.EnvironmentVariables['DOTNET_ROOT'] = Join-Path $runtimeTestRoot 'absent-dotnet'
    $start.EnvironmentVariables['DOTNET_ROOT(x86)'] = Join-Path $runtimeTestRoot 'absent-dotnet'
    $start.EnvironmentVariables['DOTNET_MULTILEVEL_LOOKUP'] = '0'
    $start.EnvironmentVariables['KYNXA_DATA_HOME'] = Join-Path $runtimeTestRoot 'Data'
    $start.EnvironmentVariables['KYNXA_EXTENSION_HOME'] = Join-Path $runtimeTestRoot 'UserTools'
    $start.EnvironmentVariables['NPM_CONFIG_USERCONFIG'] = Join-Path $runtimeTestRoot 'empty.npmrc'
    $start.EnvironmentVariables['NPM_CONFIG_CACHE'] = Join-Path $runtimeTestRoot 'npm-cache'
    $start.EnvironmentVariables['NPM_CONFIG_UPDATE_NOTIFIER'] = 'false'
    return $start
}
function Invoke-IsolatedRuntime([string]$Executable, [string]$Arguments, [string]$InputText = '') {
    $start = New-IsolatedStart $Executable $Arguments
    $start.RedirectStandardInput = $true
    $process = [Diagnostics.Process]::Start($start)
    try {
        $output = $process.StandardOutput.ReadToEndAsync(); $errorOutput = $process.StandardError.ReadToEndAsync()
        if ($InputText) { $process.StandardInput.WriteLine($InputText) }
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(15000)) { $process.Kill(); throw 'The copied runtime process timed out.' }
        if ($process.ExitCode -ne 0) { throw ("Copied runtime failed ($([IO.Path]::GetFileName($Executable)), exit $($process.ExitCode)): " + $errorOutput.Result + $output.Result) }
        return $output.Result.Trim()
    }
    finally { $process.Dispose() }
}

foreach ($directory in @('runtime', 'model-gateway', 'ToolHost')) {
    Assert-Runtime (Test-Path -LiteralPath (Join-Path $runtimeSource $directory) -PathType Container) "The app bundle is missing $directory."
    Copy-Item -LiteralPath (Join-Path $runtimeSource $directory) -Destination $runtimePackage -Recurse
}
$runtimeNode = Join-Path $runtimePackage 'runtime/node.exe'
$manifest = Get-Content -LiteralPath (Join-Path $runtimePackage 'runtime/node-runtime.json') -Raw | ConvertFrom-Json
$nodeInfo = Invoke-IsolatedRuntime $runtimeNode '-p "JSON.stringify({version:process.versions.node,arch:process.arch,execPath:process.execPath})"' | ConvertFrom-Json
Assert-Runtime ([IO.Path]::GetFullPath($nodeInfo.execPath) -eq $runtimeNode) 'The process executed the copied bundled node.exe, not an installed Node.'
$runtimeId = if ($nodeInfo.arch -eq 'ia32') { 'win-x86' } else { 'win-' + $nodeInfo.arch }
$runtimePin = $manifest.platforms.PSObject.Properties[$runtimeId].Value
Assert-Runtime ($nodeInfo.version -eq $runtimePin.version) 'Copied node.exe reports its platform-specific pinned official version.'
Assert-Runtime ((Get-FileHash -LiteralPath $runtimeNode -Algorithm SHA256).Hash.ToLowerInvariant() -eq $runtimePin.nodeSha256) 'The copied runtime matches the official executable checksum.'
Assert-Runtime ((Get-Item -LiteralPath (Join-Path $runtimePackage 'runtime/LICENSE')).Length -gt 100000) 'The complete Node and dependency licenses are present.'
foreach ($cli in @('npm', 'npx')) {
    $version = Invoke-IsolatedRuntime (Join-Path $env:SystemRoot 'System32/cmd.exe') ('/d /s /c ""' + (Join-Path $runtimePackage "runtime/$cli.cmd") + '" --version"')
    Assert-Runtime ($version -match '^\d+\.\d+\.\d+$') "Bundled $cli starts without installed Node/npm or a system PATH."
}
$runtimeHost = Join-Path $runtimePackage 'ToolHost/KYNXA.ToolHost.exe'
$hostCapabilities = Invoke-IsolatedRuntime $runtimeHost '' '{"operation":"desktop_capabilities"}' | ConvertFrom-Json
Assert-Runtime ($hostCapabilities.protocolVersion -eq 1 -and $hostCapabilities.boundary -eq 'host-desktop') 'Copied self-contained native ToolHost starts with unavailable DOTNET_ROOT and PATH.'
$desktopOperations = @('apps', 'windows', 'screenshot', 'read', 'launch', 'window', 'activate', 'move', 'click', 'scroll', 'drag', 'type', 'key')
Assert-Runtime ($hostCapabilities.operations.Count -eq $desktopOperations.Count) 'Copied ToolHost advertises the current desktop action contract.'
foreach ($operation in $desktopOperations) {
    Assert-Runtime ($hostCapabilities.operations -contains $operation) "Copied ToolHost includes $operation."
}
Assert-Runtime ($hostCapabilities.features.backgroundLaunch -eq 'best-effort-no-activate' -and
    $hostCapabilities.features.screenshotCrop -eq $true -and $hostCapabilities.features.boundedRead -eq $true) 'The copied helper retains background launch, original-pixel crop and bounded reads.'
foreach ($mode in @('resize', 'maximize', 'minimize', 'restore')) {
    Assert-Runtime ($hostCapabilities.features.windowModes -contains $mode) "The copied helper supports window $mode."
}
$hostTerminalCapabilities = Invoke-IsolatedRuntime $runtimeHost '' '{"operation":"host_terminal_capabilities"}' | ConvertFrom-Json
Assert-Runtime ($hostTerminalCapabilities.protocolVersion -eq 2 -and $hostTerminalCapabilities.boundary -eq 'host-terminal' -and
    $hostTerminalCapabilities.visibleTerminal -eq $true) 'The copied helper exposes current host terminal and real-console capability without external .NET.'

$officialProbePath = Join-Path $runtimeTestRoot 'official-package-check.mjs'
$sourceOfficialManifest = Join-Path $repositoryRoot 'apps/model-gateway/official-tools/manifest.json'
$copiedOfficialManifest = Join-Path $runtimePackage 'model-gateway/official-tools/manifest.json'
# Compare the complete source inventory before trusting packaged skill hashes or tool names.
# 先对照完整源码清单，再信任随包技能哈希及工具身份，避免过期包仅靠数量通过。
Assert-Runtime ((Get-FileHash -LiteralPath $copiedOfficialManifest -Algorithm SHA256).Hash -eq
    (Get-FileHash -LiteralPath $sourceOfficialManifest -Algorithm SHA256).Hash) 'The copied official manifest exactly matches the complete current source inventory and package identity.'
$officialProbe = @'
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { WebFetchTool } from './package with spaces/model-gateway/tools/web-fetch.mjs';
import { builtinDescriptors } from './package with spaces/model-gateway/official-tools/Tools/catalog.mjs';
const root = new URL('./package with spaces/model-gateway/official-tools/', import.meta.url);
const manifest = JSON.parse(await readFile(new URL('manifest.json', root), 'utf8'));
const toolNames = builtinDescriptors.map(tool => tool.name);
assert.equal(new Set(toolNames).size, toolNames.length, 'Packaged tool identities must be unique.');
assert.deepEqual([...toolNames].sort(), [...manifest.coreTools].sort(), 'Packaged descriptors must match every official tool identity.');
assert.equal(new Set(manifest.skills.map(skill => skill.path)).size, manifest.skills.length, 'Packaged skill identities must be unique.');
let verifiedSkillFiles = 0;
for (const skill of manifest.skills) {
  assert.ok(skill.files.length > 0, 'Each packaged skill must declare resource hashes.');
  const directory = skill.path.slice(0, skill.path.lastIndexOf('/'));
  for (const file of skill.files) {
    const bytes = await readFile(new URL('Skills/' + directory + '/' + file.path, root));
    if (createHash('sha256').update(bytes).digest('hex') !== file.sha256) throw new Error('Packaged skill hash mismatch: ' + skill.name);
    verifiedSkillFiles++;
  }
}
const reader = new WebFetchTool({ fetchPage: async () => ({ url: 'https://fixture.example.invalid/', status: 200,
  headers: { contentType: 'text/html; charset=utf-8' }, redirects: [],
  bytes: Buffer.from('<title>Package fixture</title><p>\u4e2d\u6587 English</p><a href="/source">Evidence</a><script>HiddenFixture</script>') }) });
try {
  const { value } = await reader.run({ url: 'https://fixture.example.invalid/', reason: 'Isolated package verification.' });
  const license = await readFile(new URL('./package with spaces/model-gateway/node_modules/html-to-text/LICENSE', import.meta.url), 'utf8');
  console.log(JSON.stringify({ tools: manifest.coreTools.length, skills: manifest.skills.length,
    descriptorTools: toolNames.length, verifiedSkillFiles,
    text: value.content, title: value.title, licensePresent: license.includes('MIT') }));
} finally { reader.close(); }
'@
[IO.File]::WriteAllText($officialProbePath, $officialProbe, [Text.UTF8Encoding]::new($false))
$officialPackage = Invoke-IsolatedRuntime $runtimeNode ('"' + $officialProbePath + '"') | ConvertFrom-Json
Assert-Runtime ($officialPackage.tools -eq 44 -and $officialPackage.descriptorTools -eq 44 -and
    $officialPackage.skills -eq 7 -and $officialPackage.verifiedSkillFiles -eq 13) 'The copied official package retains all 44 current tool identities and seven skills with all 13 resource hashes verified.'
Assert-Runtime ($officialPackage.title -eq 'Package fixture' -and $officialPackage.text.Contains(([char]0x4E2D).ToString() + [char]0x6587 + ' English')) 'Bundled public-page conversion reads Chinese and English without Python or external Node.'
Assert-Runtime ($officialPackage.text.Contains('https://fixture.example.invalid/source') -and -not $officialPackage.text.Contains('HiddenFixture')) 'Copied page conversion retains source URLs without executing or displaying scripts.'
Assert-Runtime $officialPackage.licensePresent 'The new HTML parser license is included in the package.'

$retrievalProbePath = Join-Path $runtimeTestRoot 'retrieval-package-check.mjs'
$sourceGatewayPackage = Get-Content -LiteralPath (Join-Path $repositoryRoot 'apps/model-gateway/package.json') -Raw | ConvertFrom-Json
[IO.File]::WriteAllText((Join-Path $runtimeTestRoot 'gateway-dependency-pins.json'),
    ($sourceGatewayPackage.dependencies | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
$retrievalProbe = @'
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { DatabaseSync } from 'node:sqlite';
const gatewayRoot = new URL('./package with spaces/model-gateway/', import.meta.url);
// Resolve native addons exclusively from the copied package; all retrieval data stays in memory.
// 原生扩展仅从复制后的随包依赖解析；所有检索数据留在内存，不访问正式索引。
const require = createRequire(new URL('server.mjs', gatewayRoot));
const sqliteVec = require('sqlite-vec');
const { Index } = require('usearch');
const dependencies = JSON.parse(await readFile(new URL('./gateway-dependency-pins.json', import.meta.url), 'utf8'));
const sqliteVecPackage = JSON.parse(await readFile(new URL('node_modules/sqlite-vec/package.json', gatewayRoot), 'utf8'));
const annPackage = JSON.parse(await readFile(new URL('node_modules/usearch/package.json', gatewayRoot), 'utf8'));
assert.equal(sqliteVecPackage.version, dependencies['sqlite-vec']);
assert.equal(annPackage.version, dependencies.usearch);
const database = new DatabaseSync(':memory:', { allowExtension: true });
try {
  sqliteVec.load(database);
  database.enableLoadExtension(false);
  const vectorVersion = database.prepare('SELECT vec_version() AS version').get().version;
  assert.equal(vectorVersion, 'v' + sqliteVecPackage.version);
  database.exec("CREATE VIRTUAL TABLE fixture_fts USING fts5(content, tokenize='unicode61')");
  const insertText = database.prepare('INSERT INTO fixture_fts(rowid,content) VALUES (?,?)');
  insertText.run(101, '\u4e2d\u6587 English package retrieval evidence');
  insertText.run(202, 'Different unrelated document');
  const lexicalMatches = database.prepare('SELECT rowid,content FROM fixture_fts WHERE fixture_fts MATCH ? ORDER BY bm25(fixture_fts)').all('retrieval AND evidence');
  assert.equal(lexicalMatches.length, 1);
  assert.equal(lexicalMatches[0].rowid, 101);
  assert.ok(lexicalMatches[0].content.includes('\u4e2d\u6587 English'));
  database.exec('CREATE VIRTUAL TABLE fixture_vectors USING vec0(embedding float[3] distance_metric=cosine)');
  const keys = new BigUint64Array([101n, 202n, 303n]);
  const vectors = [new Float32Array([1, 0, 0]), new Float32Array([0, 1, 0]), new Float32Array([-1, 0, 0])];
  const vectorBytes = vector => Buffer.from(vector.buffer, vector.byteOffset, vector.byteLength);
  const insertVector = database.prepare('INSERT INTO fixture_vectors(rowid,embedding) VALUES (?,?)');
  vectors.forEach((vector, index) => insertVector.run(keys[index], vectorBytes(vector)));
  const exactMatches = database.prepare('SELECT rowid,distance FROM fixture_vectors WHERE embedding MATCH ? AND k=3 ORDER BY distance').all(vectorBytes(vectors[0]));
  assert.deepEqual(exactMatches.map(match => match.rowid), [101, 202, 303]);
  assert.ok(Math.abs(exactMatches[0].distance) < 0.00001 && exactMatches[2].distance > 1.99);
  const annIndex = new Index({ dimensions: 3, metric: 'cos', quantization: 'f32',
    connectivity: 16, expansion_add: 32, expansion_search: 32 });
  annIndex.add(keys, vectors, 1);
  assert.equal(annIndex.size(), 3);
  const annMatches = annIndex.search(vectors[0], 3, 1);
  assert.deepEqual(Array.from(annMatches.keys, Number), [101, 202, 303]);
  assert.ok(Math.abs(annMatches.distances[0]) < 0.00001 && annMatches.distances[2] > 1.99);
  assert.equal(annIndex.remove(keys).reduce((total, count) => total + count, 0), 3);
  assert.equal(annIndex.size(), 0);
  console.log(JSON.stringify({ storage: ':memory:', fts5Matches: lexicalMatches.length, vectorVersion,
    exactMatches: exactMatches.length, annMatches: annMatches.keys.length, annVersion: annPackage.version }));
} finally { database.close(); }
'@
[IO.File]::WriteAllText($retrievalProbePath, $retrievalProbe, [Text.UTF8Encoding]::new($false))
$retrievalPackage = Invoke-IsolatedRuntime $runtimeNode ('"' + $retrievalProbePath + '"') | ConvertFrom-Json
Assert-Runtime ($retrievalPackage.storage -eq ':memory:' -and $retrievalPackage.fts5Matches -eq 1) 'Copied bundled Node performs FTS5 retrieval using only an in-memory fixture.'
Assert-Runtime ($retrievalPackage.vectorVersion -eq 'v0.1.9' -and $retrievalPackage.exactMatches -eq 3) 'Copied sqlite-vec loads its packaged native DLL and returns the expected exact cosine ranking.'
Assert-Runtime ($retrievalPackage.annVersion -eq '2.26.4' -and $retrievalPackage.annMatches -eq 3) 'Copied USearch loads its packaged native addon and returns the expected in-memory ANN ranking.'

foreach ($profileModule in @('embedding-profile.mjs', 'reranker-profile.mjs')) {
    $relativeProfile = 'model-gateway/models/retrieval/' + $profileModule
    Assert-Runtime ((Get-FileHash -LiteralPath (Join-Path $runtimePackage $relativeProfile) -Algorithm SHA256).Hash -eq
        (Get-FileHash -LiteralPath (Join-Path $repositoryRoot ('apps/' + $relativeProfile)) -Algorithm SHA256).Hash) "Copied $profileModule exactly matches the source asset pins."
}
$assetProbePath = Join-Path $runtimeTestRoot 'model-assets-check.mjs'
$assetProbe = @'
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { lstat } from 'node:fs/promises';
import { BUILTIN_EMBEDDING_PROFILE } from './package with spaces/model-gateway/models/retrieval/embedding-profile.mjs';
import { BUILTIN_RERANKER_PROFILE } from './package with spaces/model-gateway/models/retrieval/reranker-profile.mjs';
const profiles = [['embedding', BUILTIN_EMBEDDING_PROFILE], ['rerank', BUILTIN_RERANKER_PROFILE]];
const verifiedProfiles = [];
// Verify shipped bytes and licenses without loading a model or invoking inference.
// 校验随包权重、分词器及许可的真实字节；不加载模型，也不请求推理。
for (const [directory, profile] of profiles) {
  const root = new URL(`./package with spaces/runtime/${directory}/${profile.id}/`, import.meta.url);
  let totalBytes = 0;
  for (const asset of profile.files) {
    const assetUrl = new URL(asset.path, root);
    const file = await lstat(assetUrl);
    assert.ok(file.isFile() && !file.isSymbolicLink(), 'Missing or linked packaged asset: ' + asset.path);
    assert.equal(file.size, asset.bytes, 'Packaged asset size mismatch: ' + asset.path);
    const digest = createHash('sha256');
    for await (const part of createReadStream(assetUrl)) digest.update(part);
    assert.equal(digest.digest('hex'), asset.sha256, 'Packaged asset hash mismatch: ' + asset.path);
    totalBytes += file.size;
  }
  verifiedProfiles.push({ id: profile.id, modelId: profile.modelId, files: profile.files.length, totalBytes });
}
console.log(JSON.stringify({ profiles: verifiedProfiles }));
'@
[IO.File]::WriteAllText($assetProbePath, $assetProbe, [Text.UTF8Encoding]::new($false))
$assetPackage = Invoke-IsolatedRuntime $runtimeNode ('"' + $assetProbePath + '"') | ConvertFrom-Json
Assert-Runtime ($assetPackage.profiles.Count -eq 2 -and
    $assetPackage.profiles[0].id -eq 'builtin-multilingual' -and $assetPackage.profiles[0].files -eq 11 -and
    $assetPackage.profiles[1].id -eq 'builtin-multilingual-reranker' -and $assetPackage.profiles[1].files -eq 8) 'All 11 E5 and eight reranker assets, including weights, tokenizers and licenses, match their source-pinned sizes and SHA256 hashes.'

$resourceRoot = Join-Path $runtimePackage 'runtime/resource'
$resourceExecutable = Join-Path $resourceRoot 'kynxa-resource-service.exe'
Assert-Runtime (Test-Path -LiteralPath $resourceExecutable -PathType Leaf) 'The copied Rust resource runtime executable is present.'
$resourceNotices = Join-Path $resourceRoot 'licenses/THIRD-PARTY-NOTICES.txt'
Assert-Runtime ((Get-Item -LiteralPath $resourceNotices).Length -gt 0 -and
    @(Get-ChildItem -LiteralPath (Join-Path $resourceRoot 'licenses') -Recurse -File | Where-Object { $_.Name -match '^(?:LICEN[CS]E(?:[-_.].*)?|COPYING)$' }).Count -gt 0) 'The copied Rust resource runtime includes dependency notices and original licenses.'
$resourceInput = '{"id":1,"method":"health"}' + [Environment]::NewLine + '{"id":2,"method":"close"}'
$resourceOutput = Invoke-IsolatedRuntime $resourceExecutable '' $resourceInput
$resourceFrames = @($resourceOutput -split '\r?\n' | ForEach-Object { $_ | ConvertFrom-Json })
Assert-Runtime ($resourceFrames.Count -eq 2 -and $resourceFrames[0].id -eq 1 -and
    $resourceFrames[0].result.mode -eq 'rust' -and $resourceFrames[0].result.cpu.logicalCores -gt 0 -and
    $resourceFrames[0].result.memory.totalBytes -gt 0 -and $resourceFrames[0].result.activeLeases -eq 0 -and
    $resourceFrames[0].result.quarantinedLeases -eq 0 -and $resourceFrames[0].result.queuedRequests -eq 0) 'The copied Rust resource service starts and answers its current health protocol with no leases or external toolchain.'
Assert-Runtime ($resourceFrames[1].id -eq 2 -and $resourceFrames[1].result.status -eq 'closed') 'The copied Rust resource service acknowledges close and exits its owned process.'

$runtimeListener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$runtimeListener.Start(); $runtimePort = $runtimeListener.LocalEndpoint.Port; $runtimeListener.Stop()
$start = New-IsolatedStart $runtimeNode ('"' + (Join-Path $runtimePackage 'model-gateway/server.mjs') + '"')
$start.EnvironmentVariables['KYNXA_MODEL_API_PORT'] = [string]$runtimePort
$gateway = [Diagnostics.Process]::Start($start)
$output = $gateway.StandardOutput.ReadToEndAsync(); $errorOutput = $gateway.StandardError.ReadToEndAsync()
try {
    $health = $null
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while ([DateTime]::UtcNow -lt $deadline -and -not $health) {
        if ($gateway.HasExited) { throw ('Bundled gateway exited early: ' + $errorOutput.Result) }
        try { $health = Invoke-RestMethod -Uri "http://127.0.0.1:$runtimePort/health" -TimeoutSec 1 }
        catch { Start-Sleep -Milliseconds 100 }
    }
    Assert-Runtime ($null -ne $health -and $health.status -eq 'ok') 'Copied bundled Node starts the real model gateway with an empty system PATH.'
    Assert-Runtime ($health.officialToolsProtocol -eq 2 -and $health.agentProtocol -eq 5 -and $health.hostTerminalProtocol -eq 3) 'The packaged gateway exposes current official-tool, agent and host-terminal protocols.'
    Assert-Runtime ($health.modelDataHome.StartsWith($runtimeTestRoot, [StringComparison]::OrdinalIgnoreCase) -and
        $health.extensionRoot -eq (Join-Path $runtimeTestRoot 'UserTools')) 'The packaged gateway uses only isolated temporary data and user-tools paths.'
    $config = Invoke-RestMethod -Uri "http://127.0.0.1:$runtimePort/api/agent/config" -TimeoutSec 3
    Assert-Runtime ($config.mcpServers.Count -eq 11 -and @($config.mcpServers | Where-Object enabled).Count -eq 11) 'Fresh official MCP presets are enabled; reading configuration does not trigger MCP or model calls.'
    $toolCatalog = Invoke-RestMethod -Uri "http://127.0.0.1:$runtimePort/api/agent/tools" -TimeoutSec 3
    Assert-Runtime (@($toolCatalog.connections | Where-Object { $_.state -ne 'disconnected' }).Count -eq 0 -and
        @($toolCatalog.tools | Where-Object { $_.name.StartsWith('mcp.') }).Count -eq 0) 'Reading an enabled tool catalog keeps external services disconnected.'
}
finally {
    if (-not $gateway.HasExited) { $gateway.Kill(); $gateway.WaitForExit() }
    [IO.File]::WriteAllText((Join-Path $runtimeTestRoot 'gateway.stdout.txt'), $output.Result)
    [IO.File]::WriteAllText((Join-Path $runtimeTestRoot 'gateway.stderr.txt'), $errorOutput.Result)
    $gateway.Dispose()
}
$runtimeEvidence = @{
    bundleRoot = $runtimeSource; verifiedUtc = [DateTime]::UtcNow.ToString('o'); checks = $checks
    node = $nodeInfo
    official = @{ tools = $officialPackage.tools; descriptorTools = $officialPackage.descriptorTools
        skills = $officialPackage.skills; verifiedSkillFiles = $officialPackage.verifiedSkillFiles }
    retrieval = $retrievalPackage; modelAssets = $assetPackage
    resourceService = @{ health = $resourceFrames[0].result; close = $resourceFrames[1].result }
    gateway = @{ status = $health.status; officialToolsProtocol = $health.officialToolsProtocol
        agentProtocol = $health.agentProtocol; hostTerminalProtocol = $health.hostTerminalProtocol }
}
$runtimeEvidencePath = Join-Path $runtimeTestRoot 'portable-results.json'
[IO.File]::WriteAllText($runtimeEvidencePath, ($runtimeEvidence | ConvertTo-Json -Depth 12), [Text.UTF8Encoding]::new($false))
Write-Output "PASS: $checks portable Node/npm/npx, official tools, native retrieval, model asset, Rust resource service, isolated gateway and self-contained ToolHost checks."
Write-Output "Verification evidence: $runtimeEvidencePath"
