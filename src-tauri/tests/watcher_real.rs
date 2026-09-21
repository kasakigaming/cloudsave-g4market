//! Kiểm tra phát hiện game bằng một tiến trình THẬT.
//!
//! Unit test trong `watcher.rs` chỉ kiểm việc so đường dẫn. Phần rủi ro thật là
//! hệ điều hành có trả về đường dẫn exe của tiến trình hay không — nếu không,
//! watcher mù hoàn toàn với game mở ngoài Steam. Test này chép `cmd.exe` vào một
//! "thư mục cài game" giả, chạy nó, và đòi watcher nhận ra — rồi tắt nó và đòi
//! watcher thấy nó biến mất.

#![cfg(windows)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cloudsave_lib::steam::InstalledApp;
use cloudsave_lib::watcher::detect_running;
use sysinfo::System;

/// Appid giả, chắc chắn không trùng game thật nào.
const FAKE_APP: u32 = 4_000_000_001;

struct TmpDir(PathBuf);
impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wait_until(mut cond: impl FnMut() -> bool, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

#[test]
fn detects_game_process_start_and_exit() {
    let dir = TmpDir(std::env::temp_dir().join(format!("cs-fakegame-{}", std::process::id())));
    let game_dir = dir.0.join("steamapps").join("common").join("Fake Game");
    std::fs::create_dir_all(&game_dir).unwrap();

    let system32 = PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32");
    let exe = game_dir.join("FakeGame.exe");
    std::fs::copy(system32.join("cmd.exe"), &exe).expect("chép cmd.exe");

    let mut installed = BTreeMap::new();
    installed.insert(
        FAKE_APP,
        InstalledApp {
            app_id: FAKE_APP,
            name: "Fake Game".into(),
            install_dir: game_dir.clone(),
        },
    );

    let mut sys = System::new();
    assert!(
        !detect_running(&installed, &mut sys).contains(&FAKE_APP),
        "chưa chạy mà đã báo đang chạy"
    );

    // Giữ tiến trình sống ~20 giây bằng ping, đủ để quan sát.
    let mut child = Command::new(&exe)
        .args(["/c", "ping", "-n", "20", "127.0.0.1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("chạy game giả");

    let seen = wait_until(
        || detect_running(&installed, &mut sys).contains(&FAKE_APP),
        Duration::from_secs(10),
    );
    println!("game giả pid {} — phát hiện: {seen}", child.id());
    if !seen {
        let _ = child.kill();
    }
    assert!(
        seen,
        "watcher không nhận ra tiến trình chạy từ thư mục game"
    );

    child.kill().expect("tắt game giả");
    let _ = child.wait();

    let gone = wait_until(
        || !detect_running(&installed, &mut sys).contains(&FAKE_APP),
        Duration::from_secs(10),
    );
    println!("sau khi tắt — biến mất: {gone}");
    assert!(gone, "game đã tắt mà watcher vẫn báo đang chạy");
}
