//! Trích "data" đọc được từ file save — **chỉ để hiển thị**.
//!
//! Đây là phần trả lời cho ý tưởng ban đầu "dùng data của file raw". Ta có
//! tầng data, nhưng nó nằm CẠNH bytes chứ không THAY THẾ bytes:
//!
//!   - Khôi phục luôn dùng bytes trong `blob_chunks`, không bao giờ dùng cái này.
//!   - Parse thất bại là chuyện bình thường, không phải lỗi. Đa số save là
//!     nhị phân độc quyền và không có cách nào đọc được ở dạng tổng quát.
//!
//! Lý do tách bạch: save game hay có checksum nội bộ, mã hoá theo máy, hoặc
//! bố cục phụ thuộc phiên bản engine. Dựng lại file từ JSON đã parse gần như
//! chắc chắn tạo ra file khác byte, và game sẽ coi đó là save hỏng.

use serde_json::{json, Map, Value};

/// Chỉ đọc phần đầu file: preview không đáng để nuốt 50 MB vào RAM.
const SNIFF_LIMIT: usize = 256 * 1024;

/// Số khoá tối đa giữ lại, tránh nhét cả cây JSON khổng lồ vào cột `preview`.
const MAX_KEYS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Xml,
    Ini,
    Sqlite,
    Text,
    Binary,
}

impl Format {
    fn as_str(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::Xml => "xml",
            Format::Ini => "ini",
            Format::Sqlite => "sqlite",
            Format::Text => "text",
            Format::Binary => "binary",
        }
    }
}

/// Nhận dạng định dạng bằng nội dung, không tin vào phần mở rộng: rất nhiều
/// game đặt đuôi `.sav` cho file JSON thuần, và ngược lại.
pub fn sniff(bytes: &[u8]) -> Format {
    if bytes.starts_with(b"SQLite format 3\0") {
        return Format::Sqlite;
    }
    let head = &bytes[..bytes.len().min(SNIFF_LIMIT)];

    // Có byte NUL ở đầu file gần như luôn nghĩa là nhị phân.
    if head.iter().take(4096).any(|&b| b == 0) {
        return Format::Binary;
    }
    let Ok(text) = std::str::from_utf8(head) else {
        return Format::Binary;
    };
    let t = text.trim_start();

    if t.starts_with('{') {
        return Format::Json;
    }
    // Dấu `[` mở đầu là chỗ JSON và INI đụng nhau: một file INI bắt đầu bằng
    // `[Player]` trông y hệt một mảng JSON. Phải thử parse thật mới phân biệt
    // được — đoán theo ký tự đầu là sai với mọi file .ini có section.
    if t.starts_with('[') && serde_json::from_str::<Value>(t).is_ok() {
        return Format::Json;
    }
    if t.starts_with("<?xml") || t.starts_with('<') {
        return Format::Xml;
    }
    if looks_like_ini(t) {
        return Format::Ini;
    }
    Format::Text
}

/// INI = có ít nhất một dòng section `[...]` hoặc một dòng `khoá = giá trị`.
fn looks_like_ini(t: &str) -> bool {
    t.lines().take(40).any(|l| {
        let l = l.trim();
        (l.starts_with('[') && l.ends_with(']') && l.len() > 2)
            || l.split_once('=')
                .is_some_and(|(k, _)| !k.trim().is_empty() && !k.contains(['{', '"']))
    })
}

/// Sinh preview cho một file. Trả `None` khi không rút được gì hữu ích.
pub fn extract(rel_path: &str, bytes: &[u8]) -> Option<Value> {
    let fmt = sniff(bytes);
    let mut obj = Map::new();
    obj.insert("file".into(), json!(rel_path));
    obj.insert("format".into(), json!(fmt.as_str()));
    obj.insert("size".into(), json!(bytes.len()));

    let head = &bytes[..bytes.len().min(SNIFF_LIMIT)];
    // Không dùng `?` ở đây: parse hỏng thì chỉ mất phần `fields`, chứ không
    // được mất luôn cả tên file và dung lượng. Chuyện này xảy ra thường xuyên
    // và hợp lệ — vd một file JSON lớn hơn SNIFF_LIMIT sẽ bị cắt cụt giữa chừng.
    let text = std::str::from_utf8(head).ok();
    let fields = match (fmt, text) {
        (Format::Json, Some(t)) => serde_json::from_str::<Value>(t).ok().map(|v| summarize(&v)),
        (Format::Ini, Some(t)) => Some(parse_ini(t)),
        (Format::Xml, Some(t)) => Some(scrape_xml_attrs(t)),
        // Với nhị phân và SQLite ta cố tình không đoán gì. Biết đuôi file và
        // dung lượng đã đủ cho UI; đoán bừa chỉ tạo thông tin sai.
        _ => None,
    };
    if let Some(f) = fields {
        // Object rỗng chỉ làm rối UI mà không nói lên điều gì.
        if !matches!(&f, Value::Object(m) if m.is_empty()) {
            obj.insert("fields".into(), f);
        }
    }

    Some(Value::Object(obj))
}

/// Gộp preview của nhiều file thành một object cho cột `snapshots.preview`.
pub fn combine(per_file: Vec<Value>) -> Value {
    json!({
        "version": 1,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "files": per_file,
        "note": "Chỉ dùng để hiển thị. Khôi phục luôn đọc bytes từ blob_chunks."
    })
}

