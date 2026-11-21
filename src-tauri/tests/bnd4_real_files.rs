//! Đối chiếu `bnd4::rebind` với save thật và với kết quả của một công cụ khác.
//!
//! Không chạy trong `cargo test` thường: cần file save thật, mà save thật mang
//! id tài khoản của người chơi nên không nằm trong repo. Mỗi ca là một dòng
//! trong biến môi trường `CS_BND4_CASES`, các ca cách nhau bằng `;`:
//!
//! ```text
//! <file vào>|<tài khoản đích (32-bit)>|<file kết quả mong đợi, có thể bỏ trống>
//! ```
//!
//! Tài khoản của thư mục chứa file vào (17 chữ số hoặc 16 ký tự hex) được dùng
//! làm gợi ý về chủ của save, đúng như lúc khôi phục.
//!
//! ```text
//! $env:CS_BND4_CASES = "C:\saves\76561198000000001\S0000.sl2|123456|C:\expected\S0000.sl2"
//! cargo test --test bnd4_real_files -- --ignored --nocapture
//! ```
//!
//! Id được in ra ở dạng che (bốn chữ số cuối).

use std::path::Path;

use cloudsave_lib::bnd4::{self, Outcome};
use cloudsave_lib::steam::roots;

fn mask(account: u32) -> String {
    let text = roots::steam_id64(account).to_string();
    format!("..{}", &text[text.len() - 4..])
}

fn folder_account(path: &Path) -> Option<u32> {
    let name = path.parent()?.file_name()?.to_str()?;
    roots::account_from_id64(name).or_else(|| roots::account_from_id64_hex(name))
}

#[test]
#[ignore = "cần save thật: đặt CS_BND4_CASES"]
fn rebinds_real_saves_like_the_reference_tool() {
    let cases = std::env::var("CS_BND4_CASES").expect("đặt CS_BND4_CASES");
    let mut checked = 0;
    for case in cases.split(';').map(str::trim).filter(|c| !c.is_empty()) {
        let parts: Vec<&str> = case.split('|').collect();
        let (input, to) = (Path::new(parts[0]), parts[1].parse::<u32>().unwrap());
        let expected = parts.get(2).filter(|p| !p.is_empty()).map(Path::new);
        let name = input.file_name().unwrap().to_string_lossy();
        let data = std::fs::read(input).unwrap();
        let hints: Vec<u32> = folder_account(input).into_iter().collect();

        let started = std::time::Instant::now();
        let outcome = bnd4::rebind(&data, &hints, to);
        let took = started.elapsed();
        match outcome {
            Outcome::Rebound(r) => {
                let changed = data.iter().zip(&r.bytes).filter(|(a, b)| a != b).count();
                println!(
                    "{name}: {} byte, {} → {}, {} chỗ trong {} mục, {changed} byte đổi, {} — {took:.0?}",
                    data.len(),
                    mask(r.from),
                    mask(to),
                    r.replaced,
                    r.entries,
                    r.cipher.unwrap_or("không mã hoá"),
                );
                assert_eq!(r.bytes.len(), data.len(), "{name}: độ dài phải giữ nguyên");
                if let Some(expected) = expected {
                    assert!(
                        r.bytes == std::fs::read(expected).unwrap(),
                        "{name}: khác kết quả mong đợi"
                    );
                    println!("  khớp từng byte với {}", expected.display());
                }
                // Đổi ngược về chủ cũ phải ra lại đúng file ban đầu.
                match bnd4::rebind(&r.bytes, &[to], r.from) {
                    Outcome::Rebound(back) => {
                        assert!(back.bytes == data, "{name}: đổi ngược không ra file gốc")
                    }
                    other => panic!("{name}: đổi ngược thất bại: {other:?}"),
                }
                // Và file đã sửa giờ thuộc tài khoản đích.
                assert!(matches!(
                    bnd4::rebind(&r.bytes, &[to], to),
                    Outcome::AlreadyOwned
                ));
            }
            Outcome::NoAccount => {
                println!("{name}: {} byte, không ghi tài khoản nào bên trong", data.len());
                assert!(expected.is_none(), "{name}: mong đợi một file đã sửa");
            }
            other => panic!("{name}: {other:?}"),
        }
        checked += 1;
    }
    assert!(checked > 0, "CS_BND4_CASES không có ca nào");
}
