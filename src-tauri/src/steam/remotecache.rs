//! Parser cho `<steam>/userdata/<accountId>/<appId>/remotecache.vdf`.
//!
//! Steam ghi sẵn ở đây danh sách file Cloud cùng size, SHA-1 và mtime. Ta lấy
//! được hai thứ miễn phí:
//!
//!   1. Một danh sách file save **đã được Steam xác nhận**, chính xác hơn mọi
//!      suy đoán từ glob.
//!   2. Change-detection: so `size` + `localtime` với đĩa để biết file nào đổi
//!      mà không phải hash lại toàn bộ thư mục.
//!
//! Lưu ý: SHA ở đây là SHA-1 của Steam. Ta vẫn tự tính SHA-256 khi thật sự
//! upload — SHA-1 chỉ dùng để phát hiện thay đổi, không dùng làm khoá nội dung.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::roots::{RootContext, RootToken};
use super::textvdf::{self, Node};
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFile {
    /// Đường dẫn tương đối so với root, luôn dùng `/`.
    pub rel_path: String,
    pub root_numeric: u32,
    pub root_token: Option<RootToken>,
    pub size: u64,
    /// SHA-1 hex của Steam, dùng cho change-detection.
    pub steam_sha1: Option<String>,
    /// Unix epoch giây, giờ sửa đổi bản local theo Steam.
    pub local_time: Option<i64>,
    pub sync_state: Option<i32>,
}

impl CachedFile {
    /// Ghép thành đường dẫn tuyệt đối trên máy hiện tại.
    pub fn absolute(&self, ctx: &RootContext) -> Option<PathBuf> {
        let base = ctx.resolve(self.root_token?)?;
        Some(base.join(self.rel_path.replace('/', std::path::MAIN_SEPARATOR_STR)))
    }
}

#[derive(Debug, Clone, Default)]
pub struct RemoteCache {
    pub app_id: u32,
    pub change_number: Option<i64>,
    pub files: Vec<CachedFile>,
}

/// Các khoá vô hướng ở cấp app — không phải tên file.
const SCALAR_KEYS: [&str; 3] = ["ChangeNumber", "OSType", "ostype"];

pub fn parse(text: &str) -> Result<RemoteCache> {
    let kv = textvdf::parse(text)?;
    let Some((app_key, body)) = kv.unwrap_single_root() else {
        return Ok(RemoteCache::default());
    };

    let mut out = RemoteCache {
        app_id: app_key.parse().unwrap_or(0),
        change_number: body.str("ChangeNumber").and_then(|s| s.parse().ok()),
        files: Vec::new(),
    };

    for (key, node) in body.iter() {
        if SCALAR_KEYS.iter().any(|k| k.eq_ignore_ascii_case(key)) {
            continue;
        }
        let Node::Obj(f) = node else { continue };

        let root_numeric = f
            .str("root")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let root_token = RootToken::from_numeric(root_numeric);
        if root_token.is_none() {
            // Root lạ: bỏ qua thay vì đoán. Đoán sai root nghĩa là khôi phục
            // save vào nhầm thư mục, tệ hơn hẳn việc bỏ sót một file.
            log::warn!(
                "remotecache app {}: root id {} chưa biết, bỏ qua '{}'",
                out.app_id,
                root_numeric,
                key
            );
            continue;
        }

        let rel_path = key.replace('\\', "/");
        if !is_safe_rel_path(&rel_path) {
            log::warn!(
                "remotecache app {}: bỏ qua đường dẫn bất thường '{}'",
                out.app_id,
                key
            );
            continue;
        }

        out.files.push(CachedFile {
            rel_path,
            root_numeric,
            root_token,
            size: f.str("size").and_then(|s| s.parse().ok()).unwrap_or(0),
            steam_sha1: f.str("sha").map(str::to_owned),
            local_time: f.str("localtime").and_then(|s| s.parse().ok()),
            sync_state: f.str("syncstate").and_then(|s| s.parse().ok()),
        });
    }

    Ok(out)
}

/// Chặn path traversal và đường dẫn tuyệt đối trước khi chúng chạm tới
/// filesystem hay Supabase. Cùng bộ quy tắc với `rel_path_safe` trong migration.
pub fn is_safe_rel_path(p: &str) -> bool {
    if p.is_empty() || p.len() > 1024 {
        return false;
    }
    if p.starts_with('/') || p.starts_with('\\') {
        return false;
    }
    // Ổ đĩa Windows, vd `C:`.
    let bytes = p.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return false;
    }
    if p.contains('\0') || p.contains('\\') {
        return false;
    }
    !p.split('/')
        .any(|seg| seg == ".." || seg == "." || seg.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal() {
        assert!(is_safe_rel_path("StardewValley/Saves/123/123"));
        assert!(!is_safe_rel_path("../../Windows/System32/x.dll"));
        assert!(!is_safe_rel_path("/etc/passwd"));
        assert!(!is_safe_rel_path(r"C:\Windows\x"));
        assert!(!is_safe_rel_path("a//b"));
        assert!(!is_safe_rel_path(""));
    }

    #[test]
    fn parses_stardew_shape() {
        let src = r#"
"413150"
{
	"ChangeNumber"		"36"
	"OSType"		"0"
	"StardewValley/Saves/Farm_100000001/Farm_100000001"
	{
		"root"		"4"
		"size"		"4165569"
		"localtime"		"1615691940"
		"sha"		"10d19d1c4e421d382fcb4211af075ee30615ab19"
		"syncstate"		"1"
	}
}
"#;
        let rc = parse(src).unwrap();
        assert_eq!(rc.app_id, 413150);
        assert_eq!(rc.files.len(), 1);
        let f = &rc.files[0];
        assert_eq!(f.root_token, Some(RootToken::WinAppDataRoaming));
        assert_eq!(f.size, 4_165_569);
    }
}
