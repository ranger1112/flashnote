param([string]$Combo = "ctrl+alt+n")

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class HotkeySender {
  [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
}
"@

$parts = $Combo.ToLowerInvariant().Split("+")
$virtualKeys = @()
if ($parts -contains "ctrl") { $virtualKeys += 0x11 }
if ($parts -contains "alt") { $virtualKeys += 0x12 }
if ($parts -contains "shift") { $virtualKeys += 0x10 }

$keyChar = [char]::ToUpperInvariant($parts[-1][0])
$virtualKeys += [int][char]$keyChar

$keyUp = 2
foreach ($vk in $virtualKeys) {
  [HotkeySender]::keybd_event([byte]$vk, 0, 0, [UIntPtr]::Zero)
}
Start-Sleep -Milliseconds 100
[array]::Reverse($virtualKeys)
foreach ($vk in $virtualKeys) {
  [HotkeySender]::keybd_event([byte]$vk, 0, $keyUp, [UIntPtr]::Zero)
}
Start-Sleep -Milliseconds 1200
Write-Output ("sent " + $Combo)
