//! Ảnh nền người dùng tự thêm.
//!
//! Ảnh được CHÉP vào `%LOCALAPPDATA%\cloudsave-g4market\backgrounds\` — người
//! dùng có dời hay xoá ảnh gốc thì nền vẫn còn. Giao diện đọc thẳng file qua
//! asset protocol của Tauri (không mã hoá base64), nên bao nhiêu ảnh cũng được.
//!
//! Tên file là `<thời điểm thêm>-<ngẫu nhiên>.<đuôi>`: sắp theo tên = theo thứ
//! tự thêm, và không bao giờ trùng.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};

/// Ảnh lớn hơn thế này chắc là nhầm file (ảnh RAW, video…).
const MAX_BYTES: u64 = 40 * 1024 * 1024;
const EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "avif", "gif", "bmp"];

#[derive(Debug, Clone, Serialize)]
pub struct BgImage {
    /// Tên file trong thư mục nền — dùng làm id khi xoá.
    pub id: String,
    /// Đường dẫn đầy đủ, để giao diện đổi sang URL asset.
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct AddReport {
    pub added: Vec<BgImage>,
    /// File bị bỏ qua, kèm lý do.
    pub skipped: Vec<String>,
}

pub fn dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("cloudsave-g4market")
        .join("backgrounds")
}

fn is_image(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn list_in(dir: &Path) -> Vec<BgImage> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<BgImage> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_image(p))
        .filter_map(|p| {
            Some(BgImage {
                id: p.file_name()?.to_str()?.to_string(),
                path: p.to_string_lossy().into_owned(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

pub fn add_in(dir: &Path, sources: &[PathBuf]) -> Result<AddReport> {
    std::fs::create_dir_all(dir)?;
    let mut report = AddReport {
        added: Vec::new(),
        skipped: Vec::new(),
    };
    for (i, src) in sources.iter().enumerate() {
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| src.display().to_string());
        if !is_image(src) {
            report.skipped.push(format!("{name}: không phải ảnh"));
            continue;
        }
        match std::fs::metadata(src) {
            Ok(m) if m.len() > MAX_BYTES => {
                report.skipped.push(format!("{name}: lớn hơn 40 MB"));
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                report.skipped.push(format!("{name}: {e}"));
                continue;
            }
        }
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("jpg").to_ascii_lowercase();
        let id = format!(
            "{}-{:03}-{}.{ext}",
            chrono::Local::now().format("%Y%m%d%H%M%S"),
            i,
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let dst = dir.join(&id);
        if let Err(e) = std::fs::copy(src, &dst) {
            report.skipped.push(format!("{name}: {e}"));
            continue;
        }
        report.added.push(BgImage {
            id,
            path: dst.to_string_lossy().into_owned(),
        });
    }
    Ok(report)
}

pub fn remove_in(dir: &Path, id: &str) -> Result<()> {
    // Id là tên file trong thư mục nền, không bao giờ là đường dẫn.
    if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") || !is_image(Path::new(id)) {
        return Err(Error::Other(format!("id ảnh nền không hợp lệ: {id}")));
    }
    match std::fs::remove_file(dir.join(id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("cs-bg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn adds_copies_lists_in_order_and_removes() {
        let src = tmp();
        let a = src.join("Núi.JPG");
        let b = src.join("b.png");
        let txt = src.join("notes.txt");
        std::fs::write(&a, b"aaa").unwrap();
        std::fs::write(&b, b"bbb").unwrap();
        std::fs::write(&txt, b"x").unwrap();

        let dir = tmp().join("backgrounds");
        let r = add_in(&dir, &[a.clone(), txt, b]).unwrap();
        assert_eq!(r.added.len(), 2);
        assert_eq!(r.skipped.len(), 1, "file .txt bị bỏ qua");
        assert!(r.added[0].id.ends_with(".jpg"), "đuôi được viết thường");

        // Xoá ảnh gốc: bản chép vẫn còn.
        std::fs::remove_file(&a).unwrap();
        let list = list_in(&dir);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, r.added[0].id, "giữ thứ tự thêm");
        assert_eq!(std::fs::read(&list[0].path).unwrap(), b"aaa");

        remove_in(&dir, &list[0].id).unwrap();
        assert_eq!(list_in(&dir).len(), 1);
    }

    #[test]
    fn remove_rejects_paths() {
        let dir = tmp();
        assert!(remove_in(&dir, "../settings.json").is_err());
        assert!(remove_in(&dir, "a\\b.png").is_err());
        assert!(remove_in(&dir, "settings.json").is_err(), "chỉ xoá file ảnh");
        assert!(remove_in(&dir, "khong-co.png").is_ok(), "đã không có thì thôi");
    }
}
