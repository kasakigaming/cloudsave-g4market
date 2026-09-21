//! Theo dõi game đang chạy và chụp save ngay khi game tắt.
//!
//! Hai nguồn tín hiệu, gộp lại:
//!
//!   1. **Registry của Steam** — `HKCU\Software\Valve\Steam\RunningAppID` và
//!      `Apps\<appid>\Running`. Steam tự ghi khi nó khởi chạy game, nên đây là
//!      tín hiệu chính xác nhất, kể cả với game mà exe nằm ngoài thư mục cài.
//!   2. **Tiến trình** — exe nằm trong thư mục cài của một game. Bắt được game
//!      mở thẳng từ exe hay từ shortcut, lúc Steam không hề biết.
//!
//! Khi một game biến mất khỏi tập đang chạy, ta đợi `SETTLE` cho game ghi nốt
//! file (nhiều game ghi save trong lúc thoát) rồi mới chụp.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tauri::{AppHandle, Emitter, Manager};

use crate::local_store::{CaptureOutcome, Trigger};
use crate::state::AppState;
use crate::steam::InstalledApp;

const POLL: Duration = Duration::from_secs(2);
/// Đợi sau khi game tắt rồi mới chụp.
const SETTLE: Duration = Duration::from_secs(3);
/// `appmanifest_*.acf` chỉ đổi khi cài / gỡ game, không cần đọc lại mỗi 2 giây.
const INSTALLED_REFRESH: Duration = Duration::from_secs(60);
/// Chu kỳ ghi sẵn "tắt Steam Cloud" cho các tài khoản không đăng nhập.
const PREAPPLY_EVERY: Duration = Duration::from_secs(10);
/// Bấm tắt qua giao diện Steam thất bại thì bao lâu sau mới thử lại.
const UI_RETRY: Duration = Duration::from_secs(10 * 60);
/// Báo trước bao lâu rồi mới "mượn" chuột để bấm trong Steam.
const UI_COUNTDOWN: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize)]
pub struct RunningGame {
    pub app_id: u32,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GameChecked {
    pub app_id: u32,
    pub title: String,
    pub trigger: Trigger,
    /// Có khi thành công.
    pub outcome: Option<CaptureOutcome>,
    /// Có khi thất bại.
    pub error: Option<String>,
}

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move { run(app).await });
}