/// Giữ lại các giá trị vô hướng ở tầng nông — thứ thường mang ý nghĩa với
/// người chơi (level, gold, playtime, tên nhân vật).
fn summarize(v: &Value) -> Value {
    let mut out = Map::new();
    collect_scalars(v, "", &mut out, 0);
    Value::Object(out)
}

fn collect_scalars(v: &Value, prefix: &str, out: &mut Map<String, Value>, depth: u32) {
    if out.len() >= MAX_KEYS || depth > 3 {
        return;
    }
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                if out.len() >= MAX_KEYS {
                    return;
                }
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                match val {
                    Value::Object(_) | Value::Array(_) => {
                        collect_scalars(val, &key, out, depth + 1)
                    }
                    other => {
                        out.insert(key, truncate(other));
                    }
                }
            }
        }
        Value::Array(a) => {
            if prefix.is_empty() {
                return;
            }
            out.insert(format!("{prefix}[]"), json!(a.len()));
        }
        other => {
            if !prefix.is_empty() {
                out.insert(prefix.to_string(), truncate(other));
            }
        }
    }
}

fn truncate(v: &Value) -> Value {
    match v {
        Value::String(s) if s.len() > 120 => json!(format!("{}…", &s[..120])),
        other => other.clone(),
    }
}

fn parse_ini(text: &str) -> Value {
    let mut out = Map::new();
    let mut section = String::new();
    for line in text.lines() {
        if out.len() >= MAX_KEYS {
            break;
        }
        let l = line.trim();
        if l.is_empty() || l.starts_with(';') || l.starts_with('#') {
            continue;
        }
        if l.starts_with('[') && l.ends_with(']') {
            section = l[1..l.len() - 1].to_string();
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            let key = if section.is_empty() {
                k.trim().to_string()
            } else {
                format!("{section}.{}", k.trim())
            };
            out.insert(key, json!(v.trim()));
        }
    }
    Value::Object(out)
}

/// Quét thuộc tính XML bằng cách duyệt chuỗi, không dựng cây.
///
/// Đủ cho preview và tránh kéo thêm một dependency parser XML chỉ để hiển thị
/// vài con số. Không dùng cho bất cứ việc gì cần chính xác.
fn scrape_xml_attrs(text: &str) -> Value {
    let mut out = Map::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() && out.len() < MAX_KEYS {
        if bytes[i] != b'=' || i + 1 >= bytes.len() || bytes[i + 1] != b'"' {
            i += 1;
            continue;
        }
        // Lùi lại lấy tên thuộc tính.
        let mut start = i;
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            start -= 1;
        }
        let name = &text[start..i];

        let vstart = i + 2;
        let Some(vlen) = text[vstart..].find('"') else {
            break;
        };
        let value = &text[vstart..vstart + vlen];

        if !name.is_empty() && !value.is_empty() && value.len() <= 120 {
            out.insert(name.to_string(), json!(value));
        }
        i = vstart + vlen + 1;
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_formats() {
        assert_eq!(sniff(b"{\"a\":1}"), Format::Json);
        assert_eq!(sniff(b"<?xml version=\"1.0\"?><a/>"), Format::Xml);
        assert_eq!(sniff(b"[Player]\nlevel=5\n"), Format::Ini);
        assert_eq!(sniff(b"SQLite format 3\0rest"), Format::Sqlite);
        assert_eq!(sniff(&[0x00, 0x01, 0x02, 0x03]), Format::Binary);
    }

    #[test]
    fn extracts_json_fields() {
        let v = extract("save.json", br#"{"level":42,"player":{"name":"An"}}"#).unwrap();
        let f = &v["fields"];
        assert_eq!(f["level"], json!(42));
        assert_eq!(f["player.name"], json!("An"));
    }

    #[test]
    fn extracts_ini_sections() {
        let v = extract("cfg.ini", b"[Player]\nlevel=5\ngold = 1200\n").unwrap();
        assert_eq!(v["fields"]["Player.level"], json!("5"));
        assert_eq!(v["fields"]["Player.gold"], json!("1200"));
    }

    #[test]
    fn binary_yields_no_fields() {
        let v = extract("slot.sav", &[0u8, 1, 2, 3, 4]).unwrap();
        assert_eq!(v["format"], json!("binary"));
        assert!(v.get("fields").is_none());
    }

    #[test]
    fn ini_section_is_not_mistaken_for_json_array() {
        // Cả hai đều mở đầu bằng '['. Chỉ thử parse thật mới phân biệt được.
        assert_eq!(sniff(b"[Player]\nlevel=5\n"), Format::Ini);
        assert_eq!(sniff(br#"["a","b"]"#), Format::Json);
        assert_eq!(sniff(b"[1, 2, 3]"), Format::Json);
    }

    #[test]
    fn truncated_json_still_reports_basics() {
        // File JSON dài hơn SNIFF_LIMIT bị cắt giữa chừng: mất `fields` là
        // chấp nhận được, mất cả preview thì không.
        let mut big = br#"{"items":["#.to_vec();
        big.resize(SNIFF_LIMIT + 5_000, b'x');
        let v = extract("big.json", &big).unwrap();
        assert_eq!(v["file"], json!("big.json"));
        assert_eq!(v["size"], json!(big.len()));
        assert!(v.get("fields").is_none());
    }
}
