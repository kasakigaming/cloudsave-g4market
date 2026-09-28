//! AutoCloud của Steam: file đánh dấu `steam_autocloud.vdf` và thư mục `ac`.
//!
//! Đây là thứ đã "xoá" save sau khi khôi phục. Steam đặt ở gốc thư mục save một
//! file đánh dấu ghi tài khoản nào đang sở hữu chỗ save đó:
//!
//! ```text
//! "steam_autocloud.vdf"
//! {
//!     "accountid"        "111111111"
//! }
//! ```
//!
//! Khi một tài khoản khác đăng nhập, AutoCloud so tài khoản đang đăng nhập với
//! `accountid` trong file này. Lệch nhau thì Steam **dời** (move, không phải
//! copy) mọi file khớp quy tắc UFS sang `userdata/<accountid cũ>/<appid>/ac`,
//! rồi kéo bản của tài khoản mới từ `ac` của chính nó về. Log của Steam:
//!
//! ```text
//! AutoCloud found files for previous user 333333333 in root ...\StardewValley\Saves
//! AutoCloud saved (move) ...\Saves\farm_400000001\farm_400000001
//!   to ...\userdata\333333333\413150\ac\WinAppDataRoaming\StardewValley\Saves\...
//! AutoCloud restoring files from ...\userdata\111111111\413150\ac
//! ```
//!
//! Nghĩa là: khôi phục save của tài khoản A trong lúc Steam đăng nhập B thì lần
//! quét kế tiếp Steam dọn sạch chỗ save — người dùng thấy "app khôi phục xong
//! rồi Steam xoá mất". File không mất, nó nằm trong `ac`.
//!
//! Thư mục `ac` dùng đúng cấu trúc `(root token, đường dẫn tương đối)` như kho
//! của app, nên đọc lại được để cứu file.

use std::path::{Path, PathBuf};

use super::roots::RootToken;
use super::textvdf;
use crate::error::{Error, Result};

/// Tên file đánh dấu Steam đặt ở gốc thư mục save.
pub const MARKER: &str = "steam_autocloud.vdf";

/// File này có phải file đánh dấu của Steam? Nó là sổ sách của Steam, không
/// phải dữ liệu chơi — chụp vào bản lưu rồi khôi phục lại là tự bắn vào chân.
pub fn is_marker(rel_path: &str) -> bool {
    rel_path
        .rsplit('/')
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case(MARKER))
}

/// Tài khoản ghi trong file đánh dấu, nếu đọc được.
pub fn read_account(path: &Path) -> Option<u32> {
    account_in(&std::fs::read_to_string(path).ok()?)
}

fn account_in(text: &str) -> Option<u32> {
    let kv = textvdf::parse(text).ok()?;
    let (_, body) = kv.unwrap_single_root()?;
    body.str("accountid")?.trim().parse().ok()
}

/// Ghi lại file đánh dấu cho `account`. Trả về `true` nếu có thay đổi thật.
///
/// Chỉ ghi khi file đã tồn tại và đang ghi tài khoản khác: app không tạo file
/// đánh dấu ở chỗ Steam chưa từng đặt, vì như thế là bịa ra sổ sách của Steam.
pub fn retag(path: &Path, account: u32) -> Result<bool> {
    if !path.is_file() || read_account(path) == Some(account) {
        return Ok(false);
    }
    std::fs::write(path, marker_text(account))?;
    Ok(true)
}

/// Nội dung file đánh dấu cho một tài khoản, đúng dạng Steam ghi.
pub fn marker_text(account: u32) -> String {
    format!("\"{MARKER}\"\n{{\n\t\"accountid\"\t\t\"{account}\"\n}}\n")
}

/// `userdata/<account>/<appid>/ac` — nơi Steam cất file của tài khoản khác.
pub fn stash_dir(steam_root: &Path, account: u32, app_id: u32) -> PathBuf {
    steam_root
        .join("userdata")
        .join(account.to_string())
        .join(app_id.to_string())
        .join("ac")
}

/// Một file đang bị Steam giữ trong `ac`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Stashed {
    pub root: RootToken,
    pub rel_path: String,
    pub path: PathBuf,
    pub size: u64,
}

/// Mọi file trong `ac` của một tài khoản. Root token đọc từ tên thư mục cấp một
/// (Steam dùng đúng tên root của UFS, vd `WinAppDataRoaming`).
pub fn list_stash(steam_root: &Path, account: u32, app_id: u32) -> Vec<Stashed> {
    let Ok(entries) = std::fs::read_dir(stash_dir(steam_root, account, app_id)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let Some(root) = e.file_name().to_str().and_then(RootToken::from_name) else {
            continue;
        };
        let base = e.path();
        if base.is_dir() {
            walk(&base, &base, root, &mut out);
        }
    }
    out.sort_by(|a, b| (a.root, &a.rel_path).cmp(&(b.root, &b.rel_path)));
    out
}

