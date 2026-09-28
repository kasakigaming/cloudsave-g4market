//! Các lệnh Tauri mà frontend gọi được.
//!
//! Chia ba nhóm theo mức phụ thuộc:
//!
//!   - **Steam** và **local**: chạy hoàn toàn offline, không cần đăng nhập.
//!     Đây là luồng chính — quét, chụp, khôi phục đều xong ở đây.
//!   - **Cloud**: chỉ khi người dùng bấm. Đẩy đúng bản local đã chụp.
//!
//! Tầng này cố ý mỏng; logic thật nằm ở `scan`, `local_store`, `backup`,
//! `restore`, `watcher`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::backup::{self, BackupReport};
use crate::error::{Error, Result};
use crate::local_store::{CaptureOutcome, LocalSnapshot, Trigger};
use crate::restore::{self, RestoreReport};
use crate::scan::{slugify, GameScan, Scanner};
use crate::state::AppState;
use crate::steam::{RootContext, SteamUser};
use crate::steam_cloud::SteamCloudStatus;
use crate::supabase::{Session, SignUpOutcome};
use crate::watcher::{self, GameChecked, RunningGame};

#[derive(Debug, Serialize)]
pub struct SteamStatus {
    pub root: String,
    pub library_count: usize,
    pub users: Vec<SteamUser>,
    pub appinfo_apps: usize,
    /// Tài khoản đang được theo dõi — mặc định là tài khoản đăng nhập gần nhất.
    pub active_account: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GameCandidate {
    pub app_id: u32,
    pub title: String,
    pub slug: String,
    pub installed: bool,
    /// Steam khai báo `ufs.savefiles` cho game này.
    pub has_ufs: bool,
    /// Có thư mục trong `userdata/<account>/` — từng đồng bộ Cloud.
    pub has_userdata: bool,
    /// Tìm thấy trong ludusavi-manifest.
    pub in_manifest: bool,
    pub running: bool,
    /// Số bản lưu trong kho local.
    pub local_count: usize,
    pub last_local_at: Option<DateTime<Utc>>,
    /// Bản local mới nhất đã được đẩy lên cloud chưa.
    pub latest_pushed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScanProgress {
    Started {
        total: usize,
    },
    Game {
        done: usize,
        total: usize,
        app_id: u32,
        title: String,
        result: String,
    },
    Done,
}

#[derive(Debug, Default, Serialize)]
pub struct ScanAllReport {
    pub total: usize,
    /// Có thay đổi → đã ghi bản mới.
    pub created: usize,
    /// Giống bản gần nhất → không ghi.
    pub unchanged: usize,
    /// Không có file save nào trên đĩa.
    pub empty: usize,
    /// Đang chạy → để watcher chụp khi game tắt.
    pub skipped_running: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────
// Steam
// ─────────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn detect_steam(state: State<'_, AppState>) -> Result<SteamStatus> {
    let steam = state.steam()?;
    let appinfo = state.appinfo().await?;
    Ok(SteamStatus {
        root: steam.root.to_string_lossy().into_owned(),
        library_count: steam.libraries.len(),
        users: steam.users.clone(),
        appinfo_apps: appinfo.app_ids().count(),
        active_account: state.watch_account(),
    })
}

/// Đổi tài khoản Steam mà watcher và các lần chụp dùng.
/// Tài khoản Steam cho chip trên thanh trên cùng.
#[derive(Debug, Clone, Serialize)]
pub struct SteamAccount {
    pub account_id: u32,
    pub steam_id64: String,
    pub persona_name: Option<String>,
    pub account_name: Option<String>,
    /// Steam đang chạy và đang đăng nhập đúng tài khoản này.
    pub logged_in: bool,
    /// Ảnh đại diện dạng data URL, nếu Steam đã cache.
    pub avatar: Option<String>,
}

/// Dựng thông tin chip: tài khoản Steam đang đăng nhập nếu có, không thì tài
/// khoản app đang theo dõi (lần đăng nhập gần nhất).
pub fn steam_account_info(state: &AppState) -> Option<SteamAccount> {
    use base64::Engine;
    let steam = state.steam().ok()?;
    let live = crate::steam::locate::active_user_now();
    let id = live.or_else(|| state.watch_account())?;
    let u = steam.user_info(id);
    let avatar = std::fs::read(steam.avatar_file(id)).ok().map(|b| {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(b)
        )
    });
    Some(SteamAccount {
        account_id: id,
        steam_id64: u.steam_id64,
        persona_name: u.persona_name,
        account_name: u.account_name,
        logged_in: live == Some(id),
        avatar,
    })
}

#[tauri::command]
pub fn steam_account(state: State<'_, AppState>) -> Option<SteamAccount> {
    steam_account_info(&state)
}

#[tauri::command]
pub fn steam_cloud_status(state: State<'_, AppState>) -> SteamCloudStatus {
    crate::steam_cloud::status_now(&state)
}

/// Bật / tắt Steam Cloud. Tài khoản đang đăng nhập thì gạt công tắc trong
/// Settings của Steam (OCR, `steam_ui.rs`), không khởi động lại Steam.
#[tauri::command]
pub async fn set_steam_cloud(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<SteamCloudStatus> {
    let st = crate::steam_cloud::set_enabled(&state, enabled).await?;
    let _ = app.emit("steam-cloud", &st);
    Ok(st)
}

#[tauri::command]
pub fn set_auto_disable_steam_cloud(
    app: AppHandle,
    state: State<'_, AppState>,
    on: bool,
) -> Result<SteamCloudStatus> {
    crate::steam_cloud::save_auto_disable(on)?;
    state
        .steam_cloud
        .auto_disable
        .store(on, std::sync::atomic::Ordering::Relaxed);
    log::info!("tự động tắt Steam Cloud: {}", if on { "bật" } else { "tắt" });
    let st = crate::steam_cloud::status_now(&state);
    let _ = app.emit("steam-cloud", &st);
    Ok(st)
}

#[tauri::command]
pub fn set_account(state: State<'_, AppState>, account_id: u32) {
    state.set_watch_account(account_id);
}

#[tauri::command]
pub async fn list_games(state: State<'_, AppState>, account_id: u32) -> Result<Vec<GameCandidate>> {
    candidates(&state, account_id).await
}

/// Game đáng quan tâm cho một tài khoản, kèm tình trạng trong kho local.
///
/// Gộp ba tín hiệu: game đã cài, game Steam khai có save Cloud, và game từng
/// có dữ liệu trong `userdata/`. Khớp một tín hiệu là đủ — sót game khó chịu
/// hơn thừa game.
async fn candidates(state: &AppState, account_id: u32) -> Result<Vec<GameCandidate>> {
    let steam = state.steam()?;
    let appinfo = state.appinfo().await?;
    let manifest = state.manifest().await;

    let installed = steam.installed_apps();
    let with_userdata: BTreeSet<u32> = steam.apps_with_userdata(account_id).into_iter().collect();
    let running = state.running_ids();

    // Gom kho local theo slug một lần, thay vì đọc lại cho từng game.
    let mut local: BTreeMap<String, Vec<LocalSnapshot>> = BTreeMap::new();
    for s in state.store.list(None)? {
        local.entry(s.game_slug.clone()).or_default().push(s);
    }

    let mut ids: BTreeSet<u32> = installed.keys().copied().collect();
    ids.extend(with_userdata.iter().copied());

    let mut out = Vec::new();
    for app_id in ids {
        let has_ufs = !appinfo.save_rules(app_id).is_empty();
        let in_manifest = manifest
            .as_ref()
            .is_some_and(|m| m.by_steam_app(app_id).is_some());
        let has_userdata = with_userdata.contains(&app_id);
        if !has_ufs && !in_manifest && !has_userdata {
            continue;
        }

        let title = appinfo
            .app_name(app_id)
            .or_else(|| installed.get(&app_id).map(|a| a.name.clone()))
            .unwrap_or_else(|| format!("App {app_id}"));
        let slug = slugify(&title);
        let snaps = local.get(&slug);

        out.push(GameCandidate {
            app_id,
            installed: installed.contains_key(&app_id),
            has_ufs,
            has_userdata,
            in_manifest,
            running: running.contains(&app_id),
            local_count: snaps.map_or(0, Vec::len),
            last_local_at: snaps.and_then(|v| v.first()).map(|s| s.created_at),
            latest_pushed: snaps
                .and_then(|v| v.first())
                .is_some_and(|s| s.remote_id.is_some()),
            title,
            slug,
        });
    }

    out.sort_by_key(|g| g.title.to_lowercase());
    Ok(out)
}

#[tauri::command]
pub async fn scan_game(
    state: State<'_, AppState>,
    account_id: u32,
    app_id: u32,
) -> Result<GameScan> {
    let scanner = Scanner {
        steam: state.steam()?,
        appinfo: state.appinfo().await?,
        manifest: state.manifest().await,
        account_id,
    };
    tokio::task::spawn_blocking(move || scanner.scan_app(app_id))
        .await
        .map_err(|e| Error::Other(e.to_string()))
}

#[tauri::command]
pub async fn running_games(state: State<'_, AppState>) -> Result<Vec<RunningGame>> {
    Ok(state.running_games().await)
}

// ─────────────────────────────────────────────────────────────────────────
// Local — không cần đăng nhập
// ─────────────────────────────────────────────────────────────────────────

/// Quét mọi game và chụp vào kho local. Game đang chạy bị bỏ qua: file của nó
/// có thể đang ghi dở, và watcher sẽ chụp ngay khi nó tắt.
#[tauri::command]
pub async fn scan_all(
    app: AppHandle,
    state: State<'_, AppState>,
    account_id: u32,
) -> Result<ScanAllReport> {
    state.set_watch_account(account_id);
    let games = candidates(&state, account_id).await?;
    let total = games.len();
    let mut report = ScanAllReport {
        total,
        ..Default::default()
    };
    let _ = app.emit("scan-progress", ScanProgress::Started { total });

    for (i, g) in games.iter().enumerate() {
        let result = if state.is_running(g.app_id) {
            report.skipped_running += 1;
            "đang chạy — sẽ chụp khi tắt".to_string()
        } else {
            match state
                .capture_game(account_id, g.app_id, Trigger::ScanAll)
                .await
            {
                Ok(CaptureOutcome::Created { snapshot }) => {
                    report.created += 1;
                    format!("bản mới, {} file", snapshot.files.len())
                }
                Ok(CaptureOutcome::Unchanged { .. }) => {
                    report.unchanged += 1;
                    "không đổi".into()
                }
                Ok(CaptureOutcome::Empty) => {
                    report.empty += 1;
                    "không có save".into()
                }
                Err(e) => {
                    report.failed += 1;
                    report.errors.push(format!("{}: {e}", g.title));
                    format!("lỗi: {e}")
                }
            }
        };
        let _ = app.emit(
            "scan-progress",
            ScanProgress::Game {
                done: i + 1,
                total,
                app_id: g.app_id,
                title: g.title.clone(),
                result,
            },
        );
    }

    let _ = app.emit("scan-progress", ScanProgress::Done);
    log::info!(
        "quét xong tài khoản {account_id}: {} game — {} bản mới, {} không đổi, {} không có save, {} đang chạy, {} lỗi",
        report.total, report.created, report.unchanged, report.empty, report.skipped_running, report.failed
    );
    Ok(report)
}

/// Chụp một game ngay bây giờ.
#[tauri::command]
pub async fn capture_game(
    app: AppHandle,
    state: State<'_, AppState>,
    app_id: u32,
) -> Result<GameChecked> {
    if state.is_running(app_id) {
        return Err(Error::Other(
            "game đang chạy — file save có thể đang ghi dở. Bản lưu sẽ tự chụp khi game tắt."
                .into(),
        ));
    }
    Ok(watcher::check_and_emit(&app, app_id, Trigger::Manual).await)
}

#[tauri::command]
pub fn list_local(
    state: State<'_, AppState>,
    game_slug: Option<String>,
) -> Result<Vec<LocalSnapshot>> {
    state.store.list(game_slug.as_deref())
}

#[tauri::command]
pub async fn restore_local(
    state: State<'_, AppState>,
    snapshot_id: String,
) -> Result<RestoreReport> {
    let snap = state.store.get(&snapshot_id)?;
    let app_id = snap
        .steam_appid
        .ok_or_else(|| Error::Other("bản lưu này không gắn với appid Steam nào".into()))?;
    refuse_if_running(&state, app_id)?;

    // Đích là tài khoản ĐANG ĐĂNG NHẬP, không phải tài khoản đã chụp: Steam
    // dọn sạch chỗ save nếu file mang nhãn tài khoản khác (xem steam::autocloud).
    let plan = restore_plan(&state, Some(snap.account_id))?;
    let ctx = context_for(&state, plan.target_account, app_id)?;
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        restore::run_local(
            &store,
            &snap,
            &ctx,
            plan,
            &restore::default_safety_root(),
            |_| {},
        )
    })
    .await
    .map_err(|e| Error::Other(e.to_string()))?
}

