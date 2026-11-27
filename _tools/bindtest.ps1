$ErrorActionPreference = 'Stop'
try {
  Add-Type -AssemblyName System.Windows.Forms
  Add-Type -AssemblyName System.Drawing
  $log = 'D:\project\nokeys\_shots\form_keys.txt'
  'started' | Set-Content -Path $log
  $f = New-Object System.Windows.Forms.Form
  $f.Text = 'NoKeys Bind Test'
  $f.Width = 460
  $f.Height = 220
  $f.StartPosition = 'CenterScreen'
  $f.KeyPreview = $true
  $label = New-Object System.Windows.Forms.Label
  $label.Dock = 'Fill'
  $label.TextAlign = 'MiddleCenter'
  $label.Text = 'Press any key...'
  $f.Controls.Add($label)
  $f.add_KeyDown({ param($s,$e) Add-Content -Path 'D:\project\nokeys\_shots\form_keys.txt' -Value ('DOWN ' + [int]$e.KeyCode + ' ' + $e.KeyCode); $label.Text = 'Got: ' + $e.KeyCode })
  $f.add_KeyUp({ param($s,$e) Add-Content -Path 'D:\project\nokeys\_shots\form_keys.txt' -Value ('UP ' + [int]$e.KeyCode + ' ' + $e.KeyCode) })
  $t = New-Object System.Windows.Forms.Timer
  $t.Interval = 15000
  $t.add_Tick({ $t.Stop(); $f.Close() })
  $t.Start()
  [void]$f.ShowDialog()
  'ended' | Add-Content -Path 'D:\project\nokeys\_shots\form_keys.txt'
} catch {
  ('ERROR ' + $_.Exception.Message) | Set-Content -Path 'D:\project\nokeys\_shots\form_error.txt'
}