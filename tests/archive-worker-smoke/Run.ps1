param(
    [Parameter(Mandatory = $true)][string]$ToolHostExe,
    [string]$FixturePath = (Join-Path $PSScriptRoot 'fixtures/guides.tar.xz')
)

$ErrorActionPreference = 'Stop'
$archiveWorker = $null
try {
    # Keep stdin alive until the owned worker exits; EOF means cancellation in the real ToolHost protocol.
    # 真实 ToolHost 将输入关闭视作取消，因此仅在受控子进程退出后关闭输入。
    $archiveStart = [System.Diagnostics.ProcessStartInfo]::new((Resolve-Path -LiteralPath $ToolHostExe).Path)
    $archiveStart.ArgumentList.Add('--document-archive')
    $archiveStart.UseShellExecute = $false
    $archiveStart.CreateNoWindow = $true
    $archiveStart.RedirectStandardInput = $true
    $archiveStart.RedirectStandardOutput = $true
    $archiveStart.RedirectStandardError = $true
    $archiveWorker = [System.Diagnostics.Process]::Start($archiveStart)
    $archiveOutput = $archiveWorker.StandardOutput.ReadToEndAsync()
    $archiveError = $archiveWorker.StandardError.ReadToEndAsync()
    $archiveRequest = @{
        operation = 'document_archive'; name = 'guides.tar.xz'; format = 'tar.xz'
        bytesBase64 = [Convert]::ToBase64String([IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $FixturePath).Path))
    } | ConvertTo-Json -Compress
    $archiveWorker.StandardInput.WriteLine($archiveRequest)
    $archiveWorker.StandardInput.Flush()
    if (-not $archiveWorker.WaitForExit(15000)) { throw 'Owned archive worker timed out. / 受控压缩包进程超时。' }
    $archiveFrames = @($archiveOutput.Result -split '\r?\n' | Where-Object { $_ } | ForEach-Object { $_ | ConvertFrom-Json })
    $archiveMembers = @($archiveFrames | Where-Object { $_.kind -eq 'member' })
    $archiveComplete = @($archiveFrames | Where-Object { $_.kind -eq 'complete' })
    if ($archiveWorker.ExitCode -ne 0 -or $archiveMembers.Count -ne 1 -or $archiveComplete.Count -ne 1 -or
        $archiveMembers[0].member -ne 'guide.md' -or $archiveMembers[0].rawBytes -ne 43) {
        throw (($archiveFrames | ConvertTo-Json -Depth 6) + $archiveError.Result)
    }
    $archiveText = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($archiveMembers[0].bytesBase64))
    if ($archiveText -ne '中文压缩文本 English archive evidence') { throw 'TAR.XZ member text changed. / TAR.XZ 成员文本发生变化。' }
    [ordered]@{
        passed = $true; format = 'tar.xz'; streamedMember = $archiveMembers[0].member
        decodedBytes = $archiveMembers[0].rawBytes; exitCode = $archiveWorker.ExitCode
        fixtureSha256 = (Get-FileHash -LiteralPath $FixturePath -Algorithm SHA256).Hash.ToLowerInvariant()
    } | ConvertTo-Json -Compress
}
finally {
    if ($null -ne $archiveWorker) {
        if (-not $archiveWorker.HasExited) { $archiveWorker.Kill($true); $archiveWorker.WaitForExit() }
        $archiveWorker.Dispose()
    }
}
