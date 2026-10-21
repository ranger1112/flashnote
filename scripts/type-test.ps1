$process = Get-Process flashnote -ErrorAction SilentlyContinue
if (-not $process) {
  Write-Error "process not found"
  exit 1
}

$wshell = New-Object -ComObject WScript.Shell
$activated = $wshell.AppActivate($process.Id)
Write-Output ("activated=" + $activated)
Start-Sleep -Milliseconds 400

$wshell.SendKeys("hello from automated test")
Start-Sleep -Milliseconds 400
$wshell.SendKeys("^~")
Start-Sleep -Milliseconds 1200

Write-Output "keys sent"
