//! Báo và tắt Steam Cloud ngay trong app.
//!
//! Steam Cloud bật song song với CloudSave là hai bên cùng giữ một bộ save:
//! khôi phục xong, Steam có thể đồng bộ đè bản cũ trên cloud của nó xuống, hoặc
//! bật hộp thoại xung đột lúc mở game. Nên app báo đỏ khi Steam Cloud đang bật.
//!
//! Công tắc nằm trong `sharedconfig.vdf` (xem `steam::cloudcfg`). Steam chỉ đọc
//! nó LÚC ĐĂNG NHẬP, và giữ trong bộ nhớ suốt phiên. Nên cách tự động ở đây là
//! **ghi sẵn cho các tài khoản đang không đăng nhập** — lần đăng nhập tới Steam
//! đọc được giá trị tắt, không cần khởi động lại Steam (với tài khoản không lưu
//! mật khẩu, khởi động lại = phải đăng nhập lại từ đầu).
//!
//! Tài khoản đang đăng nhập mà còn bật: app mở Settings → Cloud của Steam và
//! bấm công tắc giống người dùng (`steam_ui.rs`, dùng OCR). Có hiệu lực ngay,
//! không khởi động lại Steam. App không bao giờ khởi động lại Steam.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

use crate::error::{Error, Result};
use crate::state::AppState;
use crate::steam::cloudcfg;

/// Không ghi lại cùng một tài khoản dày hơn thế này (Steam có thể ghi đè
/// file ngay sau khi đăng xuất; ta ghi lại lần sau, không giằng co liên tục).
const REWRITE_GAP: Duration = Duration::from_secs(30);
/// Id tài khoản Steam nhỏ hơn thế này coi như không phải tài khoản thật.
const MIN_ACCOUNT_ID: u32 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudState {
    /// Đang bật.
    On,
    /// Đang bật, nhưng app đã ghi sẵn "tắt" — có hiệu lực lần đăng nhập tới.
    Queued,
    Off,
    /// Không đọc được.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SteamCloudStatus {
    pub account_id: Option<u32>,
    pub state: CloudState,
    pub steam_running: bool,
    /// Tài khoản này đang đăng nhập Steam ngay lúc này.
    pub logged_in: bool,
    /// Người dùng đã bật "tự động tắt Steam Cloud".
    pub auto_disable: bool,
    /// Đang tắt / mở lại Steam.
    pub busy: bool,
    pub error: Option<String>,
}

/// Tuỳ chọn lưu trên máy, cạnh `device-id`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Settings {
    #[serde(default)]
    auto_disable_steam_cloud: bool,
}

fn app_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("cloudsave-g4market")
}

fn settings_path() -> PathBuf {
    app_dir().join("settings.json")
}

