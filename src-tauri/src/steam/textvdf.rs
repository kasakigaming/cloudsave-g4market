//! Parser tối giản cho VDF dạng text (KeyValues).
//!
//! Dùng cho `libraryfolders.vdf`, `appmanifest_*.acf`, `remotecache.vdf`,
//! `loginusers.vdf`. Định dạng rất đơn giản:
//!
//! ```text
//! "khoá"  "giá trị"
//! "khoá"
//! {
//!     "khoá con"  "giá trị"
//! }
//! ```
//!
//! Ta cố tình KHÔNG hỗ trợ `#include` / `#base` — Steam không dùng chúng trong
//! các file này và hỗ trợ thêm chỉ mở ra đường đọc file ngoài ý muốn.

use std::collections::BTreeMap;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct KeyValues {
    /// Giữ nguyên thứ tự xuất hiện không quan trọng ở đây, nhưng khoá có thể
    /// trùng (hiếm); ta giữ bản cuối cùng, giống hành vi của Steam.
    pub map: BTreeMap<String, Node>,
}

#[derive(Debug, Clone)]
pub enum Node {
    Str(String),
    Obj(KeyValues),
}

impl KeyValues {
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.map.get(key).or_else(|| {
            self.map
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v)
        })
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Node::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn obj(&self, key: &str) -> Option<&KeyValues> {
        match self.get(key)? {
            Node::Obj(o) => Some(o),
            _ => None,
        }
    }

    pub fn iter(&self) -> std::collections::btree_map::Iter<'_, String, Node> {
        self.map.iter()
    }

    /// Bóc lớp bọc ngoài cùng khi file chỉ có đúng một node gốc,
    /// vd `"241100" { ... }` trong remotecache.vdf.
    pub fn unwrap_single_root(&self) -> Option<(&String, &KeyValues)> {
        if self.map.len() != 1 {
            return None;
        }
        let (k, v) = self.map.iter().next()?;
        match v {
            Node::Obj(o) => Some((k, o)),
            _ => None,
        }
    }
}

pub fn parse(text: &str) -> Result<KeyValues> {
    let mut p = Parser {
        b: text.as_bytes(),
        i: 0,
    };
    let kv = p.parse_body(0)?;
    Ok(kv)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.i < self.b.len() {
            match self.b[self.i] {
                b' ' | b'\t' | b'\r' | b'\n' => self.i += 1,
                // Bình luận `//` tới hết dòng.
                b'/' if self.i + 1 < self.b.len() && self.b[self.i + 1] == b'/' => {
                    while self.i < self.b.len() && self.b[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                _ => break,
            }
        }
    }

    /// Đọc một token: chuỗi trong nháy kép (có escape) hoặc từ trần.
    fn token(&mut self) -> Result<String> {
        self.skip_ws();
        if self.i >= self.b.len() {
            return Err(Error::Parse("VDF: hết file khi đang đợi token".into()));
        }
        if self.b[self.i] == b'"' {
            self.i += 1;
            let mut out = Vec::new();
            while self.i < self.b.len() {
                match self.b[self.i] {
                    b'"' => {
                        self.i += 1;
                        return Ok(String::from_utf8_lossy(&out).into_owned());
                    }
                    b'\\' if self.i + 1 < self.b.len() => {
                        let esc = self.b[self.i + 1];
                        out.push(match esc {
                            b'n' => b'\n',
                            b't' => b'\t',
                            b'r' => b'\r',
                            other => other, // gồm cả \\ và \"
                        });
                        self.i += 2;
                    }
                    c => {
                        out.push(c);
                        self.i += 1;
                    }
                }
            }
            Err(Error::Parse("VDF: chuỗi không đóng nháy".into()))
        } else {
            let start = self.i;
            while self.i < self.b.len()
                && !matches!(self.b[self.i], b' ' | b'\t' | b'\r' | b'\n' | b'{' | b'}')
            {
                self.i += 1;
            }
            if start == self.i {
                return Err(Error::Parse(format!(
                    "VDF: token rỗng tại offset {}",
                    self.i
                )));
            }
            Ok(String::from_utf8_lossy(&self.b[start..self.i]).into_owned())
        }
    }

    fn parse_body(&mut self, depth: u32) -> Result<KeyValues> {
        if depth > 64 {
            return Err(Error::Parse("VDF: lồng quá sâu".into()));
        }
        let mut kv = KeyValues::default();
        loop {
            self.skip_ws();
            if self.i >= self.b.len() {
                break;
            }
            if self.b[self.i] == b'}' {
                self.i += 1;
                break;
            }
            let key = self.token()?;
            self.skip_ws();
            if self.i >= self.b.len() {
                return Err(Error::Parse(format!("VDF: khoá '{key}' thiếu giá trị")));
            }
            if self.b[self.i] == b'{' {
                self.i += 1;
                let child = self.parse_body(depth + 1)?;
                kv.map.insert(key, Node::Obj(child));
            } else {
                let val = self.token()?;
                kv.map.insert(key, Node::Str(val));
            }
        }
        Ok(kv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remotecache_shape() {
        // Rút gọn từ userdata/<id>/413150/remotecache.vdf thật.
        let src = r#"
"413150"
{
	"ChangeNumber"		"36"
	"StardewValley/Saves/Farm_100000001/Farm_100000001"
	{
		"root"		"4"
		"size"		"4165569"
		"sha"		"10d19d1c4e421d382fcb4211af075ee30615ab19"
	}
}
"#;
        let kv = parse(src).unwrap();
        let (app, body) = kv.unwrap_single_root().unwrap();
        assert_eq!(app, "413150");
        assert_eq!(body.str("ChangeNumber"), Some("36"));
        let f = body
            .obj("StardewValley/Saves/Farm_100000001/Farm_100000001")
            .unwrap();
        assert_eq!(f.str("root"), Some("4"));
        assert_eq!(f.str("size"), Some("4165569"));
    }
}
