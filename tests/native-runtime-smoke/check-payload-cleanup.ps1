$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$fixture = Join-Path $repositoryRoot ('artifacts/development/payload-cleanup-' + [Guid]::NewGuid().ToString('N'))
$script = Join-Path $repositoryRoot 'apps/desktop/Build/remove-obsolete-backend-payload.ps1'
$utf8 = [Text.UTF8Encoding]::new($false)
foreach ($relative in @('runtime/agent', 'runtime/node_modules/package', 'model-gateway/orchestration', 'Resources/Transcript', 'Data/Chats',
    'AppX/runtime/node_modules/package', 'AppX/model-gateway/orchestration', 'AppX/Resources/Transcript', 'AppX/Data/Chats')) {
    [IO.Directory]::CreateDirectory((Join-Path $fixture $relative)) | Out-Null
}
foreach ($relative in @('KYNXA.Desktop.dll', 'runtime/agent/kynxa-gateway.exe', 'runtime/node.exe', 'runtime/node_modules/package/index.js',
    'model-gateway/server.mjs', 'model-gateway/orchestration/runtime.mjs', 'Resources/Transcript/transcript.js', 'Data/Chats/user.json',
    'AppX/KYNXA.Desktop.dll', 'AppX/runtime/node.exe', 'AppX/runtime/node_modules/package/index.js', 'AppX/model-gateway/server.mjs',
    'AppX/model-gateway/orchestration/runtime.mjs', 'AppX/Resources/Transcript/transcript.js', 'AppX/Data/Chats/user.json')) {
    [IO.File]::WriteAllText((Join-Path $fixture $relative), 'isolated cleanup fixture', $utf8)
}
$legacyRevisionName = 'a' * 64
foreach ($prefix in @('', 'AppX/')) {
    $revision = Join-Path $fixture ($prefix + 'runtime/agent/revisions/' + $legacyRevisionName)
    [IO.Directory]::CreateDirectory((Join-Path $revision 'licenses/native-package-1.0.0')) | Out-Null
    foreach ($relative in @('kynxa-agent-core.node', 'kynxa-agent-core.exe', 'licenses/THIRD-PARTY-NOTICES.txt', 'licenses/native-package-1.0.0/LICENSE-MIT')) {
        [IO.File]::WriteAllText((Join-Path $revision $relative), 'isolated legacy revision fixture', $utf8)
    }
    $unrelated = Join-Path $fixture ($prefix + 'runtime/agent/revisions/not-a-legacy-revision')
    [IO.Directory]::CreateDirectory($unrelated) | Out-Null
    [IO.File]::WriteAllText((Join-Path $unrelated 'preserved.txt'), 'unknown content must survive', $utf8)
}
& $script -BundleRoot $fixture
foreach ($prefix in @('', 'AppX/')) {
    if ([IO.File]::Exists((Join-Path $fixture ($prefix + 'runtime/node.exe'))) -or
        [IO.Directory]::Exists((Join-Path $fixture ($prefix + 'runtime/node_modules'))) -or
        [IO.Directory]::Exists((Join-Path $fixture ($prefix + 'model-gateway')))) { throw 'Known obsolete backend payload survived.' }
    if ([IO.Directory]::Exists((Join-Path $fixture ($prefix + 'runtime/agent/revisions/' + $legacyRevisionName))) -or
        -not [IO.File]::Exists((Join-Path $fixture ($prefix + 'runtime/agent/revisions/not-a-legacy-revision/preserved.txt')))) {
        throw 'Exact obsolete revision cleanup removed unknown content or retained a known NAPI revision.'
    }
}
if (-not [IO.File]::Exists((Join-Path $fixture 'runtime/agent/kynxa-gateway.exe')) -or
    -not [IO.File]::Exists((Join-Path $fixture 'Resources/Transcript/transcript.js')) -or
    -not [IO.File]::Exists((Join-Path $fixture 'Data/Chats/user.json')) -or
    -not [IO.File]::Exists((Join-Path $fixture 'AppX/KYNXA.Desktop.dll')) -or
    -not [IO.File]::Exists((Join-Path $fixture 'AppX/Resources/Transcript/transcript.js')) -or
    -not [IO.File]::Exists((Join-Path $fixture 'AppX/Data/Chats/user.json'))) { throw 'Native runtime, frontend or formal data was removed.' }
& $script -BundleRoot $fixture

