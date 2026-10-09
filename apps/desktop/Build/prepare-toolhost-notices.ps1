[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$AssetsPath,
    [Parameter(Mandatory = $true)][string]$RuntimeIdentifier,
    [Parameter(Mandatory = $true)][string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Use the framework hash API in MSBuild's PowerShell host, which may not load Get-FileHash's script module.
# 使用框架哈希 API；MSBuild 的 PowerShell 宿主可能没有加载 Get-FileHash 所属脚本模块。
function Get-NoticeHash([string]$Path) {
    $noticeStream = [IO.File]::OpenRead([IO.Path]::GetFullPath($Path))
    $noticeHasher = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($noticeHasher.ComputeHash($noticeStream)).Replace('-', '').ToLowerInvariant() }
    finally { $noticeHasher.Dispose(); $noticeStream.Dispose() }
}
# Resolve exactly the runtime packs selected by ToolHost restore, rather than a
# developer's SDK folder or a guessed pack version. Preserve the original text.
# 只解析 ToolHost 还原实际选中的运行时包，不使用开发机 SDK 目录或猜测的版本；保留原始许可文本。
$runtimeAssets = Get-Content -LiteralPath $AssetsPath -Raw -Encoding UTF8 | ConvertFrom-Json
$runtimePackNames = @("Microsoft.NETCore.App.Runtime.$RuntimeIdentifier", "Microsoft.WindowsDesktop.App.Runtime.$RuntimeIdentifier", 'Microsoft.Windows.SDK.NET.Ref')
$runtimeNoticesOutput = [IO.Path]::GetFullPath($OutputDirectory)
[IO.Directory]::CreateDirectory($runtimeNoticesOutput) | Out-Null
$copiedRuntimePacks = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($framework in $runtimeAssets.project.frameworks.PSObject.Properties) {
    foreach ($dependency in $framework.Value.downloadDependencies) {
        if ($runtimePackNames -notcontains $dependency.name -or $copiedRuntimePacks.Contains($dependency.name)) { continue }
        if ($dependency.version -notmatch '^\[(\d+(?:\.\d+){2,3}),\s*\1\]$') { throw 'ToolHost runtime pack must resolve to an exact version before licensing can be bundled.' }
        $runtimePackVersion = $Matches[1]
        $runtimePackDirectory = $null
        foreach ($folder in $runtimeAssets.packageFolders.PSObject.Properties) {
            $candidate = Join-Path $folder.Name ($dependency.name.ToLowerInvariant() + '/' + $runtimePackVersion)
            if (Test-Path -LiteralPath $candidate -PathType Container) { $runtimePackDirectory = $candidate; break }
        }
        if (-not $runtimePackDirectory) { throw "Restored ToolHost runtime pack not found: $($dependency.name) $runtimePackVersion" }
        $runtimeNotices = @(Get-ChildItem -LiteralPath $runtimePackDirectory -File | Where-Object Name -Match '^(LICENSE(\.TXT)?|THIRD-PARTY-NOTICES\.TXT|ThirdPartyNotices\.txt|NOTICE(\.TXT)?)$')
        if ($dependency.name -eq 'Microsoft.Windows.SDK.NET.Ref') {
            # The SDK targeting pack declares its license by URL; bundle verified official original bytes and provenance.
            # SDK 目标包仅声明许可网址；随包保留已校验的官方原文和来源，不猜其他版本的许可。
            $winRtSourcesDirectory = Join-Path $PSScriptRoot 'licenses'
            $winRtSourcesPath = Join-Path $winRtSourcesDirectory 'toolhost-winrt-license-sources.json'
            $winRtSources = Get-Content -LiteralPath $winRtSourcesPath -Raw -Encoding UTF8 | ConvertFrom-Json
            if ($winRtSources.schemaVersion -ne 1 -or $winRtSources.files.Count -ne 2) { throw 'ToolHost WinRT license manifest is invalid.' }
            $sdkSource = $winRtSources.files | Where-Object component -EQ 'Microsoft.Windows.SDK.NET.Ref'
            $winRtSource = $winRtSources.files | Where-Object component -EQ 'WinRT.Runtime'
            if (-not $sdkSource -or $sdkSource.componentVersion -ne $runtimePackVersion -or -not $winRtSource) { throw 'ToolHost SDK license provenance does not match the selected pack.' }
            $restoredWinRt = Join-Path $runtimePackDirectory 'lib/net8.0/WinRT.Runtime.dll'
            $restoredProductVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($restoredWinRt).ProductVersion
            if ($restoredProductVersion -ne ($winRtSource.componentVersion + '+' + $winRtSource.sourceCommit)) { throw 'ToolHost WinRT license provenance does not match the restored runtime.' }
            $noticeOutputDirectory = Join-Path $runtimeNoticesOutput ($dependency.name + '/' + $runtimePackVersion)
            [IO.Directory]::CreateDirectory($noticeOutputDirectory) | Out-Null
            foreach ($source in $winRtSources.files) {
                if ($source.file -notmatch '^[a-zA-Z0-9._-]+$' -or $source.sha256 -notmatch '^[a-f0-9]{64}$') { throw 'ToolHost WinRT license identity is invalid.' }
                $noticePath = Join-Path $winRtSourcesDirectory $source.file
                $noticeInfo = Get-Item -LiteralPath $noticePath
                if ($noticeInfo.Length -ne $source.bytes -or (Get-NoticeHash $noticePath) -ne $source.sha256) { throw 'ToolHost WinRT license bytes do not match their pinned identity.' }
                Copy-Item -LiteralPath $noticePath -Destination $noticeOutputDirectory -Force
            }
            Copy-Item -LiteralPath $winRtSourcesPath -Destination $noticeOutputDirectory -Force
            Copy-Item -LiteralPath (Join-Path $runtimePackDirectory 'microsoft.windows.sdk.net.ref.nuspec') -Destination $noticeOutputDirectory -Force
            $copiedRuntimePacks.Add($dependency.name) | Out-Null
            continue
        }
        if (-not @($runtimeNotices | Where-Object Name -Match '^LICENSE').Count) { throw "ToolHost runtime pack has no original license: $($dependency.name)" }
        if ($dependency.name.StartsWith('Microsoft.NETCore.') -and -not @($runtimeNotices | Where-Object Name -Match 'NOTICES').Count) { throw 'The .NET runtime third-party notices are missing.' }
        $noticeOutputDirectory = Join-Path $runtimeNoticesOutput ($dependency.name + '/' + $runtimePackVersion)
        [IO.Directory]::CreateDirectory($noticeOutputDirectory) | Out-Null
        foreach ($notice in $runtimeNotices) { Copy-Item -LiteralPath $notice.FullName -Destination $noticeOutputDirectory -Force }
        $copiedRuntimePacks.Add($dependency.name) | Out-Null
    }
}
if ($copiedRuntimePacks.Count -ne 3) { throw 'ToolHost restore did not resolve all required runtime and SDK license sources.' }
Write-Output 'Copied original .NET, WindowsDesktop, Windows SDK and C#/WinRT licenses and available third-party notices.'
