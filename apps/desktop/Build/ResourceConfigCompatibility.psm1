Set-StrictMode -Version Latest

Import-Module (Join-Path $PSScriptRoot 'AtomicFiles.psm1') -Force

# Only configuration text accepts checkout formatting differences; binary payload checks remain byte-exact.
# 仅配置文本兼容检出时的换行与 BOM 差异，二进制资源仍按原始字节校验。
function Get-KynxaConfigHashVariants {
    param([string]$Path, [byte[]]$Bytes)
    $bytes = if ($Path) { [IO.File]::ReadAllBytes($Path) } else { $Bytes }
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
    param([Parameter(Mandatory = $true)][string]$Path, [Parameter(Mandatory = $true)][string]$ExpectedHash, [string]$SemanticHash)
    if ((Get-KynxaConfigHashVariants -Path $Path) -contains $ExpectedHash) { return $true }
    return $SemanticHash -match '^[0-9a-f]{64}$' -and (Get-KynxaConfigSemanticHash -Path $Path) -eq $SemanticHash
}

function ConvertTo-KynxaCanonicalJsonValue($Value) {
    if ($null -eq $Value) { return $null }
    if ($Value -is [Collections.IDictionary]) {
        $canonical = [ordered]@{}
        foreach ($key in @($Value.Keys | Sort-Object -CaseSensitive)) { $canonical[$key] = ConvertTo-KynxaCanonicalJsonValue $Value[$key] }
        return $canonical
    }
    if ($Value -is [Collections.IEnumerable] -and $Value -isnot [string]) {
        $items = [Collections.Generic.List[object]]::new()
        foreach ($item in $Value) { $items.Add((ConvertTo-KynxaCanonicalJsonValue $item)) }
        return ,$items.ToArray()
    }
    return $Value
}

function Get-KynxaConfigSemanticHash {
    param([string]$Path, [string]$Text)
    if ($Path) { $Text = [IO.File]::ReadAllText($Path, [Text.Encoding]::UTF8) }
    $canonical = ConvertTo-KynxaCanonicalJsonValue (ConvertFrom-KynxaResourceJson $Text)
    $serialized = ConvertTo-Json -InputObject $canonical -Depth 100 -Compress
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($hasher.ComputeHash([Text.Encoding]::UTF8.GetBytes($serialized))).Replace('-', '').ToLowerInvariant() }
    finally { $hasher.Dispose() }
}

function ConvertFrom-KynxaResourceJson([string]$Text) {
    if ($PSVersionTable.PSVersion.Major -ge 6) { return ($Text | ConvertFrom-Json -AsHashtable) }
    Add-Type -AssemblyName System.Web.Extensions
    $reader = [Web.Script.Serialization.JavaScriptSerializer]::new()
    $reader.MaxJsonLength = 64 * 1024 * 1024
    return $reader.DeserializeObject($Text)
}

