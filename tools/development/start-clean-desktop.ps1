[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')][string]$Configuration = 'Debug',
    [ValidateRange(1, 65535)][int]$Port = 5262,
    [Parameter(DontShow = $true)][switch]$InsidePackage,
    [Parameter(DontShow = $true)][string]$LaunchPlanPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$utf8 = [Text.UTF8Encoding]::new($false)
Import-Module (Join-Path $PSScriptRoot 'PortableStorage.psm1') -Force

if ($InsidePackage) {
    $plan = Get-Content -LiteralPath $LaunchPlanPath -Raw -Encoding UTF8 | ConvertFrom-Json
    try {
        # The packaged launcher passes an explicit process environment to both application and gateway.
        # 包内启动器为界面与网关显式设置进程环境；不依赖系统环境或原软件的配置指针。
        $currentPackage = [Windows.ApplicationModel.Package, Windows.ApplicationModel, ContentType = WindowsRuntime]::Current
        if ($currentPackage.Id.Name -ne $plan.PackageIdentity) { throw 'The clean launcher did not receive its own package identity.' }
        $environmentValues = @{
            KYNXA_APPLICATION_ROOT = [string]$plan.ApplicationRoot
            KYNXA_DATA_HOME = $null
            KYNXA_MODEL_HOME = $null
            KYNXA_EXTENSION_HOME = $null
            KYNXA_MODEL_API_PORT = [string]$plan.Port
            KYNXA_MODEL_API_URL = [string]$plan.Url
            WEBVIEW2_USER_DATA_FOLDER = (Join-Path $plan.DataHome 'Desktop/WebView2')
            KYNXA_EXTENSION_POINTER = $null
            KYNXA_LEGACY_DESKTOP_HOME = $null
            PATH = (Join-Path $plan.ApplicationRoot 'runtime/native') + ';' + $env:PATH
        }
        foreach ($name in $environmentValues.Keys) {
            [Environment]::SetEnvironmentVariable($name, $environmentValues[$name], 'Process')
        }
        foreach ($directory in @($plan.DataHome, $plan.ModelHome, $plan.ExtensionHome, $environmentValues.WEBVIEW2_USER_DATA_FOLDER)) {
            [IO.Directory]::CreateDirectory($directory) | Out-Null
        }
        $gateway = Start-Process -FilePath (Join-Path $plan.ApplicationRoot 'runtime/agent/kynxa-gateway.exe') `
            -WorkingDirectory $plan.ApplicationRoot -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput (Join-Path $plan.RunRoot 'gateway.stdout.log') `
            -RedirectStandardError (Join-Path $plan.RunRoot 'gateway.stderr.log')
        $deadline = [Diagnostics.Stopwatch]::StartNew()
        $ready = $false
        while ($deadline.Elapsed.TotalSeconds -lt 20) {
            if ($gateway.HasExited) { throw "Clean gateway exited with code $($gateway.ExitCode). See gateway.stderr.log." }
            try {
                $health = Invoke-RestMethod -Uri ($plan.Url + '/health') -TimeoutSec 1
                if ($health.service -eq 'kynxa-model-gateway' -and $health.runtime -eq 'rust' -and $health.status -eq 'ok') {
                    $ready = $true
                    break
                }
            }
            catch { }
            Start-Sleep -Milliseconds 150
        }
        if (-not $ready) { throw 'Clean gateway preparation timed out. See the clean-run gateway logs.' }
        # This is the user's requested visible application; its child services retain this package/environment.
        # 此处打开用户要求的可见软件窗口；后续子服务继承本包身份与独立路径环境。
        $desktop = Start-Process -FilePath $plan.DesktopExecutable -WorkingDirectory $plan.ApplicationRoot -PassThru
        $receipt = @{ started = $true; desktopProcessId = $desktop.Id; gatewayProcessId = $gateway.Id;
            packageIdentity = $plan.PackageIdentity; url = $plan.Url; dataHome = $plan.DataHome;
            modelHome = $plan.ModelHome; extensionHome = $plan.ExtensionHome; applicationRoot = $plan.ApplicationRoot }
        [IO.File]::WriteAllText($plan.ReceiptPath, ($receipt | ConvertTo-Json), $utf8)
    }
    catch {
        [IO.File]::WriteAllText($plan.ReceiptPath, (@{ started = $false; error = $_.Exception.Message } | ConvertTo-Json), $utf8)
        throw
    }
    return
}

if ($env:OS -ne 'Windows_NT' -or -not [Environment]::Is64BitProcess) { throw 'Use 64-bit Windows PowerShell to start this Windows x64 desktop.' }
if ($Port -eq 5218) { throw 'The clean desktop uses a separate port. The existing 5218 gateway is preserved.' }
$windowsPowerShell = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
Get-Command Add-AppxPackage, Get-AppxPackage, Invoke-CommandInDesktopPackage -ErrorAction Stop | Out-Null
[xml]$project = Get-Content -LiteralPath (Join-Path $repositoryRoot 'apps/desktop/KYNXA.Desktop.csproj') -Raw -Encoding UTF8
$framework = $project.SelectSingleNode('/Project/PropertyGroup/TargetFramework').InnerText
$buildRoot = Join-Path $repositoryRoot "apps/desktop/bin/x64/$Configuration/$framework/win-x64"
$applicationRoot = Join-Path $buildRoot 'AppX'
# CLI Build produces a deployment recipe; create its proper loose package using only this fresh output.
# 命令行 Build 生成部署映射；只用本次产物建立正式散装包，整目录移动避免复制数万个运行资源文件。
$buildPrefix = [IO.Path]::GetFullPath($buildRoot).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
$layoutPrefix = [IO.Path]::GetFullPath($applicationRoot).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
if (-not [IO.File]::Exists((Join-Path $applicationRoot 'AppxManifest.xml'))) {
    if (-not [IO.File]::Exists((Join-Path $buildRoot 'AppxManifest.xml'))) {
        throw 'Finish the Full desktop build first; its generated package manifest is missing.'
    }
    [IO.Directory]::CreateDirectory($applicationRoot) | Out-Null
}
$freshItems = @(Get-ChildItem -LiteralPath $buildRoot -Force | Where-Object { $_.Name -notin @('AppX', 'publish') })
foreach ($item in $freshItems) {
    $source = [IO.Path]::GetFullPath($item.FullName)
    $destination = [IO.Path]::GetFullPath((Join-Path $applicationRoot $item.Name))
    if (-not $source.StartsWith($buildPrefix, [StringComparison]::OrdinalIgnoreCase) -or
        -not $destination.StartsWith($layoutPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'The fresh package layout escaped its own build directory.'
    }
    if ([IO.File]::Exists($destination) -or [IO.Directory]::Exists($destination)) {
        throw "The clean layout already contains $($item.Name); preserve both outputs and finish one build before launching."
    }
    Move-Item -LiteralPath $source -Destination $destination
}
$recipePath = Join-Path $applicationRoot 'KYNXA.Desktop.build.appxrecipe'
if ([IO.File]::Exists($recipePath)) {
    [xml]$recipe = Get-Content -LiteralPath $recipePath -Raw -Encoding UTF8
    $recipeNamespaces = [Xml.XmlNamespaceManager]::new($recipe.NameTable)
    $recipeNamespaces.AddNamespace('msbuild', 'http://schemas.microsoft.com/developer/msbuild/2003')
    $repositoryPrefix = $repositoryRoot.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    foreach ($file in $recipe.SelectNodes('/msbuild:Project/msbuild:ItemGroup/msbuild:AppxPackagedFile', $recipeNamespaces)) {
        # MSBuild escapes reserved characters such as @ as %40; decode each recipe field exactly once.
        # MSBuild 把 @ 等保留字符编码为 %40；映射字段仅解码一次，再执行路径边界校验。
        $relativePath = [Uri]::UnescapeDataString($file.SelectSingleNode('msbuild:PackagePath', $recipeNamespaces).InnerText)
        $destination = [IO.Path]::GetFullPath((Join-Path $applicationRoot $relativePath))
        if (-not $destination.StartsWith($layoutPrefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'A package recipe path escaped the clean AppX layout.'
        }
        if ([IO.File]::Exists($destination)) { continue }
        $source = [IO.Path]::GetFullPath([Uri]::UnescapeDataString($file.GetAttribute('Include')))
        if (-not $source.StartsWith($repositoryPrefix, [StringComparison]::OrdinalIgnoreCase) -or -not [IO.File]::Exists($source)) {
            throw "The fresh deployment recipe references a missing or external source: $relativePath"
        }
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination)) | Out-Null
        Copy-Item -LiteralPath $source -Destination $destination
    }
}
$manifestPath = Join-Path $applicationRoot 'AppxManifest.xml'
if (-not [IO.File]::Exists($manifestPath)) { throw "Finish the Full desktop build first. The generated package manifest is missing: $manifestPath" }

$requiredFiles = @('KYNXA.Desktop.exe', 'runtime/agent/kynxa-gateway.exe',
    'runtime/resource/kynxa-resource-service.exe', 'runtime/native/vec0.dll', 'runtime/native/onnxruntime.dll',
    'ToolHost/KYNXA.ToolHost.exe', 'runtime/embedding/builtin-multilingual/onnx/model_quantized.onnx',
    'runtime/rerank/builtin-multilingual-reranker/onnx/model_quantized.onnx')
$mcp = Get-Content -LiteralPath (Join-Path $repositoryRoot 'apps/desktop/Build/native-assets/mcp-runtime.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$requiredFiles += @($mcp.node.path, $mcp.browser.path)
$requiredFiles += @($mcp.services | ForEach-Object { $_.entry })
foreach ($relativePath in $requiredFiles) {
    if (-not [IO.File]::Exists((Join-Path $applicationRoot $relativePath))) {
        throw "Full package payload is missing: $relativePath. Run tools/development/build-clean.ps1 first."
    }
}

# Never attach to another gateway; reuse this software's fixed data and local selections.
# 不接入其他网关；重复启动复用本软件的固定数据目录和本地路径选择。
$probe = [Net.Sockets.TcpClient]::new()
$occupied = $false
try { $occupied = $probe.ConnectAsync('127.0.0.1', $Port).Wait(300) -and $probe.Connected }
catch [AggregateException] { }
finally { $probe.Dispose() }
if ($occupied) { throw "Port $Port is occupied; the existing process was preserved. Choose another clean port." }

$hasher = [Security.Cryptography.SHA256]::Create()
try { $identitySuffix = [BitConverter]::ToString($hasher.ComputeHash([Text.Encoding]::UTF8.GetBytes($repositoryRoot.ToLowerInvariant()))).Replace('-', '').Substring(0, 12).ToLowerInvariant() }
finally { $hasher.Dispose() }
$packageIdentity = 'KYNXA.CleanPreview.' + $identitySuffix
[xml]$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8
$namespaces = [Xml.XmlNamespaceManager]::new($manifest.NameTable)
$namespaces.AddNamespace('pkg', 'http://schemas.microsoft.com/appx/manifest/foundation/windows10')
$identity = $manifest.SelectSingleNode('/pkg:Package/pkg:Identity', $namespaces)
$application = $manifest.SelectSingleNode('/pkg:Package/pkg:Applications/pkg:Application', $namespaces)
if ($null -eq $identity -or $null -eq $application) { throw 'The generated package manifest has no desktop identity/application.' }
$identity.SetAttribute('Name', $packageIdentity)
$settings = [Xml.XmlWriterSettings]::new()
$settings.Encoding = $utf8
$settings.Indent = $true
$settings.NewLineChars = "`r`n"
$writer = [Xml.XmlWriter]::Create($manifestPath, $settings)
try { $manifest.Save($writer) }
finally { $writer.Dispose() }

# Preserve all generated framework dependencies; available framework packages come only from this fresh restore.
# 保留生成清单的全部框架依赖；可用框架安装包只取本次新还原目录，不读取原环境的包缓存。
$windowsAppSdkReference = $project.SelectSingleNode('/Project/ItemGroup/PackageReference[@Include="Microsoft.WindowsAppSDK"]')
$runtimePackageRoot = Join-Path $repositoryRoot ('artifacts/clean-build/nuget/packages/microsoft.windowsappsdk.runtime/' + $windowsAppSdkReference.GetAttribute('Version') + '/tools/MSIX/win10-x64')
foreach ($dependency in $manifest.SelectNodes('/pkg:Package/pkg:Dependencies/pkg:PackageDependency', $namespaces)) {
    $dependencyName = $dependency.GetAttribute('Name')
    $minimumVersion = [version]$dependency.GetAttribute('MinVersion')
    $installed = @(Get-AppxPackage -Name $dependencyName | Where-Object { [version]$_.Version -ge $minimumVersion -and $_.Architecture -in @('X64', 'Neutral') })
    if ($installed.Count -gt 0) { continue }
    $dependencyPath = Join-Path $runtimePackageRoot ($dependencyName + '.msix')
    if (-not [IO.File]::Exists($dependencyPath)) { throw "Install the required desktop framework: $dependencyName $minimumVersion" }
    Add-AppxPackage -Path $dependencyPath
}
# Registration changes only the generated clean identity, never the source manifest or old application.
# 只注册生成的独立身份，不修改源码清单，也不替换或关闭原软件。
# Register the new loose layout as a development package, not as an already installed signed package.
# 新布局按开发包注册，不将其误当作已经安装并签名的正式包。
Add-AppxPackage -Register -Path $manifestPath
$package = Get-AppxPackage -Name $packageIdentity | Select-Object -First 1
if ($null -eq $package) { throw 'The clean package could not be registered.' }
$runRoot = Join-Path $repositoryRoot 'artifacts/clean-run/current'
[IO.Directory]::CreateDirectory($runRoot) | Out-Null
$dataHome = Get-KynxaPortablePath -ApplicationRoot $applicationRoot -PointerName 'storage.json' -PropertyName 'dataRoot' -DefaultPath 'Data'
$extensionHome = Get-KynxaPortablePath -ApplicationRoot $applicationRoot -PointerName 'extensions.json' -PropertyName 'extensionRoot' -DefaultPath 'Extensions'
$planPath = Join-Path $runRoot 'launch-plan.json'
$receiptPath = Join-Path $runRoot 'launch-result.json'
if ([IO.File]::Exists($receiptPath)) { Remove-Item -LiteralPath $receiptPath }
$plan = @{ PackageIdentity = $packageIdentity; ApplicationRoot = $applicationRoot; RunRoot = $runRoot;
    DataHome = $dataHome; ModelHome = (Join-Path $dataHome 'Models'); ExtensionHome = $extensionHome;
    Port = $Port; Url = "http://127.0.0.1:$Port"; ReceiptPath = $receiptPath;
    DesktopExecutable = (Join-Path $applicationRoot $application.GetAttribute('Executable')) }
[IO.File]::WriteAllText($planPath, ($plan | ConvertTo-Json), $utf8)
$arguments = '-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + $PSCommandPath + '" -InsidePackage -LaunchPlanPath "' + $planPath + '"'
Invoke-CommandInDesktopPackage -PackageFamilyName $package.PackageFamilyName -AppId $application.GetAttribute('Id') `
    -Command $windowsPowerShell -Args $arguments -PreventBreakaway
$waiting = [Diagnostics.Stopwatch]::StartNew()
while (-not [IO.File]::Exists($receiptPath) -and $waiting.Elapsed.TotalSeconds -lt 30) { Start-Sleep -Milliseconds 150 }
if (-not [IO.File]::Exists($receiptPath)) { throw "The package launcher did not report completion. Inspect $runRoot." }
$receipt = Get-Content -LiteralPath $receiptPath -Raw -Encoding UTF8 | ConvertFrom-Json
if (-not $receipt.started) { throw $receipt.error }
$receipt
