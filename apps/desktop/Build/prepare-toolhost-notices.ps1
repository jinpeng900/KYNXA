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
$runtimeAssets = Get-Content -LiteralPath $AssetsPath -Raw -Encoding UTF8 | ConvertFrom-Json
$runtimePackNames = @("Microsoft.NETCore.App.Runtime.$RuntimeIdentifier", "Microsoft.WindowsDesktop.App.Runtime.$RuntimeIdentifier")
$runtimeNoticesOutput = [IO.Path]::GetFullPath($OutputDirectory)
[IO.Directory]::CreateDirectory($runtimeNoticesOutput) | Out-Null
$copied = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($framework in $runtimeAssets.project.frameworks.PSObject.Properties) {
    foreach ($dependency in $framework.Value.downloadDependencies) {
        if ($runtimePackNames -notcontains $dependency.name -or $copied.Contains($dependency.name)) { continue }
        if ($dependency.version -notmatch '^\[(\d+\.\d+\.\d+),\s*\1\]$') { throw 'ToolHost runtime pack must resolve to an exact version before licensing can be bundled.' }
        $version = $Matches[1]
        $source = $null
        foreach ($folder in $runtimeAssets.packageFolders.PSObject.Properties) {
            $candidate = Join-Path $folder.Name ($dependency.name.ToLowerInvariant() + '/' + $version)
            if (Test-Path -LiteralPath $candidate -PathType Container) { $source = $candidate; break }
        }
        if (-not $source) { throw "Restored ToolHost runtime pack not found: $($dependency.name) $version" }
        $notices = @(Get-ChildItem -LiteralPath $source -File | Where-Object Name -Match '^(LICENSE(\.TXT)?|THIRD-PARTY-NOTICES\.TXT|ThirdPartyNotices\.txt|NOTICE(\.TXT)?)$')
        if (-not @($notices | Where-Object Name -Match '^LICENSE').Count) { throw "ToolHost runtime pack has no original license: $($dependency.name)" }
        if ($dependency.name.StartsWith('Microsoft.NETCore.') -and -not @($notices | Where-Object Name -Match 'NOTICES').Count) { throw 'The .NET runtime third-party notices are missing.' }
        $destination = Join-Path $runtimeNoticesOutput ($dependency.name + '/' + $version)
        [IO.Directory]::CreateDirectory($destination) | Out-Null
        foreach ($notice in $notices) { Copy-Item -LiteralPath $notice.FullName -Destination $destination -Force }
        $copied.Add($dependency.name) | Out-Null
    }
}
if ($copied.Count -ne 2) { throw 'ToolHost restore did not resolve both required runtime-pack license sources.' }
Write-Output 'Copied original .NET and WindowsDesktop runtime-pack licenses and available third-party notices.'
