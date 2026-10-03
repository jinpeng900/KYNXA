$ErrorActionPreference = 'Stop'
$fixtureProject = Join-Path $PSScriptRoot 'ModelUiSmoke.csproj'
dotnet build $fixtureProject
if ($LASTEXITCODE -ne 0) { throw 'Model native UI fixture build failed.' }
$fixtureExecutable = Join-Path $PSScriptRoot 'bin/Debug/net10.0-windows10.0.26100.0/win-x64/ModelUiSmoke.exe'
$fixtureProcess = Start-Process -FilePath $fixtureExecutable -WindowStyle Hidden -PassThru
if (-not $fixtureProcess.WaitForExit(45000)) {
    $fixtureProcess.Kill()
    throw "Model UI fixture timed out (owned process $($fixtureProcess.Id))."
}
$fixturePointer = Join-Path ([System.IO.Path]::GetTempPath()) 'kynxa-model-ui-smoke-latest.txt'
$fixtureResultPath = [System.IO.File]::ReadAllText($fixturePointer).Trim()
$fixtureResult = [System.IO.File]::ReadAllText($fixtureResultPath)
Write-Output $fixtureResult
if ($fixtureProcess.ExitCode -ne 0 -or $fixtureResult -notmatch 'PASS: \d+ native model UI checks\.') {
    throw "Model UI fixture failed. Result: $fixtureResultPath"
}
