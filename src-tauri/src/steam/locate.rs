//! Dò tìm cài đặt Steam, thư viện game, và các tài khoản trong `userdata/`.
//!
//! Tất cả đều đọc từ đĩa, không cần đăng nhập Steam và không cần mạng.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::textvdf::{self, Node};
use crate::error::{Error, Result};

/// Offset chuyển SteamID64 ↔ account id 32-bit (tên thư mục trong `userdata/`).
const STEAMID64_BASE: u64 = 76_561_197_960_265_728;

#[derive(Debug, Clone)]
pub struct SteamInstall {
    pub root: PathBuf,
    /// Mọi thư mục thư viện, gồm cả thư viện chính.
    pub libraries: Vec<PathBuf>,
    pub users: Vec<SteamUser>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SteamUser {
    pub account_id: u32,
    pub steam_id64: String,
    pub account_name: Option<String>,
    pub persona_name: Option<String>,
    /// `Timestamp` trong loginusers.vdf — lần đăng nhập gần nhất (unix giây).
    pub last_login: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledApp {
    pub app_id: u32,
    pub name: String,
    /// Thư mục cài thật, vd `D:\SteamLibrary\steamapps\common\Stardew Valley`.
    pub install_dir: PathBuf,
}

/// Tìm thư mục cài Steam.
///
/// Registry là nguồn đáng tin nhất vì người dùng hay cài sang ổ khác; các
/// đường dẫn đoán chỉ là lưới an toàn.
pub fn find_steam_root() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("CLOUDSAVE_STEAM_ROOT") {
        let p = PathBuf::from(p);
        if p.join("steamapps").is_dir() {
            return Ok(p);
        }
    }

    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        use winreg::RegKey;

        let candidates: [(_, &str, &str); 3] = [
            (HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath"),
            (
                HKEY_LOCAL_MACHINE,
                r"SOFTWARE\WOW6432Node\Valve\Steam",
                "InstallPath",
            ),
            (HKEY_LOCAL_MACHINE, r"SOFTWARE\Valve\Steam", "InstallPath"),
        ];
        for (hive, subkey, value) in candidates {
            if let Ok(key) = RegKey::predef(hive).open_subkey(subkey) {
                if let Ok(s) = key.get_value::<String, _>(value) {
                    let p = PathBuf::from(s.replace('/', "\\"));
                    if p.join("steamapps").is_dir() {
                        return Ok(p);
                    }
                }
            }
        }
    }

    for guess in default_guesses() {
        if guess.join("steamapps").is_dir() {
            return Ok(guess);
        }
    }

    Err(Error::SteamNotFound)
}

#[cfg(windows)]
fn default_guesses() -> Vec<PathBuf> {
    let mut v = Vec::new();
    for env in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(base) = std::env::var_os(env) {
            v.push(PathBuf::from(base).join("Steam"));
        }
    }
    v.push(PathBuf::from(r"C:\Steam"));
    v
}

#[cfg(not(windows))]
fn default_guesses() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(h) = dirs::home_dir() {
        v.push(h.join(".steam/steam"));
        v.push(h.join(".local/share/Steam"));
        v.push(h.join("Library/Application Support/Steam"));
    }
    v
}

impl SteamInstall {
    pub fn discover() -> Result<Self> {
        let root = find_steam_root()?;
        let libraries = read_libraries(&root);
        let users = read_users(&root);
        Ok(Self {
            root,
            libraries,
            users,
        })
    }

    /// Tài khoản Steam đang (hoặc vừa) đăng nhập trên máy này.
    ///
    /// Máy có thể có hàng chục tài khoản trong `userdata/` (máy phát triển có
    /// 93), nên không thể bắt người dùng tự chọn mỗi lần. Thứ tự ưu tiên:
    ///   1. `HKCU\Software\Valve\Steam\ActiveProcess\ActiveUser` — Steam ghi
    ///      khi đang chạy và đã đăng nhập; bằng 0 khi Steam tắt.
    ///   2. Tài khoản có `Timestamp` mới nhất trong loginusers.vdf.
    ///   3. Tài khoản đầu tiên tìm được.
    pub fn active_account_id(&self) -> Option<u32> {
        #[cfg(windows)]
        {
            use winreg::enums::HKEY_CURRENT_USER;
            use winreg::RegKey;
            if let Ok(k) =
                RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam\ActiveProcess")
            {
                if let Ok(id) = k.get_value::<u32, _>("ActiveUser") {
                    if id != 0 && self.users.iter().any(|u| u.account_id == id) {
                        return Some(id);
                    }
                }
            }
        }
        self.users
            .iter()
            .filter(|u| u.last_login.is_some())
            .max_by_key(|u| u.last_login)
            .or_else(|| self.users.first())
            .map(|u| u.account_id)
    }

    pub fn appcache_appinfo(&self) -> PathBuf {
        self.root.join("appcache").join("appinfo.vdf")
    }

