//! Bài test cuối: MỌI save thật trên máy → cloud → XOÁ khỏi đĩa → lấy về.
//!
//! Chạy trên mọi tài khoản Steam và mọi game có save. Với từng game: chụp vào
//! kho local tạm, đẩy lên cloud bằng nén cực hạn, rồi xoá toàn bộ file save
//! thật và khôi phục từ cloud. Đạt khi mọi file quay lại khớp từng byte.
//!
//! **Có tính phá huỷ.** Lưới an toàn: trước khi làm gì, mọi file được chép
//! sang một thư mục riêng. Khi test kết thúc — kể cả khi panic giữa chừng —
//! `SafetyNet::drop` kiểm tra lại từng file; file nào thiếu hay lệch thì tự
//! chép bản gốc về.
//!
//! ```text
//! $env:CS_TEST_EMAIL="..."; $env:CS_TEST_PASSWORD="..."
//! cargo test --test real_saves_e2e -- --ignored --nocapture --test-threads=1
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use cloudsave_lib::backup::{self, DeviceInfo};
use cloudsave_lib::blob;
use cloudsave_lib::local_store::{CaptureOutcome, LocalStore, Trigger};
use cloudsave_lib::restore;
use cloudsave_lib::scan::{GameScan, Scanner};
use cloudsave_lib::steam::{AppInfo, RootContext, SteamInstall};
use cloudsave_lib::supabase::{Config, Supabase};

fn load_dotenv() {
    for p in ["../.env", ".env"] {
        let Ok(text) = std::fs::read_to_string(p) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                if std::env::var_os(k.trim()).is_none() {
                    std::env::set_var(k.trim(), v.trim().trim_matches('"').trim_matches('\''));
                }
            }
        }
        return;
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Lưới an toàn
// ─────────────────────────────────────────────────────────────────────────

struct SafetyNet {
    dir: PathBuf,
    /// Đường dẫn thật → (bản sao, sha256 gốc).
    files: BTreeMap<PathBuf, (PathBuf, String)>,
}

impl SafetyNet {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("cs-safety-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self {
            dir,
            files: BTreeMap::new(),
        }
    }

    fn protect(&mut self, path: &PathBuf) {
        if self.files.contains_key(path) {
            return;
        }
        let bytes = std::fs::read(path).expect("đọc file để sao lưu an toàn");
        let copy = self.dir.join(format!("{:06}", self.files.len()));
        std::fs::write(&copy, &bytes).unwrap();
        // Đọc lại bản sao để chắc chắn nó dùng được TRƯỚC khi đụng vào bản thật.
        assert_eq!(std::fs::read(&copy).unwrap(), bytes, "bản sao an toàn hỏng");
        self.files
            .insert(path.clone(), (copy, blob::sha256_hex(&bytes)));
    }
}