async fn run(app: AppHandle) {
    let mut sys = System::new();
    let mut prev: BTreeSet<u32> = BTreeSet::new();
    let mut pending: Vec<(u32, Instant)> = Vec::new();
    let mut installed: BTreeMap<u32, InstalledApp> = BTreeMap::new();
    let mut installed_at: Option<Instant> = None;
    // Tài khoản Steam đang đăng nhập lần kiểm tra trước; `Some(None)` = Steam
    // tắt. Bắt đầu bằng `None` để lần đầu luôn báo cho UI.
    let mut last_user: Option<Option<u32>> = None;
    let mut last_cloud: Option<crate::steam_cloud::SteamCloudStatus> = None;
    let mut preapply_at: Option<Instant> = None;
    let mut preapply_recent = std::collections::HashMap::new();
    let mut ui_tried: std::collections::HashMap<u32, Instant> = std::collections::HashMap::new();

    loop {
        let state = app.state::<AppState>();

        // Steam đổi tài khoản (hoặc bật / tắt): đổi theo và báo UI. Chỉ đổi
        // tài khoản theo dõi khi Steam THẬT SỰ đổi, để lựa chọn tay trong Cài
        // đặt không bị ghi đè mỗi 2 giây.
        let live = crate::steam::locate::active_user_now();
        if last_user != Some(live) {
            if let Some(id) = live {
                if state.watch_account() != Some(id) {
                    log::info!("Steam đổi sang tài khoản {id}");
                    state.set_watch_account(id);
                }
            }
            last_user = Some(live);
            // Phiên đăng nhập mới: đọc lại từ file, không dùng kết quả bấm cũ.
            *state.steam_cloud.ui_disabled.lock().unwrap() = None;
            let info = crate::commands::steam_account_info(&state);
            match (&live, &info) {
                (Some(_), Some(a)) => log::info!(
                    "Steam đang đăng nhập: {} ({}) — app theo dõi tài khoản {}",
                    a.persona_name.as_deref().unwrap_or("?"),
                    a.account_id,
                    state.watch_account().map_or("?".into(), |x| x.to_string())
                ),
                (None, _) => log::info!("Steam đã tắt hoặc chưa đăng nhập"),
                _ => {}
            }
            let _ = app.emit("steam-account", info);
        }

        if let Ok(steam) = state.steam() {
            if installed_at.map_or(true, |t| t.elapsed() > INSTALLED_REFRESH) {
                installed = steam.installed_apps();
                installed_at = Some(Instant::now());
            }

            let running = detect_running(&installed, &mut sys);
            if running != prev {
                for &id in running.difference(&prev) {
                    let title = state.title_of(id).await;
                    log::info!("game bắt đầu: {title} ({id})");
                    let _ = app.emit("game-started", RunningGame { app_id: id, title });
                }
                for &id in prev.difference(&running) {
                    let title = state.title_of(id).await;
                    log::info!("game đã tắt: {title} ({id}) — chụp sau {SETTLE:?}");
                    let _ = app.emit("game-exited", RunningGame { app_id: id, title });
                    // Mở lại rồi tắt tiếp trong lúc chờ: hẹn lại mốc mới.
                    pending.retain(|(p, _)| *p != id);
                    pending.push((id, Instant::now() + SETTLE));
                }
                state.set_running(running.clone());
                let _ = app.emit("running-games", state.running_games().await);
                prev = running;
            }

            // Steam Cloud: báo UI khi đổi.
            let cloud = crate::steam_cloud::status(&state, crate::steam_cloud::steam_running_in(&sys));
            if last_cloud.as_ref() != Some(&cloud) {
                if last_cloud.as_ref().map(|c| (c.account_id, c.state)) != Some((cloud.account_id, cloud.state)) {
                    log::info!(
                        "Steam Cloud của tài khoản {}: {:?}{}",
                        cloud.account_id.map_or("?".into(), |x| x.to_string()),
                        cloud.state,
                        if cloud.logged_in { " (đang đăng nhập)" } else { "" }
                    );
                }
                let _ = app.emit("steam-cloud", &cloud);
                last_cloud = Some(cloud);
            }

            // Tự động: ghi sẵn "tắt" cho các tài khoản đang KHÔNG đăng nhập, để
            // lần đăng nhập tới Steam đọc được. Không bao giờ khởi động lại Steam.
            if state.steam_cloud.auto_disable.load(std::sync::atomic::Ordering::Relaxed)
                && preapply_at.map_or(true, |t| t.elapsed() > PREAPPLY_EVERY)
            {
                preapply_at = Some(Instant::now());
                let known: Vec<u32> = steam.users.iter().map(|u| u.account_id).collect();
                let active = if crate::steam_cloud::steam_running_in(&sys) { live } else { None };
                let done = crate::steam_cloud::preapply_inactive(
                    &steam.root,
                    &crate::steam_cloud::backup_dir(),
                    &known,
                    active,
                    &mut preapply_recent,
                );
                if !done.is_empty() {
                    log::info!("đã ghi sẵn tắt Steam Cloud cho {} tài khoản: {done:?}", done.len());
                }
            }

            // Tự động, tài khoản ĐANG đăng nhập còn bật: bấm tắt trong Settings
            // của Steam (OCR). Không làm khi đang chơi game.
            if let (true, true, Some(acc)) = (
                state.steam_cloud.auto_disable.load(std::sync::atomic::Ordering::Relaxed),
                cloud_on_now(&last_cloud) && prev.is_empty(),
                last_cloud.as_ref().and_then(|c| c.account_id),
            ) {
                if ui_tried.get(&acc).map_or(true, |t| t.elapsed() > UI_RETRY) {
                    ui_tried.insert(acc, Instant::now());
                    log::info!("tự động tắt Steam Cloud qua giao diện Steam sau {UI_COUNTDOWN:?}");
                    let _ = app.emit(
                        "steam-cloud-ui",
                        "Sắp tắt Steam Cloud trong cửa sổ Steam — đừng động vào chuột vài giây",
                    );
                    let app2 = app.clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(UI_COUNTDOWN).await;
                        let st = app2.state::<AppState>();
                        match crate::steam_cloud::set_enabled(&st, false).await {
                            Ok(s) => {
                                let _ = app2.emit("steam-cloud", &s);
                            }
                            Err(e) => {
                                log::warn!("tự động tắt Steam Cloud qua giao diện thất bại: {e}");
                                let _ = app2.emit("steam-cloud-ui-error", e.to_string());
                            }
                        }
                    });
                }
            }

            // Chỉ chụp game đã tắt đủ lâu VÀ chưa bật lại.
            let now = Instant::now();
            let (due, later): (Vec<_>, Vec<_>) =
                pending.into_iter().partition(|(_, at)| *at <= now);
            pending = later;
            for (id, _) in due {
                if prev.contains(&id) {
                    continue;
                }
                check_and_emit(&app, id, Trigger::GameExit).await;
            }
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Chụp một game và báo kết quả cho UI. Dùng chung cho watcher và nút bấm.
pub async fn check_and_emit(app: &AppHandle, app_id: u32, trigger: Trigger) -> GameChecked {
    let state = app.state::<AppState>();
    let title = state.title_of(app_id).await;
    let result = match state.watch_account() {
        Some(account) => state.capture_game(account, app_id, trigger).await,
        None => Err(crate::error::Error::Other(
            "chưa xác định được tài khoản Steam".into(),
        )),
    };
    let ev = match result {
        Ok(outcome) => GameChecked {
            app_id,
            title,
            trigger,
            outcome: Some(outcome),
            error: None,
        },
        Err(e) => GameChecked {
            app_id,
            title,
            trigger,
            outcome: None,
            error: Some(e.to_string()),
        },
    };
    let _ = app.emit("game-checked", &ev);
    ev
}

/// Tập appid đang chạy, gộp từ registry Steam và danh sách tiến trình.
pub fn detect_running(installed: &BTreeMap<u32, InstalledApp>, sys: &mut System) -> BTreeSet<u32> {
    let mut out = steam_registry_running();

    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
    );
    for proc_ in sys.processes().values() {
        let Some(exe) = proc_.exe() else { continue };
        if let Some(id) = owning_app(exe, installed) {
            out.insert(id);
        }
    }
    out
}

