param(
    [string]$src = "D:\project\nokeys\shot.png",
    [string]$dst = "D:\project\nokeys\crop.png",
    [int]$x = 0, [int]$y = 0, [int]$w = 600, [int]$h = 600
)
Add-Type -AssemblyName System.Drawing
$img = [System.Drawing.Image]::FromFile($src)
$x = [Math]::Max(0, [Math]::Min($x, $img.Width - $w))
$y = [Math]::Max(0, [Math]::Min($y, $img.Height - $h))
$rect = New-Object System.Drawing.Rectangle($x, $y, $w, $h)
$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.DrawImage($img, (New-Object System.Drawing.Rectangle(0, 0, $w, $h)), $rect, [System.Drawing.GraphicsUnit]::Pixel)
$bmp.Save($dst, [System.Drawing.Imaging.ImageFormat]::Png)
$img.Dispose()
Write-Host "cropped -> $dst"
