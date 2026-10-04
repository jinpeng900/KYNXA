$ErrorActionPreference = 'Stop'
$screenshotFixtureProject = Join-Path $PSScriptRoot 'ScreenshotPanelUiSmoke.csproj'
dotnet build $screenshotFixtureProject -p:Platform=x64 -p:EnableWinAppRunSupport=false
if ($LASTEXITCODE -ne 0) { throw 'Screenshot panel fixture build failed.' }
$screenshotFixtureExecutable = Join-Path $PSScriptRoot 'bin/x64/Debug/net10.0-windows10.0.26100.0/win-x64/ScreenshotPanelUiSmoke.exe'
$screenshotFixtureProcess = Start-Process -FilePath $screenshotFixtureExecutable -WindowStyle Hidden -PassThru
if (-not $screenshotFixtureProcess.WaitForExit(45000)) {
    $screenshotFixtureProcess.Kill()
    throw "Screenshot panel fixture timed out (owned process $($screenshotFixtureProcess.Id))."
}
$screenshotFixturePointer = Join-Path ([IO.Path]::GetTempPath()) 'kynxa-screenshot-panel-latest.txt'
$screenshotFixtureResultPath = [IO.File]::ReadAllText($screenshotFixturePointer).Trim()
$screenshotFixtureResult = [IO.File]::ReadAllText($screenshotFixtureResultPath)
Write-Output $screenshotFixtureResult
if ($screenshotFixtureProcess.ExitCode -ne 0 -or $screenshotFixtureResult -notmatch 'PASS: \d+ screenshot panel UI checks\.') {
    throw "Screenshot panel fixture failed. Result: $screenshotFixtureResultPath"
}
