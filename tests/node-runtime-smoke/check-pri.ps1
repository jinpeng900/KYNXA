$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('kynxa-runtime-pri-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($fixture) | Out-Null
$layout = Join-Path $fixture 'filtered.layout.resfiles'
$excluded = Join-Path $fixture 'excluded.layout.resfiles'
$unfiltered = Join-Path $fixture 'unfiltered.layout.resfiles'
$lines = @('Assets\Logo.scale-200.png', 'Web\Transcript\index.html',
    'runtime\node_modules\npm\node_modules\exponential-backoff\dist\jitter\no\no.jitter.js',
    'runtime\node_modules\npm\node_modules\node-gyp\.release-please-manifest.json',
    'ToolHost\zh-Hans\PresentationCore.resources.dll', 'TOOLHOST/de/WindowsBase.resources.dll',
    'ToolHost\licenses\Microsoft.NETCore.App.Runtime.win-x64\10.0.11\THIRD-PARTY-NOTICES.TXT',
    'runtime-copy\readme.txt', 'Assets\no.jitter.js')
$utf8 = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllLines($layout, $lines, $utf8)
[IO.File]::WriteAllLines($unfiltered, $lines, $utf8)
[IO.File]::WriteAllLines($excluded, @('Other\existing.txt'), $utf8)
$payload = Join-Path $fixture 'runtime\node.exe'
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($payload)) | Out-Null
[IO.File]::WriteAllText($payload, 'original binary fixture', $utf8)
$script = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../apps/desktop/Build/filter-runtime-pri-layout.ps1'))
& $script -LayoutPath $layout -ExcludedLayoutPath $excluded
$kept = [IO.File]::ReadAllLines($layout, [Text.Encoding]::UTF8)
$plain = [IO.File]::ReadAllLines($excluded, [Text.Encoding]::UTF8)
if (($kept -join '|') -ne 'Assets\Logo.scale-200.png|Web\Transcript\index.html|runtime-copy\readme.txt|Assets\no.jitter.js') { throw 'The ordinary desktop assets or similarly named folders changed.' }
if ($plain.Count -ne 6 -or $plain[0] -ne 'Other\existing.txt') { throw 'The runtime payloads were not retained alongside existing main-package entries.' }
if ([IO.File]::ReadAllText($unfiltered) -ne (($lines -join [Environment]::NewLine) + [Environment]::NewLine)) { throw 'The complete package layout changed.' }
if ([IO.File]::ReadAllText($payload) -ne 'original binary fixture') { throw 'A runtime payload file was changed or deleted.' }
$before = [IO.File]::ReadAllText($excluded)
& $script -LayoutPath $layout -ExcludedLayoutPath $excluded
if ([IO.File]::ReadAllText($excluded) -ne $before) { throw 'Repeated PRI preparation created duplicate package entries.' }
Write-Output 'PASS: 5 PRI payload isolation, ordinary desktop assets, package preservation and idempotency checks.'