    pub fn userdata_dir(&self, account_id: u32) -> PathBuf {
        self.root.join("userdata").join(account_id.to_string())
    }

    pub fn remotecache_path(&self, account_id: u32, app_id: u32) -> PathBuf {
        self.userdata_dir(account_id)
            .join(app_id.to_string())
            .join("remotecache.vdf")
    }

    /// Quét mọi thư viện, đọc `appmanifest_*.acf` để biết game nào đã cài và ở đâu.
    pub fn installed_apps(&self) -> BTreeMap<u32, InstalledApp> {
        let mut out = BTreeMap::new();
        for lib in &self.libraries {
            let steamapps = lib.join("steamapps");
            let Ok(entries) = std::fs::read_dir(&steamapps) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(entry.path()) else {
                    continue;
                };
                let Ok(kv) = textvdf::parse(&text) else {
                    continue;
                };
                let Some(state) = kv.obj("AppState") else {
                    continue;
                };
                let Some(app_id) = state.str("appid").and_then(|s| s.parse::<u32>().ok()) else {
                    continue;
                };
                let installdir = state.str("installdir").unwrap_or_default();
                if installdir.is_empty() {
                    continue;
                }
                out.insert(
                    app_id,
                    InstalledApp {
                        app_id,
                        name: state.str("name").unwrap_or(installdir).to_owned(),
                        install_dir: steamapps.join("common").join(installdir),
                    },
                );
            }
        }
        out
    }

    /// Các appid có thư mục trong `userdata/<account>/` — tức là từng có
    /// dữ liệu Cloud. Dùng để gợi ý game đáng sao lưu ngay cả khi appinfo
    /// không khai `ufs`.
    pub fn apps_with_userdata(&self, account_id: u32) -> Vec<u32> {
        let dir = self.userdata_dir(account_id);
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_string_lossy().parse::<u32>().ok())
            // 7 và 241100 là app nội bộ của Steam (cấu hình client, Steam
            // Controller), không phải game — chúng luôn có mặt và chỉ gây nhiễu.
            .filter(|id| !matches!(id, 7 | 241_100))
            .collect()
    }
}

fn read_libraries(root: &Path) -> Vec<PathBuf> {
    let mut libs = vec![root.to_path_buf()];

    let path = root.join("steamapps").join("libraryfolders.vdf");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return libs;
    };
    let Ok(kv) = textvdf::parse(&text) else {
        return libs;
    };
    let Some(folders) = kv.obj("libraryfolders") else {
        return libs;
    };

    for (_idx, node) in folders.iter() {
        // Mục hợp lệ là object có khoá "path"; các khoá scalar lẫn vào thì bỏ.
        let Node::Obj(o) = node else { continue };
        let Some(p) = o.str("path") else { continue };
        let p = PathBuf::from(p);
        if p.join("steamapps").is_dir() && !libs.iter().any(|x| paths_eq(x, &p)) {
            libs.push(p);
        }
    }
    libs
}

/// So sánh đường dẫn bỏ qua hoa thường trên Windows — `libraryfolders.vdf`
/// ghi thường hoá (`c:\program files (x86)\steam`) còn registry giữ nguyên.
fn paths_eq(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    }
}

fn read_users(root: &Path) -> Vec<SteamUser> {
    let mut by_id: BTreeMap<u32, SteamUser> = BTreeMap::new();

    // Nguồn 1: config/loginusers.vdf cho tên hiển thị.
    if let Ok(text) = std::fs::read_to_string(root.join("config").join("loginusers.vdf")) {
        if let Ok(kv) = textvdf::parse(&text) {
            if let Some(users) = kv.obj("users") {
                for (id64, node) in users.iter() {
                    let Node::Obj(o) = node else { continue };
                    let Ok(id64n) = id64.parse::<u64>() else {
                        continue;
                    };
                    let account_id = (id64n.saturating_sub(STEAMID64_BASE)) as u32;
                    by_id.insert(
                        account_id,
                        SteamUser {
                            account_id,
                            steam_id64: id64.clone(),
                            account_name: o.str("AccountName").map(str::to_owned),
                            persona_name: o.str("PersonaName").map(str::to_owned),
                            last_login: o.str("Timestamp").and_then(|t| t.parse().ok()),
                        },
                    );
                }
            }
        }
    }

    // Nguồn 2: thư mục userdata/. Bắt được cả tài khoản đã đăng xuất khỏi
    // loginusers.vdf nhưng vẫn còn save trên máy.
    if let Ok(entries) = std::fs::read_dir(root.join("userdata")) {
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let Ok(account_id) = e.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            if account_id == 0 {
                continue;
            }
            by_id.entry(account_id).or_insert_with(|| SteamUser {
                account_id,
                steam_id64: (STEAMID64_BASE + account_id as u64).to_string(),
                account_name: None,
                persona_name: None,
                last_login: None,
            });
        }
    }

    by_id.into_values().collect()
}
