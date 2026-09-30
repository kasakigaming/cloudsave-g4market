//! CloudSave G4Market — sao lưu save game lên Supabase.
//!
//! Bốn ý tưởng nền, theo thứ tự quan trọng:
//!
//! 1. **Đường dẫn save đọc từ chính Steam.** `appinfo.vdf` khai sẵn nhánh
//!    `ufs.savefiles` cho từng game. Đọc được hoàn toàn offline, không cần
//!    hook vào tiến trình Steam như CloudRedirect.
//!
//! 2. **Lưu theo (root_token, rel_path) chứ không phải đường dẫn tuyệt đối.**
//!    Ludusavi lưu nguyên `C:\Users\foo\...`; ta lưu `WinAppDataRoaming` +
//!    `StardewValley/Saves/...` nên khôi phục sang máy khác vẫn đúng chỗ.
//!
//! 3. **Local trước, cloud sau.** Mọi lần chụp ghi vào kho trên máy, không
//!    cần mạng hay đăng nhập. Watcher theo dõi game đang chạy và chụp ngay khi
//!    game tắt. Supabase chỉ nhận dữ liệu khi người dùng bấm đẩy.
//!
//! 4. **Bytes là nguồn chân lý.** Nội dung file đi vào Postgres dưới dạng
//!    bytea đã nén và cắt chunk, có SHA-256 kiểm chứng. Cột `preview` chỉ để
//!    hiển thị và không bao giờ tham gia vào việc dựng lại file.

pub mod applog;
pub mod backgrounds;
pub mod backup;
pub mod blob;
pub mod commands;
pub mod error;
pub mod local_store;
pub mod manifest;
pub mod pack;
pub mod preview;
pub mod remote_blob;
pub mod restore;
pub mod scan;
pub mod state;
pub mod steam;
pub mod steam_cloud;
#[cfg(windows)]
pub mod steam_ui;
pub mod supabase;
pub mod watcher;
pub mod web_backgrounds;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    applog::init();
    log::info!("khởi động {}", env!("CARGO_PKG_VERSION"));
    load_dotenv();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new())
        .setup(|app| {
            // Watcher chạy suốt vòng đời app: biết game nào đang chơi và chụp
            // save ngay khi game tắt. Không cần mạng, không cần đăng nhập.
            watcher::spawn(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Steam
            commands::detect_steam,
            commands::set_account,
            commands::steam_account,
            commands::list_games,
            commands::scan_game,
            commands::running_games,
            commands::steam_cloud_status,
            commands::set_steam_cloud,
            commands::set_auto_disable_steam_cloud,
            // Local
            commands::scan_all,
            commands::capture_game,
            commands::list_local,
            commands::restore_local,
            // Cloud
            commands::supabase_configured,
            commands::sign_in,
            commands::sign_up,
            commands::sign_out,
            commands::current_session,
            commands::push_local,
            commands::list_snapshots,
            commands::cloud_games,
            commands::restore_snapshot,
            commands::delete_snapshot,
            commands::reconcile_cloud,
            // Khác
            commands::refresh_manifest,
            commands::device_info,
            commands::list_backgrounds,
            commands::pick_backgrounds,
            commands::remove_background,
            commands::web_backgrounds,
        ])
        .run(tauri::generate_context!())
        .expect("không khởi động được cửa sổ Tauri");
}

/// Nạp `.env` từ thư mục làm việc hoặc cạnh file thực thi.
///
/// Viết tay thay vì thêm crate `dotenvy`: ta chỉ cần hai biến, và giữ được
/// nguyên tắc không bao giờ ghi đè biến môi trường đã có sẵn của hệ thống.
fn load_dotenv() {
    let mut candidates = vec![std::path::PathBuf::from(".env")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(".env"));
        }
    }

    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let k = k.trim();
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if std::env::var_os(k).is_none() {
                std::env::set_var(k, v);
            }
        }
        return;
    }
}
