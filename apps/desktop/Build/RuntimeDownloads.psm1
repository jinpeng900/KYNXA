Set-StrictMode -Version Latest
Add-Type -AssemblyName System.Net.Http
$script:DownloadUtf8 = [Text.UTF8Encoding]::new($false)

function Get-KynxaDownloadSettings {
    param([string]$Path = $env:KYNXA_DOWNLOAD_SOURCES)
    if ([string]::IsNullOrWhiteSpace($Path)) { return @{} }
    $document = [IO.File]::ReadAllText([IO.Path]::GetFullPath($Path), $script:DownloadUtf8) | ConvertFrom-Json
    $settings = @{}
    foreach ($property in $document.PSObject.Properties) { $settings[$property.Name] = $property.Value }
    return $settings
}

function Get-KynxaDownloadUrls {
    param([string]$Url, [string]$DownloadSourcesPath)
    $urls = [Collections.Generic.List[string]]::new()
    $urls.Add($Url)
    $settings = Get-KynxaDownloadSettings -Path $DownloadSourcesPath
    if ($settings.Contains('mirrors') -and $settings['mirrors']) {
        foreach ($property in $settings['mirrors'].PSObject.Properties | Sort-Object { $_.Name.Length } -Descending) {
            $prefix = $property.Name.TrimEnd('/')
            if ($Url -ne $prefix -and -not $Url.StartsWith($prefix + '/', [StringComparison]::OrdinalIgnoreCase)) { continue }
            foreach ($mirror in @($property.Value)) {
                $candidate = ([string]$mirror).TrimEnd('/') + $Url.Substring($prefix.Length)
                if (-not $urls.Contains($candidate)) { $urls.Add($candidate) }
            }
        }
    }
    foreach ($candidate in $urls) {
        $uri = [Uri]$candidate
        if (-not $uri.IsAbsoluteUri -or $uri.UserInfo -or ($uri.Scheme -ne 'https' -and -not ($uri.Scheme -eq 'http' -and $uri.IsLoopback))) {
            throw 'Download sources require HTTPS, or loopback HTTP for local fixtures, without embedded credentials.'
        }
    }
    return $urls.ToArray()
}

function Get-KynxaDownloadHash {
    param([string]$Path)
    $input = [IO.File]::OpenRead($Path)
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($hasher.ComputeHash($input)).Replace('-', '').ToLowerInvariant() }
    finally { $hasher.Dispose(); $input.Dispose() }
}

function Test-KynxaPinnedDownload {
    param([string]$Path, [string]$Sha256, [long]$ExpectedBytes)
    if (-not [IO.File]::Exists($Path)) { return $false }
    $file = [IO.FileInfo]::new($Path)
    if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Pinned download files cannot be links.' }
    return ($ExpectedBytes -eq 0 -or $file.Length -eq $ExpectedBytes) -and (Get-KynxaDownloadHash $Path) -eq $Sha256
}

