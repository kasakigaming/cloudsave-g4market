//! Thử tắt Steam Cloud qua giao diện Steam (OCR).
//!
//!     cargo run --example steam_cloud_ui               # chỉ đọc, không bấm
//!     cargo run --example steam_cloud_ui -- --click    # bấm thật
//!
//! Đặt `CS_UI_DEBUG=<thư mục>` để lưu ảnh chụp và chữ OCR đọc được.

fn main() {
    #[cfg(windows)]
    {
        let click = std::env::args().any(|a| a == "--click");
        let debug = std::env::var_os("CS_UI_DEBUG").map(std::path::PathBuf::from);
        let steam = cloudsave_lib::steam::SteamInstall::discover().expect("không tìm thấy Steam");
        match cloudsave_lib::steam_ui::disable_via_ui(&steam.root, click, debug.as_deref()) {
            Ok(r) => println!("{}", serde_json::to_string_pretty(&r).unwrap()),
            Err(e) => {
                eprintln!("LỖI: {e}");
                std::process::exit(1);
            }
        }
    }
}
