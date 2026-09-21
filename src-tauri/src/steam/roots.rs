//! Phân giải "root token" của Steam thành đường dẫn tuyệt đối trên máy hiện tại.
//!
//! Steam mô tả vị trí save bằng cặp (root, path) chứ không phải đường dẫn đầy đủ:
//!   - `appinfo.vdf` → `ufs.savefiles.*.root` dùng tên chữ, vd "WinAppDataRoaming"
//!   - `remotecache.vdf` → `root` dùng số, vd "4"
//!
//! Ta lưu token vào Supabase thay vì đường dẫn tuyệt đối, nên khôi phục sang
//! máy có tên user hoặc ổ đĩa khác vẫn ra đúng chỗ. Đây là điểm khác biệt so
//! với ludusavi (vốn lưu nguyên `C:\Users\foo\...`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// Ord là bắt buộc: `scan.rs` dùng (RootToken, rel_path) làm khoá BTreeMap
// để dedupe file được nhiều nguồn cùng chỉ ra.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum RootToken {
    /// `<steam>/userdata/<accountId>/<appId>/remote` — thư mục Steam Cloud mirror.
    SteamCloudRemote,
    /// Thư mục cài đặt của chính game.
    GameInstall,
    WinAppDataRoaming,
    WinAppDataLocal,
    WinAppDataLocalLow,
    WinMyDocuments,
    WinSavedGames,
    WinUserHome,
    WinProgramData,
    WinPublic,
    LinuxHome,
    LinuxXdgDataHome,
    LinuxXdgConfigHome,
    MacHome,
    MacAppSupport,
    MacDocuments,
}

impl RootToken {
    /// Map id số trong `remotecache.vdf`.
    ///
    /// Các giá trị đánh dấu VERIFIED đã được đối chiếu với đĩa thật:
    ///   0  → app 1868140 ghi thẳng vào `userdata/<id>/1868140/remote/`
    ///   4  → app 413150 "StardewValley/Saves/..." → `%APPDATA%\StardewValley`
    ///   18 → app 3489700 "Documents/StellarBlade/..." → `%USERPROFILE%\Documents`
    ///
    /// Phần còn lại lấy từ tài liệu cộng đồng và CHƯA đối chiếu được. Khi gặp
    /// một id lạ ta trả None và để tầng trên bỏ qua file đó thay vì đoán bừa —
    /// đoán sai root nghĩa là ghi save vào nhầm chỗ.
    pub fn from_numeric(id: u32) -> Option<Self> {
        Some(match id {
            0 => Self::SteamCloudRemote,    // VERIFIED
            1 => Self::GameInstall,         // VERIFIED
            2 => Self::WinMyDocuments,      // tài liệu cộng đồng
            3 => Self::WinAppDataLocal,     // tài liệu cộng đồng
            4 => Self::WinAppDataRoaming,   // VERIFIED
            9 => Self::WinSavedGames,       // tài liệu cộng đồng
            12 => Self::WinAppDataLocalLow, // tài liệu cộng đồng
            18 => Self::WinUserHome,        // VERIFIED
            _ => return None,
        })
    }

