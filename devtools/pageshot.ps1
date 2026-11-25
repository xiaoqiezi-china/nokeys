param(
    [string]$page = "",
    [string]$out = "D:\project\nokeys\page.png",
    [int]$x = 0, [int]$y = 0, [int]$w = 0, [int]$h = 0
)
if ($page -ne "") { $env:NOKEYS_PAGE = $page } else { Remove-Item Env:\NOKEYS_PAGE -ErrorAction SilentlyContinue }
Remove-Item Env:\NOKEYS_AUTOPEN -ErrorAction SilentlyContinue
$p = Start-Process -FilePath 'D:\project\nokeys\target\debug\nokeys.exe' -PassThru
Start-Sleep -Seconds 4
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
if ($w -gt 0) {
    $crop = New-Object System.Drawing.Bitmap($w, $h)
    $gc = [System.Drawing.Graphics]::FromImage($crop)
    $rect = New-Object System.Drawing.Rectangle($x, $y, $w, $h)
    $gc.DrawImage($bmp, (New-Object System.Drawing.Rectangle(0, 0, $w, $h)), $rect, [System.Drawing.GraphicsUnit]::Pixel)
    $crop.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
} else {
    $bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
}
Stop-Process -Id $p.Id -Force
Write-Host "saved $out"
