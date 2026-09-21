//! Kho snapshot trên máy — nơi mọi bản sao lưu được ghi TRƯỚC, không cần mạng
//! hay đăng nhập. Supabase chỉ nhận dữ liệu khi người dùng chủ động bấm đẩy,
//! và khi đó nó nhận đúng bản đã chụp ở đây chứ không quét lại đĩa.
//!
//! Bố cục trên đĩa:
//!
//! ```text
//! <root>/
//!   blobs/ab/abcdef….zst            nội dung file, nén zstd, tên = sha256 bản gốc
//!   snapshots/<slug>/<id>.json      một lần chụp: danh sách file + hash
//! ```
//!
//! Blob content-addressed nên chụp lại một game mà chỉ một file đổi thì chỉ
//! tốn thêm đúng file đó. Một snapshot chỉ được tạo khi nội dung thật sự khác
//! bản gần nhất — game tắt mà không lưu gì thì không sinh ra bản thừa.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::blob;
use crate::error::{Error, Result};
use crate::preview;
use crate::scan::GameScan;
use crate::steam::RootToken;

/// File lớn hơn mức này bị bỏ qua khi chụp. Postgres free tier có 500 MB;
/// save khổng lồ (colony RimWorld, thành phố Cities Skylines) nên đi Supabase
/// Storage chứ không nhét vào bảng.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Số snapshot giữ lại cho mỗi game. Bản đã đẩy lên cloud cũng tính vào đây —
/// cloud vẫn giữ bản của nó.
pub const KEEP_PER_GAME: usize = 30;

