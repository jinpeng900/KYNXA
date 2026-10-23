[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ManifestPath,
    [Parameter(Mandatory = $true)][string]$BootstrapPath,
    [switch]$Worker,
    [string]$CacheRoot,
    [string]$LogPath,
    [string]$ResultPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$utf8 = [Text.UTF8Encoding]::new($false)

function Write-DownloadLog {
    param([string]$Message)
    [IO.File]::AppendAllText($LogPath, $Message + [Environment]::NewLine, $utf8)
}

if ($Worker) {
    try {
        $release = Get-Content -LiteralPath $ManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
        $helper = Join-Path $PSScriptRoot 'Get-KynxaInstaller.ps1'
        $existingDirectory = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($BootstrapPath))
        $existingInstaller = Join-Path $existingDirectory $release.fileName
        $readyInstaller = $null
        # Verify a complete sibling installer in the worker, never on the UI thread.
        # 已有同目录完整安装包在后台核验，窗口不等待大文件读取。
        if ([IO.File]::Exists($existingInstaller) -and ([IO.FileInfo]::new($existingInstaller)).Length -eq [long]$release.bytes) {
            try {
                & $helper -ManifestPath $ManifestPath -DestinationDirectory $existingDirectory -Offline *>&1 |
                    ForEach-Object { Write-DownloadLog ([string]$_) }
                $readyInstaller = $existingInstaller
            }
            catch { Write-DownloadLog ('Local installer was not reusable: ' + $_.Exception.Message) }
        }
        if (-not $readyInstaller) {
            & $helper -ManifestPath $ManifestPath -DestinationDirectory $CacheRoot *>&1 |
                ForEach-Object { Write-DownloadLog ([string]$_) }
            $readyInstaller = Join-Path $CacheRoot $release.fileName
        }
        [IO.File]::WriteAllText($ResultPath, (@{ status = 'ready'; path = $readyInstaller } | ConvertTo-Json), $utf8)
        exit 0
    }
    catch {
        $failure = $_.Exception.Message
        Write-DownloadLog $failure
        [IO.File]::WriteAllText($ResultPath, (@{ status = 'failed'; message = $failure } | ConvertTo-Json), $utf8)
        exit 1
    }
}

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[Windows.Forms.Application]::EnableVisualStyles()
$isChinese = [Globalization.CultureInfo]::CurrentUICulture.Name.StartsWith('zh')
function Get-Text {
    param([string]$Chinese, [string]$English)
    if ($isChinese) { return $Chinese }
    return $English
}
function ConvertTo-PowerShellLiteral {
    param([string]$Value)
    return "'" + $Value.Replace("'", "''") + "'"
}
function Get-WritableCache {
    param([string]$ReleaseVersion)
    $bootstrapDirectory = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($BootstrapPath))
    $candidates = @(
        (Join-Path $bootstrapDirectory ('.kynxa-installer-cache/' + $ReleaseVersion)),
        (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) ('KYNXA/InstallerCache/' + $ReleaseVersion))
    )
    foreach ($candidate in $candidates) {
        $writeProbe = $null
        try {
            [IO.Directory]::CreateDirectory($candidate) | Out-Null
            $writeProbe = Join-Path $candidate ('write-' + [Guid]::NewGuid().ToString('N') + '.tmp')
            [IO.File]::WriteAllText($writeProbe, '', $utf8)
            return [IO.Path]::GetFullPath($candidate)
        }
        catch { continue }
        finally { if ($writeProbe -and [IO.File]::Exists($writeProbe)) { [IO.File]::Delete($writeProbe) } }
    }
    throw (Get-Text '安装器旁边和用户缓存目录均无法写入，请将安装器移动到可写文件夹。' 'Neither the installer folder nor the user cache is writable. Move this installer to a writable folder.')
}

$release = Get-Content -LiteralPath $ManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
$CacheRoot = Get-WritableCache ([string]$release.version)
$sessionId = [Guid]::NewGuid().ToString('N')
$LogPath = Join-Path $CacheRoot ('online-' + $sessionId + '.log')
$ResultPath = Join-Path $CacheRoot ('online-' + $sessionId + '.json')
$script:downloadProcess = $null
$script:downloadPaused = $false
$script:launchingInstaller = $false
$script:logPosition = [long]0
$script:recentActivity = ''

Add-Type -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Windows.Forms;
public sealed class KynxaInstallerButton : Button {
    protected override void OnResize(EventArgs e) {
        base.OnResize(e);
        if (Width < 12 || Height < 12) return;
        using (var path = new GraphicsPath()) {
            path.AddArc(0, 0, 12, 12, 180, 90);
            path.AddArc(Width - 13, 0, 12, 12, 270, 90);
            path.AddArc(Width - 13, Height - 13, 12, 12, 0, 90);
            path.AddArc(0, Height - 13, 12, 12, 90, 90);
            path.CloseFigure();
            var previous = Region;
            Region = new Region(path);
            if (previous != null) previous.Dispose();
        }
    }
}
'@ -ReferencedAssemblies System.Windows.Forms, System.Drawing

