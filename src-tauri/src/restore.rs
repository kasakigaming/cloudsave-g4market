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
//!
//! Và một lớp nữa, không phải để chống app ghi sai mà để chống Steam dọn mất
//! những gì app vừa ghi (xem `steam::autocloud`): mọi đường dẫn được dựng theo
//! **tài khoản Steam đang đăng nhập**, id tài khoản nằm trong đường dẫn được
//! đổi sang tài khoản đó, và `steam_autocloud.vdf` được dán lại nhãn. Không có
//! ba việc này thì khôi phục bản của tài khoản khác xong, lần quét kế tiếp
//! Steam dời sạch file sang `userdata/<tài khoản cũ>/<appid>/ac`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::blob;
use crate::error::{Error, Result};
use crate::local_store::{LocalSnapshot, LocalStore};
use crate::remote_blob;
use crate::steam::{autocloud, remotecache, roots, RootContext, RootToken};
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
    /// Tài khoản Steam đã nhận bản khôi phục này.
    pub target_account: u32,
    /// Tài khoản đã chụp bản lưu, nếu biết hoặc dò ra được.
    pub source_account: Option<u32>,
    /// `source_account` lấy từ đâu.
    pub source_origin: SourceOrigin,
    /// Số file phải đổi id tài khoản trong đường dẫn.
    pub remapped: usize,
    /// Số `steam_autocloud.vdf` phải dán lại nhãn sang tài khoản đích.
    pub markers: usize,
}

/// Khôi phục *cho ai*. Tách khỏi `RootContext` vì đây là quyết định của tầng
/// trên: `ctx` chỉ biết cách ghép đường dẫn, còn việc chọn tài khoản nào là
/// chính sách.
#[derive(Debug, Clone, Copy)]
pub struct Plan {
    /// Tài khoản Steam đang đăng nhập trên máy này — đích của lần khôi phục.
    pub target_account: u32,
    /// Tài khoản đã chụp bản lưu. `None` với snapshot cloud đẩy lên từ bản app
    /// cũ (chưa ghi tài khoản): khi đó app dò từ đường dẫn, xem `decide_remap`.
    pub source_account: Option<u32>,
}

/// Tài khoản nguồn lấy từ đâu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceOrigin {
    /// Bản lưu có ghi tài khoản đã chụp.
    Recorded,
    /// Không ghi, nhưng đường dẫn có đúng một SteamID64 — chắc chắn là nó.
    Path,
    /// Không ghi, đường dẫn có nhiều SteamID64: lấy thư mục có file mới nhất,
    /// tức tài khoản chơi gần nhất lúc chụp.
    PathNewest,
    /// Không biết — và đường dẫn cũng không có id tài khoản nào để đổi.
    Unknown,
}

/// Kết quả của `decide_remap`.
#[derive(Debug)]
struct Remap {
    /// Đổi id của tài khoản này sang tài khoản đích; `None` = giữ nguyên.
    from: Option<u32>,
    source: Option<u32>,
    origin: SourceOrigin,
    warnings: Vec<String>,
}

