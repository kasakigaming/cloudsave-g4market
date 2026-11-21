//! Khôi phục vào **tài khoản Steam đang đăng nhập**, không phải tài khoản đã chụp.
//!
//! Đây là test cho đúng cái lỗi "khôi phục xong Steam xoá file": Steam đọc
//! `steam_autocloud.vdf` ở gốc thư mục save, thấy tài khoản khác tài khoản đang
//! đăng nhập thì **dời** hết file sang `userdata/<tài khoản cũ>/<appid>/ac`.
//! Thêm nữa, ~19% quy tắc UFS có id tài khoản ngay trong đường dẫn, nên bản của
//! tài khoản A ghi nguyên si sang máy đang đăng nhập B là ghi vào thư mục game
//! không bao giờ đọc.
//!
//! Chạy trong `cargo test` thường: không cần Steam, không cần mạng. Dùng root
//! `GameInstall` vì nó phân giải theo `RootContext`, không theo thư mục thật của
//! người dùng.

use std::path::{Path, PathBuf};

use cloudsave_lib::local_store::{CaptureOutcome, LocalStore, Trigger};
use cloudsave_lib::restore;
use cloudsave_lib::scan::{GameScan, SaveFile, Source};
use cloudsave_lib::steam::{autocloud, roots, RootContext, RootToken};

const ACC_A: u32 = 111_111_111;
const ACC_B: u32 = 222_222_222;
const APP: u32 = 9_999_001;

struct Tmp(PathBuf);

