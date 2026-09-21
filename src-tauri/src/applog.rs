//! Ghi log của app ra `%LOCALAPPDATA%\cloudsave-g4market\app.log`.
//!
//! Viết tay thay vì kéo thêm crate: chỉ cần ghi từng dòng có giờ và mức độ.
//! File bị cắt đôi khi vượt `MAX_BYTES` để không phình mãi.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};

const MAX_BYTES: u64 = 1024 * 1024;

struct FileLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Log for FileLog {
    fn enabled(&self, m: &Metadata) -> bool {
        // Chỉ log của chính app — thư viện (tao, wry, reqwest…) rất ồn.
        m.level() <= Level::Info && m.target().starts_with("cloudsave")
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let _g = self.lock.lock();
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > MAX_BYTES) {
            let _ = std::fs::rename(&self.path, self.path.with_extension("old.log"));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(
                f,
                "{} {:<5} {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                r.level(),
                r.args()
            );
        }
    }

    fn flush(&self) {}
}

pub fn path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("cloudsave-g4market")
        .join("app.log")
}

pub fn init() {
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let logger = Box::leak(Box::new(FileLog {
        path,
        lock: Mutex::new(()),
    }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}