function Save-KynxaPinnedDownload {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$Url,
        [Parameter(Mandatory = $true)][string]$Destination,
        [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$Sha256,
        [ValidateRange(0, [long]::MaxValue)][long]$ExpectedBytes = 0,
        [string]$DownloadSourcesPath = $env:KYNXA_DOWNLOAD_SOURCES,
        [ValidateRange(1, 8)][int]$MaxAttempts = 3,
        [ValidateRange(1, 1800)][int]$TimeoutSeconds = 120,
        [switch]$Offline
    )
    $Destination = [IO.Path]::GetFullPath($Destination)
    $Sha256 = $Sha256.ToLowerInvariant()
    $name = [IO.Path]::GetFileName($Destination)
    if (Test-KynxaPinnedDownload $Destination $Sha256 $ExpectedBytes) { return }
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Destination)) | Out-Null
    $pending = $Destination + '.part'
    $metadataPath = $pending + '.json'
    foreach ($path in @($pending, $metadataPath, ($Destination + '.download.lock'))) {
        if ([IO.File]::Exists($path) -and ([IO.File]::GetAttributes($path) -band [IO.FileAttributes]::ReparsePoint)) { throw 'Download cache files cannot be links.' }
    }
    $lock = $null
    $deadline = [DateTime]::UtcNow.AddMinutes(2)
    while (-not $lock) {
        try { $lock = [IO.File]::Open($Destination + '.download.lock', 'OpenOrCreate', 'ReadWrite', 'None') }
        catch [IO.IOException] {
            if ([DateTime]::UtcNow -ge $deadline) { throw "Download is already in use: $name" }
            Start-Sleep -Milliseconds 200
        }
    }
    $client = $null
    try {
        if (Test-KynxaPinnedDownload $Destination $Sha256 $ExpectedBytes) { return }
        if (Test-KynxaPinnedDownload $pending $Sha256 $ExpectedBytes) {
            Move-Item -LiteralPath $pending -Destination $Destination -Force
            if ([IO.File]::Exists($metadataPath)) { Remove-Item -LiteralPath $metadataPath }
            return
        }
        if ($Offline) { throw "Pinned asset unavailable offline: $name. Supply the prepared resource pack or complete this download online." }
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        $handler = [Net.Http.HttpClientHandler]::new()
        $handler.AutomaticDecompression = [Net.DecompressionMethods]::None
        $client = [Net.Http.HttpClient]::new($handler)
        $client.Timeout = [TimeSpan]::FromSeconds($TimeoutSeconds)
        $client.DefaultRequestHeaders.UserAgent.ParseAdd('KYNXA-Build/1.0')
        $lastFailure = ''
        foreach ($sourceUrl in @(Get-KynxaDownloadUrls -Url $Url -DownloadSourcesPath $DownloadSourcesPath)) {
            $allowResume = $true
            for ($attempt = 1; $attempt -le $MaxAttempts; $attempt++) {
                $request = $null; $response = $null; $input = $null; $output = $null
                $statusCode = 0
                $offset = [long]0
                $metadata = $null
                try {
                    if ($allowResume -and [IO.File]::Exists($pending) -and [IO.File]::Exists($metadataPath)) {
                        try { $metadata = [IO.File]::ReadAllText($metadataPath, $script:DownloadUtf8) | ConvertFrom-Json } catch { $metadata = $null }
                        if ($metadata -and $metadata.sha256 -eq $Sha256 -and $metadata.expectedBytes -eq $ExpectedBytes -and $metadata.url -eq $sourceUrl -and
                            ($metadata.etag -or $metadata.lastModified)) {
                            $offset = ([IO.FileInfo]::new($pending)).Length
                            if ($ExpectedBytes -gt 0 -and $offset -ge $ExpectedBytes) { $offset = 0 }
                        }
                    }
                    Write-Host "[Download] $name : $offset bytes cached, attempt $attempt/$MaxAttempts, source $(([Uri]$sourceUrl).Host)."
                    $request = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Get, $sourceUrl)
                    if ($offset -gt 0) {
                        $request.Headers.Range = [Net.Http.Headers.RangeHeaderValue]::new($offset, $null)
                        $validator = if ($metadata.etag) { $metadata.etag } else { $metadata.lastModified }
                        [void]$request.Headers.TryAddWithoutValidation('If-Range', [string]$validator)
                    }
                    $response = $client.SendAsync($request, [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
                    $statusCode = [int]$response.StatusCode
                    if ($statusCode -notin @(200, 206)) { throw "HTTP $statusCode" }
                    $etag = if ($response.Headers.ETag -and -not $response.Headers.ETag.IsWeak) { $response.Headers.ETag.ToString() } else { '' }
                    $lastModified = if ($response.Content.Headers.LastModified) { ([DateTimeOffset]$response.Content.Headers.LastModified).ToString('R') } else { '' }
                    $totalBytes = $ExpectedBytes
                    $contentLength = $response.Content.Headers.ContentLength
                    if ($statusCode -eq 206) {
                        $range = $response.Content.Headers.ContentRange
                        # Append only an exact range of the same entity; a pinned hash is still required at completion.
                        # 只追加同一文件身份的精确范围；下载完成后仍必须通过固定哈希校验。
                        if (-not $range -or $range.Unit -ne 'bytes' -or $range.From -ne $offset -or -not $range.HasLength -or
                            $range.To -lt $range.From -or $range.To -ge $range.Length -or
                            ($ExpectedBytes -gt 0 -and $range.Length -ne $ExpectedBytes) -or
                            ($contentLength -and $contentLength -ne ($range.To - $range.From + 1)) -or
                            ($offset -gt 0 -and (($metadata.etag -and $etag -ne $metadata.etag) -or
                                (-not $metadata.etag -and $lastModified -ne $metadata.lastModified) -or
                                ($metadata.totalBytes -gt 0 -and $range.Length -ne $metadata.totalBytes)))) {
                            $allowResume = $false
                            throw 'The range or file identity changed; restarting instead of appending.'
                        }
                        $totalBytes = [long]$range.Length
                    }
                    else {
                        # A server ignoring Range returns a fresh full body, never bytes to append.
                        # 服务端忽略 Range 返回完整正文时重新写入，不能追加到旧断点。
                        $offset = 0
                        if ($ExpectedBytes -gt 0 -and $contentLength -and $contentLength -ne $ExpectedBytes) { throw 'Downloaded length differs from the pinned size.' }
                        if ($contentLength) { $totalBytes = [long]$contentLength }
                    }
                    $record = @{schemaVersion=1;sha256=$Sha256;expectedBytes=$ExpectedBytes;url=$sourceUrl;etag=$etag;lastModified=$lastModified;totalBytes=$totalBytes}
                    [IO.File]::WriteAllText($metadataPath, ($record | ConvertTo-Json), $script:DownloadUtf8)
                    $mode = if ($offset -gt 0) { [IO.FileMode]::Append } else { [IO.FileMode]::Create }
                    $output = [IO.File]::Open($pending, $mode, [IO.FileAccess]::Write, [IO.FileShare]::Read)
                    $input = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
                    $buffer = [byte[]]::new(262144)
                    $timer = [Diagnostics.Stopwatch]::StartNew()
                    $lastProgressSeconds = -2.0
                    while ($true) {
                        $idle = [Threading.CancellationTokenSource]::new()
                        try {
                            $idle.CancelAfter([TimeSpan]::FromSeconds($TimeoutSeconds))
                            $read = $input.ReadAsync($buffer, 0, $buffer.Length, $idle.Token).GetAwaiter().GetResult()
                        }
                        finally { $idle.Dispose() }
                        if ($read -eq 0) { break }
                        $output.Write($buffer, 0, $read)
                        if ($totalBytes -gt 0 -and $output.Length -gt $totalBytes) { $allowResume = $false; throw 'The response exceeded its declared file size.' }
                        if ($timer.Elapsed.TotalSeconds - $lastProgressSeconds -ge 2) {
                            $speed = ($output.Length - $offset) / [Math]::Max(0.001, $timer.Elapsed.TotalSeconds)
                            $totalLabel = if ($totalBytes -gt 0) { '{0:N1} MiB' -f ($totalBytes / 1MB) } else { 'unknown size' }
                            Write-Host ('[Download] {0}: {1:N1} MiB / {2}, {3:N1} MiB/s' -f $name, ($output.Length / 1MB), $totalLabel, ($speed / 1MB))
                            $lastProgressSeconds = $timer.Elapsed.TotalSeconds
                        }
                    }
                    $output.Dispose(); $output = $null
                    if (($totalBytes -gt 0 -and ([IO.FileInfo]::new($pending)).Length -ne $totalBytes) -or
                        -not (Test-KynxaPinnedDownload $pending $Sha256 $ExpectedBytes)) { throw 'The file is incomplete or its pinned SHA-256 differs.' }
                    Move-Item -LiteralPath $pending -Destination $Destination -Force
                    Remove-Item -LiteralPath $metadataPath
                    Write-Host "[Download] $name complete; pinned SHA-256 verified."
                    return
                }
                catch {
                    $lastFailure = $_.Exception.Message
                    Write-Host "[Download] $name interrupted: $lastFailure Partial bytes retained."
                    if ($statusCode -eq 416) { $allowResume = $false }
                    if ($statusCode -ge 400 -and $statusCode -lt 500 -and $statusCode -notin @(408, 416, 429)) { break }
                    if ($attempt -lt $MaxAttempts) { Start-Sleep -Seconds ([Math]::Min(8, 2 * $attempt)) }
                }
                finally {
                    if ($output) { $output.Dispose() }
                    if ($input) { $input.Dispose() }
                    if ($response) { $response.Dispose() }
                    if ($request) { $request.Dispose() }
                }
            }
        }
        throw "Download failed: $name. $lastFailure The .part file is kept for the next build."
    }
    finally { if ($client) { $client.Dispose() }; $lock.Dispose() }
}

Export-ModuleMember -Function Get-KynxaDownloadSettings, Get-KynxaDownloadUrls, Save-KynxaPinnedDownload