# Completion records accelerate local preparation; first installation still verifies pinned content hashes.
# 完成记录只加速本地重复准备；首次安装仍核验固定内容摘要，记录不能代替下载完整性校验。
if (-not ('KynxaResourceMetadata' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections;
using System.IO;
using System.Collections.Generic;
using System.Security.Cryptography;
public sealed class KynxaResourceFileStamp {
    public string path, sha256;
    public long bytes, modified;
}
public sealed class KynxaResourceDirectoryStamp {
    public string path;
    public long modified;
}
public sealed class KynxaResourceSnapshot {
    public KynxaResourceFileStamp[] files;
    public KynxaResourceDirectoryStamp[] directories;
}
public static class KynxaResourceMetadata {
    static string Resolve(string root, string relative) {
        string prefix = Path.GetFullPath(root).TrimEnd('\\', '/') + Path.DirectorySeparatorChar;
        string full = Path.GetFullPath(Path.Combine(prefix, relative));
        if (!full.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)) throw new IOException("Resource metadata escaped its root.");
        return full;
    }
    static bool Plain(FileSystemInfo entry) {
        return entry.Exists && (entry.Attributes & FileAttributes.ReparsePoint) == 0;
    }
    public static bool Matches(string root, string[] paths, long[] lengths, long[] modified,
        string[] directories, long[] directoryModified) {
        try {
            if (!Plain(new DirectoryInfo(root))) return false;
            for (int i = 0; i < paths.Length; i++) {
                var file = new FileInfo(Resolve(root, paths[i]));
                if (!Plain(file) || file.Length != lengths[i] || file.LastWriteTimeUtc.Ticks != modified[i]) return false;
            }
            for (int i = 0; i < directories.Length; i++) {
                var directory = new DirectoryInfo(Resolve(root, directories[i]));
                if (!Plain(directory) || directory.LastWriteTimeUtc.Ticks != directoryModified[i]) return false;
            }
            return true;
        } catch (IOException) { return false; } catch (UnauthorizedAccessException) { return false; }
    }
    public static bool Verify(string root, string[] paths, long[] lengths, string[] hashes, IDictionary previous, string scope) {
        try {
            string fullRoot = Path.GetFullPath(root).TrimEnd('\\', '/');
            if (!Plain(new DirectoryInfo(fullRoot))) return false;
            var checkedDirectories = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            if (!String.IsNullOrEmpty(scope)) {
                var pending = new Stack<string>();
                pending.Push(Resolve(fullRoot, scope));
                while (pending.Count > 0) {
                    var directory = new DirectoryInfo(pending.Pop());
                    if (!Plain(directory)) return false;
                    foreach (var child in directory.EnumerateDirectories()) {
                        if (!Plain(child)) return false;
                        pending.Push(child.FullName);
                    }
                }
            }
            using (var hasher = SHA256.Create()) {
                for (int i = 0; i < paths.Length; i++) {
                    var file = new FileInfo(Resolve(fullRoot, paths[i]));
                    if (!Plain(file) || file.Length != lengths[i]) return false;
                    var directory = file.Directory;
                    while (directory != null && !String.Equals(directory.FullName, fullRoot, StringComparison.OrdinalIgnoreCase)) {
                        if (!checkedDirectories.Add(directory.FullName)) break;
                        if (!Plain(directory)) return false;
                        directory = directory.Parent;
                    }
                    var old = previous != null && previous.Contains(paths[i]) ? previous[paths[i]] as IDictionary : null;
                    if (old != null && String.Equals(Convert.ToString(old["sha256"]), hashes[i], StringComparison.OrdinalIgnoreCase) &&
                        Convert.ToInt64(old["bytes"]) == file.Length && Convert.ToInt64(old["modified"]) == file.LastWriteTimeUtc.Ticks) continue;
                    using (var input = new FileStream(file.FullName, FileMode.Open, FileAccess.Read, FileShare.Read, 1024 * 1024, FileOptions.SequentialScan)) {
                        string hash = BitConverter.ToString(hasher.ComputeHash(input)).Replace("-", "");
                        if (!String.Equals(hash, hashes[i], StringComparison.OrdinalIgnoreCase)) return false;
                    }
                }
            }
            return true;
        } catch (IOException) { return false; } catch (UnauthorizedAccessException) { return false; }
    }
    public static KynxaResourceSnapshot Capture(string root, string[] paths, string[] hashes) {
        string prefix = Path.GetFullPath(root).TrimEnd('\\', '/') + Path.DirectorySeparatorChar;
        var files = new List<KynxaResourceFileStamp>();
        var directories = new Dictionary<string, KynxaResourceDirectoryStamp>(StringComparer.OrdinalIgnoreCase);
        for (int i = 0; i < paths.Length; i++) {
            var file = new FileInfo(Resolve(root, paths[i]));
            if (!Plain(file)) throw new IOException("Resource files must be present without links.");
            files.Add(new KynxaResourceFileStamp { path = paths[i], bytes = file.Length,
                modified = file.LastWriteTimeUtc.Ticks, sha256 = hashes[i] });
            var directory = file.Directory;
            while (directory != null && directory.FullName.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)) {
                string relative = directory.FullName.Substring(prefix.Length).Replace('\\', '/');
                if (directories.ContainsKey(relative)) break;
                if (!Plain(directory)) throw new IOException("Resource directories cannot contain links.");
                directories.Add(relative, new KynxaResourceDirectoryStamp { path = relative, modified = directory.LastWriteTimeUtc.Ticks });
                directory = directory.Parent;
            }
        }
        return new KynxaResourceSnapshot { files = files.ToArray(), directories = new List<KynxaResourceDirectoryStamp>(directories.Values).ToArray() };
    }
}
'@
}

