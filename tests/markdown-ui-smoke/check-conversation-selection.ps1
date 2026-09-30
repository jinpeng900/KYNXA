param([switch]$CheckClipboard)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class ConversationMouse {
 [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hwnd, IntPtr insertAfter, int x, int y, int cx, int cy, uint flags);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
 [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint x, uint y, uint data, UIntPtr extra);
 [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT point);
 [DllImport("user32.dll")] public static extern void keybd_event(byte key, byte scan, uint flags, UIntPtr extra);
 public struct POINT { public int X; public int Y; }
}
'@
[void][ConversationMouse]::SetProcessDpiAwarenessContext([IntPtr](-4))
$root = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst(
 [System.Windows.Automation.TreeScope]::Children,
 [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, 'KYNXA Conversation selection smoke'))
if (-not $root) { throw 'Launch MarkdownUiSmoke.exe --conversation first.' }
function Find-Id($id) {
 $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants,
  [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, $id))
}
function Selected($element) {
 $pattern = $element.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
 ($pattern.GetSelection() | ForEach-Object { $_.GetText(-1) }) -join ''
}
function Move-Pointer($x, $y) { [void][ConversationMouse]::SetCursorPos([int]$x, [int]$y) }
function Button($down) { [ConversationMouse]::mouse_event($(if ($down) { 2 } else { 4 }), 0, 0, 0, [UIntPtr]::Zero) }
function Aggregate { (Find-Id 'SelectedConversation').Current.Name }
function Verify-Frozen {
 Start-Sleep -Milliseconds 180
 $frozen = Aggregate
 if ($frozen.Length -eq 0) { throw 'Selection is empty after release.' }
 for ($i = 0; $i -lt 10; $i++) {
  Move-Pointer ($bounds.Left + 190 + $i * 12) ($bounds.Top + 65 + $i * 30)
  Start-Sleep -Milliseconds 70
  if ((Aggregate) -ne $frozen) { throw 'Mouse movement changed the released selection.' }
 }
 return $frozen
}
$original = New-Object ConversationMouse+POINT
[void][ConversationMouse]::GetCursorPos([ref]$original)
[void][ConversationMouse]::SetWindowPos([IntPtr]$root.Current.NativeWindowHandle, [IntPtr](-1), 0, 0, 0, 0, 3)
[void][ConversationMouse]::SetForegroundWindow([IntPtr]$root.Current.NativeWindowHandle)
$scrollElement = Find-Id 'ConversationScroll'
$scroll = $scrollElement.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern)
$bounds = $scrollElement.Current.BoundingRectangle
try {
 $scroll.SetScrollPercent(-1, 0)
 Start-Sleep -Milliseconds 250
 $messages = @(0..19 | ForEach-Object { Find-Id "Message$_" })
 if ($messages -contains $null) { throw 'Message text was virtualized out of the conversation.' }
 $first = $messages[0].Current.BoundingRectangle
 $last = $messages[3].Current.BoundingRectangle
 $startX = $first.Left + 2; $startY = $first.Top + 10
 $endX = $last.Left + 300; $endY = $last.Bottom - 6
 $forward = ''
 foreach ($direction in @('down', 'up')) {
  if ($direction -eq 'down') { $x1=$startX; $y1=$startY; $x2=$endX; $y2=$endY }
  else { $x1=$endX; $y1=$endY; $x2=$startX; $y2=$startY }
  Move-Pointer $x1 $y1
  Button $true
  for ($step=1; $step -le 20; $step++) {
   Move-Pointer ($x1+($x2-$x1)*$step/20) ($y1+($y2-$y1)*$step/20)
   Start-Sleep -Milliseconds 30
  }
  Start-Sleep -Milliseconds 150
  Button $false
  $text = Verify-Frozen
  foreach ($index in 0..3) {
   if ((Selected $messages[$index]).Length -eq 0) { throw "Message $index was not selected during $direction drag." }
  }
  $markers = @('USER_00', 'ASSISTANT_01', 'USER_02', 'ASSISTANT_03')
  $position = -1
  foreach ($marker in $markers) {
   $next = $text.IndexOf($marker, [StringComparison]::Ordinal)
   if ($next -le $position) { throw "Copy order is wrong or missing $marker during $direction drag." }
   $position = $next
  }
  $expected = (0..3 | ForEach-Object { Selected $messages[$_] }) -join "`r`n`r`n"
  if ($text -ne $expected) { throw 'Aggregate copy text differs from visible selected text.' }
  if ($direction -eq 'down') { $forward = $text }
  elseif ($forward -ne $text) { throw 'The same selection endpoints produced different text when dragging upwards.' }
  Write-Output "PASS $direction : four alternating user/assistant messages; ordered copy; frozen after release"
 }
 Move-Pointer $startX $startY
 Button $true
 Move-Pointer ($bounds.Left+300) ($bounds.Bottom-4)
 Start-Sleep -Milliseconds 1900
 Button $false
 $extended = Verify-Frozen
 if ($scroll.Current.VerticalScrollPercent -le 0 -or -not $extended.Contains('USER_12')) { throw 'Cross-message edge scrolling failed.' }
 Write-Output 'PASS cross-message auto-scroll includes initially offscreen messages; released selection stays fixed'
 if ($CheckClipboard) {
  Add-Type -AssemblyName System.Windows.Forms
  $originalClipboard = [System.Windows.Forms.Clipboard]::GetDataObject()
  $savedClipboard = New-Object System.Windows.Forms.DataObject
  if ($originalClipboard) {
   foreach ($format in $originalClipboard.GetFormats($false)) {
    $savedClipboard.SetData($format, $false, $originalClipboard.GetData($format, $false))
   }
  }
  try {
   [ConversationMouse]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
   [ConversationMouse]::keybd_event(0x43, 0, 0, [UIntPtr]::Zero)
   [ConversationMouse]::keybd_event(0x43, 0, 2, [UIntPtr]::Zero)
   [ConversationMouse]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
   Start-Sleep -Milliseconds 300
   if ([System.Windows.Forms.Clipboard]::GetText() -ne $extended) { throw 'Ctrl+C did not copy the complete ordered conversation selection.' }
   Write-Output 'PASS Ctrl+C copies the complete selection across user and assistant messages'
   [System.Windows.Forms.Clipboard]::SetText('KYNXA selection test sentinel')
   $lastMessage = $messages[19].Current.BoundingRectangle
   Move-Pointer ($lastMessage.Left + 100) ($lastMessage.Top + 12)
   [ConversationMouse]::mouse_event(8, 0, 0, 0, [UIntPtr]::Zero)
   [ConversationMouse]::mouse_event(16, 0, 0, 0, [UIntPtr]::Zero)
   Start-Sleep -Milliseconds 300
   $copy = [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
    [System.Windows.Automation.TreeScope]::Descendants,
    [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::MenuItem)) |
    Where-Object { $_.Current.ProcessId -eq $root.Current.ProcessId -and -not $_.Current.IsOffscreen } | Select-Object -First 1
   if (-not $copy) { throw 'Copy context menu did not open.' }
   $copyBounds = $copy.Current.BoundingRectangle
   Move-Pointer ($copyBounds.Left+$copyBounds.Width/2) ($copyBounds.Top+$copyBounds.Height/2)
   Button $true
   Button $false
   Start-Sleep -Milliseconds 300
   if ([System.Windows.Forms.Clipboard]::GetText() -ne $extended) { throw 'Context-menu copy did not copy the complete selection.' }
   Write-Output 'PASS context-menu copy includes the complete cross-message selection'
  } finally {
   [ConversationMouse]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
   if ($originalClipboard) { [System.Windows.Forms.Clipboard]::SetDataObject($savedClipboard, $true) }
   else { [System.Windows.Forms.Clipboard]::Clear() }
  }
 }
 foreach ($target in @('blank', 'input', 'sidebar', 'selected-text')) {
  $scroll.SetScrollPercent(-1, 0)
  Start-Sleep -Milliseconds 250
  $first = $messages[0].Current.BoundingRectangle
  $last = $messages[3].Current.BoundingRectangle
  $startX = $first.Left + 2; $startY = $first.Top + 10
  $endX = $last.Left + 300; $endY = $last.Bottom - 6
  Move-Pointer $startX $startY
  Button $true
  for ($step = 1; $step -le 15; $step++) {
   Move-Pointer ($startX + ($endX - $startX) * $step / 15) ($startY + ($endY - $startY) * $step / 15)
   Start-Sleep -Milliseconds 30
  }
  Button $false
  [void](Verify-Frozen)
  if ($target -eq 'blank') { $clickX = $bounds.Left + 8; $clickY = $bounds.Top + 60 }
  elseif ($target -eq 'selected-text') { $clickX = $first.Left + 110; $clickY = $first.Top + 10 }
  else {
   $targetId = if ($target -eq 'input') { 'ConversationInput' } else { 'SidebarAction' }
   $clickBounds = (Find-Id $targetId).Current.BoundingRectangle
   $clickX = $clickBounds.Left + $clickBounds.Width / 2; $clickY = $clickBounds.Top + $clickBounds.Height / 2
  }
  Move-Pointer $clickX $clickY
  Button $true
  Start-Sleep -Milliseconds 80
  Button $false
  Start-Sleep -Milliseconds 250
  if ((Aggregate).Length -ne 0 -or (Find-Id 'SelectionState').Current.Name -ne 'idle') {
   throw "Click $target left conversation selection or highlight active."
  }
  foreach ($message in $messages) {
   if ((Selected $message).Length -ne 0) { throw "Click $target left a native message selection active." }
  }
  for ($step = 0; $step -lt 5; $step++) {
   Move-Pointer ($bounds.Left + 200 + $step * 15) ($bounds.Top + 80 + $step * 20)
   Start-Sleep -Milliseconds 60
  }
  if ((Aggregate).Length -ne 0 -or (Find-Id 'SelectionState').Current.Name -ne 'idle') {
   throw "Selection reappeared on hover after clicking $target."
  }
  Write-Output "PASS click $target clears native and conversation highlights; selection stays cleared on hover"
 }
} finally {
 Button $false
 Move-Pointer $original.X $original.Y
 [void][ConversationMouse]::SetWindowPos([IntPtr]$root.Current.NativeWindowHandle, [IntPtr](-2), 0, 0, 0, 0, 3)
}
