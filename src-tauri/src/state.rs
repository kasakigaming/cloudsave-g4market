//! State dùng chung cho toàn app.
//!
//! Hai thứ đắt nhất — cây `appinfo.vdf` đã parse và ludusavi-manifest — được
//! nạp lười và giữ sau `Arc`. `appinfo.vdf` trên máy thật khoảng 6 MB và cho
//! ra hàng chục nghìn node; parse lại mỗi lần bấm nút là không chấp nhận được.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::RwLock;

use crate::backup::DeviceInfo;
use crate::error::{Error, Result};
use crate::local_store::{CaptureOutcome, LocalStore, Trigger};
use crate::manifest::ludusavi::{self, Manifest};
use crate::scan::Scanner;
use crate::steam::{AppInfo, SteamInstall};
use crate::supabase::{self, Supabase};
use crate::watcher::RunningGame;

/// Manifest coi là cũ sau số ngày này thì tải lại.
const MANIFEST_MAX_AGE_DAYS: u64 = 7;

pub struct AppState {
    steam: Option<SteamInstall>,
    appinfo: RwLock<Option<Arc<AppInfo>>>,
    manifest: RwLock<Option<Arc<Manifest>>>,
    pub supabase: Option<Supabase>,
    pub device: DeviceInfo,
    /// Kho snapshot trên máy. Mọi lần chụp ghi vào đây trước, không cần mạng.
    pub store: Arc<LocalStore>,
    /// Appid đang chạy, do watcher cập nhật.
    running: Mutex<BTreeSet<u32>>,
    /// Tài khoản Steam dùng để giải `{64BitSteamID}` và thư mục `userdata/`.
    watch_account: Mutex<Option<u32>>,
    /// Tự động tắt Steam Cloud / đang tắt dở.
    pub steam_cloud: crate::steam_cloud::Flags,
}

impl AppState {
    pub fn new() -> Self {
        let steam = match SteamInstall::discover() {
            Ok(s) => Some(s),
            Err(e) => {
                // Không tìm thấy Steam không phải lý do để app không mở được:
                // người dùng vẫn cần đăng nhập và xem lại các bản đã sao lưu.
                log::warn!("không tìm thấy Steam: {e}");
                None
            }
        };

        let supabase = match supabase::Config::from_env() {
            Ok(cfg) => Supabase::new(cfg).ok(),
            Err(_) => {
                log::warn!("chưa bật cloud — chỉ chạy được chế độ quét cục bộ");
                None
            }
        };

        let store_root = LocalStore::default_root();
        let store = LocalStore::open(&store_root)
            .or_else(|e| {
                // Không tạo được kho ở chỗ mặc định (ổ đầy, bị chặn quyền) thì
                // dùng tạm thư mục temp — app vẫn phải mở được.
                log::error!("không mở được kho local {}: {e}", store_root.display());
                LocalStore::open(std::env::temp_dir().join("cloudsave-g4market-store"))
            })
            .expect("không tạo được kho local ở bất kỳ đâu");

        let watch_account = steam.as_ref().and_then(|s| s.active_account_id());

        Self {
            steam,
            appinfo: RwLock::new(None),
            manifest: RwLock::new(None),
            supabase,
            device: DeviceInfo {
                id: device_id(),
                name: device_name(),
            },
            store: Arc::new(store),
            running: Mutex::new(BTreeSet::new()),
            watch_account: Mutex::new(watch_account),
            steam_cloud: crate::steam_cloud::Flags {
                auto_disable: crate::steam_cloud::load_auto_disable().into(),
                busy: false.into(),
                ui_disabled: Default::default(),
            },
        }
    }

    // ── Game đang chạy ───────────────────────────────────────────────────

    pub fn set_running(&self, ids: BTreeSet<u32>) {
        *self.running.lock().unwrap() = ids;
    }

    pub fn is_running(&self, app_id: u32) -> bool {
        self.running.lock().unwrap().contains(&app_id)
    }

    pub fn running_ids(&self) -> BTreeSet<u32> {
        self.running.lock().unwrap().clone()
    }

    pub async fn running_games(&self) -> Vec<RunningGame> {
        let mut out = Vec::new();
        for id in self.running_ids() {
            out.push(RunningGame {
                app_id: id,
                title: self.title_of(id).await,
            });
        }
        out
    }

