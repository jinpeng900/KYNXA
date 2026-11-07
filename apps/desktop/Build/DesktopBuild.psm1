Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'AtomicFiles.psm1') -Force
$script:buildUtf8 = [Text.UTF8Encoding]::new($false)

function Read-KynxaBuildJson {
    param([string]$Path)
    $json = [IO.File]::ReadAllText($Path, $script:buildUtf8)
    if ($PSVersionTable.PSVersion.Major -ge 6) { return ($json | ConvertFrom-Json -AsHashtable) }
    Add-Type -AssemblyName System.Web.Extensions
    $reader = [Web.Script.Serialization.JavaScriptSerializer]::new()
    $reader.MaxJsonLength = 64 * 1024 * 1024
    return $reader.DeserializeObject($json)
}

function Read-KynxaBuildState {
    param([string]$Path)
    if (-not [IO.File]::Exists($Path)) { return @{ schemaVersion = 1; steps = @{} } }
    $state = Read-KynxaBuildJson $Path
    if ($state.schemaVersion -ne 1) { throw "Unsupported build state: $Path" }
    return $state
}

function Save-KynxaBuildState {
    param([string]$Path, [Collections.IDictionary]$State)
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path)) | Out-Null
    $pending = $Path + '.pending'
    [IO.File]::WriteAllText($pending, ($State | ConvertTo-Json -Depth 16), $script:buildUtf8)
    Move-KynxaAtomicFile -Source $pending -Destination $Path
}

function Get-KynxaBuildSignature {
    param([string]$RepositoryRoot, [string[]]$Directories, [string[]]$Files, [string[]]$Values)
    $root = [IO.Path]::GetFullPath($RepositoryRoot).TrimEnd('\', '/')
    $entries = [Collections.Generic.List[string]]::new()
    $excluded = @('bin', 'obj', 'target', 'node_modules', '.git', 'Data', 'Extensions', 'artifacts')
    $pending = [Collections.Generic.Stack[string]]::new()
    foreach ($relative in $Directories) {
        $directory = Join-Path $root $relative
        if ([IO.Directory]::Exists($directory)) { $pending.Push($directory) }
        else { $entries.Add('missing:' + $relative) }
    }
    while ($pending.Count) {
        $directory = $pending.Pop()
        foreach ($path in [IO.Directory]::EnumerateFiles($directory)) {
            $file = [IO.FileInfo]::new($path)
            $entries.Add($path.Substring($root.Length + 1) + '|' + $file.Length + '|' + $file.LastWriteTimeUtc.Ticks)
        }
        foreach ($path in [IO.Directory]::EnumerateDirectories($directory)) {
            if ([IO.Path]::GetFileName($path) -in $excluded) { continue }
            if ([IO.File]::GetAttributes($path) -band [IO.FileAttributes]::ReparsePoint) { continue }
            $pending.Push($path)
        }
    }
    foreach ($relative in $Files) {
        $path = if ([IO.Path]::IsPathRooted($relative)) { $relative } else { Join-Path $root $relative }
        $file = [IO.FileInfo]::new($path)
        $entries.Add($relative + '|' + $(if ($file.Exists) { [string]$file.Length + '|' + $file.LastWriteTimeUtc.Ticks } else { 'missing' }))
    }
    $entries.Sort([StringComparer]::Ordinal)
    # 比较源码元信息与选项；每次 Run 不重读模型、资源归档或编译器缓存。
    $text = ($entries -join "`n") + "`n" + ($Values -join "`n")
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($hasher.ComputeHash($script:buildUtf8.GetBytes($text))).Replace('-', '').ToLowerInvariant() }
    finally { $hasher.Dispose() }
}

function Invoke-KynxaBuildStage {
    param([int]$Number, [string]$Name, [string]$Key, [Collections.IDictionary]$State, [string]$StatePath, [scriptblock]$Operation)
    Write-Host "[步骤 $Number/6] $Name"
    $State.steps[$Key] = @{ status = 'running'; startedAt = [DateTime]::UtcNow.ToString('o') }
    Save-KynxaBuildState $StatePath $State
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try {
        $result = & $Operation
        $State.steps[$Key] = @{ status = 'ready'; completedAt = [DateTime]::UtcNow.ToString('o'); elapsedSeconds = [Math]::Round($watch.Elapsed.TotalSeconds, 2) }
        Save-KynxaBuildState $StatePath $State
        return $result
    }
    catch {
        $State.steps[$Key] = @{ status = 'failed'; message = $_.Exception.Message; failedAt = [DateTime]::UtcNow.ToString('o') }
        Save-KynxaBuildState $StatePath $State
        throw
    }
}