function Get-KynxaResourceInputHash {
    param([string[]]$Paths = @(), [string[]]$Values = @())
    $identities = @($Paths | ForEach-Object { @(Get-KynxaConfigHashVariants -Path $_ | Sort-Object -Unique)[0] }) + $Values
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($hasher.ComputeHash([Text.Encoding]::UTF8.GetBytes(($identities -join "`n")))).Replace('-', '').ToLowerInvariant() }
    finally { $hasher.Dispose() }
}

function Get-KynxaNativeAssetsInputHash {
    param([ValidateSet('Full', 'Core')][string]$Profile = 'Full', [string]$RuntimeIdentifier = 'win-x64')
    $paths = @($PSCommandPath, (Join-Path $PSScriptRoot 'prepare-native-runtime.ps1'),
        (Join-Path $PSScriptRoot 'RuntimeDownloads.psm1'), (Join-Path $PSScriptRoot 'native-assets/native-runtime.json'),
        (Join-Path $PSScriptRoot 'native-assets/licenses/sources.json'))
    if ($Profile -eq 'Full') {
        $paths += @('prepare-mcp-runtime.ps1', 'prepare-browser-sandbox.ps1', 'prepare-office-preview-runtime.ps1',
            'native-assets/embedding.json', 'native-assets/reranker.json', 'native-assets/mcp-runtime.json',
            'native-assets/mcp/package.json', 'native-assets/mcp/package-lock.json', 'office-preview-runtime.json') |
            ForEach-Object { Join-Path $PSScriptRoot $_ }
    }
    return Get-KynxaResourceInputHash -Paths $paths -Values @($RuntimeIdentifier, $Profile)
}

function Read-KynxaPreparedComponent {
    param([string]$Root, [string]$Component)
    $path = Join-Path $Root ('.prepared/' + $Component + '.json')
    if (-not [IO.File]::Exists($path)) { return $null }
    try {
        if ([IO.File]::GetAttributes($path) -band [IO.FileAttributes]::ReparsePoint) { return $null }
        $state = ConvertFrom-KynxaResourceJson ([IO.File]::ReadAllText($path, [Text.Encoding]::UTF8))
        if ($state['schemaVersion'] -ne 1 -or $state['status'] -ne 'ready') { return $null }
        return $state
    }
    catch { return $null }
}

function Test-KynxaPreparedComponent {
    param([string]$Root, [string]$Component, [string]$InputHash, $State)
    $state = if ($State) { $State } else { Read-KynxaPreparedComponent -Root $Root -Component $Component }
    if (-not $state -or $state['inputHash'] -ne $InputHash -or -not $state['files']) { return $false }
    $files = $state['files']; $directories = $state['directories']
    return [KynxaResourceMetadata]::Matches($Root, [string[]]@($files | ForEach-Object { $_['path'] }),
        [long[]]@($files | ForEach-Object { $_['bytes'] }), [long[]]@($files | ForEach-Object { $_['modified'] }),
        [string[]]@($directories | ForEach-Object { $_['path'] }), [long[]]@($directories | ForEach-Object { $_['modified'] }))
}

function Get-KynxaPreparedFileCache {
    param([string]$Root, [string]$Component)
    $cache = [Collections.Generic.Dictionary[string, object]]::new([StringComparer]::OrdinalIgnoreCase)
    $state = Read-KynxaPreparedComponent -Root $Root -Component $Component
    if ($state) { foreach ($file in $state['files']) { $cache[$file['path']] = $file } }
    return ,$cache
}

