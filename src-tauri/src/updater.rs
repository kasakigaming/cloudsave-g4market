//! Tự cập nhật: hỏi GitHub Releases có bản mới không, tải bộ cài, kiểm chữ ký,
//! cài đè rồi mở lại app.
//!
//! Dùng `tauri-plugin-updater`. Điểm hỏi (`latest.json` của release mới nhất)
//! và khoá công khai nằm trong `tauri.conf.json`; khoá bí mật để ký KHÔNG bao
//! giờ nằm trong repo. Bộ cài tải về mà chữ ký không khớp khoá công khai nhúng
//! trong app thì bị từ chối trước khi chạy — nên dù ai đó thay được file trên
//! đường truyền, họ không đẩy được code lạ vào máy người dùng.
//!
//! Trên Windows, `download_and_install` mở bộ cài NSIS ở chế độ `passive` (chỉ
//! hiện thanh tiến độ) rồi thoát app; bộ cài chạy xong tự mở lại app.

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    /// Phiên bản mới.
    pub version: String,
    /// Phiên bản đang chạy.
    pub current: String,
    /// Ghi chú phát hành (nội dung `notes` trong `latest.json`).
    pub notes: Option<String>,
    pub date: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateProgress {
    Downloading { downloaded: u64, total: Option<u64> },
    /// Tải xong, chữ ký hợp lệ, đang mở bộ cài.
    Installing,
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Other(format!("cập nhật: {e}"))
}

/// Có bản mới hơn bản đang chạy không. `None` = đang là bản mới nhất.
pub async fn check(app: &AppHandle) -> Result<Option<UpdateInfo>> {
    let update = app.updater().map_err(err)?.check().await.map_err(err)?;
    Ok(update.map(|u| UpdateInfo {
        version: u.version.clone(),
        current: u.current_version.clone(),
        notes: u.body.clone(),
        date: u.date.map(|d| d.to_string()),
    }))
}

/// Tải, kiểm chữ ký và cài bản mới. Hỏi lại điểm cập nhật thay vì giữ kết quả
/// của lần kiểm tra trước: giữa hai lần bấm có thể đã ra bản mới hơn nữa.
pub async fn install(app: &AppHandle) -> Result<()> {
    let update = app
        .updater()
        .map_err(err)?
        .check()
        .await
        .map_err(err)?
        .ok_or_else(|| Error::Other("đang là bản mới nhất, không có gì để cập nhật".into()))?;

    log::info!("cập nhật {} → {}", update.current_version, update.version);
    let mut downloaded = 0u64;
    let progress = app.clone();
    let done = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress.emit("update-progress", UpdateProgress::Downloading { downloaded, total });
            },
            move || {
                let _ = done.emit("update-progress", UpdateProgress::Installing);
            },
        )
        .await
        .map_err(err)?;

    // Windows: bộ cài đã chạy và app đã thoát trước khi tới dòng này. Các nền
    // khác thay file tại chỗ, nên phải tự mở lại.
    app.restart();
}