function ConvertTo-KynxaProcessArgument {
    param([AllowEmptyString()][string]$Value)
    if ($Value.Length -gt 0 -and $Value -notmatch '[\s"]') { return $Value }
    return '"' + (($Value -replace '(\\*)"', '$1$1\"') -replace '(\\+)$', '$1$1') + '"'
}

function Invoke-KynxaBuildCommand {
    param([string]$FilePath, [string[]]$Arguments, [string]$WorkingDirectory, [string]$LogPath, [string]$Activity)
    $start = [Diagnostics.ProcessStartInfo]::new($FilePath)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    # 按子进程的 PowerShell 版本重建模块路径，兼容 MSBuild 嵌套调用。
    # 仅清除子进程继承的路径，保留调用者环境和缓存。
    $start.EnvironmentVariables.Remove('PSModulePath')
    $start.WorkingDirectory = $WorkingDirectory
    $start.Arguments = (@($Arguments | ForEach-Object { ConvertTo-KynxaProcessArgument $_ }) -join ' ')
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = $script:buildUtf8
    $start.StandardErrorEncoding = $script:buildUtf8
    $process = [Diagnostics.Process]::Start($start)
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $lastNotice = [DateTime]::UtcNow
    $recent = [Collections.Generic.Queue[string]]::new()
    $stdout = $process.StandardOutput.ReadLineAsync()
    $stderr = $process.StandardError.ReadLineAsync()
    $commandLog = [IO.Path]::ChangeExtension($LogPath, 'components.log')
    $writer = [IO.StreamWriter]::new($commandLog, $true, $script:buildUtf8)
    $writer.AutoFlush = $true
    try {
        while ($null -ne $stdout -or $null -ne $stderr -or -not $process.HasExited) {
            foreach ($stream in @('stdout', 'stderr')) {
                $task = Get-Variable -Name $stream -ValueOnly
                if ($null -eq $task -or -not $task.IsCompleted) { continue }
                $line = $task.GetAwaiter().GetResult()
                if ($null -eq $line) { Set-Variable -Name $stream -Value $null; continue }
                Write-Host $line
                $writer.WriteLine($line)
                $recent.Enqueue($line)
                if ($recent.Count -gt 18) { [void]$recent.Dequeue() }
                $reader = if ($stream -eq 'stdout') { $process.StandardOutput } else { $process.StandardError }
                Set-Variable -Name $stream -Value ($reader.ReadLineAsync())
            }
            if (([DateTime]::UtcNow - $lastNotice).TotalSeconds -ge 15) {
                $last = if ($recent.Count) { @($recent)[-1] } else { '等待组件输出' }
                Write-Host ("[进行中] {0}，已耗时 {1:N0} 秒；最近活动：{2}" -f $Activity, $watch.Elapsed.TotalSeconds, $last)
                $lastNotice = [DateTime]::UtcNow
            }
            if (-not $process.HasExited) { Start-Sleep -Milliseconds 40 }
        }
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) {
            throw ("{0}失败，退出码 {1}。`n{2}`n日志：{3}" -f $Activity, $process.ExitCode, (@($recent) -join "`n"), $commandLog)
        }
    }
    finally {
        # 中断构建时仅清理本次持有的子进程树，再释放项目构建锁；已完成缓存保留。
        if (-not $process.HasExited) {
            & (Join-Path $env:SystemRoot 'System32/taskkill.exe') /PID $process.Id /T /F | Out-Null
        }
        $writer.Dispose()
        $process.Dispose()
    }
}

