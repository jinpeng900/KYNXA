$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class SelectionMouse {
 [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
 [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
 [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT point);
 public struct POINT { public int X; public int Y; }
}
'@
[void][SelectionMouse]::SetProcessDpiAwarenessContext([IntPtr](-4))
$root = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst(
 [System.Windows.Automation.TreeScope]::Children,
 [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, 'KYNXA Markdown UI smoke'))
if (-not $root) { throw 'Launch the smoke test window first.' }
function Find-Id($id) {
 $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants,
  [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, $id))
}
$scrollElement = Find-Id 'ReplyScroll'
$document = Find-Id 'ReplyDocument'
$scroll = $scrollElement.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern)
$text = $document.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
$bounds = $scrollElement.Current.BoundingRectangle
$original = New-Object SelectionMouse+POINT
[void][SelectionMouse]::GetCursorPos([ref]$original)
[void][SelectionMouse]::SetForegroundWindow([IntPtr]$root.Current.NativeWindowHandle)
function Get-SelectionText { ($text.GetSelection() | ForEach-Object { $_.GetText(-1) }) -join '' }
function Get-SelectionLength { (Get-SelectionText).Length }
try {
 foreach ($direction in @('up', 'down')) {
  $scroll.SetScrollPercent(-1, 50)
  Start-Sleep -Milliseconds 250
  $before = $scroll.Current.VerticalScrollPercent
  $x = [int]($bounds.Left + 240)
  $y = [int]($bounds.Top + $bounds.Height * 0.5)
  $edge = if ($direction -eq 'up') { [int]($bounds.Top + 4) } else { [int]($bounds.Bottom - 4) }
  [void][SelectionMouse]::SetCursorPos($x, $y)
  [SelectionMouse]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
  for ($step = 1; $step -le 12; $step++) {
   [void][SelectionMouse]::SetCursorPos($x, [int]($y + ($edge - $y) * $step / 12))
   Start-Sleep -Milliseconds 20
  }
  Start-Sleep -Milliseconds 120
  $initialLength = Get-SelectionLength
  if ($initialLength -eq 0) { throw 'Drag did not start a text selection.' }
  $previous = $initialLength
  $selection = Get-SelectionText
  $anchor = if ($direction -eq 'up') { $selection.Substring([Math]::Max(0, $selection.Length - 24)) } else { $selection.Substring(0, [Math]::Min(24, $selection.Length)) }
  for ($tick = 0; $tick -lt 14; $tick++) {
   # Continue moving at the edge, then cross into the assistant avatar column.
   # 在边缘继续移动，再穿过助手头像列。
   $dragX = if ($tick -ge 5) { [int]($bounds.Left + 24) } else { $x + ($tick % 2) }
   [void][SelectionMouse]::SetCursorPos($dragX, $edge)
   Start-Sleep -Milliseconds 100
   $selection = Get-SelectionText
   if ($selection.Length -lt $previous) { throw "Selection shrank while dragging $direction : $previous -> $($selection.Length)" }
   if ($direction -eq 'up' -and -not $selection.EndsWith($anchor)) { throw 'Upward selection anchor moved.' }
   if ($direction -eq 'down' -and -not $selection.StartsWith($anchor)) { throw 'Downward selection anchor moved.' }
   $previous = $selection.Length
  }
  $after = $scroll.Current.VerticalScrollPercent
  $length = Get-SelectionLength
  [SelectionMouse]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 250
  $released = $scroll.Current.VerticalScrollPercent
  $frozen = Get-SelectionText
  for ($hover = 0; $hover -lt 6; $hover++) {
   [void][SelectionMouse]::SetCursorPos($x + $hover * 8, [int]($bounds.Top + 140 + $hover * 22))
   Start-Sleep -Milliseconds 60
   if ((Get-SelectionText) -ne $frozen) { throw 'Selection changed on mouse movement after release.' }
  }
  Start-Sleep -Milliseconds 450
  $stopped = $scroll.Current.VerticalScrollPercent
  if ($direction -eq 'up' -and $after -ge $before) { throw "Upward scroll failed: $before -> $after" }
  if ($direction -eq 'down' -and $after -le $before) { throw "Downward scroll failed: $before -> $after" }
  if ($length -le $initialLength) { throw "Selection did not extend: $initialLength -> $length" }
  if ([Math]::Abs($released - $stopped) -gt 0.01) { throw 'Scroll continued after release.' }
  Write-Output "PASS $direction : scroll $before -> $after ; selected characters $initialLength -> $length ; stops on release"
 }
 # Cross the actual assistant image while pressed, then return to the bottom edge.
 # 按住时穿过真实助手图片，再返回底部边缘。
 $scroll.SetScrollPercent(-1, 0)
 Start-Sleep -Milliseconds 300
 $avatar = (Find-Id 'ReplyAvatar').Current.BoundingRectangle
 $x = [int]($bounds.Left + 240)
 $y = [int]($bounds.Top + 180)
 [void][SelectionMouse]::SetCursorPos($x, $y)
 [SelectionMouse]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
 [void][SelectionMouse]::SetCursorPos([int]($avatar.Left + $avatar.Width / 2), [int]($avatar.Top + $avatar.Height / 2))
 Start-Sleep -Milliseconds 150
 [void][SelectionMouse]::SetCursorPos($x, [int]($bounds.Bottom - 4))
 Start-Sleep -Milliseconds 1000
 if ($scroll.Current.VerticalScrollPercent -le 0 -or (Get-SelectionLength) -eq 0) { throw 'Selection stopped after crossing the actual avatar.' }
 $down = $scroll.Current.VerticalScrollPercent
 [void][SelectionMouse]::SetCursorPos($x, [int]($bounds.Top + 4))
 Start-Sleep -Milliseconds 650
 if ($scroll.Current.VerticalScrollPercent -ge $down) { throw 'Scroll did not reverse during the same drag.' }
 [SelectionMouse]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
 Write-Output 'PASS actual avatar crossing and reversing direction without releasing the mouse'
 $scroll.SetScrollPercent(-1, 0)
} finally {
 [SelectionMouse]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
 [void][SelectionMouse]::SetCursorPos($original.X, $original.Y)
}