pub fn load_auto_disable() -> bool {
    std::fs::read(settings_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
        .map_or(false, |s| s.auto_disable_steam_cloud)
}

pub fn save_auto_disable(on: bool) -> Result<()> {
    let p = settings_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let s = Settings {
        auto_disable_steam_cloud: on,
    };
    std::fs::write(&p, serde_json::to_vec_pretty(&s).map_err(|e| Error::Other(e.to_string()))?)?;
    Ok(())
}

/// Cờ chung trong `AppState`.
#[derive(Default)]
pub struct Flags {
    pub auto_disable: AtomicBool,
    pub busy: AtomicBool,
    /// Tài khoản vừa được tắt qua giao diện Steam trong phiên đăng nhập này.
    /// Steam có thể chưa ghi file ngay, nên nhớ lại để khỏi mở Settings lần nữa.
    pub ui_disabled: std::sync::Mutex<Option<u32>>,
}

/// `steam.exe` có đang chạy không. `sys` phải vừa refresh tiến trình.
pub fn steam_running_in(sys: &System) -> bool {
    sys.processes()
        .values()
        .any(|p| p.name().to_string_lossy().eq_ignore_ascii_case("steam.exe"))
}

fn steam_running_now() -> bool {
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    steam_running_in(&sys)
}

/// Trạng thái Steam Cloud của một tài khoản, đọc từ đĩa.
pub fn account_state(steam_root: &Path, account: u32) -> Result<CloudState> {
    let remote = cloudcfg::read_enabled(&cloudcfg::sharedconfig_path(steam_root, account))?;
    let aside_path = cloudcfg::writeaside_path(steam_root, account);
    let aside = if aside_path.exists() {
        Some(cloudcfg::read_enabled(&aside_path)?)
    } else {
        None
    };
    Ok(match (remote, aside) {
        (true, Some(false)) => CloudState::Queued,
        (true, _) | (false, Some(true)) => CloudState::On,
        (false, _) => CloudState::Off,
    })
}

pub fn status(state: &AppState, steam_running: bool) -> SteamCloudStatus {
    let account_id = state.watch_account();
    let (cloud, error) = match (state.steam(), account_id) {
        (Ok(s), Some(id)) => match account_state(&s.root, id) {
            Ok(v) => (v, None),
            Err(e) => (CloudState::Unknown, Some(e.to_string())),
        },
        (Err(e), _) => (CloudState::Unknown, Some(e.to_string())),
        (_, None) => (CloudState::Unknown, None),
    };
    let live = crate::steam::locate::active_user_now();
    let logged_in = steam_running && live.is_some() && live == account_id;
    let ui_off = *state.steam_cloud.ui_disabled.lock().unwrap();
    let cloud = if logged_in && ui_off.is_some() && ui_off == account_id && cloud != CloudState::Unknown {
        CloudState::Off
    } else {
        cloud
    };
    SteamCloudStatus {
        account_id,
        state: cloud,
        steam_running,
        logged_in,
        auto_disable: state.steam_cloud.auto_disable.load(Ordering::Relaxed),
        busy: state.steam_cloud.busy.load(Ordering::Relaxed),
        error,
    }
}

pub fn status_now(state: &AppState) -> SteamCloudStatus {
    status(state, steam_running_now())
}

/// Các tài khoản đáng để ghi sẵn: có roaming config của Steam, hoặc có trong
/// `loginusers.vdf`. Thư mục `userdata/<id>` rỗng (tài khoản cũ, rác) thì bỏ.
fn candidate_accounts(steam_root: &Path, known: &[u32]) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(steam_root.join("userdata")) else {
        return Vec::new();
    };
    let mut out: Vec<u32> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        // Tài khoản thật có id lớn; `userdata/7` và các thư mục số nhỏ khác là
        // rác do công cụ bên ngoài tạo ra, không phải tài khoản.
        .filter(|&id| id >= MIN_ACCOUNT_ID)
        .filter(|&id| known.contains(&id) || cloudcfg::sharedconfig_path(steam_root, id).exists())
        .collect();
    out.sort_unstable();
    out
}

/// Tự động: ghi sẵn "tắt" cho mọi tài khoản ĐANG KHÔNG đăng nhập mà Steam
/// Cloud còn bật. `active` là tài khoản đang đăng nhập (không đụng tới: Steam
/// đang giữ nó trong bộ nhớ). Trả về các tài khoản vừa ghi.
pub fn preapply_inactive(
    steam_root: &Path,
    backup: &Path,
    known: &[u32],
    active: Option<u32>,
    recent: &mut HashMap<u32, Instant>,
) -> Vec<u32> {
    let mut done = Vec::new();
    for id in candidate_accounts(steam_root, known) {
        if Some(id) == active {
            continue;
        }
        match account_state(steam_root, id) {
            Ok(CloudState::On) => {}
            Ok(_) => continue,
            Err(e) => {
                log::warn!("không đọc được Steam Cloud của tài khoản {id}: {e}");
                continue;
            }
        }
        if recent.get(&id).is_some_and(|t| t.elapsed() < REWRITE_GAP) {
            continue;
        }
        recent.insert(id, Instant::now());
        match write_both(steam_root, backup, id, false) {
            Ok(()) => done.push(id),
            Err(e) => log::warn!("không ghi sẵn được Steam Cloud cho tài khoản {id}: {e}"),
        }
    }
    done
}

