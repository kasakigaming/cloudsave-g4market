//! Dựng lại file save trên máy hiện tại, từ bản local hoặc bản trên cloud.
//!
//! Đây là thao tác nguy hiểm nhất trong app: nó ghi đè lên tiến trình chơi
//! thật. Bốn lớp phòng vệ, theo thứ tự áp dụng — giống nhau cho cả hai nguồn:
//!
//!   1. `rel_path` được kiểm tra lại, không tin vào dữ liệu đã lưu.
//!   2. Đường dẫn cuối cùng phải nằm trong thư mục root đã phân giải.
//!   3. Nội dung phải khớp SHA-256 trước khi chạm tới đĩa.
//!   4. File hiện có được cất vào thư mục cứu hộ trước, rồi mới ghi đè bằng
//!      tmp-then-rename để không bao giờ để lại file ghi dở.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::blob::{self, Chunk};
use crate::error::{Error, Result};
use crate::local_store::{LocalSnapshot, LocalStore};
use crate::steam::{remotecache, RootContext, RootToken};
use crate::supabase::Supabase;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Progress {
    Started {
        total_files: usize,
    },
    Fetching {
        file: String,
        done: usize,
        total: usize,
    },
    Skipped {
        file: String,
        reason: String,
    },
    Done {
        restored: usize,
        safety_dir: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct RestoreReport {
    pub restored: usize,
    pub skipped: usize,
    /// Nơi chứa bản sao an toàn của các file đã bị ghi đè.
    pub safety_dir: Option<String>,
    pub warnings: Vec<String>,
}

/// Một file cần dựng lại, bất kể đến từ đâu.
#[derive(Debug, Clone)]
struct Entry {
    root: RootToken,
    rel_path: String,
    hash: String,
    chunk_count: u32,
    mtime: Option<DateTime<Utc>>,
}

// ─────────────────────────────────────────────────────────────────────────
// Hai nguồn
// ─────────────────────────────────────────────────────────────────────────

/// Khôi phục từ kho local — không cần mạng, không cần đăng nhập.
pub fn run_local<F>(
    store: &LocalStore,
    snap: &LocalSnapshot,
    ctx: &RootContext,
    safety_root: &Path,
    mut on_progress: F,
) -> Result<RestoreReport>
where
    F: FnMut(Progress),
{
    let entries: Vec<Entry> = snap
        .files
        .iter()
        .map(|f| Entry {
            root: f.root,
            rel_path: f.rel_path.clone(),
            hash: f.hash.clone(),
            chunk_count: blob::chunk_count(f.size),
            mtime: f.mtime,
        })
        .collect();

    let mut ap = Applier::new(ctx, safety_root, &snap.id);
    on_progress(Progress::Started {
        total_files: entries.len(),
    });
    for (i, e) in entries.iter().enumerate() {
        on_progress(Progress::Fetching {
            file: e.rel_path.clone(),
            done: i,
            total: entries.len(),
        });
        let Some(target) = ap.target(e, &mut on_progress) else {
            continue;
        };
        // `read_blob` đã kiểm sha256.
        let bytes = store.read_blob(&e.hash)?;
        ap.apply(e, &target, &bytes)?;
    }
    Ok(ap.finish(&mut on_progress))
}

/// Khôi phục từ một snapshot trên Supabase.
pub async fn run_remote<F>(
    sb: &Supabase,
    snapshot_id: &str,
    ctx: &RootContext,
    safety_root: &Path,
    mut on_progress: F,
) -> Result<RestoreReport>
where
    F: FnMut(Progress),
{
    let rows = sb.snapshot_files(snapshot_id).await?;
    let entries = parse_rows(&rows)?;
    if entries.is_empty() {
        return Err(Error::Other(
            "snapshot này không có file nào — có thể nó chưa upload xong".into(),
        ));
    }

    let mut ap = Applier::new(ctx, safety_root, snapshot_id);
    on_progress(Progress::Started {
        total_files: entries.len(),
    });
    for (i, e) in entries.iter().enumerate() {
        on_progress(Progress::Fetching {
            file: e.rel_path.clone(),
            done: i,
            total: entries.len(),
        });
        let Some(target) = ap.target(e, &mut on_progress) else {
            continue;
        };
        let mut chunks: Vec<Chunk> = Vec::with_capacity(e.chunk_count as usize);
        for idx in 0..e.chunk_count {
            chunks.push(sb.get_chunk(&e.hash, idx).await?);
        }
        // `assemble` kiểm sha256 và báo lỗi nếu thiếu chunk.
        let bytes = blob::assemble(chunks, &e.hash)?;
        ap.apply(e, &target, &bytes)?;
    }
    Ok(ap.finish(&mut on_progress))
}

// ─────────────────────────────────────────────────────────────────────────
// Phần ghi dùng chung
// ─────────────────────────────────────────────────────────────────────────

struct Applier<'a> {
    ctx: &'a RootContext,
    /// Tạo theo thời điểm, để hai lần khôi phục liên tiếp không đè lên bản
    /// cứu hộ của nhau.
    safety_dir: PathBuf,
    safety_used: bool,
    restored: usize,
    skipped: usize,
    warnings: Vec<String>,
}

impl<'a> Applier<'a> {
    fn new(ctx: &'a RootContext, safety_root: &Path, source_id: &str) -> Self {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        // Id local chứa '@' — hợp lệ trên Windows nhưng thay cho dễ đọc.
        let tag = source_id.replace('@', "_");
        Self {
            ctx,
            safety_dir: safety_root.join(format!("{tag}-{stamp}")),
            safety_used: false,
            restored: 0,
            skipped: 0,
            warnings: Vec::new(),
        }
    }

    /// Phân giải đích đến và khẳng định nó nằm trong root.
    fn target<F: FnMut(Progress)>(&mut self, e: &Entry, on_progress: &mut F) -> Option<PathBuf> {
        let reason = if !remotecache::is_safe_rel_path(&e.rel_path) {
            "đường dẫn bất thường"
        } else if let Some(base) = self.ctx.resolve(e.root) {
            let target = base.join(e.rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));
            // Dư thừa so với `is_safe_rel_path`, nhưng rẻ và bắt được cả các
            // trường hợp lạ do chuẩn hoá đường dẫn của hệ điều hành.
            if target.starts_with(&base) {
                return Some(target);
            }
            "thoát ra ngoài thư mục gốc"
        } else {
            "không phân giải được thư mục gốc trên máy này"
        };
        self.skipped += 1;
        self.warnings
            .push(format!("bỏ qua '{}': {reason}", e.rel_path));
        on_progress(Progress::Skipped {
            file: e.rel_path.clone(),
            reason: reason.into(),
        });
        None
    }

    fn apply(&mut self, e: &Entry, target: &Path, bytes: &[u8]) -> Result<()> {
        // File trên đĩa đã giống hệt thì khỏi đụng vào — và khỏi tạo bản cứu
        // hộ thừa.
        if let Ok(current) = std::fs::read(target) {
            if blob::sha256_hex(&current) == e.hash {
                self.restored += 1;
                return Ok(());
            }
            // Bản hiện tại vẫn có thể là bản người dùng muốn giữ. Cất đi trước;
            // không cất được thì KHÔNG ghi đè.
            if let Err(err) = stash(&self.safety_dir, &e.rel_path, target) {
                self.skipped += 1;
                self.warnings.push(format!(
                    "bỏ qua '{}': không tạo được bản cứu hộ ({err})",
                    e.rel_path
                ));
                return Ok(());
            }
            self.safety_used = true;
        }
        write_atomic(target, bytes, e.mtime)?;
        self.restored += 1;
        Ok(())
    }

    fn finish<F: FnMut(Progress)>(self, on_progress: &mut F) -> RestoreReport {
        let safety = self
            .safety_used
            .then(|| self.safety_dir.to_string_lossy().into_owned());
        on_progress(Progress::Done {
            restored: self.restored,
            safety_dir: safety.clone(),
        });
        RestoreReport {
            restored: self.restored,
            skipped: self.skipped,
            safety_dir: safety,
            warnings: self.warnings,
        }
    }
}

