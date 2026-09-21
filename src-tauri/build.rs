fn main() {
    embed_supabase_config();
    tauri_build::build()
}

/// Nhúng SUPABASE_URL và publishable key từ `../.env` vào binary lúc biên dịch.
///
/// Bộ cài không mang theo `.env`, nên nếu chỉ đọc lúc chạy thì bản cài xong sẽ
/// không lên được cloud. Publishable key vốn công khai (RLS mới bảo vệ dữ
/// liệu) nên nhúng được. Secret key thì tuyệt đối không — gặp là bỏ qua và
/// cảnh báo, vì nhúng nó là phát tán toàn quyền database cùng file exe.
///
/// Biến môi trường lúc chạy vẫn được ưu tiên hơn giá trị nhúng.
fn embed_supabase_config() {
    println!("cargo:rerun-if-changed=../.env");
    let Ok(text) = std::fs::read_to_string("../.env") else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim_matches('\'');
        if v.is_empty() {
            continue;
        }
        match k.trim() {
            "SUPABASE_URL" => println!("cargo:rustc-env=CS_BUILD_SUPABASE_URL={v}"),
            "SUPABASE_ANON_KEY" if v.starts_with("sb_secret_") => {
                println!("cargo:warning=SUPABASE_ANON_KEY là secret key — KHÔNG nhúng vào binary");
            }
            "SUPABASE_ANON_KEY" => println!("cargo:rustc-env=CS_BUILD_SUPABASE_ANON_KEY={v}"),
            _ => {}
        }
    }
}