    /// Map tên chữ trong `appinfo.vdf` → `ufs.savefiles.*.root`.
    ///
    /// Danh sách này được đối chiếu với toàn bộ `appinfo.vdf` thật: 422 quy
    /// tắc UFS trên 221 app dùng đúng 13 tên root, tất cả đều có mặt ở đây.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "gameinstall" | "app install directory" => Self::GameInstall,
            "winappdataroaming" | "winappdata" => Self::WinAppDataRoaming,
            "winappdatalocal" => Self::WinAppDataLocal,
            "winappdatalocallow" => Self::WinAppDataLocalLow,
            "winmydocuments" | "windocuments" => Self::WinMyDocuments,
            "winsavedgames" => Self::WinSavedGames,
            // `WindowsHome` là tên Steam thực sự dùng (vd Stellar Blade với
            // path "Documents/StellarBlade/{64BitSteamID}"). Thiếu nó là bỏ
            // sót luôn quy tắc save của game đó.
            "windowshome" | "winuserhome" | "winhome" => Self::WinUserHome,
            // Tên gợi ý thư mục Documents, nhưng 3 app dùng nó đều có path
            // kiểu "SavesDir" — tức tương đối so với thư mục Cloud của Steam,
            // không phải Documents. CHƯA đối chiếu được với đĩa; nếu sai thì
            // lúc sao lưu chỉ đơn giản là không tìm thấy file.
            "steamclouddocuments" => Self::SteamCloudRemote,
            "winprogramdata" => Self::WinProgramData,
            "winpublic" => Self::WinPublic,
            "linuxhome" => Self::LinuxHome,
            "linuxxdgdatahome" => Self::LinuxXdgDataHome,
            "linuxxdgconfighome" => Self::LinuxXdgConfigHome,
            "machome" => Self::MacHome,
            "macappsupport" => Self::MacAppSupport,
            "macdocuments" => Self::MacDocuments,
            _ => return None,
        })
    }
}

/// Mọi thứ cần biết để biến một token thành đường dẫn thật.
#[derive(Debug, Clone)]
pub struct RootContext {
    /// Thư mục cài Steam, vd `C:\Program Files (x86)\Steam`.
    pub steam_root: PathBuf,
    /// Account id 32-bit (tên thư mục trong `userdata/`).
    pub account_id: u32,
    pub app_id: u32,
    /// Thư mục cài của game, nếu biết.
    pub game_install: Option<PathBuf>,
}

impl RootContext {
    pub fn resolve(&self, token: RootToken) -> Option<PathBuf> {
        use RootToken::*;
        Some(match token {
            SteamCloudRemote => self
                .steam_root
                .join("userdata")
                .join(self.account_id.to_string())
                .join(self.app_id.to_string())
                .join("remote"),
            GameInstall => self.game_install.clone()?,
            WinAppDataRoaming => known_folder("AppData", "APPDATA")?,
            WinAppDataLocal => known_folder("Local AppData", "LOCALAPPDATA")?,
            WinAppDataLocalLow => known_folder("Local AppData", "LOCALAPPDATA")?
                .parent()?
                .join("LocalLow"),
            // Documents hay bị OneDrive chuyển hướng, nên phải hỏi shell chứ
            // không ghép %USERPROFILE%\Documents.
            WinMyDocuments => known_folder("Personal", "")?,
            WinSavedGames => {
                known_folder("", "").or_else(|| dirs::home_dir().map(|h| h.join("Saved Games")))?
            }
            WinUserHome | LinuxHome | MacHome => dirs::home_dir()?,
            WinProgramData => std::env::var_os("ProgramData").map(PathBuf::from)?,
            WinPublic => std::env::var_os("PUBLIC").map(PathBuf::from)?,
            LinuxXdgDataHome => dirs::data_dir()?,
            LinuxXdgConfigHome => dirs::config_dir()?,
            MacAppSupport => dirs::home_dir()?.join("Library/Application Support"),
            MacDocuments => dirs::home_dir()?.join("Documents"),
        })
    }
}

/// Đọc "User Shell Folders" từ registry để tôn trọng chuyển hướng OneDrive /
/// thư mục Documents đã bị người dùng dời đi. Rơi về biến môi trường nếu hỏng.
#[cfg(windows)]
fn known_folder(reg_name: &str, env_fallback: &str) -> Option<PathBuf> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    if !reg_name.is_empty() {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(key) =
            hkcu.open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders")
        {
            if let Ok(v) = key.get_value::<String, _>(reg_name) {
                if !v.is_empty() {
                    return Some(PathBuf::from(v));
                }
            }
        }
    }
    if !env_fallback.is_empty() {
        if let Some(v) = std::env::var_os(env_fallback) {
            return Some(PathBuf::from(v));
        }
    }
    None
}

#[cfg(not(windows))]
fn known_folder(_reg_name: &str, env_fallback: &str) -> Option<PathBuf> {
    if env_fallback.is_empty() {
        return None;
    }
    std::env::var_os(env_fallback).map(PathBuf::from)
}