/// Bật / tắt Steam Cloud cho tài khoản đang theo dõi (nút bấm tay).
///
/// Tài khoản đó đang đăng nhập thì phải tắt Steam, sửa, rồi mở lại — người
/// dùng đã được báo trước là có thể phải đăng nhập lại.
pub async fn set_enabled(state: &AppState, enabled: bool) -> Result<SteamCloudStatus> {
    let steam = state.steam()?;
    let account = state
        .watch_account()
        .ok_or_else(|| Error::Other("chưa xác định được tài khoản Steam".into()))?;

    if state.steam_cloud.busy.swap(true, Ordering::SeqCst) {
        return Err(Error::Other("đang xử lý Steam Cloud, đợi chút".into()));
    }
    let result = apply(state, &steam.root, account, enabled).await;
    state.steam_cloud.busy.store(false, Ordering::SeqCst);
    result?;
    Ok(status_now(state))
}

async fn apply(state: &AppState, steam_root: &Path, account: u32, enabled: bool) -> Result<()> {
    let label = if enabled { "bật" } else { "tắt" };
    let logged_in = steam_running_now() && crate::steam::locate::active_user_now() == Some(account);
    if !logged_in {
        write_both(steam_root, &backup_dir(), account, enabled)?;
        log::info!("đã {label} Steam Cloud cho tài khoản {account} (có hiệu lực lần đăng nhập tới)");
        return Ok(());
    }
    if enabled {
        return Err(Error::Other(
            "tài khoản đang đăng nhập — bật lại trong Steam → Settings → Cloud".into(),
        ));
    }
    disable_logged_in(state, steam_root, account).await
}

/// Tắt cho tài khoản ĐANG đăng nhập: bấm công tắc trong Settings của Steam.
pub async fn disable_logged_in(state: &AppState, steam_root: &Path, account: u32) -> Result<()> {
    if !state.running_ids().is_empty() {
        return Err(Error::Other(
            "đang có game chạy — tắt game trước (phải mở cửa sổ Settings của Steam)".into(),
        ));
    }
    #[cfg(windows)]
    {
        let root = steam_root.to_path_buf();
        let report = tokio::task::spawn_blocking(move || crate::steam_ui::disable_via_ui(&root, true, None))
            .await
            .map_err(|e| Error::Other(format!("luồng OCR bị huỷ: {e}")))?;
        let report = report?;
        log::info!(
            "tắt Steam Cloud qua giao diện Steam: {:?} → {:?} — {}",
            report.before,
            report.after,
            report.steps.join("; ")
        );
        *state.steam_cloud.ui_disabled.lock().unwrap() = Some(account);
        // Ghi luôn write-aside, để lần đăng nhập sau không phụ thuộc vào việc
        // Steam đã kịp đẩy cài đặt lên server hay chưa.
        let aside = cloudcfg::writeaside_path(steam_root, account);
        let _ = cloudcfg::write_enabled(
            &aside,
            false,
            &backup_dir().join(format!("{account}-config-sharedconfig.vdf")),
        );
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (steam_root, account);
        Err(Error::Other("chỉ hỗ trợ trên Windows".into()))
    }
}

/// Bản gốc các file của Steam, giữ trong thư mục của app — KHÔNG để cạnh file
/// của Steam, vì mọi thứ trong `7/remote/` đều bị Steam Cloud đẩy lên server.
pub fn backup_dir() -> PathBuf {
    app_dir().join("steam-backup")
}

