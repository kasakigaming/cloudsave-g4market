//! Test tích hợp chạy trên cài đặt Steam thật của máy hiện tại.
//!
//! Đánh dấu `#[ignore]` vì kết quả phụ thuộc vào máy: không có Steam, hoặc có
//! Steam nhưng chưa chơi game nào, đều là hợp lệ. Chạy thủ công bằng:
//!
//! ```text
//! cargo test --test scan_real_steam -- --ignored --nocapture
//! ```
//!
//! Đây là thứ duy nhất chứng minh được cả chuỗi đi từ `appinfo.vdf` nhị phân
//! tới một đường dẫn file có thật trên đĩa.

use std::sync::Arc;

use cloudsave_lib::scan::Scanner;
use cloudsave_lib::steam::{AppInfo, SteamInstall};

fn load() -> Option<(SteamInstall, Arc<AppInfo>)> {
    let steam = match SteamInstall::discover() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("bỏ qua: {e}");
            return None;
        }
    };
    let bytes = std::fs::read(steam.appcache_appinfo()).expect("đọc appinfo.vdf");
    let appinfo = Arc::new(AppInfo::parse(&bytes).expect("parse appinfo.vdf"));
    Some((steam, appinfo))
}

#[test]
#[ignore = "phụ thuộc cài đặt Steam trên máy"]
fn parses_real_appinfo() {
    let Some((steam, appinfo)) = load() else {
        return;
    };

    let total = appinfo.app_ids().count();
    let with_ufs = appinfo
        .app_ids()
        .filter(|id| !appinfo.save_rules(*id).is_empty())
        .count();

    println!("Steam root      : {}", steam.root.display());
    println!("thư viện        : {}", steam.libraries.len());
    println!("tài khoản       : {}", steam.users.len());
    println!("app trong cache : {total}");
    println!("app khai ufs    : {with_ufs}");

    assert!(
        total > 100,
        "appinfo.vdf chỉ có {total} app — nghi parser hỏng"
    );
    assert!(with_ufs > 0, "không app nào khai ufs.savefiles");

    // Mọi root name mà Steam dùng đều phải map được. Một cái chưa biết nghĩa là
    // ta đang âm thầm bỏ sót save của game đó.
    let mut unmapped: Vec<String> = Vec::new();
    for id in appinfo.app_ids() {
        for rule in appinfo.save_rules(id) {
            if cloudsave_lib::steam::RootToken::from_name(&rule.root).is_none()
                && !unmapped.contains(&rule.root)
            {
                unmapped.push(rule.root.clone());
            }
        }
    }
    println!("root chưa map   : {unmapped:?}");
    assert!(unmapped.is_empty(), "root chưa được map: {unmapped:?}");
}

#[test]
#[ignore = "phụ thuộc cài đặt Steam trên máy"]
fn scans_installed_games_end_to_end() {
    let Some((steam, appinfo)) = load() else {
        return;
    };

    let installed = steam.installed_apps();
    println!("game đã cài: {}\n", installed.len());

    let mut games_with_saves = 0usize;
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;

    for user in &steam.users {
        let scanner = Scanner {
            steam: steam.clone(),
            appinfo: appinfo.clone(),
            // Bỏ manifest: test này không được phụ thuộc vào mạng.
            manifest: None,
            account_id: user.account_id,
        };

        // Ứng viên = game đã cài, cộng game từng có dữ liệu Cloud.
        let mut ids: Vec<u32> = installed.keys().copied().collect();
        ids.extend(steam.apps_with_userdata(user.account_id));
        ids.sort_unstable();
        ids.dedup();

        for app_id in ids {
            let scan = scanner.scan_app(app_id);
            if scan.files.is_empty() {
                continue;
            }
            games_with_saves += 1;
            total_files += scan.files.len();
            total_bytes += scan.total_bytes;

            println!(
                "[{}] {} — {} file, {} byte  (slug: {})",
                user.account_id,
                scan.title,
                scan.files.len(),
                scan.total_bytes,
                scan.slug
            );
            for f in scan.files.iter().take(4) {
                println!(
                    "      {:?} + {}   [{:?}]  {} B",
                    f.root, f.rel_path, f.source, f.size
                );

                // Bất biến của toàn bộ thiết kế: mọi file tìm được phải thực
                // sự tồn tại, và rel_path phải nối lại đúng thành abs_path.
                assert!(
                    f.abs_path.is_file(),
                    "không tồn tại: {}",
                    f.abs_path.display()
                );
                assert!(!f.rel_path.contains('\\'), "rel_path phải dùng '/'");
                assert!(
                    f.abs_path
                        .to_string_lossy()
                        .replace('\\', "/")
                        .ends_with(&f.rel_path),
                    "abs_path {} không kết thúc bằng rel_path {}",
                    f.abs_path.display(),
                    f.rel_path
                );
            }
            if scan.files.len() > 4 {
                println!("      … và {} file nữa", scan.files.len() - 4);
            }
            for w in &scan.warnings {
                println!("      cảnh báo: {w}");
            }
        }
    }

    println!("\ntổng: {games_with_saves} game, {total_files} file, {total_bytes} byte");
}

#[test]
#[ignore = "phụ thuộc cài đặt Steam trên máy"]
fn backup_roundtrip_on_real_saves() {
    use cloudsave_lib::blob;

    let Some((steam, appinfo)) = load() else {
        return;
    };

    // Chạy toàn bộ đường đi của dữ liệu — đọc, băm, nén, cắt chunk, ghép lại —
    // trên save thật, và đòi hỏi byte-exact. Đây là bất biến quan trọng nhất
    // của app: sai một byte là game từ chối nạp save.
    let mut checked = 0usize;
    let mut bytes_checked = 0u64;

    for user in &steam.users {
        let scanner = Scanner {
            steam: steam.clone(),
            appinfo: appinfo.clone(),
            manifest: None,
            account_id: user.account_id,
        };
        let mut ids: Vec<u32> = steam.installed_apps().keys().copied().collect();
        ids.extend(steam.apps_with_userdata(user.account_id));
        ids.sort_unstable();
        ids.dedup();

        for app_id in ids {
            for f in scanner.scan_app(app_id).files {
                let Ok(original) = std::fs::read(&f.abs_path) else {
                    continue;
                };
                let prepared = blob::prepare(&original);
                let restored = blob::assemble(prepared.chunks.clone(), &prepared.hash)
                    .unwrap_or_else(|e| panic!("ghép lại {} thất bại: {e}", f.rel_path));

                assert_eq!(
                    restored,
                    original,
                    "round-trip không byte-exact: {}",
                    f.abs_path.display()
                );
                assert_eq!(prepared.plain_size, original.len() as u64);

                checked += 1;
                bytes_checked += original.len() as u64;
            }
        }
    }

    println!("round-trip byte-exact trên {checked} file thật, {bytes_checked} byte");
    assert!(checked > 0, "không có file save nào để kiểm tra");
}
