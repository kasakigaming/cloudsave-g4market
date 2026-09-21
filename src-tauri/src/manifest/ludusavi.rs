//! Tầng dự phòng: [ludusavi-manifest] — cơ sở dữ liệu ~19.000 game lấy từ
//! PCGamingWiki.
//!
//! Dùng khi `appinfo.vdf` không khai `ufs` (game không bật Steam Cloud, hoặc
//! game non-Steam). Manifest được **tải lúc chạy và cache trên đĩa**, cố ý
//! không nhúng vào binary: nó lớn, đổi liên tục, và kèm theo là ràng buộc bản
//! quyền cần kiểm tra riêng trước khi phân phối lại.
//!
//! [ludusavi-manifest]: https://github.com/mtkennerly/ludusavi-manifest

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::Result;
use crate::steam::RootToken;

const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/mtkennerly/ludusavi-manifest/master/data/manifest.yaml";

// ─────────────────────────────────────────────────────────────────────────
// Hình dạng YAML
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct GameEntry {
    #[serde(default)]
    pub files: BTreeMap<String, FileEntry>,
    #[serde(default)]
    pub registry: BTreeMap<String, FileEntry>,
    #[serde(default, rename = "installDir")]
    pub install_dir: BTreeMap<String, serde_yaml::Value>,
    #[serde(default)]
    pub steam: Option<StoreId>,
    #[serde(default)]
    pub gog: Option<StoreId>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StoreId {
    pub id: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileEntry {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub when: Vec<Constraint>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Constraint {
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub store: Option<String>,
}

impl FileEntry {
    /// Chỉ lấy mục được gắn tag `save`. Manifest cũng chứa `config` (thiết lập
    /// đồ hoạ, phím tắt) — hữu ích nhưng không phải thứ người dùng sợ mất.
    pub fn is_save(&self) -> bool {
        self.tags.iter().any(|t| t == "save")
    }

    pub fn applies_to_windows_steam(&self) -> bool {
        if self.when.is_empty() {
            return true;
        }
        self.when.iter().any(|c| {
            let os_ok = c.os.as_deref().map_or(true, |o| o == "windows");
            let store_ok = c.store.as_deref().map_or(true, |s| s == "steam");
            os_ok && store_ok
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Manifest đã nạp
// ─────────────────────────────────────────────────────────────────────────

pub struct Manifest {
    games: BTreeMap<String, GameEntry>,
    by_steam_id: BTreeMap<u32, String>,
}

impl Manifest {
    pub fn from_yaml(text: &str) -> Result<Self> {
        let games: BTreeMap<String, GameEntry> = serde_yaml::from_str(text)?;
        let mut by_steam_id = BTreeMap::new();
        for (name, entry) in &games {
            if let Some(id) = entry.steam.as_ref().and_then(|s| s.id) {
                by_steam_id.insert(id, name.clone());
            }
        }
        Ok(Self { games, by_steam_id })
    }

    pub fn len(&self) -> usize {
        self.games.len()
    }

    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }

    pub fn by_steam_app(&self, app_id: u32) -> Option<(&str, &GameEntry)> {
        let name = self.by_steam_id.get(&app_id)?;
        Some((name.as_str(), self.games.get(name)?))
    }

    pub fn by_title(&self, title: &str) -> Option<(&str, &GameEntry)> {
        self.games
            .get_key_value(title)
            .map(|(k, v)| (k.as_str(), v))
            .or_else(|| {
                self.games
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(title))
                    .map(|(k, v)| (k.as_str(), v))
            })
    }

    /// Đọc từ cache trên đĩa; tải về nếu chưa có hoặc đã quá `max_age_days`.
    pub async fn load_or_fetch(cache_dir: &Path, max_age_days: u64) -> Result<Self> {
        let path = cache_dir.join("ludusavi-manifest.yaml");

        let stale = match std::fs::metadata(&path) {
            Ok(m) => m
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|age| age.as_secs() > max_age_days * 86_400)
                .unwrap_or(true),
            Err(_) => true,
        };

        if !stale {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(m) = Self::from_yaml(&text) {
                    return Ok(m);
                }
                log::warn!("manifest cache hỏng, tải lại");
            }
        }

        match fetch(&path).await {
            Ok(text) => Self::from_yaml(&text),
            Err(e) => {
                // Mất mạng không nên làm app vô dụng: cache cũ vẫn tốt hơn
                // không có gì, và nguồn chính (appinfo.vdf) vẫn chạy bình thường.
                log::warn!("không tải được manifest ({e}), thử dùng cache cũ");
                let text = std::fs::read_to_string(&path).map_err(|_| e)?;
                Self::from_yaml(&text)
            }
        }
    }
}

async fn fetch(cache_path: &Path) -> Result<String> {
    let text = reqwest::Client::new()
        .get(MANIFEST_URL)
        .header("User-Agent", "cloudsave-g4market")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Ghi tạm rồi đổi tên: nếu tắt máy giữa chừng ta không để lại một file
    // cache cụt đầu mà lần sau vẫn tưởng là hợp lệ.
    let tmp = cache_path.with_extension("yaml.tmp");
    std::fs::write(&tmp, &text)?;
    std::fs::rename(&tmp, cache_path)?;
    Ok(text)
}

// ─────────────────────────────────────────────────────────────────────────
// Placeholder → RootToken
// ─────────────────────────────────────────────────────────────────────────

/// Một đường dẫn manifest đã tách thành (root, phần còn lại).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    pub root: RootToken,
    /// Phần sau root, có thể còn chứa glob `*` / `**`.
    pub rel_glob: String,
}

/// Ngữ cảnh để thay các placeholder không phải root (tên user, id store...).
#[derive(Debug, Clone, Default)]
pub struct Substitutions {
    pub os_user_name: Option<String>,
    pub store_user_id: Option<String>,
    pub store_game_id: Option<String>,
    /// Tên thư mục cài, cho `<game>`.
    pub game_dir: Option<String>,
}

/// Tách một đường dẫn manifest thành root token + phần tương đối.
///
/// Trả `None` khi placeholder không ánh xạ được sang root token nào ta hỗ trợ
/// (vd `<winDir>`). Bỏ qua tốt hơn là đoán: ghi save nhầm chỗ là hỏng dữ liệu
/// thật của người dùng.
pub fn resolve_path(raw: &str, subs: &Substitutions) -> Option<ResolvedPath> {
    let p = raw.replace('\\', "/");

    // Không mở đầu bằng placeholder nghĩa là đường dẫn tuyệt đối trần — hiếm,
    // và không mang sang máy khác được, nên bỏ qua.
    let stripped = p.strip_prefix('<')?;
    let end = stripped.find('>')?;
    let (placeholder, rest) = (&stripped[..end], stripped[end + 1..].to_string());

    let root = match placeholder {
        // `<base>` = `<root>/<game>`, tức thư mục cài game.
        "base" | "root" => RootToken::GameInstall,
        "home" => RootToken::WinUserHome,
        "winAppData" => RootToken::WinAppDataRoaming,
        "winLocalAppData" => RootToken::WinAppDataLocal,
        "winLocalAppDataLow" => RootToken::WinAppDataLocalLow,
        "winDocuments" => RootToken::WinMyDocuments,
        "winPublic" => RootToken::WinPublic,
        "winProgramData" => RootToken::WinProgramData,
        "xdgData" => RootToken::LinuxXdgDataHome,
        "xdgConfig" => RootToken::LinuxXdgConfigHome,
        other => {
            log::debug!("manifest: placeholder <{other}> chưa hỗ trợ, bỏ qua '{raw}'");
            return None;
        }
    };

    // `<root>` là thư mục thư viện, nên `<root>/<game>` mới ra thư mục game.
    // Ta đã map cả hai về GameInstall, vậy phải nuốt luôn `<game>` đứng sau
    // để không thừa một cấp thư mục.
    let mut rel = rest;
    if placeholder == "root" {
        if let Some(r) = rel.trim_start_matches('/').strip_prefix("<game>") {
            rel = r.to_string();
        }
    }

    let rel = rel
        .replace(
            "<osUserName>",
            subs.os_user_name.as_deref().unwrap_or("<osUserName>"),
        )
        .replace(
            "<storeUserId>",
            subs.store_user_id.as_deref().unwrap_or("*"),
        )
        .replace(
            "<storeGameId>",
            subs.store_game_id.as_deref().unwrap_or("*"),
        )
        .replace("<game>", subs.game_dir.as_deref().unwrap_or("*"));

    // Placeholder nào còn sót lại nghĩa là ta chưa hiểu hết đường dẫn.
    if rel.contains('<') {
        log::debug!("manifest: còn placeholder chưa giải trong '{raw}', bỏ qua");
        return None;
    }

    Some(ResolvedPath {
        root,
        rel_glob: rel.trim_matches('/').to_string(),
    })
}

/// Thư mục cache mặc định cho manifest.
pub fn default_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("cloudsave-g4market")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subs() -> Substitutions {
        Substitutions {
            os_user_name: Some("Legion".into()),
            store_user_id: Some("987654321".into()),
            store_game_id: Some("413150".into()),
            game_dir: Some("Stardew Valley".into()),
        }
    }

    #[test]
    fn maps_win_app_data() {
        let r = resolve_path("<winAppData>/StardewValley/Saves", &subs()).unwrap();
        assert_eq!(r.root, RootToken::WinAppDataRoaming);
        assert_eq!(r.rel_glob, "StardewValley/Saves");
    }

    #[test]
    fn root_swallows_game_placeholder() {
        let r = resolve_path("<root>/<game>/savedata", &subs()).unwrap();
        assert_eq!(r.root, RootToken::GameInstall);
        assert_eq!(r.rel_glob, "savedata");
    }

    #[test]
    fn substitutes_store_user_id() {
        let r = resolve_path("<winDocuments>/My Games/<storeUserId>/sav", &subs()).unwrap();
        assert_eq!(r.rel_glob, "My Games/987654321/sav");
    }

    #[test]
    fn skips_unsupported_placeholder() {
        assert!(resolve_path("<winDir>/system32/x", &subs()).is_none());
    }
}