/// Quyết định đổi id tài khoản nào trong đường dẫn, dựa vào tài khoản Steam
/// đang đăng nhập (`plan.target_account`).
///
/// Quy tắc, theo thứ tự:
///
/// 1. Bản lưu **đã có sẵn thư mục của tài khoản đích** (SteamID64 hay Steam3 id
///    của nó nằm trong đường dẫn): không đổi gì. Đổi thì thư mục của tài khoản
///    nguồn đổ vào đúng chỗ thư mục của tài khoản đích — hai bộ save đè nhau.
///    Chuyện này có thật: một game chơi bằng hai tài khoản trên cùng máy thì
///    bản chụp chứa cả hai thư mục.
/// 2. Có ghi tài khoản đã chụp → đổi id của tài khoản đó.
/// 3. Không ghi (bản đẩy từ app cũ) → dò SteamID64 trong đường dẫn. Một id:
///    chính nó. Nhiều id: thư mục có file mới nhất. Chỉ đổi đúng một tài khoản,
///    các thư mục còn lại giữ nguyên, nên không bao giờ hai thư mục gộp làm một.
///
/// Steam3 id (số 32-bit trần) không dò bằng hình dạng được — dễ nhầm với số do
/// game tự sinh — nên chỉ đổi khi đã biết tài khoản nguồn.
fn decide_remap(entries: &[Entry], plan: Plan) -> Remap {
    use std::collections::BTreeMap;

    let target = plan.target_account;
    let target32 = target.to_string();
    // Mỗi tài khoản trong đường dẫn → (mtime mới nhất, số file).
    let mut seen: BTreeMap<u32, (Option<DateTime<Utc>>, usize)> = BTreeMap::new();
    let mut target_present = false;
    for e in entries.iter().filter(|e| !autocloud::is_marker(&e.rel_path)) {
        for acc in roots::accounts_in_path(&e.rel_path) {
            let slot = seen.entry(acc).or_insert((None, 0));
            slot.0 = slot.0.max(e.mtime);
            slot.1 += 1;
        }
        target_present |= e.rel_path.split('/').any(|seg| seg == target32);
    }
    target_present |= seen.contains_key(&target);

    let (source, origin) = match plan.source_account {
        Some(a) => (Some(a), SourceOrigin::Recorded),
        None => match seen.len() {
            0 => (None, SourceOrigin::Unknown),
            1 => (seen.keys().next().copied(), SourceOrigin::Path),
            _ => (
                seen.iter()
                    .max_by_key(|(_, (t, n))| (*t, *n))
                    .map(|(a, _)| *a),
                SourceOrigin::PathNewest,
            ),
        },
    };

    let mut warnings = Vec::new();
    let from = match source {
        Some(s) if s != target && target_present => {
            warnings.push(format!(
                "bản lưu có sẵn thư mục của tài khoản {target} đang đăng nhập — \
                 khôi phục nguyên đường dẫn, không đổi thư mục nào"
            ));
            None
        }
        Some(s) if s != target => {
            let how = match origin {
                SourceOrigin::Path => " (dò từ đường dẫn)".to_string(),
                SourceOrigin::PathNewest => format!(
                    " (đường dẫn có {} tài khoản, chọn thư mục có file mới nhất)",
                    seen.len()
                ),
                _ => String::new(),
            };
            warnings.push(format!(
                "bản lưu thuộc tài khoản Steam {s}{how}, khôi phục cho tài khoản \
                 {target} đang đăng nhập"
            ));
            Some(s)
        }
        _ => None,
    };
    Remap {
        from,
        source,
        origin,
        warnings,
    }
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
    plan: Plan,
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

    let mut ap = Applier::new(ctx, plan, &entries, safety_root, &snap.id);
    on_progress(Progress::Started {
        total_files: entries.len(),
    });
    for (i, e) in entries.iter().enumerate() {
        on_progress(Progress::Fetching {
            file: e.rel_path.clone(),
            done: i,
            total: entries.len(),
        });
        if autocloud::is_marker(&e.rel_path) {
            ap.apply_marker(e, &mut on_progress);
            continue;
        }
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
    plan: Plan,
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

    let mut ap = Applier::new(ctx, plan, &entries, safety_root, snapshot_id);
    // Nhiều file chung tổ tiên delta thì mỗi tổ tiên chỉ tải một lần.
    let mut cache = remote_blob::Cache::default();
    on_progress(Progress::Started {
        total_files: entries.len(),
    });
    for (i, e) in entries.iter().enumerate() {
        on_progress(Progress::Fetching {
            file: e.rel_path.clone(),
            done: i,
            total: entries.len(),
        });
        if autocloud::is_marker(&e.rel_path) {
            ap.apply_marker(e, &mut on_progress);
            continue;
        }
        let Some(target) = ap.target(e, &mut on_progress) else {
            continue;
        };
        // Xử lý cả hai định dạng và cả chuỗi delta; mọi mắt đều kiểm sha256.
        let bytes = remote_blob::fetch(sb, &e.hash, Some(e.chunk_count), &mut cache).await?;
        ap.apply(e, &target, &bytes)?;
    }
    Ok(ap.finish(&mut on_progress))
}

// ─────────────────────────────────────────────────────────────────────────
// Phần ghi dùng chung
// ─────────────────────────────────────────────────────────────────────────

struct Applier<'a> {
    ctx: &'a RootContext,
    plan: Plan,
    remap: Remap,
    /// Tạo theo thời điểm, để hai lần khôi phục liên tiếp không đè lên bản
    /// cứu hộ của nhau.
    safety_dir: PathBuf,
    safety_used: bool,
    restored: usize,
    skipped: usize,
    remapped: usize,
    /// Các `steam_autocloud.vdf` đã xét — mỗi file chỉ xét một lần dù có hàng
    /// trăm file save dùng chung nó.
    seen_markers: std::collections::BTreeSet<PathBuf>,
    /// Trong số đó, bao nhiêu file thật sự phải ghi lại.
    retagged: usize,
    warnings: Vec<String>,
}

/// Đích đến của một file, kèm gốc root để dò `steam_autocloud.vdf` mà không đi
/// ra ngoài phạm vi root.
struct Target {
    base: PathBuf,
    path: PathBuf,
}