function Test-KynxaNuGetAssets {
    param([string]$AssetsPath)
    if (-not [IO.File]::Exists($AssetsPath)) { return $false }
    try {
        $assets = Read-KynxaBuildJson $AssetsPath
        $locations = @{}
        foreach ($library in $assets.libraries.GetEnumerator()) {
            if ($library.Value.type -ne 'package') { continue }
            $found = $false
            foreach ($folder in $assets.packageFolders.Keys) {
                $path = Join-Path $folder $library.Value.path
                if ([IO.File]::Exists((Join-Path $path '.nupkg.metadata'))) { $locations[$library.Key] = $path; $found = $true; break }
            }
            if (-not $found) { return $false }
        }
        foreach ($target in $assets.targets.Values) {
            foreach ($library in $target.GetEnumerator()) {
                if (-not $locations.ContainsKey($library.Key)) { continue }
                foreach ($group in @('compile','runtime','native','build','buildTransitive')) {
                    if (-not $library.Value.ContainsKey($group)) { continue }
                    foreach ($relative in $library.Value[$group].Keys) {
                        if ($relative -eq '_._' -or $relative.EndsWith('/_._')) { continue }
                        if (-not [IO.File]::Exists((Join-Path $locations[$library.Key] $relative))) { return $false }
                    }
                }
            }
        }
        return $assets.targets.Count -gt 0
    }
    catch { return $false }
}

function Get-KynxaDesktopLayout {
    param([string]$RepositoryRoot, [string]$Configuration)
    [xml]$project = Get-Content -LiteralPath (Join-Path $RepositoryRoot 'apps/desktop/KYNXA.Desktop.csproj') -Raw -Encoding UTF8
    $framework = $project.SelectSingleNode('/Project/PropertyGroup/TargetFramework').InnerText
    $output = Join-Path $RepositoryRoot "apps/desktop/bin/x64/$Configuration/$framework/win-x64"
    $recipe = Join-Path $output 'KYNXA.Desktop.build.appxrecipe'
    if (-not [IO.File]::Exists($recipe)) { throw "Desktop build recipe missing: $recipe" }
    [xml]$document = Get-Content -LiteralPath $recipe -Raw -Encoding UTF8
    $layoutNode = $document.SelectSingleNode('//*[local-name()="LayoutDir"]')
    $layout = if ($layoutNode -and $layoutNode.InnerText) { $layoutNode.InnerText } else { Join-Path $RepositoryRoot "artifacts/development/appx/x64/$Configuration" }
    return @{ output = $output; recipe = $recipe; applicationRoot = [IO.Path]::GetFullPath($layout); intermediate = (Join-Path $RepositoryRoot "apps/desktop/obj/x64/$Configuration/$framework/win-x64") }
}

