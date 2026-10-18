[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$BundleRoot, [string]$ResultPath)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
$source = [IO.Path]::GetFullPath($BundleRoot)
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('kynxa-native-package-' + [Guid]::NewGuid().ToString('N'))
$package = Join-Path $fixture 'package with spaces'
[IO.Directory]::CreateDirectory($package) | Out-Null
$checks = [Collections.Generic.List[string]]::new()
function Assert-NativePackage([bool]$Condition, [string]$Label) {
    if (-not $Condition) { throw $Label }
    $checks.Add($Label)
}
function Get-PackageHash([string]$Path) {
    $stream=[IO.File]::OpenRead($Path);$hasher=[Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($hasher.ComputeHash($stream)).Replace('-','').ToLowerInvariant() }
    finally { $stream.Dispose();$hasher.Dispose() }
}
function New-IsolatedNativeStart([string]$Executable) {
    $start=[Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute=$false;$start.CreateNoWindow=$true;$start.WorkingDirectory=$package
    $start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true;$start.RedirectStandardInput=$true
    $start.EnvironmentVariables['PATH']=(Join-Path $package 'runtime/native')+';'+(Join-Path $env:SystemRoot 'System32')
    $start.EnvironmentVariables['DOTNET_ROOT']=Join-Path $fixture 'missing-dotnet'
    $start.EnvironmentVariables['DOTNET_MULTILEVEL_LOOKUP']='0'
    $start.EnvironmentVariables['KYNXA_DATA_HOME']=Join-Path $fixture 'isolated-data'
    $start.EnvironmentVariables['KYNXA_MODEL_HOME']=Join-Path $fixture 'isolated-models'
    $start.EnvironmentVariables['KYNXA_EXTENSION_HOME']=Join-Path $fixture 'isolated-extensions'
    $start.EnvironmentVariables['KYNXA_APPLICATION_ROOT']=$package
    return $start
}
function Stop-OwnedNativeProcess($Process) {
    if (-not $Process.HasExited) {
        # The live owned handle identifies this fixture; never stop a process by name.
        # 自有活动句柄明确标识本夹具；不按名称关闭用户的其他进程。
        & (Join-Path $env:SystemRoot 'System32/taskkill.exe') /PID $Process.Id /T /F | Out-Null
        [void]$Process.WaitForExit(5000)
    }
}
function Invoke-IsolatedNative([string]$Executable, [string]$InputText) {
    $process=[Diagnostics.Process]::Start((New-IsolatedNativeStart $Executable))
    try {
        $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
        $process.StandardInput.WriteLine($InputText);$process.StandardInput.Close()
        if (-not $process.WaitForExit(15000)) { Stop-OwnedNativeProcess $process;throw 'Isolated native helper timed out.' }
        if ($process.ExitCode -ne 0) { throw ('Isolated native helper failed: '+$stderr.Result) }
        return $stdout.Result.Trim()
    }
    finally { $process.Dispose() }
}

foreach ($directory in @('runtime','ToolHost','official-tools')) {
    Assert-NativePackage ([IO.Directory]::Exists((Join-Path $source $directory))) "Required payload directory exists: $directory"
    Copy-Item -LiteralPath (Join-Path $source $directory) -Destination $package -Recurse
}
$officeScripts = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
$officeRoot = Join-Path $package 'runtime/office'
if ([IO.Directory]::Exists($officeRoot)) {
    $officeSpecification = Get-Content -LiteralPath (Join-Path $PSScriptRoot '../../apps/desktop/Build/office-preview-runtime.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    $officeRecord = Get-Content -LiteralPath (Join-Path $officeRoot 'runtime.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-NativePackage ($officeRecord.version -eq $officeSpecification.version -and $officeRecord.archiveSha256 -eq $officeSpecification.sha256 -and
        $officeRecord.executable -eq $officeSpecification.executable) 'Private Office converter matches the pinned archive identity.'
    Assert-NativePackage ([IO.File]::Exists((Join-Path $officeRoot $officeSpecification.executable))) 'Pinned private Office converter executable is bundled.'
    # The fixed Office archive includes help pages and sample macros, not a JS gateway.
    # 固定 Office 归档自带帮助页与示例宏，仅放行这些确切脚本，仍拒绝其他 Node/NAPI/JS 载荷。
    foreach ($relative in @('help/a11y-toggle.js','help/flexsearch.debug.js','help/help.js','help/help2.js','help/hid2file.js',
        'help/languages.js','help/paginathing.js','help/polyfills.js','help/prism.js',
        'share/Scripts/javascript/ExportSheetsToHTML/exportsheetstohtml.js','share/Scripts/javascript/HelloWorld/helloworld.js',
        'share/Scripts/javascript/Highlight/ButtonPressHandler.js','share/Scripts/javascript/Highlight/ShowDialog.js')) {
        [void]$officeScripts.Add('runtime/office/' + $relative)
    }
}
$forbidden=@(Get-ChildItem -LiteralPath $package -File -Recurse | Where-Object {
    $relative = $_.FullName.Substring($package.Length + 1).Replace('\','/')
    -not $_.FullName.StartsWith((Join-Path $package 'runtime/mcp')+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase) -and
    ($_.Name -match '^(node|npm|npx)(\.exe|\.cmd)?$' -or ($_.Extension -in '.node','.mjs','.cjs','.js' -and -not $officeScripts.Contains($relative)) -or
        $_.FullName -match '[\\/]node_modules[\\/]')
})
Assert-NativePackage ($forbidden.Count -eq 0) 'Independent Rust backend contains no Node, NAPI or JS gateway outside private MCP; pinned Office help/sample scripts are separate.'
$mcpManifest=Get-Content -LiteralPath (Join-Path $package 'runtime/mcp/manifest.json') -Raw -Encoding UTF8 | ConvertFrom-Json
Assert-NativePackage ($mcpManifest.coreDependsOnNode -eq $false -and $mcpManifest.downloadsAtRuntime -eq $false) 'Private MCP runtime is separate from the Rust core and performs no runtime installation.'
$privateNode=Join-Path $package $mcpManifest.node.path
Assert-NativePackage ((Get-PackageHash $privateNode) -eq $mcpManifest.node.sha256 -and [IO.File]::Exists((Join-Path $package $mcpManifest.node.license))) 'Pinned private Node executable and original license are bundled.'
foreach ($service in $mcpManifest.services) {
    Assert-NativePackage ((Get-PackageHash (Join-Path $package $service.entry)) -eq $service.sha256) "Pinned MCP entry matches: $($service.id)"
}
$nativeManifest=Get-Content -LiteralPath (Join-Path $package 'runtime/native/licenses/native-runtime-manifest.json') -Raw -Encoding UTF8 | ConvertFrom-Json
foreach ($dependency in $nativeManifest.packages) {
    foreach ($asset in $dependency.files) {
        $path=Join-Path $package $asset.path
        Assert-NativePackage ([IO.File]::Exists($path) -and (Get-Item -LiteralPath $path).Length -eq $asset.bytes -and (Get-PackageHash $path) -eq $asset.sha256) "Pinned native DLL/license matches: $($asset.path)"
    }
}
foreach ($asset in $nativeManifest.localFiles) {
    $path=Join-Path $package $asset.path
    Assert-NativePackage ([IO.File]::Exists($path) -and (Get-Item -LiteralPath $path).Length -eq $asset.bytes -and (Get-PackageHash $path) -eq $asset.sha256) "Bundled upstream license matches: $($asset.path)"
}
foreach ($directory in @('runtime/embedding/builtin-multilingual','runtime/rerank/builtin-multilingual-reranker')) {
    $manifest=Get-Content -LiteralPath (Join-Path $package ($directory+'/manifest.json')) -Raw -Encoding UTF8 | ConvertFrom-Json
    foreach ($asset in $manifest.files) {
        $path=Join-Path $package ($directory+'/'+$asset.path)
        Assert-NativePackage ([IO.File]::Exists($path) -and (Get-Item -LiteralPath $path).Length -eq $asset.bytes -and (Get-PackageHash $path) -eq $asset.sha256) "Pinned model/license matches: $directory/$($asset.path)"
    }
}
$capabilities=Invoke-IsolatedNative (Join-Path $package 'ToolHost/KYNXA.ToolHost.exe') '{"operation":"desktop_capabilities"}' | ConvertFrom-Json
Assert-NativePackage ($capabilities.protocolVersion -eq 1 -and $capabilities.boundary -eq 'host-desktop') 'Self-contained ToolHost starts without system .NET or Node on PATH.'
$listener=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0);$listener.Start();$port=$listener.LocalEndpoint.Port;$listener.Stop()
$start=New-IsolatedNativeStart (Join-Path $package 'runtime/agent/kynxa-gateway.exe')
$start.EnvironmentVariables['KYNXA_MODEL_API_PORT']=[string]$port
$gateway=[Diagnostics.Process]::Start($start)
try {
    $stdout=$gateway.StandardOutput.ReadToEndAsync();$stderr=$gateway.StandardError.ReadToEndAsync()
    $deadline=[DateTime]::UtcNow.AddSeconds(30);$health=$null
    while ([DateTime]::UtcNow -lt $deadline -and -not $gateway.HasExited) {
        try { $health=Invoke-RestMethod -Uri "http://127.0.0.1:$port/health" -TimeoutSec 2;break }
        catch { Start-Sleep -Milliseconds 200 }
    }
    if (-not $health) { Stop-OwnedNativeProcess $gateway;throw ('Isolated native gateway failed to start: '+$stderr.Result) }
    Assert-NativePackage ($health.runtime -eq 'rust' -and $health.nodeRuntime -eq $false) 'Native HTTP gateway starts with real ORT and resource service; its core does not depend on Node.'
    $catalog=Invoke-RestMethod -Uri "http://127.0.0.1:$port/api/conversations/catalog?includeMessages=false" -TimeoutSec 5
    Assert-NativePackage ($catalog.Revision -eq 0) 'Native data catalog initializes in an isolated directory.'
    $tools=Invoke-RestMethod -Uri "http://127.0.0.1:$port/api/agent/tools" -TimeoutSec 5
    Assert-NativePackage ($tools.tools.name -contains 'filesystem.read' -and $tools.tools.name -contains 'browser.navigate') 'Native file and browser abilities are discoverable without connecting external MCPs.'
}
finally { Stop-OwnedNativeProcess $gateway;$gateway.Dispose() }
$report=@{passed=$checks.Count;failed=0;checks=@($checks);fixture=$fixture;runtime='rust';nodeRuntime=$false;externalNodeRuntime='private-pinned';scope='native portable payload, not MSIX installation or model quality'}
if ($ResultPath) { [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($ResultPath)))|Out-Null;[IO.File]::WriteAllText([IO.Path]::GetFullPath($ResultPath),($report|ConvertTo-Json -Depth 5),[Text.UTF8Encoding]::new($false)) }
$report|ConvertTo-Json -Depth 5