impl<'a> Applier<'a> {
    fn new(
        ctx: &'a RootContext,
        plan: Plan,
        entries: &[Entry],
        safety_root: &Path,
        source_id: &str,
    ) -> Self {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        // Id local chứa '@' — hợp lệ trên Windows nhưng thay cho dễ đọc.
        let tag = source_id.replace('@', "_");
        let mut remap = decide_remap(entries, plan);
        let warnings = std::mem::take(&mut remap.warnings);
        Self {
            ctx,
            plan,
            remap,
            safety_dir: safety_root.join(format!("{tag}-{stamp}")),
            safety_used: false,
            restored: 0,
            skipped: 0,
            remapped: 0,
            seen_markers: std::collections::BTreeSet::new(),
            retagged: 0,
            warnings,
        }
    }

    /// Đường dẫn tương đối đã đổi id tài khoản sang tài khoản đích.
    fn rel_for(&mut self, rel_path: &str) -> String {
        let Some(src) = self.remap.from else {
            return rel_path.to_string();
        };
        let out = roots::remap_account(rel_path, src, self.plan.target_account);
        if out != rel_path {
            self.remapped += 1;
        }
        out
    }

    /// Phân giải đích đến và khẳng định nó nằm trong root.
    fn target<F: FnMut(Progress)>(&mut self, e: &Entry, on_progress: &mut F) -> Option<Target> {
        let rel = self.rel_for(&e.rel_path);
        let reason = if !remotecache::is_safe_rel_path(&rel) {
            "đường dẫn bất thường"
        } else if let Some(base) = self.ctx.resolve(e.root) {
            let path = base.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            // Dư thừa so với `is_safe_rel_path`, nhưng rẻ và bắt được cả các
            // trường hợp lạ do chuẩn hoá đường dẫn của hệ điều hành.
            if path.starts_with(&base) {
                return Some(Target { base, path });
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

    fn apply(&mut self, e: &Entry, target: &Target, bytes: &[u8]) -> Result<()> {
        let path = &target.path;
        // File trên đĩa đã giống hệt thì khỏi đụng vào — và khỏi tạo bản cứu
        // hộ thừa.
        if let Ok(current) = std::fs::read(path) {
            if blob::sha256_hex(&current) == e.hash {
                self.restored += 1;
                self.claim_dir(target);
                return Ok(());
            }
            // Bản hiện tại vẫn có thể là bản người dùng muốn giữ. Cất đi trước;
            // không cất được thì KHÔNG ghi đè.
            if let Err(err) = stash(&self.safety_dir, &e.rel_path, path) {
                self.skipped += 1;
                self.warnings.push(format!(
                    "bỏ qua '{}': không tạo được bản cứu hộ ({err})",
                    e.rel_path
                ));
                return Ok(());
            }
            self.safety_used = true;
        }
        write_atomic(path, bytes, e.mtime)?;
        self.restored += 1;
        self.claim_dir(target);
        Ok(())
    }

    /// Dán nhãn tài khoản đích lên `steam_autocloud.vdf` của chỗ save vừa ghi.
    ///
    /// Không có bước này thì file vừa khôi phục vẫn mang nhãn tài khoản cũ, và
    /// lần AutoCloud kế tiếp Steam dời hết sang `userdata/<tài khoản cũ>/…/ac`.
    fn claim_dir(&mut self, target: &Target) {
        let Some(marker) = autocloud::marker_for(&target.base, &target.path) else {
            return;
        };
        if !self.seen_markers.insert(marker.clone()) {
            return;
        }
        match autocloud::retag(&marker, self.plan.target_account) {
            Ok(true) => {
                self.retagged += 1;
                log::info!(
                    "đã dán nhãn tài khoản {} lên {}",
                    self.plan.target_account,
                    marker.display()
                );
            }
            Ok(false) => {}
            Err(e) => self.warnings.push(format!(
                "không ghi lại được {}: {e} — Steam có thể dời save đi khi đổi tài khoản",
                marker.display()
            )),
        }
    }

    /// Bản lưu cũ có chụp cả `steam_autocloud.vdf`: KHÔNG ghi lại nội dung cũ
    /// (nó mang id tài khoản đã chụp), chỉ ghi nhãn của tài khoản đích.
    fn apply_marker<F: FnMut(Progress)>(&mut self, e: &Entry, on_progress: &mut F) {
        let Some(target) = self.target(e, on_progress) else {
            return;
        };
        let account = self.plan.target_account;
        // File này Steam tạo ra và app biết chắc nội dung của nó, nên ghi mới
        // cũng được — chỗ save đã có nó lúc chụp.
        let text = autocloud::marker_text(account);
        if autocloud::read_account(&target.path) == Some(account) {
            return;
        }
        match write_atomic(&target.path, text.as_bytes(), None) {
            Ok(()) => {
                self.seen_markers.insert(target.path);
                self.retagged += 1;
            }
            Err(err) => self.warnings.push(format!(
                "không ghi được '{}': {err}",
                e.rel_path
            )),
        }
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
            target_account: self.plan.target_account,
            source_account: self.remap.source,
            source_origin: self.remap.origin,
            remapped: self.remapped,
            markers: self.retagged,
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
