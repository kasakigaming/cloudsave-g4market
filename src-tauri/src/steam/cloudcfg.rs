//! Công tắc "Enable Steam Cloud" (Steam Settings → Cloud) của từng tài khoản.
//!
//! Steam lưu nó trong `userdata/<account>/7/remote/sharedconfig.vdf`:
//!
//! ```text
//! "UserRoamingConfigStore" { "Software" { "Valve" { "Steam" {
//!     "CloudEnabled"  "1"
//! } } } }
//! ```
//!
//! Không có file hoặc không có khoá = mặc định của Steam, tức là BẬT.
//!
//! Ghi file chỉ có tác dụng khi Steam đang tắt: Steam giữ giá trị trong bộ nhớ
//! và ghi đè file khi thoát. Việc tắt / mở lại Steam do `steam_cloud.rs` lo.
//!
//! Chỉ sửa file trong `7/remote/` là KHÔNG đủ: file đó được Steam Cloud đồng
//! bộ, và lúc đăng nhập Steam lấy bản trên server đè xuống (thấy trong
//! `logs/cloud_log.txt`: "SHA mismatch … Download complete"). Đường Steam dành
//! cho thay đổi lúc offline là "write-aside store" ở
//! `userdata/<account>/config/sharedconfig.vdf` — `logs/configstore_log.txt`
//! ghi "integrating write-aside store into cloud store" — nên ta ghi cả hai.
//!
//! Sửa file bằng cách thay đúng đoạn giá trị (không parse rồi in lại), để các
//! khoá khác và cách thụt lề của Steam còn nguyên. Khoá `cloudenabled` của
//! từng game (nằm sâu hơn, trong `apps/<id>`) không bị đụng tới.

use std::path::{Path, PathBuf};

use super::textvdf;
use crate::error::{Error, Result};

/// Đường dẫn khoá công tắc, tính từ gốc file.
const KEY_PATH: [&str; 4] = ["UserRoamingConfigStore", "Software", "Valve", "Steam"];
const KEY: &str = "CloudEnabled";

pub fn sharedconfig_path(steam_root: &Path, account_id: u32) -> PathBuf {
    steam_root
        .join("userdata")
        .join(account_id.to_string())
        .join("7")
        .join("remote")
        .join("sharedconfig.vdf")
}

/// File Steam gộp vào bản cloud lúc đăng nhập (thay đổi làm khi offline).
pub fn writeaside_path(steam_root: &Path, account_id: u32) -> PathBuf {
    steam_root
        .join("userdata")
        .join(account_id.to_string())
        .join("config")
        .join("sharedconfig.vdf")
}

/// Steam Cloud có đang bật cho tài khoản này không.
pub fn read_enabled(path: &Path) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e.into()),
    };
    Ok(enabled_in(&text)?)
}

fn enabled_in(text: &str) -> Result<bool> {
    let kv = textvdf::parse(text)?;
    let mut node = &kv;
    for k in KEY_PATH {
        match node.obj(k) {
            Some(n) => node = n,
            None => return Ok(true),
        }
    }
    Ok(node.str(KEY).map_or(true, |v| v.trim() != "0"))
}