fn parse_rows(rows: &Value) -> Result<Vec<Entry>> {
    let arr = rows
        .as_array()
        .ok_or_else(|| Error::Parse("danh sách file không phải mảng".into()))?;

    let mut out = Vec::with_capacity(arr.len());
    for r in arr {
        let Some(root) = r
            .get("root_token")
            .and_then(|v| serde_json::from_value::<RootToken>(v.clone()).ok())
        else {
            continue;
        };
        let (Some(rel_path), Some(hash)) = (
            r.get("rel_path").and_then(Value::as_str),
            r.get("blob_hash").and_then(Value::as_str),
        ) else {
            continue;
        };
        out.push(Entry {
            root,
            rel_path: rel_path.to_string(),
            hash: hash.to_string(),
            chunk_count: r
                .get("chunk_count")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .max(1) as u32,
            mtime: r
                .get("mtime_utc")
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.with_timezone(&Utc)),
        });
    }
    Ok(out)
}

/// Chép file hiện có sang thư mục cứu hộ, giữ nguyên cấu trúc thư mục con.
fn stash(safety_dir: &Path, rel_path: &str, current: &Path) -> std::io::Result<()> {
    let dest = safety_dir.join(rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(current, dest)?;
    Ok(())
}

/// Ghi ra file tạm cùng thư mục rồi `rename` đè lên.
///
/// Cùng thư mục là bắt buộc: rename chỉ nguyên tử khi nguồn và đích nằm trên
/// cùng một volume. Mất điện giữa chừng để lại một file `.tmp` thừa chứ không
/// để lại một save cụt.
fn write_atomic(target: &Path, bytes: &[u8], mtime: Option<DateTime<Utc>>) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp = target.with_extension(format!(
        "{}.cloudsave-tmp",
        target
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));

    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        // Ép xuống đĩa trước khi rename: nếu không, rename có thể hoàn tất
        // trong khi nội dung vẫn còn trong cache và mất khi cúp điện.
        file.sync_all()?;
        if let Some(t) = mtime {
            let _ = file.set_modified(std::time::SystemTime::from(t));
        }
    }

    // std::fs::rename trên Windows không phải lúc nào cũng đè được file đang
    // tồn tại, nên xoá đích trước. Bản cũ đã được cất ở `stash`.
    #[cfg(windows)]
    if target.exists() {
        std::fs::remove_file(target)?;
    }

    std::fs::rename(&tmp, target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::Io(e)
    })?;
    Ok(())
}

/// Thư mục mặc định chứa các bản cứu hộ trước khi ghi đè.
pub fn default_safety_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("cloudsave-g4market")
        .join("pre-restore")
}