/// Game nào sở hữu file exe này, dựa vào thư mục cài.
fn owning_app(exe: &Path, installed: &BTreeMap<u32, InstalledApp>) -> Option<u32> {
    let exe_s = normalize(exe);
    installed.values().find_map(|app| {
        let mut dir = normalize(&app.install_dir);
        if dir.is_empty() {
            return None;
        }
        if !dir.ends_with('/') {
            dir.push('/');
        }
        // So cả dấu '/' cuối để "Game" không khớp nhầm "Game 2".
        exe_s.starts_with(&dir).then_some(app.app_id)
    })
}

fn normalize(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    // `\\?\C:\…` do một số API trả về.
    let s = s.strip_prefix("//?/").unwrap_or(&s).to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

#[cfg(windows)]
fn steam_registry_running() -> BTreeSet<u32> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let mut out = BTreeSet::new();
    let Ok(steam) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam") else {
        return out;
    };
    if let Ok(id) = steam.get_value::<u32, _>("RunningAppID") {
        if id != 0 {
            out.insert(id);
        }
    }
    if let Ok(apps) = steam.open_subkey("Apps") {
        for name in apps.enum_keys().flatten() {
            let Ok(id) = name.parse::<u32>() else {
                continue;
            };
            let Ok(k) = apps.open_subkey(&name) else {
                continue;
            };
            if k.get_value::<u32, _>("Running").unwrap_or(0) == 1 {
                out.insert(id);
            }
        }
    }
    out
}

#[cfg(not(windows))]
fn steam_registry_running() -> BTreeSet<u32> {
    BTreeSet::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn installed() -> BTreeMap<u32, InstalledApp> {
        let mut m = BTreeMap::new();
        for (id, dir) in [
            (413150, r"C:\Steam\steamapps\common\Stardew Valley"),
            (1, r"C:\Steam\steamapps\common\Game"),
            (2, r"C:\Steam\steamapps\common\Game 2"),
        ] {
            m.insert(
                id,
                InstalledApp {
                    app_id: id,
                    name: format!("app {id}"),
                    install_dir: PathBuf::from(dir),
                },
            );
        }
        m
    }

    #[test]
    fn maps_exe_to_game_by_install_dir() {
        let i = installed();
        let exe = PathBuf::from(r"C:\Steam\steamapps\common\Stardew Valley\Stardew Valley.exe");
        assert_eq!(owning_app(&exe, &i), Some(413150));
    }

    #[test]
    fn matching_is_case_insensitive_on_windows() {
        if !cfg!(windows) {
            return;
        }
        let i = installed();
        let exe = PathBuf::from(r"c:\steam\STEAMAPPS\common\stardew valley\bin\x.exe");
        assert_eq!(owning_app(&exe, &i), Some(413150));
    }

    #[test]
    fn sibling_dir_with_shared_prefix_does_not_match() {
        // "Game" không được nuốt exe của "Game 2".
        let i = installed();
        let exe = PathBuf::from(r"C:\Steam\steamapps\common\Game 2\run.exe");
        assert_eq!(owning_app(&exe, &i), Some(2));
    }

    #[test]
    fn unrelated_exe_matches_nothing() {
        let i = installed();
        assert_eq!(owning_app(Path::new(r"C:\Windows\explorer.exe"), &i), None);
        assert_eq!(
            owning_app(Path::new(r"C:\Steam\steam.exe"), &i),
            None,
            "bản thân Steam không phải game"
        );
    }

    #[test]
    fn handles_verbatim_prefix() {
        let i = installed();
        let exe = PathBuf::from(r"\\?\C:\Steam\steamapps\common\Game\a.exe");
        assert_eq!(owning_app(&exe, &i), Some(1));
    }
}

/// Tài khoản đang đăng nhập và Steam Cloud của nó còn bật (chưa bấm tắt).
fn cloud_on_now(c: &Option<crate::steam_cloud::SteamCloudStatus>) -> bool {
    c.as_ref().is_some_and(|c| {
        c.logged_in
            && !c.busy
            && matches!(c.state, crate::steam_cloud::CloudState::On | crate::steam_cloud::CloudState::Queued)
    })
}