$form = [Windows.Forms.Form]::new()
$form.Text = 'KYNXA ' + $release.version
$form.ClientSize = [Drawing.Size]::new(592, 336)
$form.StartPosition = 'CenterScreen'
$form.FormBorderStyle = 'FixedDialog'
$form.MaximizeBox = $false
$form.MinimizeBox = $true
$form.AutoScaleMode = 'Dpi'
$form.Font = [Drawing.Font]::new('Microsoft YaHei UI', 9)
$form.BackColor = [Drawing.Color]::FromArgb(247, 246, 244)
$form.ForeColor = [Drawing.Color]::FromArgb(59, 57, 54)

function New-Label {
    param([int]$Left, [int]$Top, [int]$Width, [int]$Height, [string]$Text)
    $label = [Windows.Forms.Label]::new()
    $label.SetBounds($Left, $Top, $Width, $Height)
    $label.Text = $Text
    $label.AutoEllipsis = $true
    $form.Controls.Add($label)
    return $label
}
$logoPath = Join-Path $PSScriptRoot 'kynxa-installer-logo.png'
if ([IO.File]::Exists($logoPath)) {
    $logo = [Windows.Forms.PictureBox]::new()
    $logo.SetBounds(28, 27, 48, 48)
    $logo.SizeMode = 'Zoom'
    $logo.Image = [Drawing.Image]::FromFile($logoPath)
    $form.Controls.Add($logo)
}
$title = New-Label 90 29 464 40 ('KYNXA ' + $release.version)
$title.Font = [Drawing.Font]::new('Segoe UI', 20, [Drawing.FontStyle]::Bold)
$subtitle = New-Label 28 98 536 42 (Get-Text '正在准备完整安装程序，完成后选择安装位置和语言。' 'Preparing the full installer. Choose installation locations and language next.')
$status = New-Label 28 151 536 28 (Get-Text '正在连接可用下载源…' 'Connecting to an available download source…')
$status.Font = [Drawing.Font]::new($form.Font, [Drawing.FontStyle]::Bold)
$track = [Windows.Forms.Panel]::new()
$track.SetBounds(28, 190, 536, 6)
$track.BackColor = [Drawing.Color]::FromArgb(222, 219, 214)
$fill = [Windows.Forms.Panel]::new()
$fill.SetBounds(0, 0, 0, 6)
$fill.BackColor = [Drawing.Color]::FromArgb(116, 97, 73)
$track.Controls.Add($fill)
$form.Controls.Add($track)
$details = New-Label 28 208 536 43 (Get-Text '下载中断会保留进度；再次打开或点击继续即可恢复。' 'Interrupted downloads keep their progress. Reopen this installer or choose Resume.')
$details.ForeColor = [Drawing.Color]::FromArgb(117, 112, 104)
$cacheLabel = New-Label 28 286 320 34 $CacheRoot
$cacheLabel.Font = [Drawing.Font]::new($form.Font.FontFamily, 8)
$cacheLabel.ForeColor = $details.ForeColor
$tooltip = [Windows.Forms.ToolTip]::new()
$tooltip.SetToolTip($cacheLabel, $CacheRoot)
$action = [KynxaInstallerButton]::new()
$action.SetBounds(355, 280, 98, 34)
$action.FlatStyle = 'Flat'
$action.FlatAppearance.BorderSize = 0
$action.BackColor = [Drawing.Color]::FromArgb(116, 97, 73)
$action.ForeColor = [Drawing.Color]::White
$action.Text = Get-Text '暂停' 'Pause'
$form.Controls.Add($action)
$close = [KynxaInstallerButton]::new()
$close.SetBounds(465, 280, 99, 34)
$close.FlatStyle = 'Flat'
$close.FlatAppearance.BorderSize = 0
$close.BackColor = [Drawing.Color]::FromArgb(232, 229, 224)
$close.Text = Get-Text '关闭' 'Close'
$form.Controls.Add($close)
$timer = [Windows.Forms.Timer]::new()
$timer.Interval = 400

