// Không bao giờ hiện cửa sổ console đen, kể cả bản debug: log đã ghi ra
// `%LOCALAPPDATA%\cloudsave-g4market\app.log` (xem applog.rs).
#![windows_subsystem = "windows"]

fn main() {
    cloudsave_lib::run()
}
