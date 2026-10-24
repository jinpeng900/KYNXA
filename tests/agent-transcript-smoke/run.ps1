param(
    [string]$NodePath = '',
    [string]$ResultDirectory = ''
)

$ErrorActionPreference = 'Stop'
if (-not $NodePath) { $NodePath = (Get-Command node -ErrorAction SilentlyContinue).Source }
if (-not $NodePath -or -not (Test-Path -LiteralPath $NodePath)) {
    throw 'Node.js is required for the isolated CDP test driver. Pass -NodePath if it is not on PATH.'
}
$agentEdgeCandidates = @(
    (Join-Path ${env:ProgramFiles(x86)} 'Microsoft/Edge/Application/msedge.exe'),
    (Join-Path $env:ProgramFiles 'Microsoft/Edge/Application/msedge.exe')
)
$agentEdgePath = $agentEdgeCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $agentEdgePath) { throw 'Microsoft Edge is required for the isolated headless DOM check.' }
$agentFixtureRoot = if ($ResultDirectory) { [IO.Path]::GetFullPath($ResultDirectory) }
    else { Join-Path ([IO.Path]::GetTempPath()) ('kynxa-agent-transcript-' + [Guid]::NewGuid().ToString('N')) }
New-Item -ItemType Directory -Force -Path $agentFixtureRoot | Out-Null
$agentScriptPath = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../apps/desktop/Resources/Transcript/transcript.js'))
$agentScriptUri = [Uri]::new($agentScriptPath).AbsoluteUri
$agentStylePath = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../apps/desktop/Resources/Transcript/transcript.css'))
$agentLinksPath = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../apps/desktop/Resources/Transcript/tool-web-links.js'))
$agentMarkdownLinksPath = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../apps/desktop/Resources/Transcript/markdown-links.js'))
$agentPresentationPath = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../apps/desktop/Resources/Transcript/tool-presentation.js'))
$agentMessagePath = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../apps/desktop/Resources/Transcript/message-presentation.js'))
$agentHtml = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'fixture.html')).Replace('__TRANSCRIPT_SCRIPT__', $agentScriptUri).Replace('__WEB_LINKS_SCRIPT__', [Uri]::new($agentLinksPath).AbsoluteUri).Replace('__TOOL_PRESENTATION_SCRIPT__', [Uri]::new($agentPresentationPath).AbsoluteUri).Replace('__MESSAGE_PRESENTATION_SCRIPT__', [Uri]::new($agentMessagePath).AbsoluteUri).Replace('__TRANSCRIPT_STYLE__', [Uri]::new($agentStylePath).AbsoluteUri)
$agentHtml = $agentHtml.Replace('__MARKDOWN_LINKS_SCRIPT__', [Uri]::new($agentMarkdownLinksPath).AbsoluteUri)
$agentFixturePath = Join-Path $agentFixtureRoot 'fixture.html'
[IO.File]::WriteAllText($agentFixturePath, $agentHtml, [Text.UTF8Encoding]::new($false))
$agentFixtureUri = [Uri]::new($agentFixturePath).AbsoluteUri
# Drive actual browser frames; virtual-time dump-dom does not reliably run the viewport observers.
# 使用真实浏览器帧，虚拟时钟配合 dump-dom 不能可靠驱动视口观察器。
& $NodePath (Join-Path $PSScriptRoot 'cdp-checks.mjs') $agentEdgePath $agentFixtureUri $agentFixtureRoot
if ($LASTEXITCODE -ne 0) { throw "Compact progress DOM checks failed; see $agentFixtureRoot" }
