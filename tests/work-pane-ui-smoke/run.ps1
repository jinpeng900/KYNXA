$ErrorActionPreference = 'Stop'
$workPaneFixtureProject = Join-Path $PSScriptRoot 'WorkPaneUiSmoke.csproj'
dotnet build $workPaneFixtureProject -p:Platform=x64 -p:EnableWinAppRunSupport=false
if ($LASTEXITCODE -ne 0) { throw 'Work pane fixture build failed.' }
$workPaneFixtureExecutable = Join-Path $PSScriptRoot 'bin/x64/Debug/net10.0-windows10.0.26100.0/win-x64/WorkPaneUiSmoke.exe'
$workPaneFixtureProcess = Start-Process -FilePath $workPaneFixtureExecutable -WindowStyle Hidden -PassThru
if (-not $workPaneFixtureProcess.WaitForExit(45000)) {
    $workPaneFixtureProcess.Kill()
    throw "Work pane fixture timed out (owned process $($workPaneFixtureProcess.Id))."
}
$workPaneFixturePointer = Join-Path ([IO.Path]::GetTempPath()) 'kynxa-work-pane-latest.txt'
$workPaneFixtureResultPath = [IO.File]::ReadAllText($workPaneFixturePointer).Trim()
$workPaneFixtureResult = [IO.File]::ReadAllText($workPaneFixtureResultPath)
Write-Output $workPaneFixtureResult
if ($workPaneFixtureProcess.ExitCode -ne 0 -or $workPaneFixtureResult -notmatch 'PASS: \d+ tabbed work pane checks\.') {
    throw "Work pane fixture failed. Result: $workPaneFixtureResultPath"
}
