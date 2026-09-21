//! Đẩy một bản lưu LOCAL lên Supabase — chỉ chạy khi người dùng bấm.
//!
//! Cố ý đẩy đúng bản đã chụp trong kho local chứ không quét lại đĩa: thứ người
//! dùng thấy trong danh sách và bấm "đẩy" phải là thứ lên cloud, kể cả khi file
//! trên đĩa đã đổi kể từ lúc chụp.
//!
//! Trình tự:
//!
//!   1. Kiểm tra xung đột TRƯỚC khi upload byte nào.
//!   2. Mỗi file chưa có trên server: nén theo `pack.rs` — thử mọi codec và
//!      delta so với phiên bản trước lẫn file anh em, giữ cái nhỏ nhất.
//!   3. Chỉ khi mọi blob đã nằm trên server mới ghi metadata và đánh dấu
//!      `complete`.
//!
//! Nhờ thứ tự đó, đứt mạng giữa chừng để lại một snapshot `pending` vô hại
//! thay vì một snapshot `complete` trỏ tới blob không tồn tại.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::blob;
use crate::error::{Error, Result};
use crate::local_store::{LocalFile, LocalSnapshot, LocalStore};
use crate::pack::{self, Base, MAX_DEPTH};

use crate::remote_blob;
use crate::supabase::Supabase;