fn walk(base: &Path, dir: &Path, root: RootToken, out: &mut Vec<Stashed>) {
    const MAX: usize = 5_000;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if out.len() >= MAX {
            return;
        }
        let path = e.path();
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            walk(base, &path, root, out);
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let Ok(rel) = path.strip_prefix(base) else {
            continue;
        };
        let rel_path = rel.to_string_lossy().replace('\\', "/");
        if is_marker(&rel_path) || !super::remotecache::is_safe_rel_path(&rel_path) {
            continue;
        }
        out.push(Stashed {
            root,
            rel_path,
            path,
            size: meta.len(),
        });
    }
}

/// Tìm file đánh dấu áp dụng cho `file`: đi từ thư mục chứa nó ngược lên tới
/// `base` (gốc của root token). Steam đặt file này ở gốc quy tắc UFS, mà app
/// không biết gốc đó ở tầng nào, nên phải dò.
pub fn marker_for(base: &Path, file: &Path) -> Option<PathBuf> {
    let mut dir = file.parent()?;
    loop {
        let candidate = dir.join(MARKER);
        if candidate.is_file() {
            return Some(candidate);
        }
        if dir == base {
            return None;
        }
        dir = dir.parent()?;
        if !dir.starts_with(base) {
            return None;
        }
    }
}

/// Lỗi rõ nghĩa khi không xác định được tài khoản đích.
pub fn no_account() -> Error {
    Error::Other(
        "chưa biết Steam đang đăng nhập tài khoản nào — hãy mở Steam và đăng nhập \
         trước khi khôi phục"
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_marker_shape_steam_writes() {
        let src = "\"steam_autocloud.vdf\"\n{\n\t\"accountid\"\t\t\"111111111\"\n}\n";
        assert_eq!(account_in(src), Some(111_111_111));
        // Đọc lại được thứ chính mình ghi ra.
        assert_eq!(account_in(&marker_text(42)), Some(42));
    }

    #[test]
    fn spots_marker_by_file_name() {
        assert!(is_marker(MARKER));
        assert!(is_marker("SB/Saved/SaveGames/steam_autocloud.vdf"));
        assert!(!is_marker("Saves/farm_1/SaveGameInfo"));
        assert!(!is_marker("Saves/steam_autocloud.vdf.bak"));
    }

    #[test]
    fn retag_only_touches_existing_file_with_other_account() {
        let dir = std::env::temp_dir().join(format!("cs-ac-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(MARKER);

        // Chưa có file: không tạo mới.
        assert!(!retag(&p, 7).unwrap());
        assert!(!p.exists());

        std::fs::write(&p, marker_text(1111)).unwrap();
        assert!(retag(&p, 2222).unwrap());
        assert_eq!(read_account(&p), Some(2222));
        // Đã đúng tài khoản: không ghi lại.
        assert!(!retag(&p, 2222).unwrap());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lists_stash_with_root_token_from_dir_name() {
        let root = std::env::temp_dir().join(format!("cs-stash-{}", uuid::Uuid::new_v4()));
        let ac = stash_dir(&root, 1111, 413_150);
        let save = ac
            .join("WinAppDataRoaming")
            .join("StardewValley/Saves/farm_1");
        std::fs::create_dir_all(&save).unwrap();
        std::fs::write(save.join("farm_1"), b"x").unwrap();
        std::fs::write(save.join(MARKER), b"y").unwrap();
        // Thư mục không phải root token thì bỏ qua.
        let junk = ac.join("NotARoot");
        std::fs::create_dir_all(&junk).unwrap();
        std::fs::write(junk.join("a.sav"), b"z").unwrap();

        let v = list_stash(&root, 1111, 413_150);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].root, RootToken::WinAppDataRoaming);
        assert_eq!(v[0].rel_path, "StardewValley/Saves/farm_1/farm_1");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn finds_marker_upwards_but_not_above_base() {
        let base = std::env::temp_dir().join(format!("cs-mk-{}", uuid::Uuid::new_v4()));
        let saves = base.join("StardewValley/Saves");
        let farm = saves.join("farm_1");
        std::fs::create_dir_all(&farm).unwrap();
        std::fs::write(saves.join(MARKER), marker_text(9)).unwrap();
        let file = farm.join("farm_1");
        std::fs::write(&file, b"x").unwrap();

        assert_eq!(marker_for(&base, &file), Some(saves.join(MARKER)));
        // Không có file đánh dấu nào trong phạm vi base.
        let other = base.join("Other/x.sav");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, b"x").unwrap();
        assert_eq!(marker_for(&base, &other), None);

        std::fs::remove_dir_all(&base).ok();
    }
}
