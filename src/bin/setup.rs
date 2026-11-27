#![windows_subsystem = "windows"]

//! Installer / uninstaller for NoKeys.
//!
//! One self-contained EXE: the application is appended to this file by
//! _tools/pack.ps1 followed by a 16-byte trailer (payload length + magic).
//! Everything is installed for the current user only, so no elevation is asked
//! for and nothing outside the user's profile is touched.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::core::{Interface, HSTRING, PCWSTR, PWSTR, GUID};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, REG_DWORD,
    REG_SZ,
};
use windows::Win32::UI::Shell::{
    IShellLinkW, SHGetKnownFolderPath, ShellLink, FOLDERID_Desktop, FOLDERID_Programs,
    KF_FLAG_DEFAULT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO,
    MESSAGEBOX_STYLE,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TRAILER_LEN: u64 = 16;
/// Payload trailer: 8 bytes of little-endian length, then these 8 bytes.
const MAGIC: &[u8; 8] = b"NOKEYS01";
const APP_EXE: &str = "nokeys.exe";
const UNINSTALL_EXE: &str = "uninstall.exe";
const APP_NAME: &str = "NoKeys";
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let uninstall_mode = std::env::args().any(|a| a.eq_ignore_ascii_case("--uninstall"))
        || std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
            .is_some_and(|n| n.starts_with("uninstall"));

    let outcome = if uninstall_mode { do_uninstall() } else { do_install() };
    if let Err(e) = outcome {
        message(&format!("{APP_NAME} {VERSION}\n\n{e}"), MB_ICONERROR);
    }
}

// ---------------- install ----------------

fn do_install() -> Result<(), String> {
    let self_exe = std::env::current_exe().map_err(|e| format!("无法定位安装程序：{e}"))?;
    let (app_bytes, setup_bytes) = split_package(&self_exe)?;
    let dir = install_dir();
    let app_exe = dir.join(APP_EXE);

    if !ask(&format!(
        "{APP_NAME} {VERSION}\n\n将安装到：\n{}\n\n开始安装？",
        dir.display()
    )) {
        return Ok(());
    }

    // A running copy holds its exe open and would write its config afterwards.
    stop_running();

    fs::create_dir_all(&dir).map_err(|e| format!("无法创建目录 {}：{e}", dir.display()))?;
    fs::write(&app_exe, &app_bytes).map_err(|e| format!("无法写入 {}：{e}", app_exe.display()))?;
    // The uninstaller is this installer without the payload, so it stays small.
    let uninstaller = dir.join(UNINSTALL_EXE);
    fs::write(&uninstaller, &setup_bytes).map_err(|e| format!("无法写入 {}：{e}", uninstaller.display()))?;

    create_shortcuts(&app_exe)?;
    write_uninstall_entry(&dir, &app_exe, &uninstaller)?;

    if ask(&format!(
        "安装完成。\n\n程序：{}\n快捷方式：开始菜单 + 桌面\n\n是否立即启动 {APP_NAME}？",
        app_exe.display()
    )) {
        let _ = Command::new(&app_exe).spawn();
    }
    Ok(())
}

// ---------------- uninstall ----------------

fn do_uninstall() -> Result<(), String> {
    let dir = install_dir();
    if !ask(&format!(
        "{APP_NAME} {VERSION}\n\n将从以下位置卸载：\n{}\n\n确定卸载？",
        dir.display()
    )) {
        return Ok(());
    }
    stop_running();
    remove_shortcuts();
    let key = HSTRING::from(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\NoKeys");
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &key);
    }
    // The running uninstaller cannot delete itself, so the last step is handed to
    // a short-lived shell that waits for this process to exit.
    let script = format!(
        "ping -n 2 127.0.0.1 >nul & del /f /q \"{}\" \"{}\" & rmdir \"{}\"",
        dir.join(APP_EXE).display(),
        dir.join(UNINSTALL_EXE).display(),
        dir.display()
    );
    let _ = Command::new("cmd")
        .arg("/C")
        .raw_arg(script)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    message(
        &format!("{APP_NAME} 已卸载。\n\n配置保留在 %APPDATA%\\NoKeys。"),
        MB_ICONINFORMATION,
    );
    Ok(())
}

// ---------------- files ----------------

fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Programs").join(APP_NAME)
}

