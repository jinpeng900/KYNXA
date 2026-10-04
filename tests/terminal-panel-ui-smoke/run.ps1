$ErrorActionPreference = 'Stop'
$terminalFixtureProject = Join-Path $PSScriptRoot 'TerminalPanelUiSmoke.csproj'
dotnet build $terminalFixtureProject -p:Platform=x64 -p:EnableWinAppRunSupport=false
if ($LASTEXITCODE -ne 0) { throw 'Terminal panel fixture build failed.' }
$terminalFixtureExecutable = Join-Path $PSScriptRoot 'bin/x64/Debug/net10.0-windows10.0.26100.0/win-x64/TerminalPanelUiSmoke.exe'
$terminalFixtureProcess = Start-Process -FilePath $terminalFixtureExecutable -WindowStyle Hidden -PassThru
if (-not $terminalFixtureProcess.WaitForExit(45000)) {
    $terminalFixtureProcess.Kill()
    throw "Terminal panel fixture timed out (owned process $($terminalFixtureProcess.Id))."
}
$terminalFixturePointer = Join-Path ([IO.Path]::GetTempPath()) 'kynxa-terminal-panel-latest.txt'
$terminalFixtureResultPath = [IO.File]::ReadAllText($terminalFixturePointer).Trim()
$terminalFixtureResult = [IO.File]::ReadAllText($terminalFixtureResultPath)
Write-Output $terminalFixtureResult
if ($terminalFixtureProcess.ExitCode -ne 0 -or $terminalFixtureResult -notmatch 'PASS: \d+ terminal panel checks\.') {
    throw "Terminal panel fixture failed. Result: $terminalFixtureResultPath"
}
