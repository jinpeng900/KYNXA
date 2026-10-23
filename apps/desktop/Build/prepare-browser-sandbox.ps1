[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$BundleRoot)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Import Get-Acl explicitly; Windows PowerShell may not auto-load it in nested builds.
# 显式导入 Get-Acl 所在模块，避免 Windows PowerShell 在嵌套构建中加载失败。
$securityModule = Join-Path $PSHOME 'Modules/Microsoft.PowerShell.Security/Microsoft.PowerShell.Security.psd1'
if ([IO.File]::Exists($securityModule)) {
    Import-Module -Name $securityModule -Force -ErrorAction Stop
} elseif (-not (Get-Command Get-Acl -ErrorAction SilentlyContinue)) {
    throw 'Microsoft.PowerShell.Security is required to prepare the bundled browser sandbox permissions.'
}

# ZIPs lose Windows ACLs; restore sandbox read access only for bundled browser files.
# ZIP 不保留 Windows ACL，只恢复随包浏览器文件的沙箱读取权限。
$root = [IO.Path]::GetFullPath($BundleRoot)
foreach ($application in @($root, (Join-Path $root 'AppX'))) {
    $manifestPath = Join-Path $application 'runtime/mcp/manifest.json'
    if (-not [IO.File]::Exists($manifestPath)) { continue }
    $manifest = [IO.File]::ReadAllText($manifestPath) | ConvertFrom-Json
    if ($manifest.schemaVersion -ne 1) { throw 'Unsupported bundled browser manifest.' }
    $relative = [string]$manifest.browser.path
    if (-not $relative.StartsWith('runtime/mcp/browsers/', [StringComparison]::Ordinal) -or
        [IO.Path]::IsPathRooted($relative) -or $relative -match '(^|[\\/])\.\.([\\/]|$)') {
        throw 'Bundled browser permissions must stay inside runtime/mcp/browsers.'
    }
    $executable = [IO.Path]::GetFullPath((Join-Path $application $relative))
    if (-not [IO.File]::Exists($executable)) { continue }
    $directory = [IO.Path]::GetDirectoryName($executable)
    $ancestor = $directory
    while ($ancestor -and $ancestor.StartsWith($application, [StringComparison]::OrdinalIgnoreCase)) {
        if ([IO.File]::GetAttributes($ancestor) -band [IO.FileAttributes]::ReparsePoint) { throw 'Bundled browser paths cannot contain reparse points.' }
        $ancestor = [IO.Path]::GetDirectoryName($ancestor)
    }
    $rules = (Get-Acl -LiteralPath $directory).GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])
    $ready = $true
    foreach ($identity in @('S-1-15-2-1', 'S-1-15-2-2')) {
        $rights = [long]0
        foreach ($rule in $rules) {
            if ($rule.IdentityReference.Value -eq $identity -and $rule.AccessControlType -eq 'Allow' -and
                ($rule.InheritanceFlags -band 3) -eq 3 -and ($rule.PropagationFlags -band 2) -eq 0) {
                $rights = $rights -bor [long]$rule.FileSystemRights
            }
        }
        if (($rights -band 1179817) -ne 1179817) { $ready = $false }
    }
    if ($ready) { continue }
    $tool = Join-Path ([Environment]::GetFolderPath('System')) 'icacls.exe'
    & $tool $directory '/grant' '*S-1-15-2-1:(OI)(CI)(RX)' '*S-1-15-2-2:(OI)(CI)(RX)' '/Q'
    if ($LASTEXITCODE -ne 0) { throw 'Cannot prepare the bundled browser sandbox read/execute permissions.' }
    Write-Output '[Browser] Portable sandbox read/execute permissions prepared.'
}
