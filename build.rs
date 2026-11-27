//! Build script: compiles the Slint UI and embeds the Windows resources
//! (application icon + version info) into every binary.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=ui/app.slint");
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");

    slint_build::compile("ui/app.slint").unwrap();

    embed_resources();
}

/// Windows resources need rc.exe from the Windows SDK. Without it the build still
/// succeeds, just without the icon and the version tab.
fn embed_resources() {
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found, building without icon/version resources");
        return;
    };
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let (major, minor, patch) = parse_version(&version);
    let icon = manifest.join("assets").join("app.ico");

    // rc.exe does not treat backslashes in a quoted path as literal separators,
    // so the icon is referenced with forward slashes.
    let icon = icon.display().to_string().replace('\\', "/");

    let script = vec![
        format!("1 ICON \"{icon}\""),
        String::from("1 VERSIONINFO"),
        format!("FILEVERSION {major},{minor},{patch},0"),
        format!("PRODUCTVERSION {major},{minor},{patch},0"),
        String::from("FILEFLAGSMASK 0x3fL"),
        String::from("FILEFLAGS 0x0L"),
        String::from("FILEOS 0x40004L"),
        String::from("FILETYPE 0x1L"),
        String::from("FILESUBTYPE 0x0L"),
        String::from("BEGIN"),
        String::from("  BLOCK \"StringFileInfo\""),
        String::from("  BEGIN"),
        String::from("    BLOCK \"040904b0\""),
        String::from("    BEGIN"),
        String::from("      VALUE \"CompanyName\", \"NoKeys\""),
        String::from("      VALUE \"FileDescription\", \"NoKeys\""),
        format!("      VALUE \"FileVersion\", \"{version}\""),
        String::from("      VALUE \"InternalName\", \"nokeys\""),
        String::from("      VALUE \"OriginalFilename\", \"nokeys.exe\""),
        String::from("      VALUE \"ProductName\", \"NoKeys\""),
        format!("      VALUE \"ProductVersion\", \"{version}\""),
        String::from("    END"),
        String::from("  END"),
        String::from("  BLOCK \"VarFileInfo\""),
        String::from("  BEGIN"),
        String::from("    VALUE \"Translation\", 0x409, 1200"),
        String::from("  END"),
        String::from("END"),
    ]
    .join("\r\n");

    let rc_path = out.join("nokeys.rc");
    if std::fs::write(&rc_path, script).is_err() {
        return;
    }
    let res = out.join("nokeys.res");
    match Command::new(&rc)
        .current_dir(&manifest)
        .args(["/nologo", "/fo"])
        .arg(&res)
        .arg(&rc_path)
        .output()
    {
        Ok(o) if o.status.success() => {
            println!("cargo:rustc-link-arg-bins={}", res.display());
        }
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout);
            println!("cargo:warning=rc.exe failed: {}", text.trim());
        }
        Err(e) => println!("cargo:warning=rc.exe could not run: {e}"),
    }
}

fn parse_version(text: &str) -> (u32, u32, u32) {
    let mut parts = text.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

fn find_rc() -> Option<PathBuf> {
    for root in [
        PathBuf::from("C:/Program Files (x86)/Windows Kits/10/bin"),
        PathBuf::from("C:/Program Files/Windows Kits/10/bin"),
    ] {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for dir in dirs.iter().rev() {
            let candidate = dir.join("x64").join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
