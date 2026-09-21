//! Test end-to-end thật: Steam trên đĩa → Supabase → ghi lại ra đĩa.
//!
//! **Test này có tính phá huỷ.** Nó xoá file save thật rồi khôi phục từ
//! Supabase và đòi byte-exact. Chỉ chạy khi đủ biến môi trường:
//!
//! ```text
//! $env:CS_TEST_EMAIL="..."; $env:CS_TEST_PASSWORD="..."
//! cargo test --test supabase_e2e -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `SUPABASE_URL` và `SUPABASE_ANON_KEY` đọc từ `.env` ở thư mục gốc dự án.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use cloudsave_lib::backup::{self, DeviceInfo};
use cloudsave_lib::blob;
use cloudsave_lib::error::Error;
use cloudsave_lib::local_store::{CaptureOutcome, LocalStore, Trigger};
use cloudsave_lib::restore;
use cloudsave_lib::scan::{GameScan, Scanner};
use cloudsave_lib::steam::{AppInfo, RootContext, SteamInstall};
use cloudsave_lib::supabase::{Config, Supabase};

/// Stardew Valley — save nằm ở `%APPDATA%`, nhiều file, dung lượng thật ~11 MB.
const TEST_APP_ID: u32 = 413_150;

// ─────────────────────────────────────────────────────────────────────────
// Thiết lập
// ─────────────────────────────────────────────────────────────────────────

fn load_dotenv() {
    // Test chạy với cwd = src-tauri, `.env` nằm ở thư mục cha.
    for p in ["../.env", ".env"] {
        let Ok(text) = std::fs::read_to_string(p) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if std::env::var_os(k.trim()).is_none() {
                    std::env::set_var(k.trim(), v);
                }
            }
        }
        return;
    }
}

struct Harness {
    sb: Supabase,
    scanner: Scanner,
    ctx: RootContext,
    device: DeviceInfo,
    account_id: u32,
    /// Kho local riêng cho test, trong thư mục tạm — không đụng kho thật.
    store: LocalStore,
    _store_dir: TmpDir,
}

struct TmpDir(PathBuf);
impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Trả `None` (kèm lý do in ra) khi môi trường chưa đủ, để test không fail
/// trên máy người khác.
async fn setup() -> Option<Harness> {
    load_dotenv();

    let (Ok(email), Ok(password)) = (
        std::env::var("CS_TEST_EMAIL"),
        std::env::var("CS_TEST_PASSWORD"),
    ) else {
        eprintln!("bỏ qua: thiếu CS_TEST_EMAIL / CS_TEST_PASSWORD");
        return None;
    };
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bỏ qua: {e}");
            return None;
        }
    };

    let sb = Supabase::new(cfg).expect("khởi tạo client");
    let session = sb
        .sign_in(&email, &password)
        .await
        .expect("đăng nhập Supabase");
    println!("đăng nhập: {} ({})", email, session.user_id);

    let steam = SteamInstall::discover().expect("tìm Steam");
    let bytes = std::fs::read(steam.appcache_appinfo()).expect("đọc appinfo.vdf");
    let appinfo = Arc::new(AppInfo::parse(&bytes).expect("parse appinfo.vdf"));

    let account_id = steam.users.first().expect("có tài khoản Steam").account_id;
    let game_install = steam
        .installed_apps()
        .get(&TEST_APP_ID)
        .map(|a| a.install_dir.clone());

    let store_dir = TmpDir(std::env::temp_dir().join(format!("cs-e2e-{}", std::process::id())));
    let store = LocalStore::open(&store_dir.0).expect("mở kho local tạm");

    Some(Harness {
        account_id,
        store,
        _store_dir: store_dir,
        sb,
        scanner: Scanner {
            steam: steam.clone(),
            appinfo,
            manifest: None,
            account_id,
        },
        ctx: RootContext {
            steam_root: steam.root.clone(),
            account_id,
            app_id: TEST_APP_ID,
            game_install,
        },
        device: DeviceInfo {
            id: "e2e-test-device".into(),
            name: "máy chạy test".into(),
        },
    })
}

/// Xoá mọi snapshot của một game.
///
/// Test này có tính phá huỷ nên phải chạy lại được nhiều lần: một lần chạy
/// trước chết giữa chừng sẽ để lại snapshot, và nếu ta giả định database đang
/// trống thì lần sau fail vì rác của lần trước chứ không phải vì lỗi thật.
async fn purge(sb: &Supabase, slug: &str) -> usize {
    let Ok(list) = sb.list_snapshots(Some(slug)).await else {
        return 0;
    };
    let ids: Vec<String> = list
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("id")?.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    for id in &ids {
        let _ = sb.delete_snapshot(id).await;
    }
    let _ = sb.gc().await;
    ids.len()
}