impl Drop for Tmp {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn tmp(tag: &str) -> Tmp {
    let p = std::env::temp_dir().join(format!("cs-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).unwrap();
    Tmp(p)
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn save_file(root: RootToken, rel: &str, abs: &Path) -> SaveFile {
    let meta = std::fs::metadata(abs).unwrap();
    SaveFile {
        root,
        rel_path: rel.to_string(),
        abs_path: abs.to_path_buf(),
        size: meta.len(),
        mtime: None,
        source: Source::Ufs,
    }
}

#[test]
fn restores_into_the_account_steam_is_logged_into() {
    let home = tmp("game");
    let store_dir = tmp("store");
    let safety = tmp("safety");
    let game = home.0.join("install");

    // Chỗ save của tài khoản A: thư mục mang SteamID64 của A, kèm file đánh dấu
    // của Steam ở gốc — đúng như Stellar Blade / Elden Ring trên đĩa thật.
    let id_a = roots::steam_id64(ACC_A).to_string();
    let id_b = roots::steam_id64(ACC_B).to_string();
    let rel_save = format!("SaveGames/{id_a}/slot0.sav");
    let abs_save = game.join(rel_save.replace('/', std::path::MAIN_SEPARATOR_STR));
    write(&abs_save, b"noi dung save cua A");
    let abs_marker = game.join("SaveGames").join(autocloud::MARKER);
    write(&abs_marker, autocloud::marker_text(ACC_A).as_bytes());

    let scan = GameScan {
        app_id: Some(APP),
        slug: "acc-test".into(),
        title: "Acc Test".into(),
        files: vec![
            save_file(RootToken::GameInstall, &rel_save, &abs_save),
            save_file(
                RootToken::GameInstall,
                &format!("SaveGames/{}", autocloud::MARKER),
                &abs_marker,
            ),
        ],
        total_bytes: 0,
        sources: vec![Source::Ufs],
        warnings: Vec::new(),
    };

    let store = LocalStore::open(&store_dir.0).unwrap();
    let CaptureOutcome::Created { snapshot } = store.capture(&scan, ACC_A, Trigger::Manual).unwrap()
    else {
        panic!("phải tạo được snapshot");
    };

    // Xoá sạch, rồi khôi phục trong lúc Steam đăng nhập tài khoản B.
    std::fs::remove_dir_all(game.join("SaveGames")).unwrap();
    let ctx = RootContext {
        steam_root: home.0.join("steam"),
        account_id: ACC_B,
        app_id: APP,
        game_install: Some(game.clone()),
    };
    let plan = restore::Plan {
        target_account: ACC_B,
        source_account: Some(ACC_A),
    };
    let r = restore::run_local(&store, &snapshot, &ctx, plan, &safety.0, |_| {}).unwrap();

    // 1. Save nằm trong thư mục của B, không phải của A.
    let want = game.join("SaveGames").join(&id_b).join("slot0.sav");
    assert_eq!(
        std::fs::read(&want).unwrap(),
        b"noi dung save cua A",
        "save phải nằm ở {}",
        want.display()
    );
    assert!(
        !game.join("SaveGames").join(&id_a).exists(),
        "không được ghi vào thư mục của tài khoản cũ"
    );
    assert_eq!(r.remapped, 1);

    // 2. File đánh dấu mang tài khoản B — nếu không, lần quét sau Steam dời sạch.
    let marker = game.join("SaveGames").join(autocloud::MARKER);
    assert_eq!(autocloud::read_account(&marker), Some(ACC_B));
    assert_eq!(r.markers, 1);

    // 3. Báo cáo nói rõ đã khôi phục cho ai.
    assert_eq!(r.target_account, ACC_B);
    assert_eq!(r.source_account, Some(ACC_A));
    assert_eq!(r.restored, 1, "chỉ 1 file save; file đánh dấu không tính");
    assert!(
        r.warnings.iter().any(|w| w.contains(&ACC_A.to_string())),
        "phải cảnh báo là bản lưu thuộc tài khoản khác: {:?}",
        r.warnings
    );
}

#[test]
fn same_account_restore_changes_nothing_about_paths() {
    let home = tmp("game-same");
    let store_dir = tmp("store-same");
    let safety = tmp("safety-same");
    let game = home.0.join("install");

    let id_a = roots::steam_id64(ACC_A).to_string();
    let rel_save = format!("SaveGames/{id_a}/slot0.sav");
    let abs_save = game.join(rel_save.replace('/', std::path::MAIN_SEPARATOR_STR));
    write(&abs_save, b"x");
    let marker = game.join("SaveGames").join(autocloud::MARKER);
    write(&marker, autocloud::marker_text(ACC_A).as_bytes());

    let scan = GameScan {
        app_id: Some(APP),
        slug: "acc-same".into(),
        title: "Acc Same".into(),
        files: vec![save_file(RootToken::GameInstall, &rel_save, &abs_save)],
        total_bytes: 0,
        sources: vec![Source::Ufs],
        warnings: Vec::new(),
    };
    let store = LocalStore::open(&store_dir.0).unwrap();
    let CaptureOutcome::Created { snapshot } = store.capture(&scan, ACC_A, Trigger::Manual).unwrap()
    else {
        panic!("phải tạo được snapshot");
    };

    std::fs::remove_file(&abs_save).unwrap();
    let ctx = RootContext {
        steam_root: home.0.join("steam"),
        account_id: ACC_A,
        app_id: APP,
        game_install: Some(game.clone()),
    };
    let r = restore::run_local(
        &store,
        &snapshot,
        &ctx,
        restore::Plan {
            target_account: ACC_A,
            source_account: Some(ACC_A),
        },
        &safety.0,
        |_| {},
    )
    .unwrap();

    assert_eq!(std::fs::read(&abs_save).unwrap(), b"x");
    assert_eq!(r.remapped, 0);
    assert_eq!(r.markers, 0, "file đánh dấu đã đúng tài khoản, không ghi lại");
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
}

// ─────────────────────────────────────────────────────────────────────────
// Bản đẩy từ app cũ: không ghi tài khoản đã chụp, app phải tự dò.
// ─────────────────────────────────────────────────────────────────────────

const ACC_X: u32 = 333_333_333;

/// Dựng một bản lưu gồm các file `(rel_path, nội dung, mtime giây)` dưới
/// root `GameInstall`, chụp vào kho rồi xoá sạch thư mục save.
fn snapshot_of(
    game: &Path,
    store: &LocalStore,
    slug: &str,
    account: u32,
    files: &[(&str, &[u8], i64)],
) -> cloudsave_lib::local_store::LocalSnapshot {
    let mut list = Vec::new();
    for (rel, bytes, secs) in files {
        let abs = game.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        write(&abs, bytes);
        let mut f = save_file(RootToken::GameInstall, rel, &abs);
        f.mtime = chrono::DateTime::from_timestamp(*secs, 0);
        list.push(f);
    }
    let scan = GameScan {
        app_id: Some(APP),
        slug: slug.into(),
        title: slug.into(),
        files: list,
        total_bytes: 0,
        sources: vec![Source::Ufs],
        warnings: Vec::new(),
    };
    let CaptureOutcome::Created { snapshot } = store.capture(&scan, account, Trigger::Manual).unwrap()
    else {
        panic!("phải tạo được snapshot");
    };
    std::fs::remove_dir_all(game.join("SaveGames")).unwrap();
    snapshot
}

fn ctx_for(home: &Path, game: &Path, account: u32) -> RootContext {
    RootContext {
        steam_root: home.join("steam"),
        account_id: account,
        app_id: APP,
        game_install: Some(game.to_path_buf()),
    }
}

fn dir_of(account: u32) -> String {
    roots::steam_id64(account).to_string()
}

#[test]
fn old_cloud_snapshot_with_one_id_goes_to_logged_in_account() {
    let (home, store_dir, safety) = (tmp("g1"), tmp("s1"), tmp("x1"));
    let game = home.0.join("install");
    let store = LocalStore::open(&store_dir.0).unwrap();
    let rel = format!("SaveGames/{}/slot0.sav", dir_of(ACC_A));
    let snap = snapshot_of(&game, &store, "one-id", ACC_A, &[(&rel, b"save A", 1_000)]);

    // Như bản đẩy từ app cũ: không biết tài khoản nguồn.
    let plan = restore::Plan {
        target_account: ACC_B,
        source_account: None,
    };
    let r = restore::run_local(&store, &snap, &ctx_for(&home.0, &game, ACC_B), plan, &safety.0, |_| {})
        .unwrap();

    // Thư mục của B chưa có → được tạo ra và nhận save.
    let want = game.join("SaveGames").join(dir_of(ACC_B)).join("slot0.sav");
    assert_eq!(std::fs::read(&want).unwrap(), b"save A");
    assert!(!game.join("SaveGames").join(dir_of(ACC_A)).exists());
    assert_eq!(r.source_account, Some(ACC_A));
    assert_eq!(r.source_origin, restore::SourceOrigin::Path);
    assert_eq!(r.remapped, 1);
}

#[test]
fn several_ids_pick_the_newest_folder_and_never_merge_two() {
    let (home, store_dir, safety) = (tmp("g2"), tmp("s2"), tmp("x2"));
    let game = home.0.join("install");
    let store = LocalStore::open(&store_dir.0).unwrap();
    let rel_a = format!("SaveGames/{}/slot0.sav", dir_of(ACC_A));
    let rel_x = format!("SaveGames/{}/slot0.sav", dir_of(ACC_X));
    // X chơi gần hơn (mtime lớn hơn) → X là tài khoản cần chuyển sang B.
    let snap = snapshot_of(
        &game,
        &store,
        "two-ids",
        ACC_A,
        &[(&rel_a, b"save A", 1_000), (&rel_x, b"save X", 9_000)],
    );

    let plan = restore::Plan {
        target_account: ACC_B,
        source_account: None,
    };
    let r = restore::run_local(&store, &snap, &ctx_for(&home.0, &game, ACC_B), plan, &safety.0, |_| {})
        .unwrap();

    let saves = game.join("SaveGames");
    assert_eq!(std::fs::read(saves.join(dir_of(ACC_B)).join("slot0.sav")).unwrap(), b"save X");
    // Thư mục của A giữ nguyên, không bị gộp vào thư mục của B.
    assert_eq!(std::fs::read(saves.join(dir_of(ACC_A)).join("slot0.sav")).unwrap(), b"save A");
    assert_eq!(r.source_account, Some(ACC_X));
    assert_eq!(r.source_origin, restore::SourceOrigin::PathNewest);
    assert_eq!(r.remapped, 1, "chỉ đổi đúng một thư mục");
}

#[test]
fn snapshot_already_holding_target_folder_is_restored_as_is() {
    // Lỗi của bản 0.1.2: bản do A chụp có cả thư mục của B (hai tài khoản chơi
    // chung máy). Khôi phục cho B mà đổi A → B thì hai thư mục đè lên nhau.
    let (home, store_dir, safety) = (tmp("g3"), tmp("s3"), tmp("x3"));
    let game = home.0.join("install");
    let store = LocalStore::open(&store_dir.0).unwrap();
    let rel_a = format!("SaveGames/{}/slot0.sav", dir_of(ACC_A));
    let rel_b = format!("SaveGames/{}/slot0.sav", dir_of(ACC_B));
    let snap = snapshot_of(
        &game,
        &store,
        "holds-target",
        ACC_A,
        &[(&rel_a, b"save A", 9_000), (&rel_b, b"save B", 1_000)],
    );

    for source in [Some(ACC_A), None] {
        let plan = restore::Plan {
            target_account: ACC_B,
            source_account: source,
        };
        let r = restore::run_local(&store, &snap, &ctx_for(&home.0, &game, ACC_B), plan, &safety.0, |_| {})
            .unwrap();
        let saves = game.join("SaveGames");
        assert_eq!(
            std::fs::read(saves.join(dir_of(ACC_B)).join("slot0.sav")).unwrap(),
            b"save B",
            "save của B phải là save của B (source = {source:?})"
        );
        assert_eq!(std::fs::read(saves.join(dir_of(ACC_A)).join("slot0.sav")).unwrap(), b"save A");
        assert_eq!(r.remapped, 0, "source = {source:?}");
        assert!(
            r.warnings.iter().any(|w| w.contains("có sẵn thư mục")),
            "{:?}",
            r.warnings
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Id tài khoản nằm trong nội dung file, không chỉ trong đường dẫn.
// ─────────────────────────────────────────────────────────────────────────

/// Một file save có SteamID64 của `account` ghi ở dạng số 8 byte, hai chỗ —
/// cùng kiểu với `S0000.sl2` của Sekiro đo trên máy thật.
fn save_naming(account: u32) -> Vec<u8> {
    let id = roots::steam_id64(account).to_le_bytes();
    let mut v = b"HEAD".to_vec();
    v.extend_from_slice(&id);
    v.extend_from_slice(&[0u8; 32]);
    v.extend_from_slice(&id);
    v.extend_from_slice(b"TAIL");
    v
}

#[test]
fn save_naming_its_account_inside_is_recorded_at_backup_and_reported_at_restore() {
    use cloudsave_lib::embedded_id::Form;

    let (home, store_dir, safety) = (tmp("g-in"), tmp("s-in"), tmp("x-in"));
    let game = home.0.join("install");
    let store = LocalStore::open(&store_dir.0).unwrap();
    let rel_bound = format!("SaveGames/{}/slot0.sav", dir_of(ACC_A));
    let content = save_naming(ACC_A);
    let snap = snapshot_of(
        &game,
        &store,
        "bound",
        ACC_A,
        &[(&rel_bound, &content, 9_000), ("SaveGames/settings.ini", b"volume=7", 9_000)],
    );

    // Lúc chụp: file nào gắn tài khoản được ghi lại kèm bản lưu.
    let bound = snap.files.iter().find(|f| f.rel_path == rel_bound).unwrap();
    assert_eq!(bound.account_refs.len(), 1, "{:?}", bound.account_refs);
    assert_eq!(bound.account_refs[0].form, Form::Id64Le);
    assert_eq!(bound.account_refs[0].count, 2);
    assert_eq!(bound.account_refs[0].first_offset, 4);
    let plain = snap.files.iter().find(|f| f.rel_path.ends_with("settings.ini")).unwrap();
    assert!(plain.account_refs.is_empty());

    // Khôi phục sang tài khoản khác: báo đúng file đó, và KHÔNG sửa nội dung —
    // đây không phải định dạng app biết sửa.
    let plan = restore::Plan {
        target_account: ACC_B,
        source_account: Some(ACC_A),
    };
    let r = restore::run_local(&store, &snap, &ctx_for(&home.0, &game, ACC_B), plan, &safety.0, |_| {})
        .unwrap();
    assert_eq!(r.account_bound, vec![rel_bound.clone()]);
    assert!(
        r.warnings.iter().any(|w| w.contains("bên trong nội dung") && w.contains("2 chỗ")),
        "{:?}",
        r.warnings
    );
    let restored = game.join("SaveGames").join(dir_of(ACC_B)).join("slot0.sav");
    assert_eq!(std::fs::read(restored).unwrap(), content, "nội dung phải giữ nguyên từng byte");
}

#[test]
fn restoring_to_the_same_account_reports_no_account_bound_file() {
    let (home, store_dir, safety) = (tmp("g-in2"), tmp("s-in2"), tmp("x-in2"));
    let game = home.0.join("install");
    let store = LocalStore::open(&store_dir.0).unwrap();
    let rel = format!("SaveGames/{}/slot0.sav", dir_of(ACC_A));
    let snap = snapshot_of(&game, &store, "bound-same", ACC_A, &[(&rel, &save_naming(ACC_A), 9_000)]);

    let plan = restore::Plan {
        target_account: ACC_A,
        source_account: Some(ACC_A),
    };
    let r = restore::run_local(&store, &snap, &ctx_for(&home.0, &game, ACC_A), plan, &safety.0, |_| {})
        .unwrap();
    assert!(r.account_bound.is_empty(), "{:?}", r.account_bound);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
}

#[test]
fn old_snapshot_without_recorded_account_still_reports_bound_file() {
    // Bản đẩy từ app cũ: không ghi tài khoản, app dò ra từ đường dẫn rồi dò
    // tiếp trong nội dung bằng chính tài khoản vừa dò được.
    let (home, store_dir, safety) = (tmp("g-in3"), tmp("s-in3"), tmp("x-in3"));
    let game = home.0.join("install");
    let store = LocalStore::open(&store_dir.0).unwrap();
    let rel = format!("SaveGames/{}/slot0.sav", dir_of(ACC_X));
    let snap = snapshot_of(&game, &store, "bound-old", ACC_X, &[(&rel, &save_naming(ACC_X), 9_000)]);

    let plan = restore::Plan {
        target_account: ACC_B,
        source_account: None,
    };
    let r = restore::run_local(&store, &snap, &ctx_for(&home.0, &game, ACC_B), plan, &safety.0, |_| {})
        .unwrap();
    assert_eq!(r.account_bound, vec![rel]);
}

// ─────────────────────────────────────────────────────────────────────────
// Save FromSoftware (BND4): id trong nội dung được đổi theo, không chỉ báo.
// ─────────────────────────────────────────────────────────────────────────

/// Một BND4 không mã hoá, bố cục như `S0000.sl2` của Sekiro: header 0x40, mỗi
/// mục một header 0x20, mỗi mục mở đầu bằng MD5 của phần còn lại. SteamID64 của
/// `account` nằm ở mục đầu và mục cuối.
fn sl2_of(account: u32) -> Vec<u8> {
    use md5::{Digest, Md5};

    let id = roots::steam_id64(account).to_le_bytes();
    let mut profile = vec![0x11u8; 0x300];
    profile[0x34..0x3C].copy_from_slice(&id);
    let slot = vec![0x22u8; 0x200];
    let mut summary = vec![0x33u8; 0x180];
    summary[0x10..0x18].copy_from_slice(&id);
    let bodies = [profile, slot, summary];

    let mut out = vec![0u8; 0x40 + bodies.len() * 0x20];
    out[..4].copy_from_slice(b"BND4");
    out[0x0C..0x10].copy_from_slice(&(bodies.len() as u32).to_le_bytes());
    out[0x10..0x18].copy_from_slice(&0x40u64.to_le_bytes());
    out[0x20..0x28].copy_from_slice(&0x20u64.to_le_bytes());
    for (i, body) in bodies.iter().enumerate() {
        let base = 0x40 + i * 0x20;
        let off = out.len() as u32;
        out[base + 0x08..base + 0x10].copy_from_slice(&(body.len() as u64 + 16).to_le_bytes());
        out[base + 0x10..base + 0x14].copy_from_slice(&off.to_le_bytes());
        out.extend_from_slice(&Md5::digest(body));
        out.extend_from_slice(body);
    }
    out
}

#[test]
fn fromsoftware_save_is_rebound_to_the_account_it_is_restored_for() {
    let (home, store_dir, safety) = (tmp("g-sl2"), tmp("s-sl2"), tmp("x-sl2"));
    let game = home.0.join("install");
    let store = LocalStore::open(&store_dir.0).unwrap();
    let rel = format!("SaveGames/{}/S0000.sl2", dir_of(ACC_A));
    let original = sl2_of(ACC_A);
    let snap = snapshot_of(&game, &store, "sl2", ACC_A, &[(&rel, &original, 9_000)]);

    let plan = restore::Plan {
        target_account: ACC_B,
        source_account: Some(ACC_A),
    };
    let ctx = ctx_for(&home.0, &game, ACC_B);
    let r = restore::run_local(&store, &snap, &ctx, plan, &safety.0, |_| {}).unwrap();

    // File nằm trong thư mục của B và ghi B là chủ — đúng từng byte như một
    // save do chính B tạo ra, kể cả các MD5.
    let restored = game.join("SaveGames").join(dir_of(ACC_B)).join("S0000.sl2");
    assert_eq!(std::fs::read(&restored).unwrap(), sl2_of(ACC_B));
    assert_eq!(r.rebound, vec![rel.clone()]);
    assert!(r.account_bound.is_empty(), "{:?}", r.account_bound);
    assert!(
        !r.warnings.iter().any(|w| w.contains("bên trong")),
        "đã sửa được thì không còn gì để cảnh báo: {:?}",
        r.warnings
    );

    // Kho vẫn giữ nội dung gốc dưới đúng sha256 của nó.
    assert_eq!(store.read_blob(&snap.files[0].hash).unwrap(), original);

    // Khôi phục lần nữa: file trên đĩa đã đúng (so với bản ĐÃ SỬA, không phải
    // với hash của bản lưu) nên không ghi lại và không sinh bản cứu hộ.
    let again = restore::run_local(&store, &snap, &ctx, plan, &safety.0, |_| {}).unwrap();
    assert_eq!(again.restored, 1);
    assert_eq!(again.safety_dir, None);
    assert_eq!(std::fs::read(&restored).unwrap(), sl2_of(ACC_B));
}

#[test]
fn hex_named_save_folder_is_remapped_and_its_save_rebound() {
    // Dark Souls II / III đặt tên thư mục bằng SteamID64 viết thành 16 ký tự hex.
    let (home, store_dir, safety) = (tmp("g-hex"), tmp("s-hex"), tmp("x-hex"));
    let game = home.0.join("install");
    let store = LocalStore::open(&store_dir.0).unwrap();
    let hex_dir = |account: u32| format!("{:016x}", roots::steam_id64(account));
    let rel = format!("SaveGames/{}/DS30000.sl2", hex_dir(ACC_A));
    let snap = snapshot_of(&game, &store, "sl2-hex", ACC_A, &[(&rel, &sl2_of(ACC_A), 9_000)]);

    // Bản đẩy từ app cũ: không ghi tài khoản nguồn, phải dò ra từ thư mục hex.
    let plan = restore::Plan {
        target_account: ACC_B,
        source_account: None,
    };
    let r = restore::run_local(&store, &snap, &ctx_for(&home.0, &game, ACC_B), plan, &safety.0, |_| {})
        .unwrap();

    let saves = game.join("SaveGames");
    assert_eq!(
        std::fs::read(saves.join(hex_dir(ACC_B)).join("DS30000.sl2")).unwrap(),
        sl2_of(ACC_B)
    );
    assert!(!saves.join(hex_dir(ACC_A)).exists());
    assert_eq!(r.source_account, Some(ACC_A));
    assert_eq!((r.remapped, r.rebound.len()), (1, 1));
}

#[test]
fn snapshot_holding_two_accounts_keeps_each_save_with_its_owner() {
    // Hai tài khoản chơi chung máy: bản lưu có thư mục của cả A lẫn B và được
    // khôi phục nguyên đường dẫn. File trong thư mục của A phải vẫn ghi A.
    let (home, store_dir, safety) = (tmp("g-two"), tmp("s-two"), tmp("x-two"));
    let game = home.0.join("install");
    let store = LocalStore::open(&store_dir.0).unwrap();
    let rel_a = format!("SaveGames/{}/S0000.sl2", dir_of(ACC_A));
    let rel_b = format!("SaveGames/{}/S0000.sl2", dir_of(ACC_B));
    let snap = snapshot_of(
        &game,
        &store,
        "sl2-two",
        ACC_A,
        &[(&rel_a, &sl2_of(ACC_A), 9_000), (&rel_b, &sl2_of(ACC_B), 1_000)],
    );

    let plan = restore::Plan {
        target_account: ACC_B,
        source_account: Some(ACC_A),
    };
    let r = restore::run_local(&store, &snap, &ctx_for(&home.0, &game, ACC_B), plan, &safety.0, |_| {})
        .unwrap();

    let saves = game.join("SaveGames");
    assert_eq!(std::fs::read(saves.join(dir_of(ACC_A)).join("S0000.sl2")).unwrap(), sl2_of(ACC_A));
    assert_eq!(std::fs::read(saves.join(dir_of(ACC_B)).join("S0000.sl2")).unwrap(), sl2_of(ACC_B));
    assert!(r.rebound.is_empty(), "{:?}", r.rebound);
}

#[test]
fn restoring_a_fromsoftware_save_to_its_own_account_changes_no_byte() {
    let (home, store_dir, safety) = (tmp("g-own"), tmp("s-own"), tmp("x-own"));
    let game = home.0.join("install");
    let store = LocalStore::open(&store_dir.0).unwrap();
    let rel = format!("SaveGames/{}/S0000.sl2", dir_of(ACC_A));
    let original = sl2_of(ACC_A);
    let snap = snapshot_of(&game, &store, "sl2-own", ACC_A, &[(&rel, &original, 9_000)]);

    let plan = restore::Plan {
        target_account: ACC_A,
        source_account: Some(ACC_A),
    };
    let r = restore::run_local(&store, &snap, &ctx_for(&home.0, &game, ACC_A), plan, &safety.0, |_| {})
        .unwrap();
    let restored = game.join("SaveGames").join(dir_of(ACC_A)).join("S0000.sl2");
    assert_eq!(std::fs::read(restored).unwrap(), original);
    assert!(r.rebound.is_empty() && r.warnings.is_empty(), "{:?}", r.warnings);
}

#[test]
fn save_placed_under_another_account_s_name_is_made_loadable() {
    // Save tải từ người khác rồi chép vào thư mục của mình: file ghi X, thư mục
    // là của A. Khôi phục cho A thì file phải ghi A — game mới đọc được.
    let (home, store_dir, safety) = (tmp("g-dl"), tmp("s-dl"), tmp("x-dl"));
    let game = home.0.join("install");
    let store = LocalStore::open(&store_dir.0).unwrap();
    let rel = format!("SaveGames/{}/S0000.sl2", dir_of(ACC_A));
    let snap = snapshot_of(&game, &store, "sl2-dl", ACC_A, &[(&rel, &sl2_of(ACC_X), 9_000)]);

    let plan = restore::Plan {
        target_account: ACC_A,
        source_account: Some(ACC_A),
    };
    let r = restore::run_local(&store, &snap, &ctx_for(&home.0, &game, ACC_A), plan, &safety.0, |_| {})
        .unwrap();
    let restored = game.join("SaveGames").join(dir_of(ACC_A)).join("S0000.sl2");
    assert_eq!(std::fs::read(restored).unwrap(), sl2_of(ACC_A));
    assert_eq!(r.rebound, vec![rel]);
}
