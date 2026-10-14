Set-StrictMode -Version Latest

# Only configuration text accepts checkout formatting differences; binary payload checks remain byte-exact.
# 仅配置文本兼容检出时的换行与 BOM 差异，二进制资源仍按原始字节校验。
function Get-KynxaConfigHashVariants {
    param([Parameter(Mandatory = $true)][string]$Path)
    $bytes = [IO.File]::ReadAllBytes($Path)
    $encoding = [Text.UTF8Encoding]::new($false, $true)
    $text = $encoding.GetString($bytes)
    if ($text.Length -gt 0 -and $text[0] -eq [char]0xfeff) { $text = $text.Substring(1) }
    $lfText = $text.Replace("`r`n", "`n")
    $hashes = [Collections.Generic.List[string]]::new()
    $hasher = [Security.Cryptography.SHA256]::Create()
    try {
        $hashes.Add([BitConverter]::ToString($hasher.ComputeHash($bytes)).Replace('-', '').ToLowerInvariant())
        foreach ($formatted in @($lfText, $lfText.Replace("`n", "`r`n"))) {
            $formattedBytes = $encoding.GetBytes($formatted)
            $hashes.Add([BitConverter]::ToString($hasher.ComputeHash($formattedBytes)).Replace('-', '').ToLowerInvariant())
            $withBom = [byte[]](@(0xef, 0xbb, 0xbf) + $formattedBytes)
            $hashes.Add([BitConverter]::ToString($hasher.ComputeHash($withBom)).Replace('-', '').ToLowerInvariant())
        }
    }
    finally { $hasher.Dispose() }
    return $hashes.ToArray()
}

function Test-KynxaConfigHash {
    param([Parameter(Mandatory = $true)][string]$Path, [Parameter(Mandatory = $true)][string]$ExpectedHash)
    return (Get-KynxaConfigHashVariants -Path $Path) -contains $ExpectedHash
}

Export-ModuleMember -Function Get-KynxaConfigHashVariants, Test-KynxaConfigHash