/// Ảnh chụp nội dung thật của các file trong scan, để so sau khi khôi phục.
fn snapshot_disk(scan: &GameScan) -> BTreeMap<PathBuf, (u64, String)> {
    scan.files
        .iter()
        .filter_map(|f| {
            let bytes = std::fs::read(&f.abs_path).ok()?;
            Some((
                f.abs_path.clone(),
                (bytes.len() as u64, blob::sha256_hex(&bytes)),
            ))
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────
// Test
// ─────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "cần Supabase thật và GHI ĐÈ save thật"]
async fn full_local_first_roundtrip() {
    let Some(h) = setup().await else { return };

    // ── 1. Quét ──────────────────────────────────────────────────────────
    let scan = h.scanner.scan_app(TEST_APP_ID);
    assert!(
        !scan.files.is_empty(),
        "không tìm thấy save cho app {TEST_APP_ID}"
    );
    println!(
        "\n[1] quét: {} — {} file, {} byte",
        scan.title,
        scan.files.len(),
        scan.total_bytes
    );
    let before = snapshot_disk(&scan);
    assert_eq!(before.len(), scan.files.len(), "có file không đọc được");

    // ── 2. Chụp vào kho local — không cần mạng ───────────────────────────
    let CaptureOutcome::Created { snapshot: local } = h
        .store
        .capture(&scan, h.account_id, Trigger::ScanAll)
        .expect("chụp local")
    else {
        panic!("kho trống thì lần chụp đầu phải tạo bản mới")
    };
    println!(
        "[2] chụp local: {} — {} file, {} byte",
        local.id,
        local.files.len(),
        local.total_bytes
    );
    assert_eq!(local.files.len(), scan.files.len());
    assert!(
        local.remote_id.is_none(),
        "chưa bấm đẩy thì chưa được lên cloud"
    );

    // ── 3. Chụp lại khi không có gì đổi → không sinh bản thừa ────────────
    //    (đây là thứ xảy ra mỗi lần game tắt mà người chơi không lưu)
    let again = h
        .store
        .capture(&scan, h.account_id, Trigger::GameExit)
        .expect("chụp lần hai");
    assert!(
        matches!(again, CaptureOutcome::Unchanged { .. }),
        "không đổi gì mà vẫn sinh bản mới"
    );
    assert_eq!(h.store.list(Some(&scan.slug)).unwrap().len(), 1);
    println!("[3] chụp lại: không đổi → không ghi thêm");

    // ── 4. Dọn rác cloud của lần chạy trước ──────────────────────────────
    let stale = purge(&h.sb, &scan.slug).await;
    if stale > 0 {
        println!("[4] dọn {stale} snapshot cloud sót từ lần chạy trước");
    }

    // ── 5. Bấm đẩy: đúng bản local lên cloud ─────────────────────────────
    let pushed = backup::push(&h.sb, &h.store, &local, &h.device, true, |_| {})
        .await
        .expect("đẩy lên cloud");
    println!(
        "[5] đẩy: snapshot cloud {} — tải lên {} byte, dedupe {} byte",
        pushed.snapshot_id, pushed.uploaded_bytes, pushed.deduped_bytes
    );
    assert_eq!(
        pushed.uploaded_bytes, pushed.total_bytes,
        "cloud vừa dọn sạch thì phải tải lên toàn bộ"
    );
    let marked = h.store.get(&local.id).expect("đọc lại bản local");
    assert_eq!(
        marked.remote_id.as_deref(),
        Some(pushed.snapshot_id.as_str()),
        "bản local phải ghi nhận đã đẩy"
    );

    // ── 6. Đẩy lại → không gửi byte nào ──────────────────────────────────
    let repush = backup::push(&h.sb, &h.store, &local, &h.device, true, |_| {})
        .await
        .expect("đẩy lần hai");
    assert_eq!(repush.uploaded_bytes, 0, "content-addressing hỏng");
    println!("[6] đẩy lại: tải lên 0 byte");

    // ── 7. Xung đột ──────────────────────────────────────────────────────
    let other = DeviceInfo {
        id: "e2e-may-khac".into(),
        name: "máy khác".into(),
    };
    match backup::push(&h.sb, &h.store, &local, &other, false, |_| {}).await {
        Err(Error::Conflict { .. }) => println!("[7] xung đột: máy khác bị chặn đúng"),
        Err(e) => panic!("mong đợi Conflict, nhận: {e}"),
        Ok(_) => panic!("máy khác ghi đè được mà không báo xung đột"),
    }

    let safety = restore::default_safety_root();

    // ── 8. PHÁ, rồi khôi phục TỪ LOCAL — không đụng tới mạng ─────────────
    wipe(&before);
    let rl =
        restore::run_local(&h.store, &local, &h.ctx, &safety, |_| {}).expect("khôi phục từ local");
    assert_eq!(rl.restored, before.len());
    assert_eq!(rl.skipped, 0);
    let n = verify(&before);
    println!("[8] xoá đĩa → khôi phục từ LOCAL: byte-exact {n} byte");

    // ── 9. PHÁ, rồi khôi phục TỪ CLOUD ───────────────────────────────────
    wipe(&before);
    let rr = restore::run_remote(&h.sb, &pushed.snapshot_id, &h.ctx, &safety, |_| {})
        .await
        .expect("khôi phục từ cloud");
    assert_eq!(rr.restored, before.len());
    assert_eq!(rr.skipped, 0);
    let n = verify(&before);
    println!("[9] xoá đĩa → khôi phục từ CLOUD: byte-exact {n} byte");

    // ── 10. Dọn dẹp ──────────────────────────────────────────────────────
    for id in [&pushed.snapshot_id, &repush.snapshot_id] {
        h.sb.delete_snapshot(id).await.expect("xoá snapshot");
    }
    let freed = h.sb.gc().await.expect("gc");
    let left =
        h.sb.list_snapshots(Some(&scan.slug))
            .await
            .expect("liệt kê");
    assert_eq!(left.as_array().map(Vec::len).unwrap_or(0), 0);
    println!("[10] dọn cloud: gc giải phóng {freed} chunk");
    println!("\nTẤT CẢ ĐỀU ĐẠT");
}

fn wipe(before: &BTreeMap<PathBuf, (u64, String)>) {
    for path in before.keys() {
        std::fs::remove_file(path).expect("xoá file save");
    }
    assert!(before.keys().all(|p| !p.exists()));
}

/// So từng file trên đĩa với ảnh chụp ban đầu; trả về tổng số byte đã kiểm.
fn verify(before: &BTreeMap<PathBuf, (u64, String)>) -> u64 {
    let mut total = 0;
    for (path, (size, hash)) in before {
        let got = std::fs::read(path)
            .unwrap_or_else(|e| panic!("file không quay lại: {} ({e})", path.display()));
        assert_eq!(
            got.len() as u64,
            *size,
            "sai dung lượng: {}",
            path.display()
        );
        assert_eq!(
            &blob::sha256_hex(&got),
            hash,
            "KHÔNG byte-exact: {}",
            path.display()
        );
        total += *size;
    }
    total
}

/// Kiểm tra riêng phần checksum bảo vệ: dữ liệu hỏng phải bị chặn TRƯỚC khi
/// chạm tới đĩa, chứ không phải sau.
#[tokio::test]
#[ignore = "cần Supabase thật"]
async fn corrupted_blob_is_rejected_before_writing() {
    let Some(h) = setup().await else { return };

    let data = b"noi dung goc".to_vec();
    let prepared = blob::prepare(&data);

    // Ghep lai voi hash sai -> phai bao ChecksumMismatch.
    let wrong = blob::sha256_hex(b"noi dung khac");
    match blob::assemble(prepared.chunks.clone(), &wrong) {
        Err(Error::ChecksumMismatch { .. }) => println!("checksum chặn đúng"),
        other => panic!("mong đợi ChecksumMismatch, nhận: {other:?}"),
    }

    // Upload roi tai ve phai byte-exact.
    for c in &prepared.chunks {
        h.sb.put_chunk(&prepared.hash, c)
            .await
            .expect("upload chunk");
    }
    let mut fetched = Vec::new();
    for idx in 0..prepared.chunks.len() as u32 {
        fetched.push(
            h.sb.get_chunk(&prepared.hash, idx)
                .await
                .expect("tải chunk"),
        );
    }
    let got = blob::assemble(fetched, &prepared.hash).expect("ghép lại");
    assert_eq!(got, data, "round-trip qua Supabase không byte-exact");
    println!("round-trip qua Supabase: byte-exact");

    h.sb.gc().await.expect("gc");
}
