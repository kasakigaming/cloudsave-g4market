//! Đẩy một bản lưu LOCAL lên Supabase — chỉ chạy khi người dùng bấm.
//!
//! Cố ý đẩy đúng bản đã chụp trong kho local chứ không quét lại đĩa: thứ người
//! dùng thấy trong danh sách và bấm "đẩy" phải là thứ lên cloud, kể cả khi file
//! trên đĩa đã đổi kể từ lúc chụp.
//!
//! Trình tự:
//!
//!   1. Kiểm tra xung đột TRƯỚC khi upload byte nào.
//!   2. Upload blob còn thiếu (hỏi server trước để khỏi gửi lại thứ đã có).
//!   3. Chỉ khi mọi blob đã nằm trên server mới ghi metadata và đánh dấu
//!      `complete`.
//!
//! Nhờ thứ tự đó, đứt mạng giữa chừng để lại một snapshot `pending` vô hại
//! thay vì một snapshot `complete` trỏ tới blob không tồn tại.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::blob;
use crate::error::{Error, Result};
use crate::local_store::{LocalSnapshot, LocalStore};
use crate::supabase::Supabase;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Progress {
    Started {
        total_files: usize,
        total_bytes: u64,
    },
    Uploading {
        file: String,
        chunk: u32,
        chunks: u32,
    },
    Finalizing,
    Done {
        snapshot_id: String,
        uploaded_bytes: u64,
        deduped_bytes: u64,
    },
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupReport {
    /// Id snapshot trên Supabase.
    pub snapshot_id: String,
    pub local_id: String,
    pub file_count: usize,
    pub total_bytes: u64,
    pub uploaded_bytes: u64,
    /// Byte không cần gửi vì server đã có blob trùng nội dung.
    pub deduped_bytes: u64,
}

/// `force` bỏ qua kiểm tra xung đột — chỉ dùng khi người dùng đã chủ động
/// chọn "ghi đè bằng bản của máy này" trên UI.
pub async fn push<F>(
    sb: &Supabase,
    store: &LocalStore,
    snap: &LocalSnapshot,
    device: &DeviceInfo,
    force: bool,
    mut on_progress: F,
) -> Result<BackupReport>
where
    F: FnMut(Progress),
{
    if snap.files.is_empty() {
        return Err(Error::Other("bản lưu này không có file nào".into()));
    }

    // ── 1. Xung đột ──────────────────────────────────────────────────────
    // Save game không merge được. Nếu một máy khác đã đẩy bản mới hơn thì
    // người dùng phải tự chọn giữ bên nào.
    let head = sb.head_snapshot(&snap.game_slug).await?;
    let parent_id = head
        .as_ref()
        .and_then(|h| h["id"].as_str().map(str::to_owned));

    if !force {
        if let Some(h) = &head {
            let other = h["device_id"].as_str().unwrap_or("");
            if !other.is_empty() && other != device.id {
                return Err(Error::Conflict {
                    other_device: other.to_string(),
                    remote_head: parent_id.clone().unwrap_or_default(),
                });
            }
        }
    }

    on_progress(Progress::Started {
        total_files: snap.files.len(),
        total_bytes: snap.total_bytes,
    });

    // ── 2. Dedupe rồi upload ─────────────────────────────────────────────
    let hashes: Vec<String> = snap.files.iter().map(|f| f.hash.clone()).collect();
    let have = sb.have_blobs(&hashes).await?;

    let mut uploaded_bytes = 0u64;
    let mut deduped_bytes = 0u64;
    // Hai file cùng nội dung trong một snapshot chỉ cần gửi một lần.
    let mut sent: HashSet<&str> = HashSet::new();

    for f in &snap.files {
        let expected = blob::chunk_count(f.size) as i64;
        let on_server = have.iter().any(|(h, n)| h == &f.hash && *n >= expected);
        if on_server || sent.contains(f.hash.as_str()) {
            deduped_bytes += f.size;
            continue;
        }

        // `read_blob` tự kiểm sha256, nên blob hỏng trên đĩa không bao giờ
        // được upload thành một bản sao lưu hỏng.
        let bytes = store.read_blob(&f.hash)?;
        let prepared = blob::prepare(&bytes);
        let total_chunks = prepared.chunks.len() as u32;
        for c in &prepared.chunks {
            on_progress(Progress::Uploading {
                file: f.rel_path.clone(),
                chunk: c.idx + 1,
                chunks: total_chunks,
            });
            sb.put_chunk(&f.hash, c).await?;
        }
        sent.insert(f.hash.as_str());
        uploaded_bytes += f.size;
    }

    // ── 3. Metadata, rồi mới đánh dấu hoàn tất ───────────────────────────
    on_progress(Progress::Finalizing);

    let snapshot_id = sb
        .insert_snapshot(json!({
            "game_slug":   snap.game_slug,
            "game_title":  snap.game_title,
            "steam_appid": snap.steam_appid,
            "device_id":   device.id,
            "device_name": device.name,
            "parent_id":   parent_id,
            "file_count":  snap.files.len(),
            "total_bytes": snap.total_bytes,
            "status":      "pending",
        }))
        .await?;

    let rows: Vec<_> = snap
        .files
        .iter()
        .map(|f| {
            json!({
                "snapshot_id": snapshot_id,
                "root_token":  f.root,
                "rel_path":    f.rel_path,
                "size":        f.size,
                "mtime_utc":   f.mtime.map(|t| t.to_rfc3339()),
                "blob_hash":   f.hash,
                "chunk_count": blob::chunk_count(f.size),
            })
        })
        .collect();

    // Hỏng ở đây thì snapshot ở lại `pending` và bị lọc khỏi mọi truy vấn —
    // không bao giờ bị nhầm là bản sao lưu dùng được.
    sb.insert_files(rows).await?;
    sb.finalize_snapshot(&snapshot_id, snap.preview.clone())
        .await?;

    store.mark_pushed(&snap.id, &snapshot_id)?;

    on_progress(Progress::Done {
        snapshot_id: snapshot_id.clone(),
        uploaded_bytes,
        deduped_bytes,
    });

    Ok(BackupReport {
        snapshot_id,
        local_id: snap.id.clone(),
        file_count: snap.files.len(),
        total_bytes: snap.total_bytes,
        uploaded_bytes,
        deduped_bytes,
    })
}
