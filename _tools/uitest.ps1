# UI automation helpers for local end-to-end testing of NoKeys.
# Load with:  . D:\project\nokeys\_tools\uitest.ps1
param()

Add-Type -AssemblyName System.Windows.Forms, System.Drawing

if (-not ('KI' -as [type])) {
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Collections.Generic;
public class KI {
  [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
  [DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, int dx, int dy, uint dwData, UIntPtr dwExtraInfo);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string cls, string title);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr parent, IntPtr after, string cls, string title);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern int GetWindowLongW(IntPtr h, int idx);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder sb, int max);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  public static void KeyDown(byte vk, bool side) { keybd_event(vk, 0, 0, UIntPtr.Zero); }
  public static void KeyUp(byte vk) { keybd_event(vk, 0, 2, UIntPtr.Zero); }
  public static void Click(int x, int y) {
    SetCursorPos(x, y);
    System.Threading.Thread.Sleep(40);
    mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(40);
    mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);
  }
  public static string[] Windows() {
    var list = new List<string>();
    EnumWindows((h, l) => {
      var sb = new StringBuilder(512);
      GetWindowTextW(h, sb, 512);
      var t = sb.ToString();
      if (t.Length > 0 && IsWindowVisible(h)) {
        uint pid; GetWindowThreadProcessId(h, out pid);
        RECT r; GetWindowRect(h, out r);
        list.Add(string.Format("{0}|{1}|{2}|{3},{4},{5},{6}", h.ToInt64(), pid, t, r.Left, r.Top, r.Right, r.Bottom));
      }
      return true;
    }, IntPtr.Zero);
    return list.ToArray();
  }
}
"@
}

function Shot([string]$path) {
  $b = [System.Windows.Forms.SystemInformation]::VirtualScreen
  $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($b.Left, $b.Top, 0, 0, $bmp.Size)
  $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
  Write-Output "shot -> $path"
}

function WindowsList { [KI]::Windows() }
function Close-Win([string]$title) { $h = [KI]::FindWindowW($null, $title); if ($h -ne [IntPtr]::Zero) { [KI]::PostMessageW($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero); $true } else { $false } }
function KeyDown([int]$vk) { [KI]::KeyDown([byte]$vk, $false) }
function KeyUp([int]$vk) { [KI]::KeyUp([byte]$vk) }
function Chord([int[]]$vks, [int]$holdMs = 120) {
  foreach ($v in $vks) { [KI]::KeyDown([byte]$v, $false); Start-Sleep -Milliseconds 12 }
  Start-Sleep -Milliseconds $holdMs
  for ($i = $vks.Count - 1; $i -ge 0; $i--) { [KI]::KeyUp([byte]$vks[$i]); Start-Sleep -Milliseconds 12 }
}
function ClickAt([int]$x, [int]$y) { [KI]::Click($x, $y) }

# ---- window lookup by process name (titles may contain invisible chars) ----
function WinForProcess([string]$procName) {
  $procs = @(Get-Process -Name $procName -ErrorAction SilentlyContinue)
  foreach ($pr in $procs) {
    $found = $null
    [void][KI]::EnumWindows({ param($h, $l)
      $pid2 = 0
      [void][KI]::GetWindowThreadProcessId($h, [ref]$pid2)
      if ($pid2 -eq $pr.Id -and [KI]::IsWindowVisible($h)) {
        $r = New-Object KI+RECT
        [void][KI]::GetWindowRect($h, [ref]$r)
        $w = $r.Right - $r.Left
        if ($w -gt 300) { $script:foundWin = $h }
      }
      return $true
    }, [IntPtr]::Zero)
    if ($script:foundWin) { $found = $script:foundWin; $script:foundWin = $null; return $found }
  }
  return [IntPtr]::Zero
}
function WinRect($h) {
  $r = New-Object KI+RECT
  [void][KI]::GetWindowRect($h, [ref]$r)
  return @($r.Left, $r.Top, $r.Right, $r.Bottom)
}
function SetClip([string]$text) {
  Set-Clipboard -Value $text
  Start-Sleep -Milliseconds 120
}
function ChordKey([int]$mod, [int]$vk) {
  [KI]::KeyDown([byte]$mod, $false); Start-Sleep -Milliseconds 40
  [KI]::KeyDown([byte]$vk, $false); Start-Sleep -Milliseconds 40
  [KI]::KeyUp([byte]$vk); Start-Sleep -Milliseconds 30
  [KI]::KeyUp([byte]$mod)
}

if (-not ('LASTIN' -as [type])) {
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public class LASTIN {
  [StructLayout(LayoutKind.Sequential)] public struct LASTINPUTINFO { public uint cbSize; public uint dwTime; }
  [DllImport("user32.dll")] public static extern bool GetLastInputInfo(ref LASTINPUTINFO plii);
  [DllImport("kernel32.dll")] public static extern uint GetTickCount();
  public static uint AgeMs() {
    LASTINPUTINFO li = new LASTINPUTINFO();
    li.cbSize = (uint)Marshal.SizeOf(typeof(LASTINPUTINFO));
    if (!GetLastInputInfo(ref li)) return 999999;
    return GetTickCount() - li.dwTime;
  }
}
"@
}
# Injects one harmless key and reports whether the OS actually saw it. A key
# swallowed by a low-level hook never reaches the input queue, so the
# last-input timestamp does not move.
function Test-KeyDelivered {
  param([int]$vk = 0x41)
  Start-Sleep -Milliseconds 600
  $before = [LASTIN]::AgeMs()
  [KI]::KeyDown([byte]$vk, $false)
  Start-Sleep -Milliseconds 40
  [KI]::KeyUp([byte]$vk)
  Start-Sleep -Milliseconds 300
  $after = [LASTIN]::AgeMs()
  $delivered = ($after -lt 500)
  Write-Output ("    key delivered={0} (age {1} -> {2})" -f $delivered, $before, $after)
  return $delivered
}

function Get-SwallowCount {
  $log = Join-Path $env:APPDATA 'NoKeys\trace.log'
  $line = (Get-Content $log -ErrorAction SilentlyContinue | Select-String -Pattern 'swallowed=' | Select-Object -Last 1).Line
  if (-not $line) { return 0 }
  if ($line -match 'swallowed=(\d+)') { return [int]$Matches[1] }
  return 0
}
# Ground truth for "is the app eating my keyboard": inject one key and check the
# hook's own swallow counter. GetLastInputInfo is useless here because it tracks
# the hardware event, which advances even when a hook swallows it.
function Test-Swallowed {
  param([string]$label = '')
  $before = Get-SwallowCount
  [KI]::KeyDown([byte]0x41, $false); Start-Sleep -Milliseconds 40; [KI]::KeyUp([byte]0x41)
  Start-Sleep -Milliseconds 2400
  $after = Get-SwallowCount
  $ate = $after -gt $before
  Write-Output ("    {0,-42} ate_key={1} ({2}->{3})" -f $label, $ate, $before, $after)
  return $ate
}
