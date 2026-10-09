#Requires -Version 7.0
[CmdletBinding()]
param(
    [ValidateSet('format', 'all', 'block-copy', 'metadata', 'diagrams', 'typography', 'message-time-cache', 'reply-timing', 'retrieval-tools', 'inline-math', 'computer-tools', 'multiline')]
    [string]$Scenario = 'format',
    [switch]$BuildOnly,
    [switch]$Rebuild,
    [switch]$SingleProcessor,
    [ValidateRange(60, 7200)]
    [int]$TimeoutSeconds = 300
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-VisualStudioMSBuild {
    $vswherePath = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path -LiteralPath $vswherePath) {
        $installations = @(& $vswherePath -latest -products '*' -requires Microsoft.Component.MSBuild -property installationPath)
        foreach ($installation in $installations) {
            $candidate = Join-Path $installation 'MSBuild\Current\Bin\amd64\MSBuild.exe'
            if (Test-Path -LiteralPath $candidate) { return $candidate }
        }
    }

    # Discover the installed VS host without depending on a developer-specific path.
    # 不依赖开发者机器的固定路径，发现已安装的 VS 构建宿主。
    foreach ($programFilesRoot in @($env:ProgramFiles, ${env:ProgramFiles(x86)})) {
        if ([string]::IsNullOrWhiteSpace($programFilesRoot)) { continue }
        $visualStudioRoot = Join-Path $programFilesRoot 'Microsoft Visual Studio'
        if (!(Test-Path -LiteralPath $visualStudioRoot)) { continue }
        foreach ($versionDirectory in (Get-ChildItem -LiteralPath $visualStudioRoot -Directory | Sort-Object Name -Descending)) {
            foreach ($editionDirectory in (Get-ChildItem -LiteralPath $versionDirectory.FullName -Directory)) {
                $candidate = Join-Path $editionDirectory.FullName 'MSBuild\Current\Bin\amd64\MSBuild.exe'
                if (Test-Path -LiteralPath $candidate) { return $candidate }
            }
        }
    }
    throw 'VS amd64 MSBuild was not found. Install the Visual Studio WinUI development tools.'
}

$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$projectPath = Join-Path $PSScriptRoot 'TranscriptUiSmoke.csproj'
$artifactRoot = Join-Path $repoRoot 'artifacts\transcript-ui-smoke\vs-framework-csc-x64'
$objRoot = (Join-Path $artifactRoot 'obj') + [IO.Path]::DirectorySeparatorChar
$binRoot = (Join-Path $artifactRoot 'bin') + [IO.Path]::DirectorySeparatorChar
$runName = '{0}-{1}-{2}' -f (Get-Date -Format 'yyyyMMdd-HHmmss'), $Scenario, [Guid]::NewGuid().ToString('N').Substring(0, 8)
$runRoot = Join-Path $artifactRoot (Join-Path 'runs' $runName)
$temporaryRoot = Join-Path $runRoot 'temp'
$buildLog = Join-Path $runRoot 'build.log'
$resultPath = Join-Path $temporaryRoot 'kynxa-transcript-smoke.txt'
$metricsPath = Join-Path $temporaryRoot 'kynxa-transcript-smoke.json'
$failurePath = Join-Path $temporaryRoot 'kynxa-transcript-failure.json'
[string[]]$scenarioArguments = @()
if ($Scenario -ne 'all') { $scenarioArguments = @('--' + $Scenario + '-only') }
$environmentNames = @('DOTNET_PROCESSOR_COUNT', 'DOTNET_TieredCompilation', 'TEMP', 'TMP', 'WEBVIEW2_USER_DATA_FOLDER')
$previousEnvironment = @{}
$fixtureProcess = $null
$buildLock = $null
$runnerProcess = $null
$originalProcessorAffinity = [IntPtr]::Zero
$processorAffinityChanged = $false
$originalAffinityHex = $null
$buildAffinityHex = $null

