[CmdletBinding()]
param([string]$Name = 'KYNXA')
# Point a desktop shortcut at this checkout's registered development package.
# The identity depends on the checkout path, so run this again after moving or renaming the folder.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$application = Join-Path $repository 'artifacts\development\appx\x64\Debug'
[xml]$manifest = Get-Content -LiteralPath (Join-Path $application 'AppxManifest.xml') -Raw -Encoding UTF8
$identity = $manifest.SelectSingleNode('//*[local-name()="Identity"]').GetAttribute('Name')
$package = Get-AppxPackage -Name $identity | Select-Object -First 1
if (-not $package) { throw "Package $identity is not registered. Run build-desktop.ps1 -Action Run first." }
$link = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path ([Environment]::GetFolderPath('Desktop')) "$Name.lnk"))
$link.TargetPath = Join-Path $env:SystemRoot 'explorer.exe'
$link.Arguments = "shell:AppsFolder\$($package.PackageFamilyName)!App"
$link.IconLocation = (Join-Path $application 'Assets\AppIcon.ico') + ',0'
$link.Description = 'Start KYNXA'
$link.Save()
Write-Host "Shortcut updated: $($link.FullName) -> $($link.Arguments)"
