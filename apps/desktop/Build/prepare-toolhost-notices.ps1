[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$AssetsPath,
    [Parameter(Mandatory = $true)][string]$RuntimeIdentifier,
    [Parameter(Mandatory = $true)][string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# Resolve exactly the runtime packs selected by ToolHost restore, rather than a
# developer's SDK folder or a guessed pack version. Preserve the original text.
# 只解析 ToolHost 还原实际选中的运行时包，不使用开发机 SDK 目录或猜测的版本；保留原始许可文本。
$runtimeAssets = Get-Content -LiteralPath $AssetsPath -Raw -Encoding UTF8 | ConvertFrom-Json
$runtimePackNames = @("Microsoft.NETCore.App.Runtime.$RuntimeIdentifier", "Microsoft.WindowsDesktop.App.Runtime.$RuntimeIdentifier")
$runtimeNoticesOutput = [IO.Path]::GetFullPath($OutputDirectory)
[IO.Directory]::CreateDirectory($runtimeNoticesOutput) | Out-Null
$copiedRuntimePacks = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($framework in $runtimeAssets.project.frameworks.PSObject.Properties) {
    foreach ($dependency in $framework.Value.downloadDependencies) {
        if ($runtimePackNames -notcontains $dependency.name -or $copiedRuntimePacks.Contains($dependency.name)) { continue }
        if ($dependency.version -notmatch '^\[(\d+\.\d+\.\d+),\s*\1\]$') { throw 'ToolHost runtime pack must resolve to an exact version before licensing can be bundled.' }
        $runtimePackVersion = $Matches[1]
        $runtimePackDirectory = $null
        foreach ($folder in $runtimeAssets.packageFolders.PSObject.Properties) {
            $candidate = Join-Path $folder.Name ($dependency.name.ToLowerInvariant() + '/' + $runtimePackVersion)
            if (Test-Path -LiteralPath $candidate -PathType Container) { $runtimePackDirectory = $candidate; break }
        }
        if (-not $runtimePackDirectory) { throw "Restored ToolHost runtime pack not found: $($dependency.name) $runtimePackVersion" }
        $runtimeNotices = @(Get-ChildItem -LiteralPath $runtimePackDirectory -File | Where-Object Name -Match '^(LICENSE(\.TXT)?|THIRD-PARTY-NOTICES\.TXT|ThirdPartyNotices\.txt|NOTICE(\.TXT)?)$')
        if (-not @($runtimeNotices | Where-Object Name -Match '^LICENSE').Count) { throw "ToolHost runtime pack has no original license: $($dependency.name)" }
        if ($dependency.name.StartsWith('Microsoft.NETCore.') -and -not @($runtimeNotices | Where-Object Name -Match 'NOTICES').Count) { throw 'The .NET runtime third-party notices are missing.' }
        $noticeOutputDirectory = Join-Path $runtimeNoticesOutput ($dependency.name + '/' + $runtimePackVersion)
        [IO.Directory]::CreateDirectory($noticeOutputDirectory) | Out-Null
        foreach ($notice in $runtimeNotices) { Copy-Item -LiteralPath $notice.FullName -Destination $noticeOutputDirectory -Force }
        $copiedRuntimePacks.Add($dependency.name) | Out-Null
    }
}
if ($copiedRuntimePacks.Count -ne 2) { throw 'ToolHost restore did not resolve both required runtime-pack license sources.' }
Write-Output 'Copied original .NET and WindowsDesktop runtime-pack licenses and available third-party notices.'