function Update-KynxaDesktopLayout {
    param([string]$RepositoryRoot, [Collections.IDictionary]$Layout)
    [xml]$recipe = Get-Content -LiteralPath $Layout.recipe -Raw -Encoding UTF8
    [xml]$manifest = Get-Content -LiteralPath (Join-Path $Layout.output 'AppxManifest.xml') -Raw -Encoding UTF8
    $identity = $manifest.SelectSingleNode('//*[local-name()="Identity"]')
    $installed = Get-AppxPackage -Name $identity.Name | Select-Object -First 1
    $installedLocation = if ($installed) { [string]$installed.InstallLocation } else { '' }
    # 复用本项目已有开发包注册目录；不覆盖其他项目副本或正式安装包。
    # 已删除源码可能留下无安装路径的注册信息；转用独立开发身份，不复用失效目录。
    if ($installed -and $installed.IsDevelopmentMode -and -not [string]::IsNullOrWhiteSpace($installedLocation) -and
        $installedLocation.StartsWith($RepositoryRoot.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) {
        $Layout.applicationRoot = $installedLocation
    }
    elseif ($installed) {
        $name = 'KYNXA.Dev.' + (Get-KynxaBuildSignature $RepositoryRoot @() @() @($RepositoryRoot)).Substring(0, 16)
        $identity.SetAttribute('Name', $name)
    }
    $applicationRoot = [IO.Path]::GetFullPath($Layout.applicationRoot)
    [IO.Directory]::CreateDirectory($applicationRoot) | Out-Null
    foreach ($process in @(Get-Process -Name KYNXA.Desktop -ErrorAction SilentlyContinue)) {
        if ($process.Path -and $process.Path.StartsWith($applicationRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
            [void]$process.CloseMainWindow()
            # A window hidden in the tray ignores the close request; this is a local development build, so stop it directly.
            # 隐藏到托盘的窗口不响应关闭请求；这是本机开发版，直接结束。
            if (-not $process.WaitForExit(5000)) { Stop-Process -Id $process.Id -Force }
        }
    }
    foreach ($process in @(Get-Process -Name kynxa-gateway -ErrorAction SilentlyContinue)) {
        if ($process.Path -and $process.Path.StartsWith($applicationRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
            & (Join-Path $env:SystemRoot 'System32/taskkill.exe') /PID $process.Id /T /F | Out-Null
            if (-not $process.WaitForExit(5000)) { throw 'The previous development gateway could not be stopped for its update.' }
        }
    }
    $files = @($recipe.SelectNodes('//*[local-name()="AppxPackagedFile"]'))
    $copy = [Collections.Generic.List[object]]::new()
    foreach ($file in $files) {
        $packagePath = $file.SelectSingleNode('*[local-name()="PackagePath"]')
        $relative = if ($packagePath) { [Uri]::UnescapeDataString($packagePath.InnerText) } else { '' }
        if (-not $relative -or $relative -match '^(Data|Extensions)[\\/]' -or $relative -in @('storage.json','extensions.json')) { continue }
        $target = [IO.Path]::GetFullPath((Join-Path $applicationRoot $relative))
        if (-not $target.StartsWith($applicationRoot.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Package file escaped the application layout.' }
        $source = [Uri]::UnescapeDataString($file.GetAttribute('Include'))
        $copy.Add(@($source, $target))
    }
    # 用 .NET 批量比较并复制变更文件，避免对数万资源反复执行 PowerShell 文件系统命令。
    if (-not ('Kynxa.Build.LayoutCopy' -as [type])) {
        Add-Type -TypeDefinition @'
namespace Kynxa.Build {
    public static class LayoutCopy {
        public static void Copy(string[][] files) {
            foreach (var pair in files) {
                var source = new System.IO.FileInfo(pair[0]);
                var target = new System.IO.FileInfo(pair[1]);
                if (target.Exists && source.Length == target.Length && source.LastWriteTimeUtc == target.LastWriteTimeUtc) continue;
                System.IO.Directory.CreateDirectory(target.DirectoryName);
                source.CopyTo(target.FullName, true);
            }
        }
    }
}
'@
    }
    [Kynxa.Build.LayoutCopy]::Copy([string[][]]$copy.ToArray())
    # 早期自包含布局留下的运行时文件会遮挡框架包，导致 WinUI 启动崩溃。
    $expected = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($pair in $copy) { [void]$expected.Add([IO.Path]::GetFileName($pair[1])) }
    foreach ($file in [IO.Directory]::GetFiles($applicationRoot)) {
        $name = [IO.Path]::GetFileName($file)
        if ($name -match '\.(dll|winmd|pri)$|^workloads\..*json$|^RestartAgent\.exe$' -and -not $expected.Contains($name)) { [IO.File]::Delete($file) }
    }
    $embed = Join-Path $Layout.intermediate 'embed'
    if ([IO.Directory]::Exists($embed)) {
        foreach ($path in [IO.Directory]::EnumerateFiles($embed, '*.xbf', 'AllDirectories')) {
            $target = Join-Path $applicationRoot $path.Substring($embed.Length + 1)
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($target)) | Out-Null
            [IO.File]::Copy($path, $target, $true)
        }
    }
    $manifest.Save((Join-Path $applicationRoot 'AppxManifest.xml'))
    return $Layout
}

function Start-KynxaDesktopBuild {
    param([string]$RepositoryRoot, [Collections.IDictionary]$Layout, [string]$StateDirectory)
    $root = $Layout.applicationRoot
    $existing = @(Get-Process -Name KYNXA.Desktop -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq (Join-Path $root 'KYNXA.Desktop.exe') })
    if ($existing.Count) { Write-Host "软件已运行，进程 $($existing[0].Id)。"; return }
    [xml]$manifest = Get-Content -LiteralPath (Join-Path $root 'AppxManifest.xml') -Raw -Encoding UTF8
    $identity = $manifest.SelectSingleNode('//*[local-name()="Identity"]')
    foreach ($dependency in $manifest.SelectNodes('//*[local-name()="PackageDependency"]')) {
        $available = @(Get-AppxPackage -Name $dependency.Name | Where-Object { [version]$_.Version -ge [version]$dependency.MinVersion })
        if ($available.Count) { continue }
        $assets = Get-Content -LiteralPath (Join-Path $RepositoryRoot 'apps/desktop/obj/project.assets.json') -Raw -Encoding UTF8 | ConvertFrom-Json
        $candidate = $null
        foreach ($folder in $assets.packageFolders.PSObject.Properties) {
            foreach ($library in $assets.libraries.PSObject.Properties | Where-Object Name -Like 'Microsoft.WindowsAppSDK.Runtime/*') {
                $path = Join-Path $folder.Name ($library.Value.path + '/tools/MSIX/win10-x64/' + $dependency.Name + '.msix')
                if ([IO.File]::Exists($path)) { $candidate = $path; break }
            }
        }
        if (-not $candidate) { throw "Desktop runtime missing: $($dependency.Name) $($dependency.MinVersion). Run Setup to restore its package." }
        Add-AppxPackage -Path $candidate
    }
    $package = Get-AppxPackage -Name $identity.Name | Select-Object -First 1
    # 开发版相同版本不能重复注册，依赖丢失时须先解除注册，并保留数据。
    if ($package -and $manifest.SelectNodes('//*[local-name()="PackageDependency"]').Count -and -not @($package.Dependencies).Count) {
        Remove-AppxPackage -Package $package.PackageFullName -PreserveApplicationData
        $package = $null
    }
    # 目录相同仍可能保留旧包依赖图；版本变化须更新注册，避免更新后 WinRT 类激活失败。
    if (-not $package -or $package.InstallLocation -ne $root -or [version]$package.Version -ne [version]$identity.Version) {
        Add-AppxPackage -Register -Path (Join-Path $root 'AppxManifest.xml')
        $package = Get-AppxPackage -Name $identity.Name | Select-Object -First 1
        if (-not $package -or $package.InstallLocation -ne $root -or [version]$package.Version -ne [version]$identity.Version) { throw 'Windows package registration does not match the application directory and version.' }
    }
    $plan = @{ applicationRoot = $root; packageIdentity = $identity.Name; receipt = (Join-Path $StateDirectory 'launch.json') }
    $planPath = Join-Path $StateDirectory 'launch-plan.json'
    [IO.File]::WriteAllText($planPath, ($plan | ConvertTo-Json), $script:buildUtf8)
    if ([IO.File]::Exists($plan.receipt)) { [IO.File]::Delete($plan.receipt) }
    $helper = Join-Path $PSScriptRoot 'LaunchDesktop.ps1'
    $arguments = '-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File ' + (ConvertTo-KynxaProcessArgument $helper) + ' -PlanPath ' + (ConvertTo-KynxaProcessArgument $planPath)
    Invoke-CommandInDesktopPackage -PackageFamilyName $package.PackageFamilyName -AppId App `
        -Command (Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe') -Args $arguments
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    while (-not [IO.File]::Exists($plan.receipt) -and $deadline.Elapsed.TotalSeconds -lt 20) { Start-Sleep -Milliseconds 100 }
    if (-not [IO.File]::Exists($plan.receipt)) { throw "Desktop launch did not complete. See $StateDirectory." }
    $receipt = Get-Content -LiteralPath $plan.receipt -Raw -Encoding UTF8 | ConvertFrom-Json
    if (-not $receipt.started) { throw $receipt.error }
    # 进程启动不等于软件可用，须等到窗口出现。
    $deadline.Restart()
    while ($deadline.Elapsed.TotalSeconds -lt 30) {
        $process = Get-Process -Id $receipt.processId -ErrorAction SilentlyContinue
        if (-not $process) { throw "软件启动后立即退出（进程 $($receipt.processId)）。请查看 Windows 事件查看器的应用程序日志。" }
        if ($process.MainWindowHandle -ne [IntPtr]::Zero) { Write-Host "软件已启动，进程 $($receipt.processId)。"; return }
        Start-Sleep -Milliseconds 200
    }
    throw "软件进程仍在运行，但 30 秒内没有出现窗口（进程 $($receipt.processId)）。"
}

Export-ModuleMember -Function Read-KynxaBuildState, Save-KynxaBuildState, Get-KynxaBuildSignature, Invoke-KynxaBuildStage, Invoke-KynxaBuildCommand, Test-KynxaNuGetAssets, Get-KynxaDesktopLayout, Update-KynxaDesktopLayout, Start-KynxaDesktopBuild