const PREVIEW_FILE_LIMIT: usize = 8;
const ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// Quét toàn bộ lúc mở app hoặc khi bấm "Quét tất cả".
    ScanAll,
    /// Người dùng bấm chụp một game.
    Manual,
    /// Watcher thấy game vừa tắt.
    GameExit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalFile {
    pub root: RootToken,
    pub rel_path: String,
    pub size: u64,
    pub mtime: Option<DateTime<Utc>>,
    /// sha256 hex của nội dung gốc — cũng là tên blob.
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalSnapshot {
    /// `<slug>@<yyyymmddThhmmssZ>-<8 hex>`; chỉ gồm ký tự an toàn cho tên file.
    pub id: String,
    pub game_slug: String,
    pub game_title: String,
    pub steam_appid: Option<u32>,
    pub account_id: u32,
    pub created_at: DateTime<Utc>,
    pub trigger: Trigger,
    pub files: Vec<LocalFile>,
    pub total_bytes: u64,
    /// File không đọc được lúc chụp (thường vì game đang giữ khoá ghi).
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub preview: Option<Value>,
    /// Id snapshot tương ứng trên Supabase, sau khi đã đẩy.
    #[serde(default)]
    pub remote_id: Option<String>,
    #[serde(default)]
    pub pushed_at: Option<DateTime<Utc>>,
}

impl LocalSnapshot {
    /// Chữ ký nội dung: hai snapshot cùng chữ ký là cùng một trạng thái save.
    fn signature(&self) -> Vec<(RootToken, &str, &str)> {
        let mut v: Vec<_> = self
            .files
            .iter()
            .map(|f| (f.root, f.rel_path.as_str(), f.hash.as_str()))
            .collect();
        v.sort();
        v
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureOutcome {
    /// Nội dung khác bản gần nhất → đã ghi snapshot mới.
    Created { snapshot: LocalSnapshot },
    /// Giống hệt bản gần nhất → không ghi gì.
    Unchanged { snapshot: LocalSnapshot },
    /// Game không có file save nào đọc được.
    Empty,
}

pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    pub fn default_root() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("cloudsave-g4market")
            .join("store")
    }

    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("blobs"))?;
        std::fs::create_dir_all(root.join("snapshots"))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    // ── Chụp ─────────────────────────────────────────────────────────────

    pub fn capture(
        &self,
        scan: &GameScan,
        account_id: u32,
        trigger: Trigger,
    ) -> Result<CaptureOutcome> {
        let mut files = Vec::new();
        let mut warnings = scan.warnings.clone();
        let mut previews = Vec::new();

        for f in &scan.files {
            if f.size > MAX_FILE_BYTES {
                warnings.push(format!(
                    "bỏ qua '{}': lớn hơn {} MB",
                    f.rel_path,
                    MAX_FILE_BYTES / (1024 * 1024)
                ));
                continue;
            }
            let bytes = match std::fs::read(&f.abs_path) {
                Ok(b) => b,
                Err(e) => {
                    // Game đang giữ khoá ghi, hoặc file vừa bị xoá giữa lúc quét.
                    warnings.push(format!("bỏ qua '{}': không đọc được ({e})", f.rel_path));
                    continue;
                }
            };
            let hash = blob::sha256_hex(&bytes);
            self.put_blob(&hash, &bytes)?;

            if previews.len() < PREVIEW_FILE_LIMIT {
                if let Some(p) = preview::extract(&f.rel_path, &bytes) {
                    previews.push(p);
                }
            }
            files.push(LocalFile {
                root: f.root,
                rel_path: f.rel_path.clone(),
                size: bytes.len() as u64,
                mtime: f.mtime,
                hash,
            });
        }

        if files.is_empty() {
            return Ok(CaptureOutcome::Empty);
        }

        let now = Utc::now();
        let snap = LocalSnapshot {
            id: new_id(&scan.slug, now),
            game_slug: scan.slug.clone(),
            game_title: scan.title.clone(),
            steam_appid: scan.app_id,
            account_id,
            created_at: now,
            trigger,
            total_bytes: files.iter().map(|f| f.size).sum(),
            files,
            warnings,
            preview: Some(preview::combine(previews)),
            remote_id: None,
            pushed_at: None,
        };

        if let Some(latest) = self.latest(&scan.slug)? {
            if latest.signature() == snap.signature() {
                return Ok(CaptureOutcome::Unchanged { snapshot: latest });
            }
        }

        self.write_snapshot(&snap)?;
        self.prune(&snap.game_slug, KEEP_PER_GAME)?;
        Ok(CaptureOutcome::Created { snapshot: snap })
    }

    // ── Đọc ──────────────────────────────────────────────────────────────

    /// Snapshot mới nhất trước.
    pub fn list(&self, slug: Option<&str>) -> Result<Vec<LocalSnapshot>> {
        let base = self.root.join("snapshots");
        let dirs: Vec<PathBuf> = match slug {
            Some(s) => {
                check_slug(s)?;
                vec![base.join(s)]
            }
            None => std::fs::read_dir(&base)?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect(),
        };

        let mut out = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                match std::fs::read(&p)
                    .map_err(Error::from)
                    .and_then(|b| serde_json::from_slice::<LocalSnapshot>(&b).map_err(Error::from))
                {
                    Ok(s) => out.push(s),
                    // Một file hỏng không được làm mất cả danh sách.
                    Err(err) => log::warn!("bỏ qua snapshot hỏng {}: {err}", p.display()),
                }
            }
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        Ok(out)
    }

    pub fn latest(&self, slug: &str) -> Result<Option<LocalSnapshot>> {
        Ok(self.list(Some(slug))?.into_iter().next())
    }

    pub fn get(&self, id: &str) -> Result<LocalSnapshot> {
        let path = self.snapshot_path(id)?;
        let bytes = std::fs::read(&path)
            .map_err(|_| Error::Other(format!("không tìm thấy bản lưu local '{id}'")))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Đọc nội dung một file và **kiểm sha256** trước khi trả về. Blob hỏng
    /// trên đĩa (ổ lỗi, bị sửa tay) phải bị chặn ở đây, không được lọt ra
    /// thành một file save hỏng hay một bản upload hỏng.
    pub fn read_blob(&self, hash: &str) -> Result<Vec<u8>> {
        let path = self.blob_path(hash)?;
        let compressed = std::fs::read(&path)
            .map_err(|_| Error::Other(format!("thiếu blob {hash} trong kho local")))?;
        let bytes = zstd::decode_all(compressed.as_slice())
            .map_err(|e| Error::Parse(format!("giải nén blob {hash}: {e}")))?;
        let actual = blob::sha256_hex(&bytes);
        if actual != hash {
            return Err(Error::ChecksumMismatch {
                expected: hash.to_string(),
                actual,
            });
        }
        Ok(bytes)
    }

    // ── Ghi ──────────────────────────────────────────────────────────────

    pub fn mark_pushed(&self, id: &str, remote_id: &str) -> Result<LocalSnapshot> {
        let mut snap = self.get(id)?;
        snap.remote_id = Some(remote_id.to_string());
        snap.pushed_at = Some(Utc::now());
        self.write_snapshot(&snap)?;
        Ok(snap)
    }

    /// Gỡ đánh dấu "đã lên cloud" của mọi bản local không còn bản tương ứng
    /// trên cloud (bị xoá trong app, trong Dashboard, hay từ máy khác).
    /// `alive` là tập id snapshot hiện có trên cloud. Trả về số bản đã gỡ.
    pub fn forget_missing_remote(
        &self,
        alive: &std::collections::HashSet<String>,
    ) -> Result<usize> {
        let mut cleared = 0;
        for mut snap in self.list(None)? {
            let gone = snap.remote_id.as_ref().is_some_and(|r| !alive.contains(r));
            if gone {
                snap.remote_id = None;
                snap.pushed_at = None;
                self.write_snapshot(&snap)?;
                cleared += 1;
            }
        }
        Ok(cleared)
    }

    /// Gỡ đánh dấu của những bản local trỏ tới một snapshot cloud vừa bị xoá.
    pub fn forget_remote(&self, remote_id: &str) -> Result<usize> {
        let mut cleared = 0;
        for mut snap in self.list(None)? {
            if snap.remote_id.as_deref() == Some(remote_id) {
                snap.remote_id = None;
                snap.pushed_at = None;
                self.write_snapshot(&snap)?;
                cleared += 1;
            }
        }
        Ok(cleared)
    }

    /// Giữ `keep` bản mới nhất của một game, xoá phần còn lại rồi dọn blob.
    pub fn prune(&self, slug: &str, keep: usize) -> Result<usize> {
        let all = self.list(Some(slug))?;
        let mut removed = 0;
        for old in all.iter().skip(keep) {
            if std::fs::remove_file(self.snapshot_path(&old.id)?).is_ok() {
                removed += 1;
            }
        }
        if removed > 0 {
            self.gc()?;
        }
        Ok(removed)
    }

    /// Xoá blob không còn snapshot nào tham chiếu.
    pub fn gc(&self) -> Result<usize> {
        let live: std::collections::HashSet<String> = self
            .list(None)?
            .into_iter()
            .flat_map(|s| s.files.into_iter().map(|f| f.hash))
            .collect();

        let mut removed = 0;
        for shard in std::fs::read_dir(self.root.join("blobs"))?.flatten() {
            let Ok(entries) = std::fs::read_dir(shard.path()) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let Some(hash) = name.strip_suffix(".zst") else {
                    continue;
                };
                if !live.contains(hash) && std::fs::remove_file(e.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    fn put_blob(&self, hash: &str, bytes: &[u8]) -> Result<()> {
        let path = self.blob_path(hash)?;
        if path.exists() {
            return Ok(()); // content-addressed: tồn tại nghĩa là đã đúng nội dung
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let compressed = zstd::encode_all(bytes, ZSTD_LEVEL)
            .map_err(|e| Error::Other(format!("nén blob: {e}")))?;
        write_atomic(&path, &compressed)
    }

    fn write_snapshot(&self, snap: &LocalSnapshot) -> Result<()> {
        let path = self.snapshot_path(&snap.id)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_atomic(&path, &serde_json::to_vec_pretty(snap)?)
    }

    // ── Đường dẫn ────────────────────────────────────────────────────────
    //
    // Id và hash đến từ frontend, nên phải kiểm tra trước khi ghép vào đường
    // dẫn — nếu không, một id như "../../x" sẽ đọc/ghi ra ngoài kho.

    fn blob_path(&self, hash: &str) -> Result<PathBuf> {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Other(format!("hash không hợp lệ: {hash}")));
        }
        Ok(self
            .root
            .join("blobs")
            .join(&hash[..2])
            .join(format!("{hash}.zst")))
    }

    fn snapshot_path(&self, id: &str) -> Result<PathBuf> {
        let (slug, stamp) = id
            .split_once('@')
            .ok_or_else(|| Error::Other(format!("id bản lưu không hợp lệ: {id}")))?;
        check_slug(slug)?;
        if stamp.is_empty()
            || !stamp
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Error::Other(format!("id bản lưu không hợp lệ: {id}")));
        }
        Ok(self
            .root
            .join("snapshots")
            .join(slug)
            .join(format!("{id}.json")))
    }
}

