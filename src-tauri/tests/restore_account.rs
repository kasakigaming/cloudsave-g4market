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