/// Khôi phục cho tài khoản Steam nào.
///
/// Ưu tiên tài khoản Steam đang đăng nhập thật (registry `ActiveUser`). Steam
/// tắt thì dùng tài khoản app đang theo dõi — đó cũng là tài khoản sẽ đăng nhập
/// lần tới trong đại đa số trường hợp. Không có cái nào thì dừng, vì đoán sai
/// tài khoản nghĩa là Steam dời save đi ngay lần quét kế tiếp.
fn restore_plan(state: &AppState, source_account: Option<u32>) -> Result<restore::Plan> {
    let target = crate::steam::locate::active_user_now()
        .or_else(|| state.watch_account())
        .ok_or_else(crate::steam::autocloud::no_account)?;
    Ok(restore::Plan {
        target_account: target,
        source_account,
    })
}

fn refuse_if_running(state: &AppState, app_id: u32) -> Result<()> {
    if state.is_running(app_id) {
        // Ghi đè save trong lúc game chạy: game sẽ ghi đè lại khi thoát, hoặc
        // tệ hơn là đọc phải nửa file cũ nửa file mới.
        return Err(Error::Other("hãy tắt game trước khi khôi phục".into()));
    }
    Ok(())
}

fn context_for(state: &AppState, account_id: u32, app_id: u32) -> Result<RootContext> {
    let steam = state.steam()?;
    Ok(RootContext {
        game_install: steam
            .installed_apps()
            .get(&app_id)
            .map(|a| a.install_dir.clone()),
        steam_root: steam.root.clone(),
        account_id,
        app_id,
    })
}