/// Số file anh em lớn nhất thử làm tham chiếu cho mỗi file. Hai là đủ bắt
/// được cặp "save chính / bản hôm qua" kiểu Stardew mà không nhân chi phí nén.
const SIBLING_BASES: usize = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Progress {
    Started {
        total_files: usize,
        total_bytes: u64,
    },
    /// Đang nén một file (thử nhiều codec; file lớn có thể mất vài chục giây).
    Packing {
        file: String,
    },
    Uploading {
        file: String,
        stored_bytes: u64,
        delta: bool,
    },
    Finalizing,
    Done {
        snapshot_id: String,
        uploaded_bytes: u64,
        stored_bytes: u64,
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
    /// Dung lượng GỐC của các file phải gửi (chưa có trên server).
    pub uploaded_bytes: u64,
    /// Số byte THỰC SỰ gửi đi và lưu trên cloud, sau khi nén / delta.
    pub stored_bytes: u64,
    /// Byte không cần gửi vì server đã có blob trùng nội dung.
    pub deduped_bytes: u64,
    /// Số file được lưu dưới dạng delta (so với phiên bản trước hoặc file anh em).
    pub delta_files: usize,
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

    // ── 2. Cái gì đã có trên server ──────────────────────────────────────
    let mut hashes: Vec<String> = snap.files.iter().map(|f| f.hash.clone()).collect();
    hashes.sort();
    hashes.dedup();
    let mut meta = sb.blob_meta(&hashes).await?;
    let legacy: HashMap<String, i64> = sb.have_blobs(&hashes).await?.into_iter().collect();

    // Bản gốc delta ứng viên cho từng file: cùng (root, rel_path) trong lần
    // đẩy gần nhất trước bản này. Hỏi server về tất cả ứng viên một lượt.
    let bases = candidate_bases(store, snap)?;
    let base_hashes: Vec<String> = bases.values().cloned().collect();
    let base_meta = sb.blob_meta(&base_hashes).await?;
    let base_legacy: HashMap<String, i64> =
        sb.have_blobs(&base_hashes).await?.into_iter().collect();

    // ── 3. Nén và upload phần còn thiếu ──────────────────────────────────
    let mut uploaded_bytes = 0u64;
    let mut stored_bytes = 0u64;
    let mut deduped_bytes = 0u64;
    let mut delta_files = 0usize;

    // Blob nào đang dùng được làm bản gốc, kèm độ sâu chuỗi của nó.
    let mut available: HashMap<String, u32> = HashMap::new();
    for (h, m) in &meta {
        available.insert(h.clone(), m.depth);
    }
    for f in &snap.files {
        if legacy
            .get(&f.hash)
            .is_some_and(|n| *n >= blob::chunk_count(f.size) as i64)
        {
            available.insert(f.hash.clone(), 0);
        }
    }
    for (h, m) in &base_meta {
        available.insert(h.clone(), m.depth);
    }
    for h in base_legacy.keys() {
        available.entry(h.clone()).or_insert(0);
    }

    // Những file phải gửi, mỗi nội dung một lần, LỚN TRƯỚC: file nhỏ thường
    // là một phần của file lớn (SaveGameInfo nằm gọn trong save chính của
    // Stardew), nên file lớn phải lên trước để file nhỏ tham chiếu được.
    let mut to_send: Vec<&LocalFile> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for f in &snap.files {
        if available.contains_key(&f.hash) || !seen.insert(f.hash.as_str()) {
            deduped_bytes += f.size;
        } else {
            to_send.push(f);
        }
    }
    to_send.sort_by_key(|f| std::cmp::Reverse(f.size));

    // File anh em theo kích thước giảm dần — ứng viên tham chiếu cho nhau.
    let mut siblings: Vec<&LocalFile> = snap.files.iter().collect();
    siblings.sort_by_key(|f| std::cmp::Reverse(f.size));

    let mut plain_cache: HashMap<String, std::sync::Arc<Vec<u8>>> = HashMap::new();
    let mut read_plain = |hash: &str| -> Option<std::sync::Arc<Vec<u8>>> {
        if let Some(p) = plain_cache.get(hash) {
            return Some(p.clone());
        }
        // `read_blob` tự kiểm sha256: blob hỏng trên đĩa không bao giờ được
        // upload thành một bản sao lưu hỏng, cũng không được dùng làm gốc.
        let p = std::sync::Arc::new(store.read_blob(hash).ok()?);
        plain_cache.insert(hash.to_string(), p.clone());
        Some(p)
    };

    for f in to_send {
        let plain = read_plain(&f.hash).ok_or_else(|| {
            Error::Other(format!("thiếu hoặc hỏng blob local của '{}'", f.rel_path))
        })?;

        // Ứng viên bản gốc: phiên bản trước của chính file này, rồi tối đa
        // `SIBLING_BASES` file anh em lớn nhất đã có trên server. Chỉ nhận bản
        // có trên server (để giải về sau) và còn trong kho local (để nén bây giờ).
        let mut base_hashes: Vec<String> = Vec::new();
        if let Some(h) = bases.get(&key(f)) {
            base_hashes.push(h.clone());
        }
        let sibling_hashes: Vec<String> = siblings
            .iter()
            .filter(|s| s.hash != f.hash && available.contains_key(&s.hash))
            .map(|s| s.hash.clone())
            .filter(|h| !base_hashes.contains(h))
            .take(SIBLING_BASES)
            .collect();
        base_hashes.extend(sibling_hashes);
        let candidates: Vec<(String, std::sync::Arc<Vec<u8>>, u32)> = base_hashes
            .into_iter()
            .filter_map(|h| {
                let depth = *available.get(&h)?;
                if depth >= MAX_DEPTH {
                    return None;
                }
                Some((h.clone(), read_plain(&h)?, depth))
            })
            .collect();

        on_progress(Progress::Packing {
            file: f.rel_path.clone(),
        });
        // Nén cực hạn là việc nặng CPU (thử song song nhiều codec); đừng làm
        // nghẽn runtime async.
        let plain_for_pack = plain.clone();
        let packed = tokio::task::spawn_blocking(move || {
            let b: Vec<Base<'_>> = candidates
                .iter()
                .map(|(h, p, d)| Base {
                    hash: h,
                    plain: p,
                    depth: *d,
                })
                .collect();
            pack::pack(&plain_for_pack, &b)
        })
        .await
        .map_err(|e| Error::Other(format!("luồng nén bị huỷ: {e}")))??;

        let is_delta = packed.encoding.is_delta();
        on_progress(Progress::Uploading {
            file: f.rel_path.clone(),
            stored_bytes: packed.bytes.len() as u64,
            delta: is_delta,
        });
        let chunks = remote_blob::upload(sb, &f.hash, &packed).await?;

        available.insert(f.hash.clone(), packed.depth);
        meta.insert(
            f.hash.clone(),
            crate::supabase::BlobMeta {
                hash: f.hash.clone(),
                encoding: packed.encoding,
                base_hash: packed.base_hash.clone(),
                depth: packed.depth,
                chunk_count: chunks,
                stored_size: packed.bytes.len() as u64,
            },
        );
        uploaded_bytes += f.size;
        stored_bytes += packed.bytes.len() as u64;
        if is_delta {
            delta_files += 1;
        }
    }

    // ── 4. Metadata, rồi mới đánh dấu hoàn tất ───────────────────────────
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
            // Blob định dạng mới: số lát thật. Kiểu cũ: số chunk kiểu cũ — đường
            // khôi phục kiểu cũ cần con số này.
            let chunk_count = meta
                .get(&f.hash)
                .map_or_else(|| blob::chunk_count(f.size), |m| m.chunk_count);
            json!({
                "snapshot_id": snapshot_id,
                "root_token":  f.root,
                "rel_path":    f.rel_path,
                "size":        f.size,
                "mtime_utc":   f.mtime.map(|t| t.to_rfc3339()),
                "blob_hash":   f.hash,
                "chunk_count": chunk_count,
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
        stored_bytes,
        deduped_bytes,
    });

    Ok(BackupReport {
        snapshot_id,
        local_id: snap.id.clone(),
        file_count: snap.files.len(),
        total_bytes: snap.total_bytes,
        uploaded_bytes,
        stored_bytes,
        deduped_bytes,
        delta_files,
    })
}

fn key(f: &LocalFile) -> (String, String) {
    (format!("{:?}", f.root), f.rel_path.clone())
}

/// Với mỗi file của `snap`, hash của phiên bản trước gần nhất ĐÃ ĐẨY lên
/// cloud (cùng root và đường dẫn, khác nội dung).
fn candidate_bases(
    store: &LocalStore,
    snap: &LocalSnapshot,
) -> Result<HashMap<(String, String), String>> {
    let wanted: HashSet<(String, String)> = snap.files.iter().map(key).collect();
    let current: HashMap<(String, String), &str> = snap
        .files
        .iter()
        .map(|f| (key(f), f.hash.as_str()))
        .collect();

    let mut out = HashMap::new();
    // `list` trả mới nhất trước, nên lần đầu gặp một đường dẫn là phiên bản
    // gần nhất của nó.
    for older in store.list(Some(&snap.game_slug))? {
        if older.id == snap.id || older.remote_id.is_none() || older.created_at >= snap.created_at {
            continue;
        }
        for f in &older.files {
            let k = key(f);
            if !wanted.contains(&k) || out.contains_key(&k) {
                continue;
            }
            if current.get(&k).copied() == Some(f.hash.as_str()) {
                continue; // không đổi — đã dedupe cả file, không cần delta
            }
            out.insert(k, f.hash.clone());
        }
        if out.len() == wanted.len() {
            break;
        }
    }
    Ok(out)
}
