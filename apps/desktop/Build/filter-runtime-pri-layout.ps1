[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$LayoutPath,
    [Parameter(Mandatory = $true)][string]$ExcludedLayoutPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# These are separate executable payloads. WinUI must not interpret their file
# names or satellite languages as localized resources of the desktop UI.
$layoutFile = [IO.Path]::GetFullPath($LayoutPath)
$excludedFile = [IO.Path]::GetFullPath($ExcludedLayoutPath)
$layoutLines = [IO.File]::ReadAllLines($layoutFile, [Text.Encoding]::UTF8)
$kept = [Collections.Generic.List[string]]::new()
$plainPayload = [Collections.Generic.List[string]]::new()
foreach ($line in $layoutLines) {
    $relativePath = $line.Replace('/', '\')
    if ($relativePath.StartsWith('runtime\', [StringComparison]::OrdinalIgnoreCase) -or
        $relativePath.StartsWith('ToolHost\', [StringComparison]::OrdinalIgnoreCase)) {
        $plainPayload.Add($line)
    }
    else { $kept.Add($line) }
}
if ($plainPayload.Count -eq 0) { return }

$excluded = [Collections.Generic.List[string]]::new()
$known = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
if ([IO.File]::Exists($excludedFile)) {
    foreach ($line in [IO.File]::ReadAllLines($excludedFile, [Text.Encoding]::UTF8)) {
        $excluded.Add($line)
        [void]$known.Add($line.Replace('/', '\'))
    }
}
foreach ($line in $plainPayload) {
    if ($known.Add($line.Replace('/', '\'))) { $excluded.Add($line) }
}

# The SDK reads the excluded list when constructing a main package file map,
# so these files remain ordinary payloads even when resource packs are split.
# Do not alter the unfiltered layout, Content items, or physical bundle files.
$utf8 = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllLines($excludedFile, $excluded, $utf8)
[IO.File]::WriteAllLines($layoutFile, $kept, $utf8)
Write-Output "WinUI PRI: retained $($kept.Count) UI assets; $($plainPayload.Count) runtime files remain plain package payloads."