// ─────────────────────────────────────────────────────────────────────────
// Cloud — chỉ khi người dùng bấm
// ─────────────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn supabase_configured(state: State<'_, AppState>) -> bool {
    state.supabase.is_some()
}

#[tauri::command]
pub async fn sign_in(
    state: State<'_, AppState>,
    email: String,
    password: String,
) -> Result<Session> {
    state.sb()?.sign_in(&email, &password).await
}

#[tauri::command]
pub async fn sign_up(
    state: State<'_, AppState>,
    email: String,
    password: String,
) -> Result<SignUpOutcome> {
    state.sb()?.sign_up(&email, &password).await
}

#[tauri::command]
pub async fn sign_out(state: State<'_, AppState>) -> Result<()> {
    if let Some(sb) = &state.supabase {
        sb.sign_out().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn current_session(state: State<'_, AppState>) -> Result<Option<Session>> {
    match &state.supabase {
        Some(sb) => Ok(sb.session().await),
        None => Ok(None),
    }
}

/// Đẩy một bản local lên Supabase.
#[tauri::command]
pub async fn push_local(
    app: AppHandle,
    state: State<'_, AppState>,
    snapshot_id: String,
    force: bool,
) -> Result<BackupReport> {
    let sb = state.sb()?;
    if sb.session().await.is_none() {
        return Err(Error::NotAuthenticated);
    }
    let snap = state.store.get(&snapshot_id)?;
    backup::push(sb, &state.store, &snap, &state.device, force, |p| {
        let _ = app.emit("push-progress", &p);
    })
    .await
}

#[tauri::command]
pub async fn list_snapshots(
    state: State<'_, AppState>,
    game_slug: Option<String>,
) -> Result<Value> {
    state.sb()?.list_snapshots(game_slug.as_deref()).await
}

#[tauri::command]
pub async fn restore_snapshot(
    app: AppHandle,
    state: State<'_, AppState>,
    snapshot_id: String,
    app_id: u32,
) -> Result<RestoreReport> {
    refuse_if_running(&state, app_id)?;
    let sb = state.sb()?;
    // Tài khoản đã chụp bản này, để đổi id tài khoản nằm trong đường dẫn. Bản
    // đẩy lên từ app cũ không có thông tin đó — khi đó chỉ khôi phục nguyên si.
    let source = sb.snapshot_account(&snapshot_id).await;
    let plan = restore_plan(&state, source)?;
    let ctx = context_for(&state, plan.target_account, app_id)?;
    let report = restore::run_remote(
        sb,
        &snapshot_id,
        &ctx,
        plan,
        &restore::default_safety_root(),
        |p| {
            let _ = app.emit("restore-progress", &p);
        },
    )
    .await?;
    // Bản đẩy từ app cũ mà dò ra tài khoản chắc chắn (đúng một SteamID64 trong
    // đường dẫn): ghi lại lên cloud cho lần sau khỏi phải dò. Hỏng thì thôi —
    // schema chưa có cột, hay mất mạng giữa chừng, đều không ảnh hưởng save.
    if let (restore::SourceOrigin::Path, Some(acc)) = (report.source_origin, report.source_account) {
        if let Err(e) = sb.set_snapshot_account(&snapshot_id, acc).await {
            log::warn!("không ghi được tài khoản Steam cho snapshot {snapshot_id}: {e}");
        }
    }
    Ok(report)
}

#[tauri::command]
pub async fn delete_snapshot(state: State<'_, AppState>, snapshot_id: String) -> Result<i64> {
    let sb = state.sb()?;
    sb.delete_snapshot(&snapshot_id).await?;
    // Bản local tương ứng không còn nằm trên cloud nữa — gỡ dấu cloud, nếu
    // không danh sách game vẫn hiện "☁ cloud" cho một bản đã mất.
    state.store.forget_remote(&snapshot_id)?;
    // Xoá snapshot không tự giải phóng dung lượng: blob còn đó cho tới khi
    // không snapshot nào tham chiếu nữa.
    sb.gc().await
}

/// Đối chiếu dấu "đã lên cloud" của kho local với danh sách thật trên cloud,
/// gỡ dấu của những bản đã bị xoá ở nơi khác (Dashboard, máy khác). Trả về
/// số bản đã gỡ dấu; 0 nếu chưa đăng nhập hoặc không đối chiếu được an toàn.
#[tauri::command]
pub async fn reconcile_cloud(state: State<'_, AppState>) -> Result<usize> {
    let Some(sb) = &state.supabase else {
        return Ok(0);
    };
    if sb.session().await.is_none() {
        return Ok(0);
    }
    match sb.remote_ids().await? {
        Some(alive) => state.store.forget_missing_remote(&alive),
        None => Ok(0),
    }
}

#[tauri::command]
pub async fn refresh_manifest(state: State<'_, AppState>) -> Result<usize> {
    state.reload_manifest().await
}

#[tauri::command]
pub fn device_info(state: State<'_, AppState>) -> Value {
    serde_json::json!({
        "id":    state.device.id,
        "name":  state.device.name,
        "store": state.store.root().to_string_lossy(),
    })
}

impl AppState {
    fn sb(&self) -> Result<&crate::supabase::Supabase> {
        self.supabase.as_ref().ok_or(Error::NotConfigured)
    }
}

// ── Ảnh nền tự chọn ───────────────────────────────────────────────────────

#[tauri::command]
pub fn list_backgrounds() -> Vec<crate::backgrounds::BgImage> {
    crate::backgrounds::list_in(&crate::backgrounds::dir())
}

/// Mở hộp thoại chọn ảnh (chọn được nhiều), chép vào thư mục nền của app.
/// Người dùng bấm huỷ thì trả về danh sách rỗng.
#[tauri::command]
pub async fn pick_backgrounds(app: AppHandle) -> Result<crate::backgrounds::AddReport> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app
        .dialog()
        .file()
        .set_title("Chọn ảnh nền")
        .add_filter("Ảnh", &["jpg", "jpeg", "png", "webp", "avif", "gif", "bmp"])
        .blocking_pick_files()
        .unwrap_or_default();
    let paths: Vec<std::path::PathBuf> = picked.into_iter().filter_map(|f| f.into_path().ok()).collect();
    let report = crate::backgrounds::add_in(&crate::backgrounds::dir(), &paths)?;
    if !report.added.is_empty() {
        log::info!("đã thêm {} ảnh nền", report.added.len());
    }
    Ok(report)
}

#[tauri::command]
pub fn remove_background(id: String) -> Result<()> {
    crate::backgrounds::remove_in(&crate::backgrounds::dir(), &id)
}

/// Một loạt ảnh phong cảnh ngẫu nhiên từ Wikimedia Commons (chế độ xoay vòng).
#[tauri::command]
pub async fn web_backgrounds() -> Result<Vec<crate::web_backgrounds::WebPhoto>> {
    crate::web_backgrounds::fetch_batch().await
}