/// Ghi vào bản trong `7/remote/` (Steam đọc khi khởi động) và vào write-aside
/// store (Steam gộp vào bản cloud lúc đăng nhập, để server không đè lại).
fn write_both(steam_root: &Path, dir: &Path, account: u32, enabled: bool) -> Result<()> {
    let remote = cloudcfg::sharedconfig_path(steam_root, account);
    let aside = cloudcfg::writeaside_path(steam_root, account);
    let aside_bak = dir.join(format!("{account}-config-sharedconfig.vdf"));

    if !remote.exists() {
        // Tài khoản chưa từng có roaming config: chỉ để lại write-aside, không
        // tự bịa file đồng bộ của Steam.
        return cloudcfg::write_enabled(&aside, enabled, &aside_bak);
    }
    cloudcfg::write_enabled(&remote, enabled, &dir.join(format!("{account}-remote-sharedconfig.vdf")))?;
    if !aside.exists() {
        // Chưa có write-aside: dựng từ bản vừa ghi, để Steam gộp ra đúng bộ
        // cài đặt hiện tại chỉ khác đúng công tắc này.
        if let Some(d) = aside.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::copy(&remote, &aside)?;
        return Ok(());
    }
    cloudcfg::write_enabled(&aside, enabled, &aside_bak)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON: &str = "\"UserRoamingConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"CloudEnabled\"\t\t\"1\"\n\t\t\t}\n\t\t}\n\t}\n}\n";

    fn fake_steam() -> PathBuf {
        let root = std::env::temp_dir().join(format!("cs-steamcloud-{}", uuid::Uuid::new_v4()));
        for (id, remote) in [(1111u32, Some(ON)), (2222, Some(ON)), (3333, None), (4444, None), (7, Some(ON))] {
            let d = root.join("userdata").join(id.to_string());
            std::fs::create_dir_all(&d).unwrap();
            if let Some(t) = remote {
                let p = cloudcfg::sharedconfig_path(&root, id);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, t).unwrap();
            }
        }
        root
    }

    #[test]
    fn preapplies_every_inactive_account_but_not_the_active_one() {
        let root = fake_steam();
        let mut recent = HashMap::new();
        // 3333 có trong loginusers.vdf nhưng chưa có roaming config; 4444 là
        // thư mục rác — không đụng.
        let done = preapply_inactive(&root, &root.join("backup"), &[3333], Some(1111), &mut recent);
        assert_eq!(done, vec![2222, 3333]);

        assert_eq!(account_state(&root, 1111).unwrap(), CloudState::On, "đang đăng nhập: để yên");
        assert_eq!(account_state(&root, 2222).unwrap(), CloudState::Off);
        assert_eq!(account_state(&root, 3333).unwrap(), CloudState::Queued);
        assert!(!cloudcfg::sharedconfig_path(&root, 3333).exists(), "không bịa file đồng bộ");
        assert!(!root.join("userdata/4444/config").exists());
        assert!(!root.join("userdata/7/config").exists(), "thư mục số nhỏ không phải tài khoản");
        assert!(
            !cloudcfg::read_enabled(&cloudcfg::writeaside_path(&root, 2222)).unwrap(),
            "có write-aside để Steam gộp lúc đăng nhập"
        );
    }

    #[test]
    fn account_that_logs_out_gets_written_next_pass() {
        let root = fake_steam();
        let mut recent = HashMap::new();
        preapply_inactive(&root, &root.join("backup"), &[], Some(1111), &mut recent);
        // 1111 đăng xuất, 2222 đăng nhập.
        let done = preapply_inactive(&root, &root.join("backup"), &[], Some(2222), &mut recent);
        assert_eq!(done, vec![1111]);
        assert_eq!(account_state(&root, 1111).unwrap(), CloudState::Off);
    }

    #[test]
    fn steam_overwriting_after_logout_is_retried_after_gap_only() {
        let root = fake_steam();
        let mut recent = HashMap::new();
        preapply_inactive(&root, &root.join("backup"), &[], None, &mut recent);
        // Steam ghi đè cả hai file ngay sau đó (vd lúc thoát).
        std::fs::write(cloudcfg::sharedconfig_path(&root, 2222), ON).unwrap();
        std::fs::remove_file(cloudcfg::writeaside_path(&root, 2222)).unwrap();
        assert!(preapply_inactive(&root, &root.join("backup"), &[], None, &mut recent).is_empty(), "chưa hết REWRITE_GAP");
        recent.clear();
        assert_eq!(preapply_inactive(&root, &root.join("backup"), &[], None, &mut recent), vec![2222]);
    }

    #[test]
    fn queued_account_is_not_rewritten() {
        let root = fake_steam();
        let mut recent = HashMap::new();
        preapply_inactive(&root, &root.join("backup"), &[], None, &mut recent);
        // Steam đè bản remote về 1 nhưng write-aside "tắt" vẫn còn: đã hẹn,
        // không ghi nữa (tránh giằng co với Steam).
        std::fs::write(cloudcfg::sharedconfig_path(&root, 2222), ON).unwrap();
        recent.clear();
        assert_eq!(account_state(&root, 2222).unwrap(), CloudState::Queued);
        assert!(!preapply_inactive(&root, &root.join("backup"), &[], None, &mut recent).contains(&2222));
    }
}