fn check_slug(slug: &str) -> Result<()> {
    let ok = !slug.is_empty()
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(Error::Other(format!("slug không hợp lệ: {slug}")))
    }
}

fn new_id(slug: &str, now: DateTime<Utc>) -> String {
    let rand = uuid::Uuid::new_v4().simple().to_string();
    format!("{slug}@{}-{}", now.format("%Y%m%dT%H%M%SZ"), &rand[..8])
}

/// Ghi tmp cùng thư mục rồi rename: tắt máy giữa chừng không để lại một blob
/// hay snapshot cụt mà lần sau vẫn tưởng là hợp lệ.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::Io(e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{SaveFile, Source};

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tmpdir() -> Tmp {
        let p = std::env::temp_dir().join(format!("cs-store-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }

    fn scan_of(dir: &Path, files: &[(&str, &[u8])]) -> GameScan {
        let mut out = Vec::new();
        for (rel, content) in files {
            let abs = dir.join(rel);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(&abs, content).unwrap();
            out.push(SaveFile {
                root: RootToken::WinAppDataRoaming,
                rel_path: rel.to_string(),
                abs_path: abs,
                size: content.len() as u64,
                mtime: None,
                source: Source::Ufs,
            });
        }
        GameScan {
            app_id: Some(1),
            slug: "test-game".into(),
            title: "Test Game".into(),
            total_bytes: out.iter().map(|f| f.size).sum(),
            files: out,
            sources: vec![Source::Ufs],
            warnings: vec![],
        }
    }

    #[test]
    fn captures_then_detects_unchanged_then_change() {
        let saves = tmpdir();
        let store_dir = tmpdir();
        let store = LocalStore::open(&store_dir.0).unwrap();

        let scan = scan_of(
            &saves.0,
            &[("Game/slot1.sav", b"level 1"), ("Game/cfg.ini", b"x=1")],
        );
        let first = store.capture(&scan, 7, Trigger::ScanAll).unwrap();
        let CaptureOutcome::Created { snapshot: s1 } = first else {
            panic!("lần đầu phải tạo snapshot")
        };
        assert_eq!(s1.files.len(), 2);

        // Không đổi gì → không sinh bản thừa.
        let again = store.capture(&scan, 7, Trigger::GameExit).unwrap();
        assert!(matches!(again, CaptureOutcome::Unchanged { .. }));
        assert_eq!(store.list(Some("test-game")).unwrap().len(), 1);

        // Đổi một file → bản mới; blob của file không đổi được dùng lại.
        let scan2 = scan_of(
            &saves.0,
            &[("Game/slot1.sav", b"level 2"), ("Game/cfg.ini", b"x=1")],
        );
        let third = store.capture(&scan2, 7, Trigger::GameExit).unwrap();
        assert!(matches!(third, CaptureOutcome::Created { .. }));
        assert_eq!(store.list(Some("test-game")).unwrap().len(), 2);

        let blobs = walkdir::WalkDir::new(store_dir.0.join("blobs"))
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_file())
            .count();
        assert_eq!(blobs, 3, "cfg.ini không đổi phải dùng chung một blob");
    }

    #[test]
    fn blob_roundtrip_is_verified() {
        let saves = tmpdir();
        let store_dir = tmpdir();
        let store = LocalStore::open(&store_dir.0).unwrap();
        let scan = scan_of(&saves.0, &[("a.sav", b"noi dung that")]);
        let CaptureOutcome::Created { snapshot } =
            store.capture(&scan, 1, Trigger::Manual).unwrap()
        else {
            panic!()
        };
        let hash = &snapshot.files[0].hash;
        assert_eq!(store.read_blob(hash).unwrap(), b"noi dung that");

        // Làm hỏng blob trên đĩa → phải bị chặn, không được trả dữ liệu sai.
        let p = store.blob_path(hash).unwrap();
        std::fs::write(&p, zstd::encode_all(&b"bi sua"[..], 3).unwrap()).unwrap();
        assert!(matches!(
            store.read_blob(hash),
            Err(Error::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn forgets_cloud_marks_that_no_longer_exist() {
        let saves = tmpdir();
        let store_dir = tmpdir();
        let store = LocalStore::open(&store_dir.0).unwrap();
        let mut ids = Vec::new();
        for (i, v) in [b"v1".as_slice(), b"v2"].iter().enumerate() {
            let scan = scan_of(&saves.0, &[("s.sav", v)]);
            let CaptureOutcome::Created { snapshot } =
                store.capture(&scan, 1, Trigger::Manual).unwrap()
            else {
                panic!()
            };
            store
                .mark_pushed(&snapshot.id, &format!("remote-{i}"))
                .unwrap();
            ids.push(snapshot.id);
        }
        // Trên cloud chỉ còn remote-1: bản trỏ tới remote-0 phải mất dấu cloud.
        let alive: std::collections::HashSet<String> = ["remote-1".to_string()].into();
        assert_eq!(store.forget_missing_remote(&alive).unwrap(), 1);
        assert!(store.get(&ids[0]).unwrap().remote_id.is_none());
        assert_eq!(
            store.get(&ids[1]).unwrap().remote_id.as_deref(),
            Some("remote-1")
        );

        assert_eq!(store.forget_remote("remote-1").unwrap(), 1);
        assert!(store.get(&ids[1]).unwrap().remote_id.is_none());
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let store_dir = tmpdir();
        let store = LocalStore::open(&store_dir.0).unwrap();
        for bad in ["../x@1", "a@../../etc", "noat", "A@1", "a@1/2", "a@"] {
            assert!(store.snapshot_path(bad).is_err(), "phải chặn id {bad}");
        }
        assert!(store.blob_path("../../etc/passwd").is_err());
        assert!(store
            .snapshot_path("stardew-valley@20260921T061500Z-ab12cd34")
            .is_ok());
    }

    #[test]
    fn prune_keeps_newest_and_collects_blobs() {
        let saves = tmpdir();
        let store_dir = tmpdir();
        let store = LocalStore::open(&store_dir.0).unwrap();
        for i in 0..5 {
            let content = format!("v{i}");
            let scan = scan_of(&saves.0, &[("s.sav", content.as_bytes())]);
            store.capture(&scan, 1, Trigger::Manual).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(store.list(Some("test-game")).unwrap().len(), 5);
        store.prune("test-game", 2).unwrap();
        let left = store.list(Some("test-game")).unwrap();
        assert_eq!(left.len(), 2);
        // Blob của bản bị xoá cũng phải đi theo.
        for s in &left {
            store.read_blob(&s.files[0].hash).unwrap();
        }
        let blobs = walkdir::WalkDir::new(store_dir.0.join("blobs"))
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_file())
            .count();
        assert_eq!(blobs, 2);
    }
}