    /// Tên hiển thị của một appid; không bao giờ thất bại.
    pub async fn title_of(&self, app_id: u32) -> String {
        if let Ok(a) = self.appinfo().await {
            if let Some(n) = a.app_name(app_id) {
                return n;
            }
        }
        if let Ok(s) = self.steam() {
            if let Some(a) = s.installed_apps().get(&app_id) {
                return a.name.clone();
            }
        }
        format!("App {app_id}")
    }

    // ── Tài khoản theo dõi ───────────────────────────────────────────────

    pub fn watch_account(&self) -> Option<u32> {
        *self.watch_account.lock().unwrap()
    }

    pub fn set_watch_account(&self, id: u32) {
        *self.watch_account.lock().unwrap() = Some(id);
    }

    // ── Chụp ─────────────────────────────────────────────────────────────

    /// Quét một game rồi chụp vào kho local. Dùng chung cho watcher, nút bấm
    /// và "quét tất cả".
    pub async fn capture_game(
        &self,
        account_id: u32,
        app_id: u32,
        trigger: Trigger,
    ) -> Result<CaptureOutcome> {
        let scanner = Scanner {
            steam: self.steam()?,
            appinfo: self.appinfo().await?,
            manifest: self.manifest().await,
            account_id,
        };
        let store = self.store.clone();
        // Đọc file và nén là việc chặn luồng; đừng làm nghẽn runtime async.
        tokio::task::spawn_blocking(move || {
            let scan = scanner.scan_app(app_id);
            store.capture(&scan, account_id, trigger)
        })
        .await
        .map_err(|e| Error::Other(format!("luồng chụp bị huỷ: {e}")))?
    }

    pub fn steam(&self) -> Result<SteamInstall> {
        self.steam.clone().ok_or(Error::SteamNotFound)
    }

    pub async fn appinfo(&self) -> Result<Arc<AppInfo>> {
        if let Some(a) = self.appinfo.read().await.clone() {
            return Ok(a);
        }
        let mut guard = self.appinfo.write().await;
        // Một luồng khác có thể đã nạp xong trong lúc ta đợi khoá ghi.
        if let Some(a) = guard.clone() {
            return Ok(a);
        }
        let steam = self.steam()?;
        let bytes = std::fs::read(steam.appcache_appinfo())?;
        let parsed = Arc::new(AppInfo::parse(&bytes)?);
        *guard = Some(parsed.clone());
        Ok(parsed)
    }

    pub async fn appinfo_owned(&self) -> Result<Arc<AppInfo>> {
        self.appinfo().await
    }

    /// Manifest là tuỳ chọn: không có mạng thì nguồn Steam vẫn chạy bình thường.
    pub async fn manifest(&self) -> Option<Arc<Manifest>> {
        if let Some(m) = self.manifest.read().await.clone() {
            return Some(m);
        }
        let mut guard = self.manifest.write().await;
        if let Some(m) = guard.clone() {
            return Some(m);
        }
        match Manifest::load_or_fetch(&ludusavi::default_cache_dir(), MANIFEST_MAX_AGE_DAYS).await {
            Ok(m) => {
                log::info!("đã nạp ludusavi-manifest: {} game", m.len());
                let m = Arc::new(m);
                *guard = Some(m.clone());
                Some(m)
            }
            Err(e) => {
                log::warn!("không nạp được ludusavi-manifest: {e}");
                None
            }
        }
    }

    pub async fn manifest_owned(&self) -> Option<Arc<Manifest>> {
        self.manifest().await
    }

    /// Ép tải lại manifest, bỏ qua cache. Trả về số game trong bản mới.
    pub async fn reload_manifest(&self) -> Result<usize> {
        let m = Manifest::load_or_fetch(&ludusavi::default_cache_dir(), 0).await?;
        let n = m.len();
        *self.manifest.write().await = Some(Arc::new(m));
        Ok(n)
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Id máy ổn định, sinh một lần rồi lưu lại.
///
/// Quan trọng với phát hiện xung đột: nếu id đổi mỗi lần mở app thì mọi lần
/// sao lưu đều trông như đến từ một máy lạ và sẽ báo xung đột giả.
fn device_id() -> String {
    let dir = dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("cloudsave-g4market");
    let path: PathBuf = dir.join("device-id");

    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::create_dir_all(&dir);
    if let Err(e) = std::fs::write(&path, &id) {
        log::warn!("không lưu được device id ({e}) — xung đột có thể báo sai");
    }
    id
}

fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "máy không rõ tên".into())
}
