param(
    [string]$mode = "wheel",
    [string]$hover = "",
    [string]$out = "D:\project\nokeys\shot.png"
)
$env:NOKEYS_AUTOPEN = $mode
if ($hover -ne "") { $env:NOKEYS_HOVER = $hover } else { Remove-Item Env:\NOKEYS_HOVER -ErrorAction SilentlyContinue }
$log = "D:\project\nokeys\run.log"
Remove-Item $log -ErrorAction SilentlyContinue
$p = Start-Process -FilePath 'D:\project\nokeys\target\debug\nokeys.exe' -PassThru -RedirectStandardError $log
Start-Sleep -Seconds 4
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
Stop-Process -Id $p.Id -Force
Write-Host "--- log ---"
Get-Content $log