/// Ghi công tắc vào file `path`. Lần đầu sửa thì giữ bản gốc ở `backup`.
///
/// `backup` phải nằm NGOÀI thư mục `remote/`: mọi file trong đó đều bị Steam
/// Cloud coi là dữ liệu cần đồng bộ lên server.
pub fn write_enabled(path: &Path, enabled: bool, backup: &Path) -> Result<()> {
    let old = match std::fs::read_to_string(path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let new = match &old {
        Some(t) => set_enabled_text(t, enabled)?,
        None => fresh_file(enabled),
    };
    if old.as_deref() == Some(new.as_str()) {
        return Ok(());
    }
    if let Some(t) = &old {
        if !backup.exists() {
            if let Some(d) = backup.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(backup, t)?;
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("vdf.cloudsave-tmp");
    std::fs::write(&tmp, &new)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn value(enabled: bool) -> &'static str {
    if enabled {
        "1"
    } else {
        "0"
    }
}

fn fresh_file(enabled: bool) -> String {
    format!(
        "\"UserRoamingConfigStore\"\n{{\n\t\"Software\"\n\t{{\n\t\t\"Valve\"\n\t\t{{\n\t\t\t\"Steam\"\n\t\t\t{{\n\t\t\t\t\"{KEY}\"\t\t\"{}\"\n\t\t\t}}\n\t\t}}\n\t}}\n}}\n",
        value(enabled)
    )
}

#[derive(Debug)]
enum Tok {
    /// Chuỗi trong ngoặc kép; `span` là vị trí phần NỘI DUNG (không gồm `"`).
    Str { text: String, span: (usize, usize) },
    Open,
    /// Vị trí của dấu `}`.
    Close(usize),
}

fn tokenize(src: &str) -> Result<Vec<Tok>> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j] != b'"' {
                    if b[j] == b'\\' {
                        j += 1;
                    }
                    j += 1;
                }
                if j >= b.len() {
                    return Err(Error::Other("sharedconfig.vdf: thiếu dấu \" đóng".into()));
                }
                out.push(Tok::Str {
                    text: src[start..j].to_string(),
                    span: (start, j),
                });
                i = j + 1;
            }
            b'{' => {
                out.push(Tok::Open);
                i += 1;
            }
            b'}' => {
                out.push(Tok::Close(i));
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    Ok(out)
}

/// Đổi (hoặc thêm) `CloudEnabled` trong khối `…/Valve/Steam`.
pub fn set_enabled_text(src: &str, enabled: bool) -> Result<String> {
    let toks = tokenize(src)?;
    let mut path: Vec<String> = Vec::new();
    let mut pending_key: Option<String> = None;
    // Vị trí `}` đóng khối Steam, để chèn khoá nếu chưa có.
    let mut steam_close: Option<usize> = None;
    let at_steam = |p: &[String]| {
        p.len() == KEY_PATH.len() && p.iter().zip(KEY_PATH).all(|(a, b)| a.eq_ignore_ascii_case(b))
    };

    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Str { text, span } => {
                if let Some(key) = pending_key.take() {
                    // Cặp "khoá" "giá trị".
                    if at_steam(&path) && key.eq_ignore_ascii_case(KEY) {
                        let mut out = String::with_capacity(src.len());
                        out.push_str(&src[..span.0]);
                        out.push_str(value(enabled));
                        out.push_str(&src[span.1..]);
                        return Ok(out);
                    }
                } else {
                    pending_key = Some(text.clone());
                }
            }
            Tok::Open => {
                let key = pending_key
                    .take()
                    .ok_or_else(|| Error::Other("sharedconfig.vdf: '{' không có tên khối".into()))?;
                path.push(key);
            }
            Tok::Close(pos) => {
                if at_steam(&path) && steam_close.is_none() {
                    steam_close = Some(*pos);
                }
                if path.pop().is_none() {
                    return Err(Error::Other("sharedconfig.vdf: thừa '}'".into()));
                }
                pending_key = None;
            }
        }
        i += 1;
    }

    let Some(close) = steam_close else {
        return Err(Error::Other(
            "sharedconfig.vdf: không thấy khối Software/Valve/Steam — không dám sửa".into(),
        ));
    };
    // Chèn ngay trước `}` của khối Steam, thụt lề như các dòng khác. Nếu `}`
    // không đứng riêng một dòng thì chèn thẳng trước nó.
    let line_start = src[..close].rfind('\n').map_or(0, |p| p + 1);
    let indent = &src[line_start..close];
    let (at, line) = if indent.chars().all(char::is_whitespace) {
        (line_start, format!("{indent}\t\"{KEY}\"\t\t\"{}\"\n", value(enabled)))
    } else {
        (close, format!("\"{KEY}\" \"{}\" ", value(enabled)))
    };
    let mut out = String::with_capacity(src.len() + 32);
    out.push_str(&src[..at]);
    out.push_str(&line);
    out.push_str(&src[at..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("cs-cloudcfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const REAL: &str = "\"UserRoamingConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"SurveyDate\"\t\t\"2023-04-09\"\n\t\t\t\t\"CloudEnabled\"\t\t\"1\"\n\t\t\t}\n\t\t}\n\t}\n}\n";

    #[test]
    fn flips_existing_key_and_keeps_the_rest() {
        let off = set_enabled_text(REAL, false).unwrap();
        assert_eq!(off, REAL.replace("\"CloudEnabled\"\t\t\"1\"", "\"CloudEnabled\"\t\t\"0\""));
        assert!(!enabled_in(&off).unwrap());
        let on = set_enabled_text(&off, true).unwrap();
        assert_eq!(on, REAL);
    }

    #[test]
    fn inserts_key_when_missing() {
        let src = REAL.replace("\t\t\t\t\"CloudEnabled\"\t\t\"1\"\n", "");
        assert!(enabled_in(&src).unwrap(), "không có khoá = mặc định bật");
        let off = set_enabled_text(&src, false).unwrap();
        assert!(!enabled_in(&off).unwrap());
        assert!(off.contains("\t\t\t\t\"CloudEnabled\"\t\t\"0\"\n\t\t\t}"));
        assert!(off.contains("SurveyDate"));
    }

    #[test]
    fn ignores_per_app_cloudenabled() {
        // Khoá cùng tên của từng game nằm sâu hơn — không phải công tắc chung.
        let src = "\"UserRoamingConfigStore\"\n{\n\t\"Software\"\n\t{\n\t\t\"Valve\"\n\t\t{\n\t\t\t\"Steam\"\n\t\t\t{\n\t\t\t\t\"apps\"\n\t\t\t\t{\n\t\t\t\t\t\"413150\"\n\t\t\t\t\t{\n\t\t\t\t\t\t\"cloudenabled\"\t\t\"1\"\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n}\n";
        let off = set_enabled_text(src, false).unwrap();
        assert!(off.contains("\"cloudenabled\"\t\t\"1\""), "khoá của game giữ nguyên");
        assert!(!enabled_in(&off).unwrap());
    }

    #[test]
    fn missing_file_is_created_and_read_back() {
        let dir = tmpdir();
        let p = dir.join("7").join("remote").join("sharedconfig.vdf");
        let bak = dir.join("bak.vdf");
        assert!(read_enabled(&p).unwrap());
        write_enabled(&p, false, &bak).unwrap();
        assert!(!read_enabled(&p).unwrap());
        write_enabled(&p, true, &bak).unwrap();
        assert!(read_enabled(&p).unwrap());
    }

    #[test]
    fn keeps_one_backup_of_the_original() {
        let dir = tmpdir();
        let p = dir.join("sharedconfig.vdf");
        std::fs::write(&p, REAL).unwrap();
        let bak = dir.join("backup").join("sharedconfig.vdf");
        write_enabled(&p, false, &bak).unwrap();
        write_enabled(&p, true, &bak).unwrap();
        write_enabled(&p, false, &bak).unwrap();
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            REAL,
            "bản sao lưu là file gốc, không bị ghi đè lần sau"
        );
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(
            !names.iter().any(|n| n.starts_with("sharedconfig.vdf.")),
            "không để lại file phụ cạnh file của Steam: {names:?}"
        );
    }

    #[test]
    fn inserts_into_single_line_block() {
        let src = "\"UserRoamingConfigStore\" { \"Software\" { \"Valve\" { \"Steam\" { \"A\" \"1\" } } } }";
        let off = set_enabled_text(src, false).unwrap();
        assert!(!enabled_in(&off).unwrap());
        assert!(textvdf::parse(&off).is_ok());
    }

    #[test]
    fn refuses_unknown_layout() {
        assert!(set_enabled_text("\"Other\"\n{\n}\n", false).is_err());
    }
}
