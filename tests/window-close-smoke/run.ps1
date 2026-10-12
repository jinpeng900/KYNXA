$ErrorActionPreference = 'Stop'
$fixtureProject = Join-Path $PSScriptRoot 'WindowCloseSmoke.csproj'
dotnet build $fixtureProject
if ($LASTEXITCODE -ne 0) { throw 'Window-close native UI fixture build failed.' }
$fixtureExecutable = Join-Path $PSScriptRoot 'bin/Debug/net10.0-windows10.0.26100.0/win-x64/WindowCloseSmoke.exe'
$fixtureProcess = Start-Process -FilePath $fixtureExecutable -WindowStyle Hidden -PassThru
if (-not $fixtureProcess.WaitForExit(30000)) {
    $fixtureProcess.Kill()
    throw "Window-close UI fixture timed out (owned process $($fixtureProcess.Id))."
}
$fixturePointer = Join-Path ([System.IO.Path]::GetTempPath()) 'kynxa-window-close-smoke-latest.txt'
$fixtureResultPath = [System.IO.File]::ReadAllText($fixturePointer).Trim()
$fixtureResult = [System.IO.File]::ReadAllText($fixtureResultPath)
Write-Output $fixtureResult
if ($fixtureProcess.ExitCode -ne 0 -or $fixtureResult -notmatch 'PASS: \d+ native window-close UI checks\.') {
    throw "Window-close UI fixture failed. Result: $fixtureResultPath"
}