/// Splits this file into (application bytes, installer bytes without payload).
fn split_package(path: &Path) -> Result<(Vec<u8>, Vec<u8>), String> {
    const BAD: &str = "安装包不完整。请重新下载。";
    let mut file = File::open(path).map_err(|e| format!("无法读取安装包：{e}"))?;
    let total = file.metadata().map_err(|e| format!("无法读取安装包：{e}"))?.len();
    if total < TRAILER_LEN {
        return Err(BAD.into());
    }
    file.seek(SeekFrom::End(-(TRAILER_LEN as i64)))
        .map_err(|e| format!("无法读取安装包：{e}"))?;
    let mut trailer = [0u8; TRAILER_LEN as usize];
    file.read_exact(&mut trailer).map_err(|e| format!("无法读取安装包：{e}"))?;
    if &trailer[8..] != MAGIC {
        return Err(BAD.into());
    }
    let payload_len = i64::from_le_bytes(trailer[..8].try_into().unwrap());
    let setup_len = total as i64 - TRAILER_LEN as i64 - payload_len;
    if payload_len <= 0 || setup_len <= 0 {
        return Err(BAD.into());
    }

    let mut setup = vec![0u8; setup_len as usize];
    file.seek(SeekFrom::Start(0)).map_err(|e| format!("无法读取安装包：{e}"))?;
    file.read_exact(&mut setup).map_err(|e| format!("无法读取安装包：{e}"))?;

    let mut app = vec![0u8; payload_len as usize];
    file.read_exact(&mut app).map_err(|e| format!("无法读取安装包：{e}"))?;
    Ok((app, setup))
}

fn stop_running() {
    let _ = Command::new("taskkill")
        .args(["/IM", APP_EXE, "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

// ---------------- shortcuts ----------------

fn create_shortcuts(app_exe: &Path) -> Result<(), String> {
    let targets = [
        known_folder(&FOLDERID_Desktop)?.join(format!("{APP_NAME}.lnk")),
        known_folder(&FOLDERID_Programs)?.join(format!("{APP_NAME}.lnk")),
    ];
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let target = HSTRING::from(app_exe.as_os_str());
        link.SetPath(&target).map_err(|e| e.to_string())?;
        let _ = link.SetDescription(&HSTRING::from("全局轮盘快捷启动器"));
        let _ = link.SetIconLocation(&target, 0);
        if let Some(dir) = app_exe.parent() {
            let _ = link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()));
        }
        let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
        for lnk in targets {
            file.Save(&HSTRING::from(lnk.as_os_str()), true)
                .map_err(|e| format!("无法创建快捷方式 {}：{e}", lnk.display()))?;
        }
    }
    Ok(())
}

fn remove_shortcuts() {
    for id in [&FOLDERID_Desktop, &FOLDERID_Programs] {
        if let Ok(dir) = known_folder(id) {
            let _ = fs::remove_file(dir.join(format!("{APP_NAME}.lnk")));
        }
    }
}

fn known_folder(id: &GUID) -> Result<PathBuf, String> {
    unsafe {
        let path: PWSTR = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).map_err(|e| e.to_string())?;
        let text = path.to_string().map_err(|e| e.to_string())?;
        CoTaskMemFree(Some(path.0 as *const core::ffi::c_void));
        Ok(PathBuf::from(text))
    }
}

// ---------------- registry ----------------

fn write_uninstall_entry(dir: &Path, app_exe: &Path, uninstaller: &Path) -> Result<(), String> {
    let key = HSTRING::from(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\NoKeys");
    let size_kb = fs::metadata(app_exe).map(|m| m.len() / 1024).unwrap_or(0) as u32;
    let values: [(&str, String); 7] = [
        ("DisplayName", APP_NAME.to_string()),
        ("DisplayVersion", VERSION.to_string()),
        ("Publisher", "NoKeys".to_string()),
        ("DisplayIcon", format!("{},0", app_exe.display())),
        ("InstallLocation", dir.display().to_string()),
        ("UninstallString", format!("\"{}\"", uninstaller.display())),
        ("QuietUninstallString", format!("\"{}\" --uninstall", uninstaller.display())),
    ];
    unsafe {
        let mut handle = HKEY::default();
        RegCreateKeyW(HKEY_CURRENT_USER, &key, &mut handle)
            .ok()
            .map_err(|e| format!("无法写入注册表：{e}"))?;
        for (name, value) in values.iter() {
            set_string(handle, &HSTRING::from(*name), value)?;
        }
        set_dword(handle, &HSTRING::from("NoModify"), 1)?;
        set_dword(handle, &HSTRING::from("NoRepair"), 1)?;
        set_dword(handle, &HSTRING::from("EstimatedSize"), size_kb)?;
        let _ = RegCloseKey(handle);
    }
    Ok(())
}

unsafe fn set_string(key: HKEY, name: &HSTRING, value: &str) -> Result<(), String> {
    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
    RegSetValueExW(key, name, None, REG_SZ, Some(bytes))
        .ok()
        .map_err(|e| format!("无法写入注册表：{e}"))
}

unsafe fn set_dword(key: HKEY, name: &HSTRING, value: u32) -> Result<(), String> {
    let bytes = value.to_le_bytes();
    RegSetValueExW(key, name, None, REG_DWORD, Some(&bytes))
        .ok()
        .map_err(|e| format!("无法写入注册表：{e}"))
}

// ---------------- dialogs ----------------

fn message(text: &str, style: MESSAGEBOX_STYLE) {
    let body = HSTRING::from(text);
    unsafe {
        MessageBoxW(None, PCWSTR(body.as_ptr()), windows::core::w!("NoKeys"), style | MB_OK);
    }
}

fn ask(text: &str) -> bool {
    let body = HSTRING::from(text);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(body.as_ptr()),
            windows::core::w!("NoKeys"),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}