function Test-KynxaPreparedFile {
    param([string]$Root, [string]$Path, [long]$Bytes, [string]$Sha256, $Cache)
    $file = [IO.FileInfo]::new($Path)
    if (-not $file.Exists -or $file.Length -ne $Bytes -or ($file.Attributes -band [IO.FileAttributes]::ReparsePoint)) { return $false }
    $prefix = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    if ($Cache -and $file.FullName.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        $key = $file.FullName.Substring($prefix.Length).Replace('\', '/')
        if ($Cache.ContainsKey($key)) {
            $previous = $Cache[$key]
            if ($previous['bytes'] -eq $Bytes -and $previous['sha256'] -eq $Sha256 -and $previous['modified'] -eq $file.LastWriteTimeUtc.Ticks) { return $true }
        }
    }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -eq $Sha256
}

function Test-KynxaResourceFiles {
    param([string]$Root, [object[]]$Files, [Collections.IDictionary]$Cache, [string]$Scope)
    return [KynxaResourceMetadata]::Verify($Root, [string[]]@($Files | ForEach-Object { $_.path }),
        [long[]]@($Files | ForEach-Object { $_.bytes }), [string[]]@($Files | ForEach-Object { $_.sha256 }), $Cache, $Scope)
}

function Save-KynxaPreparedComponent {
    param([string]$Root, [string]$Component, [string]$InputHash, [object[]]$Files, [string[]]$Scope)
    $directory = Join-Path $Root '.prepared'
    [IO.Directory]::CreateDirectory($directory) | Out-Null
    if ([IO.File]::GetAttributes($directory) -band [IO.FileAttributes]::ReparsePoint) { throw 'Resource preparation state cannot be a link.' }
    $existing = Read-KynxaPreparedComponent -Root $Root -Component $Component
    if ($existing -and $existing['inputHash'] -eq $InputHash -and @($existing['files']).Count -eq $Files.Count) {
        $identities = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($file in $existing['files']) { $identities[$file['path']] = $file['sha256'] }
        $sameFiles = $identities.Count -eq $Files.Count
        foreach ($file in $Files) {
            if (-not $identities.ContainsKey($file.path) -or $identities[$file.path] -ne $file.sha256) { $sameFiles = $false; break }
            [void]$identities.Remove($file.path)
        }
        # Setup and MSBuild share this record; unchanged resources must not republish the same ready file.
        # Setup 与 MSBuild 共用完成记录；输入、清单和文件元数据未变时直接复用，避免重复替换就绪文件。
        if ($sameFiles -and (Test-KynxaPreparedComponent -Root $Root -Component $Component -InputHash $InputHash -State $existing)) { return }
    }
    $snapshot = [KynxaResourceMetadata]::Capture($Root, [string[]]@($Files | ForEach-Object { $_.path }), [string[]]@($Files | ForEach-Object { $_.sha256 }))
    $directories = if ($Scope) {
        @($snapshot.directories | Where-Object {
            $relative = $_.path
            @($Scope | Where-Object { $relative -eq $_ -or $relative.StartsWith($_.TrimEnd('/') + '/', [StringComparison]::OrdinalIgnoreCase) }).Count -gt 0
        })
    } else { $snapshot.directories }
    $record = @{schemaVersion=1;status='ready';inputHash=$InputHash;files=$snapshot.files;directories=$directories}
    $path = Join-Path $directory ($Component + '.json')
    $pending = $path + '.write-' + [Guid]::NewGuid().ToString('N')
    try {
        [IO.File]::WriteAllText($pending, ($record | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
        Move-KynxaAtomicFile -Source $pending -Destination $path
    }
    finally { if ([IO.File]::Exists($pending)) { Remove-Item -LiteralPath $pending } }
}

Export-ModuleMember -Function Get-KynxaConfigHashVariants, Test-KynxaConfigHash, Get-KynxaConfigSemanticHash, Get-KynxaResourceInputHash, Get-KynxaNativeAssetsInputHash,
    Read-KynxaPreparedComponent, Test-KynxaPreparedComponent, Get-KynxaPreparedFileCache, Test-KynxaPreparedFile, Test-KynxaResourceFiles, Save-KynxaPreparedComponent