impl Drop for SafetyNet {
    fn drop(&mut self) {
        let mut repaired = 0;
        for (path, (copy, hash)) in &self.files {
            let ok = std::fs::read(path)
                .map(|b| blob::sha256_hex(&b) == *hash)
                .unwrap_or(false);
            if !ok {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::copy(copy, path).expect("KHÔNG chép lại được bản an toàn");
                repaired += 1;
            }
        }
        println!(
            "\n[lưới an toàn] {} file được bảo vệ, {} file phải chép lại từ bản sao",
            self.files.len(),
            repaired
        );
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Case {
    scan: GameScan,
    account_id: u32,
    app_id: u32,
    install: Option<PathBuf>,
}

#[tokio::test]
#[ignore = "PHÁ HUỶ: xoá mọi save thật rồi khôi phục từ cloud"]
async fn every_real_save_survives_delete_and_cloud_restore() {
    load_dotenv();
    let (Ok(email), Ok(password)) = (
        std::env::var("CS_TEST_EMAIL"),
        std::env::var("CS_TEST_PASSWORD"),
    ) else {
        eprintln!("bỏ qua: thiếu CS_TEST_EMAIL / CS_TEST_PASSWORD");
        return;
    };
    let sb = Supabase::new(Config::from_env().expect("cấu hình Supabase")).unwrap();
    sb.sign_in(&email, &password).await.expect("đăng nhập");

    let steam = SteamInstall::discover().expect("tìm Steam");
    let appinfo =
        Arc::new(AppInfo::parse(&std::fs::read(steam.appcache_appinfo()).unwrap()).unwrap());
    let installed = steam.installed_apps();

    // ── 1. Gom mọi save thật, mỗi bộ file một lần ────────────────────────
    // Nhiều tài khoản có thể trỏ cùng một chỗ (vd %APPDATA% của Stardew), nên
    // khoá chống trùng là tập đường dẫn tuyệt đối.
    let t0 = Instant::now();
    let mut cases: Vec<Case> = Vec::new();
    let mut seen: BTreeSet<Vec<PathBuf>> = BTreeSet::new();
    for u in &steam.users {
        let scanner = Scanner {
            steam: steam.clone(),
            appinfo: appinfo.clone(),
            manifest: None,
            account_id: u.account_id,
        };
        let mut ids: Vec<u32> = installed.keys().copied().collect();
        ids.extend(steam.apps_with_userdata(u.account_id));
        ids.sort_unstable();
        ids.dedup();
        for app_id in ids {
            let scan = scanner.scan_app(app_id);
            if scan.files.is_empty() {
                continue;
            }
            let mut paths: Vec<PathBuf> = scan.files.iter().map(|f| f.abs_path.clone()).collect();
            paths.sort();
            if !seen.insert(paths) {
                continue;
            }
            cases.push(Case {
                scan,
                account_id: u.account_id,
                app_id,
                install: installed.get(&app_id).map(|a| a.install_dir.clone()),
            });
        }
    }
    let n_files: usize = cases.iter().map(|c| c.scan.files.len()).sum();
    let n_bytes: u64 = cases.iter().map(|c| c.scan.total_bytes).sum();
    println!(
        "[1] {} bộ save, {} file, {} byte  ({:.0}s)",
        cases.len(),
        n_files,
        n_bytes,
        t0.elapsed().as_secs_f64()
    );
    assert!(!cases.is_empty(), "máy không có save nào để test");

    // ── 2. Lưới an toàn cho MỌI file trước khi đụng vào bất cứ thứ gì ─────
    let mut net = SafetyNet::new();
    for c in &cases {
        for f in &c.scan.files {
            net.protect(&f.abs_path);
        }
    }
    println!("[2] đã sao lưu an toàn {} file", net.files.len());

    // ── 3. Chụp local rồi đẩy lên cloud ──────────────────────────────────
    let store_dir = std::env::temp_dir().join(format!("cs-real-e2e-{}", std::process::id()));
    let store = LocalStore::open(&store_dir).unwrap();
    let device = DeviceInfo {
        id: "e2e-real-saves".into(),
        name: "test toàn bộ save".into(),
    };

    // Dọn snapshot cũ của các game này trên tài khoản test.
    let slugs: BTreeSet<String> = cases.iter().map(|c| c.scan.slug.clone()).collect();
    for slug in &slugs {
        if let Ok(list) = sb.list_snapshots(Some(slug)).await {
            for id in list
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["id"].as_str())
            {
                let _ = sb.delete_snapshot(id).await;
            }
        }
    }
    let _ = sb.gc().await;

    println!("\n[3] đẩy lên cloud:");
    println!(
        "    {:<30} {:>4} {:>13} {:>11} {:>8} {:>6} {:>6}",
        "game", "file", "gốc (B)", "lưu (B)", "tỷ lệ", "delta", "giây"
    );
    let mut remote_ids: Vec<Option<String>> = Vec::new();
    let (mut sum_up, mut sum_stored) = (0u64, 0u64);
    for c in &cases {
        let snap = match store
            .capture(&c.scan, c.account_id, Trigger::Manual)
            .unwrap()
        {
            CaptureOutcome::Created { snapshot } | CaptureOutcome::Unchanged { snapshot } => {
                snapshot
            }
            CaptureOutcome::Empty => {
                remote_ids.push(None);
                continue;
            }
        };
        let t = Instant::now();
        let r = backup::push(&sb, &store, &snap, &device, true, |_| {})
            .await
            .unwrap_or_else(|e| panic!("đẩy {} thất bại: {e}", c.scan.title));
        let ratio = if r.uploaded_bytes > 0 {
            format!(
                "{:.2}%",
                100.0 * r.stored_bytes as f64 / r.uploaded_bytes as f64
            )
        } else {
            "dedupe".into()
        };
        println!(
            "    {:<30} {:>4} {:>13} {:>11} {:>8} {:>6} {:>6.1}",
            c.scan.title.chars().take(30).collect::<String>(),
            r.file_count,
            r.uploaded_bytes,
            r.stored_bytes,
            ratio,
            r.delta_files,
            t.elapsed().as_secs_f64()
        );
        sum_up += r.uploaded_bytes;
        sum_stored += r.stored_bytes;
        remote_ids.push(Some(r.snapshot_id));
    }
    println!(
        "    TỔNG gửi đi: {sum_up} B gốc → {sum_stored} B lưu trên cloud ({:.2}%)",
        100.0 * sum_stored as f64 / sum_up.max(1) as f64
    );

    // ── 4. XOÁ toàn bộ save thật khỏi đĩa ────────────────────────────────
    let mut deleted = 0;
    for path in net.files.keys() {
        std::fs::remove_file(path).expect("xoá file save");
        deleted += 1;
    }
    assert!(net.files.keys().all(|p| !p.exists()));
    println!("\n[4] đã XOÁ {deleted} file save thật khỏi đĩa");

    // ── 5. Lấy từng game về từ cloud và so từng byte ─────────────────────
    let safety_root = std::env::temp_dir().join(format!("cs-real-pre-{}", std::process::id()));
    let (mut ok_files, mut bad) = (0usize, Vec::new());
    for (c, rid) in cases.iter().zip(&remote_ids) {
        let Some(rid) = rid else { continue };
        let ctx = RootContext {
            steam_root: steam.root.clone(),
            account_id: c.account_id,
            app_id: c.app_id,
            game_install: c.install.clone(),
        };
        let rr = restore::run_remote(&sb, rid, &ctx, &safety_root, |_| {})
            .await
            .unwrap_or_else(|e| panic!("khôi phục {} thất bại: {e}", c.scan.title));
        for f in &c.scan.files {
            let want = &net.files[&f.abs_path].1;
            match std::fs::read(&f.abs_path) {
                Ok(b) if blob::sha256_hex(&b) == *want => ok_files += 1,
                Ok(_) => bad.push(format!("LỆCH: {}", f.abs_path.display())),
                Err(_) => bad.push(format!("THIẾU: {}", f.abs_path.display())),
            }
        }
        assert_eq!(
            rr.skipped, 0,
            "{}: bỏ qua {} file",
            c.scan.title, rr.skipped
        );
    }
    let _ = std::fs::remove_dir_all(&safety_root);
    println!(
        "[5] lấy về từ cloud: {ok_files}/{} file khớp từng byte với bản gốc",
        net.files.len()
    );
    for b in &bad {
        println!("    {b}");
    }

    // ── 6. Dọn cloud và kho tạm ──────────────────────────────────────────
    for rid in remote_ids.iter().flatten() {
        let _ = sb.delete_snapshot(rid).await;
    }
    let freed = sb.gc().await.unwrap_or(0);
    let _ = std::fs::remove_dir_all(&store_dir);
    println!("[6] dọn cloud: gc giải phóng {freed} chunk");

    assert!(bad.is_empty(), "{} file không khôi phục đúng", bad.len());
    assert_eq!(ok_files, net.files.len());
    println!("\nTẤT CẢ SAVE ĐỀU QUAY VỀ NGUYÊN VẸN");
    // `net` drop ở đây: kiểm tra lại lần nữa, lẽ ra 0 file phải chép lại.
}

/// Đo nén cực hạn trên mọi save thật, không cần mạng — dùng đúng thuật toán
/// chọn bản gốc của `backup::push` (file lớn trước, tối đa 2 file anh em lớn
/// nhất làm tham chiếu) và giải ngược từng file để xác minh.
#[test]
#[ignore = "đo trên save thật, chạy lâu"]
fn extreme_compression_report() {
    use cloudsave_lib::pack::{self, Base};

    let steam = SteamInstall::discover().expect("tìm Steam");
    let appinfo =
        Arc::new(AppInfo::parse(&std::fs::read(steam.appcache_appinfo()).unwrap()).unwrap());
    let installed = steam.installed_apps();

    let mut seen_content: BTreeSet<String> = BTreeSet::new();
    let mut by_codec: BTreeMap<&'static str, usize> = BTreeMap::new();
    let (mut raw_total, mut old_total, mut new_total) = (0u64, 0u64, 0u64);
    println!(
        "\n{:<30} {:>4} {:>12} {:>12} {:>11} {:>7} {:>6}",
        "game", "file", "gốc", "zstd-3 cũ", "cực hạn", "tỷ lệ", "giây"
    );
    for u in &steam.users {
        let scanner = Scanner {
            steam: steam.clone(),
            appinfo: appinfo.clone(),
            manifest: None,
            account_id: u.account_id,
        };
        let mut ids: Vec<u32> = installed.keys().copied().collect();
        ids.extend(steam.apps_with_userdata(u.account_id));
        ids.sort_unstable();
        ids.dedup();
        for app_id in ids {
            let scan = scanner.scan_app(app_id);
            // Chỉ tính nội dung chưa gặp — giống dedupe trên cloud.
            let mut files: Vec<(String, Vec<u8>)> = scan
                .files
                .iter()
                .filter_map(|f| std::fs::read(&f.abs_path).ok())
                .map(|b| (blob::sha256_hex(&b), b))
                .filter(|(h, _)| seen_content.insert(h.clone()))
                .collect();
            if files.is_empty() {
                continue;
            }
            files.sort_by_key(|(_, b)| std::cmp::Reverse(b.len()));

            let t = Instant::now();
            let (mut raw, mut old, mut new) = (0u64, 0u64, 0u64);
            for (i, (h, plain)) in files.iter().enumerate() {
                let bases: Vec<Base<'_>> = files[..i]
                    .iter()
                    .take(2)
                    .map(|(bh, bp)| Base {
                        hash: bh,
                        plain: bp,
                        depth: 0,
                    })
                    .collect();
                let p = pack::pack(plain, &bases).expect("nén");
                let base = p
                    .base_hash
                    .as_ref()
                    .and_then(|bh| files.iter().find(|(x, _)| x == bh))
                    .map(|(_, b)| b.as_slice());
                let back = pack::unpack(p.encoding, &p.bytes, base).expect("giải ngược");
                assert_eq!(blob::sha256_hex(&back), *h, "không byte-exact");
                *by_codec.entry(p.encoding.as_str()).or_default() += 1;
                raw += plain.len() as u64;
                old += blob::prepare(plain)
                    .chunks
                    .iter()
                    .map(|c| c.data.len() as u64)
                    .sum::<u64>();
                new += p.bytes.len() as u64;
            }
            println!(
                "{:<30} {:>4} {:>12} {:>12} {:>11} {:>6.2}% {:>6.1}",
                scan.title.chars().take(30).collect::<String>(),
                files.len(),
                raw,
                old,
                new,
                100.0 * new as f64 / raw as f64,
                t.elapsed().as_secs_f64()
            );
            raw_total += raw;
            old_total += old;
            new_total += new;
        }
    }
    println!(
        "\nTỔNG: gốc {raw_total} B | zstd-3 cũ {old_total} B | cực hạn {new_total} B \
         ({:.2}% so với gốc, {:.1}% so với cách cũ)",
        100.0 * new_total as f64 / raw_total as f64,
        100.0 * new_total as f64 / old_total as f64
    );
    println!("codec được chọn: {by_codec:?}");
}