function Stop-DownloadWorker {
    if ($script:downloadProcess) {
        if (-not $script:downloadProcess.HasExited) {
            $script:downloadProcess.Kill()
            [void]$script:downloadProcess.WaitForExit(1000)
        }
        $script:downloadProcess.Dispose()
        $script:downloadProcess = $null
    }
}
function Start-DownloadWorker {
    Stop-DownloadWorker
    if ([IO.File]::Exists($ResultPath)) { [IO.File]::Delete($ResultPath) }
    $script:downloadPaused = $false
    $status.Text = Get-Text '正在连接可用下载源…' 'Connecting to an available download source…'
    $action.Text = Get-Text '暂停' 'Pause'
    $command = '& ' + (ConvertTo-PowerShellLiteral $PSCommandPath) + ' -Worker -ManifestPath ' +
        (ConvertTo-PowerShellLiteral $ManifestPath) + ' -BootstrapPath ' + (ConvertTo-PowerShellLiteral $BootstrapPath) +
        ' -CacheRoot ' + (ConvertTo-PowerShellLiteral $CacheRoot) + ' -LogPath ' + (ConvertTo-PowerShellLiteral $LogPath) +
        ' -ResultPath ' + (ConvertTo-PowerShellLiteral $ResultPath)
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = Join-Path $PSHOME 'powershell.exe'
    $startInfo.Arguments = '-NoLogo -NoProfile -ExecutionPolicy Bypass -EncodedCommand ' +
        [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
    $startInfo.CreateNoWindow = $true
    $startInfo.UseShellExecute = $false
    $startInfo.WindowStyle = 'Hidden'
    $script:downloadProcess = [Diagnostics.Process]::Start($startInfo)
    $timer.Start()
}

$action.Add_Click({
    if ($script:downloadProcess -and -not $script:downloadProcess.HasExited) {
        Stop-DownloadWorker
        $script:downloadPaused = $true
        $status.Text = Get-Text '已暂停，下载进度已保留。' 'Paused. Download progress is preserved.'
        $action.Text = Get-Text '继续' 'Resume'
    }
    else { Start-DownloadWorker }
})
$close.Add_Click({ $form.Close() })
$form.Add_FormClosing({ $timer.Stop(); Stop-DownloadWorker })
$timer.Add_Tick({
    try {
        # Read only newly appended log bytes, so a long download never blocks the UI by rereading its history.
        # 只读取新增日志字节，长时间下载不会因反复读取整份日志拖慢界面。
        if ([IO.File]::Exists($LogPath)) {
            $logStream = [IO.File]::Open($LogPath, 'Open', 'Read', 'ReadWrite')
            try {
                [void]$logStream.Seek($script:logPosition, 'Begin')
                $reader = [IO.StreamReader]::new($logStream, $utf8)
                try {
                    $newLog = $reader.ReadToEnd()
                    $script:logPosition = $logStream.Position
                    $lines = @($newLog -split '[\r\n]+' | Where-Object { $_.Length -gt 0 })
                    if ($lines.Count -gt 0) { $script:recentActivity = [string]$lines[-1] }
                }
                finally { $reader.Dispose() }
            }
            finally { $logStream.Dispose() }
        }
        $partialBytes = [long]0
        foreach ($partial in [IO.Directory]::GetFiles($CacheRoot, ($release.fileName + '.part*'))) {
            if ($partial.EndsWith('.json', [StringComparison]::OrdinalIgnoreCase)) { continue }
            $partialBytes = [Math]::Max($partialBytes, ([IO.FileInfo]::new($partial)).Length)
        }
        $finalPath = Join-Path $CacheRoot $release.fileName
        if ([IO.File]::Exists($finalPath)) { $partialBytes = ([IO.FileInfo]::new($finalPath)).Length }
        if ([long]$release.bytes -gt 0) {
            $fill.Width = [int]($track.Width * [Math]::Min(1.0, $partialBytes / [double]$release.bytes))
        }
        if (-not $script:downloadPaused -and $script:downloadProcess) {
            if ($partialBytes -gt 0) {
                $status.Text = (Get-Text '已下载 {0:N1} / {1:N1} MiB' 'Downloaded {0:N1} / {1:N1} MiB') -f ($partialBytes / 1MB), ([long]$release.bytes / 1MB)
            }
            if ($script:recentActivity) { $details.Text = $script:recentActivity }
            if ($script:downloadProcess.HasExited) {
                $timer.Stop()
                $result = if ([IO.File]::Exists($ResultPath)) { Get-Content -LiteralPath $ResultPath -Raw -Encoding UTF8 | ConvertFrom-Json } else { $null }
                if ($result -and $result.status -eq 'ready') {
                    $script:launchingInstaller = $true
                    $status.Text = Get-Text '完整安装程序已就绪，正在打开…' 'The full installer is ready. Opening…'
                    # Launch only the EXE verified by the shared download helper.
                    # 只启动共用下载工具核验通过的 EXE。
                    Start-Process -FilePath ([string]$result.path) -WorkingDirectory ([IO.Path]::GetDirectoryName([string]$result.path))
                    $form.Close()
                }
                else {
                    $status.Text = Get-Text '下载未完成，重试会接着下载。' 'Download incomplete. Retry continues the download.'
                    $details.Text = if ($result) { [string]$result.message } else { Get-Text '下载进程已退出，已保留缓存。' 'The download process exited. Cached progress is preserved.' }
                    $action.Text = Get-Text '重试' 'Retry'
                }
            }
        }
    }
    catch {
        $details.Text = $_.Exception.Message
        if ($script:launchingInstaller) {
            $script:launchingInstaller = $false
            $status.Text = Get-Text '启动安装程序失败，可以重试。' 'The installer could not be opened. Retry is available.'
            $action.Text = Get-Text '重试' 'Retry'
        }
    }
})
$form.Add_Shown({ Start-DownloadWorker })
try { [void]$form.ShowDialog() }
finally {
    $timer.Dispose()
    $tooltip.Dispose()
    foreach ($control in $form.Controls) {
        if ($control -is [Windows.Forms.PictureBox] -and $control.Image) { $control.Image.Dispose() }
    }
    $form.Dispose()
}
