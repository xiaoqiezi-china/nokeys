// Lightweight opt-in tracing. Never call this from inside a low-level hook
// callback: file I/O there can exceed the hook timeout and Windows silently
// removes the hook.

use std::io::Write;
use std::sync::OnceLock;

static TRACE_FILE: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();

fn file() -> Option<&'static std::path::PathBuf> {
    TRACE_FILE
        .get_or_init(|| {
            if std::env::var("NOKEYS_TRACE").is_err() {
                return None;
            }
            let dir = crate::config::config_dir();
            std::fs::create_dir_all(&dir).ok()?;
            Some(dir.join("trace.log"))
        })
        .as_ref()
}

pub fn trace(args: std::fmt::Arguments) {
    let Some(path) = file() else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{args}");
    }
}

#[macro_export]
macro_rules! ntrace {
    ($($arg:tt)*) => {
        $crate::trace::trace(format_args!($($arg)*))
    };
}
