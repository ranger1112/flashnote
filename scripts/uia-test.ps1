Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$process = Get-Process flashnote -ErrorAction SilentlyContinue
if (-not $process) {
  Write-Error "process not found"
  exit 1
}

$root = [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
$condition = New-Object System.Windows.Automation.PropertyCondition(
  [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
  [System.Windows.Automation.ControlType]::Edit
)
$edit = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
if ($null -eq $edit) {
  Write-Error "textarea not found via UI Automation"
  exit 1
}

$pattern = $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
$pattern.SetValue("hello from automated test")
$edit.SetFocus()
Start-Sleep -Milliseconds 300

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class KeySender {
  [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
}
"@

[KeySender]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
Start-Sleep -Milliseconds 200
[KeySender]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
[KeySender]::keybd_event(0x0D, 0, 0, [UIntPtr]::Zero)
[KeySender]::keybd_event(0x0D, 0, 2, [UIntPtr]::Zero)
[KeySender]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 1400

Write-Output "uia input sent"