New-Item -ItemType Directory -Path $temporaryRoot -Force | Out-Null
try {
    # Serialize this runner's shared outputs; never clean the project's normal obj/bin.
    # 串行使用本入口的独立产物，始终保留项目原有的 obj/bin。
    $buildLock = [IO.File]::Open((Join-Path $artifactRoot 'runner.lock'), [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    foreach ($environmentName in $environmentNames) {
        $previousEnvironment[$environmentName] = [Environment]::GetEnvironmentVariable($environmentName, 'Process')
    }
    [Environment]::SetEnvironmentVariable('DOTNET_PROCESSOR_COUNT', '1', 'Process')
    [Environment]::SetEnvironmentVariable('DOTNET_TieredCompilation', '0', 'Process')
    [Environment]::SetEnvironmentVariable('TEMP', $temporaryRoot, 'Process')
    [Environment]::SetEnvironmentVariable('TMP', $temporaryRoot, 'Process')
    [Environment]::SetEnvironmentVariable('WEBVIEW2_USER_DATA_FOLDER', (Join-Path $temporaryRoot 'webview2'), 'Process')

    $msbuildPath = Get-VisualStudioMSBuild
    # Keep the SDK/target runtime while using VS's .NET Framework compiler host.
    # 保留 SDK 和目标应用运行时，仅将测试编译宿主切换为 VS 的 .NET Framework 编译器。
    $compilerDirectory = [IO.Path]::GetFullPath((Join-Path ([IO.Path]::GetDirectoryName($msbuildPath)) '..\Roslyn'))
    $compilerPath = Join-Path $compilerDirectory 'csc.exe'
    $compilerConfigPath = Join-Path $compilerDirectory 'csc.exe.config'
    if (!(Test-Path -LiteralPath $compilerPath) -or !(Test-Path -LiteralPath $compilerConfigPath)) {
        throw "The Visual Studio .NET Framework compiler is missing: $compilerDirectory"
    }
    $compilerVersion = (Get-Item -LiteralPath $compilerPath).VersionInfo.ProductVersion
    $buildProperties = @(
        '/p:Configuration=Debug', '/p:Platform=x64', '/p:RuntimeIdentifier=win-x64',
        '/p:ConcurrentBuild=false', '/p:UseSharedCompilation=false',
        "/p:CscToolPath=$compilerDirectory", '/p:CscToolExe=csc.exe',
        "/p:BaseIntermediateOutputPath=$objRoot", "/p:MSBuildProjectExtensionsPath=$objRoot", "/p:BaseOutputPath=$binRoot"
    )
    $buildTarget = if ($Rebuild) { 'Rebuild' } else { 'Build' }
    $buildArguments = @(
        $projectPath, '/restore', "/t:$buildTarget", '/m:1', '/nr:false', '/nologo', '/verbosity:minimal', '/fileLogger',
        "/fileLoggerParameters:LogFile=$buildLog;Encoding=UTF-8"
    ) + $buildProperties
    Write-Host "MSBuild: $msbuildPath"
    Write-Host "Compiler: $compilerPath ($compilerVersion)"
    Write-Host "Artifacts: $runRoot"
    if ($SingleProcessor) {
        # Children inherit this temporary scheduling mask; it is a local compatibility
        # diagnostic, not a confirmed hardware diagnosis or a machine-wide setting.
        # 子进程继承此临时调度掩码；它仅用于本机兼容诊断，不代表已确诊硬件或更改全机设置。
        # https://learn.microsoft.com/en-us/windows/win32/procthread/inheritance
        $runnerProcess = [Diagnostics.Process]::GetCurrentProcess()
        $runnerProcess.Refresh()
        $originalProcessorAffinity = $runnerProcess.ProcessorAffinity
        $originalMask = $originalProcessorAffinity.ToInt64()
        $selectedMask = 0L
        for ($bitIndex = 0; $bitIndex -lt ([IntPtr]::Size * 8); $bitIndex++) {
            $candidateMask = 1L -shl $bitIndex
            if (($originalMask -band $candidateMask) -ne 0) {
                $selectedMask = $candidateMask
                break
            }
        }
        if ($selectedMask -eq 0) { throw 'The current process has no available processor affinity bit.' }
        $originalAffinityHex = '0x{0:X}' -f $originalMask
        $buildAffinityHex = '0x{0:X}' -f $selectedMask
        $processorAffinityChanged = $true
        $runnerProcess.ProcessorAffinity = [IntPtr]::new($selectedMask)
        $runnerProcess.Refresh()
        if ($runnerProcess.ProcessorAffinity.ToInt64() -ne $selectedMask) { throw 'Could not apply the requested build affinity.' }
        Write-Host "Build processor affinity: $buildAffinityHex (original $originalAffinityHex)"
    }
    & $msbuildPath @buildArguments
    if ($LASTEXITCODE -ne 0) { throw "Build failed. See $buildLog" }

    # Query the same project/properties so a successful build cannot launch an old binary.
    # 以相同项目和属性查询真实目标路径，防止构建成功后启动历史程序集。
    $targetOutput = @(& $msbuildPath $projectPath '/nologo' '/verbosity:quiet' '/nr:false' '/getProperty:TargetPath,UseAppHost,MSBuildRuntimeType,Platform,RuntimeIdentifier,CscToolPath,CscToolExe' @buildProperties)
    if ($LASTEXITCODE -ne 0) { throw 'Could not resolve the built target path.' }
    # Restore normal scheduling before starting the native fixture or its WebView children.
    # 启动原生夹具及其 WebView 子进程前，恢复调用进程原有的调度范围。
    if ($processorAffinityChanged) {
        $runnerProcess.ProcessorAffinity = $originalProcessorAffinity
        $runnerProcess.Refresh()
        if ($runnerProcess.ProcessorAffinity -ne $originalProcessorAffinity) { throw 'Could not restore the original process affinity.' }
        $processorAffinityChanged = $false
    }
    $targetProperties = (($targetOutput -join [Environment]::NewLine) | ConvertFrom-Json).Properties
    $managedTargetPath = [IO.Path]::GetFullPath($targetProperties.TargetPath)
    $targetPath = [IO.Path]::ChangeExtension($managedTargetPath, '.exe')
    if ($targetProperties.MSBuildRuntimeType -ne 'Full' -or $targetProperties.Platform -ne 'x64' -or $targetProperties.RuntimeIdentifier -ne 'win-x64') {
        throw 'The evaluated build host, platform or runtime identifier does not match this runner.'
    }
    if (![string]::Equals([IO.Path]::GetFullPath($targetProperties.CscToolPath), $compilerDirectory, [StringComparison]::OrdinalIgnoreCase) -or $targetProperties.CscToolExe -ne 'csc.exe') {
        throw 'The evaluated compiler does not match the Visual Studio .NET Framework compiler.'
    }
    if ($targetProperties.UseAppHost -ne 'true' -or !$managedTargetPath.StartsWith($binRoot, [StringComparison]::OrdinalIgnoreCase) -or !(Test-Path -LiteralPath $managedTargetPath) -or !(Test-Path -LiteralPath $targetPath)) {
        throw "The built executable is missing or outside the isolated output: $targetPath"
    }
    [ordered]@{
        scenario = $Scenario
        msbuildPath = $msbuildPath
        compilerPath = $compilerPath
        compilerConfigPath = $compilerConfigPath
        compilerVersion = $compilerVersion
        buildTarget = $buildTarget
        singleProcessor = [bool]$SingleProcessor
        originalProcessorAffinity = $originalAffinityHex
        buildProcessorAffinity = $buildAffinityHex
        processorAffinityRestoredBeforeFixture = !$processorAffinityChanged
        managedTargetPath = $managedTargetPath
        managedTargetSha256 = (Get-FileHash -LiteralPath $managedTargetPath -Algorithm SHA256).Hash
        targetPath = $targetPath
        targetSha256 = (Get-FileHash -LiteralPath $targetPath -Algorithm SHA256).Hash
        properties = $targetProperties
        buildOnly = [bool]$BuildOnly
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $runRoot 'run.json') -Encoding UTF8
    if ($BuildOnly) {
        Write-Host "PASS: isolated Debug x64 build. Target: $targetPath"
        return
    }

    # Each run owns its TEMP and WebView profile, excluding stale PASS/metrics files.
    # 每次运行拥有独立 TEMP 和 WebView 配置，避免误读旧的 PASS 或指标。
    $startedUtc = [DateTime]::UtcNow
    $startParameters = @{
        FilePath = $targetPath
        WorkingDirectory = [IO.Path]::GetDirectoryName($targetPath)
        WindowStyle = 'Hidden'
        PassThru = $true
        RedirectStandardOutput = (Join-Path $runRoot 'runner.stdout.log')
        RedirectStandardError = (Join-Path $runRoot 'runner.stderr.log')
    }
    if ($scenarioArguments.Count -gt 0) { $startParameters.ArgumentList = $scenarioArguments }
    $fixtureProcess = Start-Process @startParameters
    if (!$fixtureProcess.WaitForExit($TimeoutSeconds * 1000)) {
        Stop-Process -Id $fixtureProcess.Id -ErrorAction SilentlyContinue
        throw "Fixture timed out after $TimeoutSeconds seconds. Only this runner's process was stopped."
    }
    $fixtureProcess.WaitForExit()

    foreach ($evidencePath in @($resultPath, $metricsPath, $failurePath)) {
        if (Test-Path -LiteralPath $evidencePath) {
            $evidenceFile = Get-Item -LiteralPath $evidencePath
            if ($evidenceFile.LastWriteTimeUtc -lt $startedUtc) { throw "Stale fixture evidence: $evidencePath" }
            Copy-Item -LiteralPath $evidencePath -Destination (Join-Path $runRoot $evidenceFile.Name)
        }
    }
    if (!(Test-Path -LiteralPath $resultPath)) { throw "Fixture exited without a result. Exit code: $($fixtureProcess.ExitCode)" }
    $result = (Get-Content -LiteralPath $resultPath -Raw).Trim()
    Write-Host $result
    if ($fixtureProcess.ExitCode -ne 0 -or $result -notmatch '^PASS:' -or $result -match '(?m)^FAIL:') {
        throw "Fixture failed (exit $($fixtureProcess.ExitCode)). See $runRoot"
    }
    Write-Host "Evidence: $runRoot"
}
finally {
    try {
        if ($processorAffinityChanged) { $runnerProcess.ProcessorAffinity = $originalProcessorAffinity }
    }
    finally {
        if ($null -ne $fixtureProcess) {
            if (!$fixtureProcess.HasExited) { Stop-Process -Id $fixtureProcess.Id -ErrorAction SilentlyContinue }
            $fixtureProcess.Dispose()
        }
        foreach ($environmentName in $previousEnvironment.Keys) {
            # Pass a CLR null explicitly; PowerShell otherwise binds $null to an empty string.
            # 显式传入 CLR null，避免 PowerShell 将不存在的原变量恢复为空字符串。
            if ($null -eq $previousEnvironment[$environmentName]) {
                [Environment]::SetEnvironmentVariable($environmentName, [NullString]::Value, 'Process')
            }
            else {
                [Environment]::SetEnvironmentVariable($environmentName, $previousEnvironment[$environmentName], 'Process')
            }
        }
        if ($null -ne $buildLock) { $buildLock.Dispose() }
        if ($null -ne $runnerProcess) { $runnerProcess.Dispose() }
    }
}
