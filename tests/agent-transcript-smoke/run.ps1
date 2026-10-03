$ErrorActionPreference = 'Stop'
$agentEdgeCandidates = @(
    (Join-Path ${env:ProgramFiles(x86)} 'Microsoft/Edge/Application/msedge.exe'),
    (Join-Path $env:ProgramFiles 'Microsoft/Edge/Application/msedge.exe')
)
$agentEdgePath = $agentEdgeCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $agentEdgePath) { throw 'Microsoft Edge is required for the isolated headless DOM check.' }
$agentFixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('kynxa-agent-transcript-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $agentFixtureRoot | Out-Null
$agentScriptPath = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../apps/desktop/Resources/Transcript/transcript.js'))
$agentScriptUri = [Uri]::new($agentScriptPath).AbsoluteUri
$agentHtml = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'fixture.html')).Replace('__TRANSCRIPT_SCRIPT__', $agentScriptUri)
$agentFixturePath = Join-Path $agentFixtureRoot 'fixture.html'
[IO.File]::WriteAllText($agentFixturePath, $agentHtml, [Text.UTF8Encoding]::new($false))
$agentOutputPath = Join-Path $agentFixtureRoot 'dom.txt'
$agentErrorPath = Join-Path $agentFixtureRoot 'browser.txt'
$agentProfilePath = Join-Path $agentFixtureRoot 'edge-profile'
$agentFixtureUri = [Uri]::new($agentFixturePath).AbsoluteUri
$agentArguments = @('--headless', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--disable-extensions',
    ('--user-data-dir="' + $agentProfilePath + '"'), '--allow-file-access-from-files', '--dump-dom', ('"' + $agentFixtureUri + '"'))
$agentProcess = Start-Process -FilePath $agentEdgePath -ArgumentList $agentArguments -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput $agentOutputPath -RedirectStandardError $agentErrorPath
if (-not $agentProcess.WaitForExit(30000)) {
    $agentProcess.Kill()
    throw 'The isolated headless browser timed out.'
}
$agentDeadline = [DateTime]::UtcNow.AddSeconds(30)
$agentResult = ''
while ([DateTime]::UtcNow -lt $agentDeadline -and -not $agentResult) {
    $agentStream = [IO.File]::Open($agentOutputPath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
    $agentReader = [IO.StreamReader]::new($agentStream)
    try { $agentDom = $agentReader.ReadToEnd() } finally { $agentReader.Dispose() }
    $agentResult = [Text.RegularExpressions.Regex]::Match($agentDom, '<pre id="result">((?:PASS:|FAIL:).*?)</pre>', [Text.RegularExpressions.RegexOptions]::Singleline).Groups[1].Value
    if (-not $agentResult) { Start-Sleep -Milliseconds 100 }
}
if (-not $agentResult.StartsWith('PASS:')) { throw [Net.WebUtility]::HtmlDecode($agentResult) }
Write-Output ([Net.WebUtility]::HtmlDecode($agentResult))