# All rejection fixtures are preflighted before deleting even an otherwise known Node file.
# 拒绝夹具均先完成整批核验；即使有已知 Node 文件，也不能先删后发现正式数据。
[IO.Directory]::CreateDirectory((Join-Path $fixture 'model-gateway/data')) | Out-Null
[IO.File]::WriteAllText((Join-Path $fixture 'model-gateway/data/catalog.json'), 'formal data fixture', $utf8)
[IO.File]::WriteAllText((Join-Path $fixture 'runtime/node.exe'), 'must survive rejection', $utf8)
$rejected = $false
try { & $script -BundleRoot $fixture } catch { $rejected = $_.Exception.Message.Contains('Possible formal data') }
if (-not $rejected -or -not [IO.File]::Exists((Join-Path $fixture 'runtime/node.exe')) -or
    -not [IO.File]::Exists((Join-Path $fixture 'model-gateway/data/catalog.json'))) { throw 'Formal data rejection was not atomic.' }
Remove-Item -LiteralPath (Join-Path $fixture 'model-gateway/data/catalog.json')

# A stale AppX may lack its replacement executable, but its formal data and assembly identity remain protected.
# 旧 AppX 可以尚未复制新网关；仍须保护其正式数据并核验程序集身份，再由父输出标记授权精确清理。
[IO.Directory]::CreateDirectory((Join-Path $fixture 'AppX/model-gateway/data')) | Out-Null
[IO.File]::WriteAllText((Join-Path $fixture 'AppX/model-gateway/data/catalog.json'), 'staging formal data fixture', $utf8)
[IO.File]::WriteAllText((Join-Path $fixture 'AppX/runtime/node.exe'), 'must survive staging rejection', $utf8)
$rejected = $false
try { & $script -BundleRoot $fixture } catch { $rejected = $_.Exception.Message.Contains('Possible formal data') }
if (-not $rejected -or -not [IO.File]::Exists((Join-Path $fixture 'runtime/node.exe')) -or
    -not [IO.File]::Exists((Join-Path $fixture 'AppX/runtime/node.exe')) -or
    -not [IO.File]::Exists((Join-Path $fixture 'AppX/model-gateway/data/catalog.json'))) { throw 'AppX formal data rejection was not atomic.' }
Remove-Item -LiteralPath (Join-Path $fixture 'AppX/model-gateway/data/catalog.json')

$invalidRevision = Join-Path $fixture ('AppX/runtime/agent/revisions/' + ('b' * 64))
[IO.Directory]::CreateDirectory($invalidRevision) | Out-Null
[IO.File]::WriteAllText((Join-Path $invalidRevision 'kynxa-agent-core.node'), 'must survive rejected revision', $utf8)
[IO.File]::WriteAllText((Join-Path $invalidRevision 'catalog.json'), 'possible formal data', $utf8)
$rejected = $false
try { & $script -BundleRoot $fixture } catch { $rejected = $_.Exception.Message.Contains('Unknown content or possible formal data found in an obsolete NAPI revision') }
if (-not $rejected -or -not [IO.File]::Exists((Join-Path $invalidRevision 'kynxa-agent-core.node')) -or
    -not [IO.File]::Exists((Join-Path $invalidRevision 'catalog.json')) -or
    -not [IO.File]::Exists((Join-Path $fixture 'runtime/node.exe'))) { throw 'Obsolete revision shape/data rejection was not atomic.' }
Remove-Item -LiteralPath (Join-Path $invalidRevision 'catalog.json')

Remove-Item -LiteralPath (Join-Path $fixture 'AppX/KYNXA.Desktop.dll')
$rejected = $false
try { & $script -BundleRoot $fixture } catch { $rejected = $_.Exception.Message.Contains('Desktop assembly marker is absent in the package staging folder') }
if (-not $rejected -or -not [IO.File]::Exists((Join-Path $fixture 'runtime/node.exe')) -or
    -not [IO.File]::Exists((Join-Path $fixture 'AppX/runtime/node.exe'))) { throw 'Unidentified AppX staging payload was modified.' }
[IO.File]::WriteAllText((Join-Path $fixture 'AppX/KYNXA.Desktop.dll'), 'isolated cleanup fixture', $utf8)

Remove-Item -LiteralPath (Join-Path $fixture 'runtime/agent/kynxa-gateway.exe')
$rejected = $false
try { & $script -BundleRoot $fixture } catch { $rejected = $_.Exception.Message.Contains('Native replacement marker is absent') }
if (-not $rejected -or -not [IO.File]::Exists((Join-Path $fixture 'runtime/node.exe')) -or
    -not [IO.File]::Exists((Join-Path $fixture 'AppX/runtime/node.exe'))) { throw 'Parent replacement identity was bypassed.' }

$outside = Join-Path ([IO.Path]::GetTempPath()) ('kynxa-cleanup-scope-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($outside) | Out-Null
$rejected = $false
try { & $script -BundleRoot $outside } catch { $rejected = $_.Exception.Message.Contains('limited to the verified') }
if (-not $rejected) { throw 'Unverified output scope was accepted.' }
Write-Output 'PASS: 11 root/AppX/revision cleanup, unknown/frontend/data preservation, idempotency, atomic shape/data rejection, parent/staging identity and output-scope checks.'
