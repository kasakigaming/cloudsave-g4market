//! Parser cho `<steam>/appcache/appinfo.vdf` (binary VDF).
//!
//! Đây là nguồn đường dẫn save chính xác nhất và đọc được hoàn toàn offline:
//! Steam tự khai báo chỗ để save của từng game trong nhánh `ufs.savefiles`.
//! Đây cũng chính là dữ liệu mà Steam Cloud (và CloudRedirect) dựa vào.
//!
//! Layout (đã đối chiếu với file thật, magic 0x07564429 = v29):
//!
//! ```text
//! magic              u32     0x07564427 | 0x07564428 | 0x07564429
//! universe           u32
//! stringTableOffset  i64     (chỉ v29+)
//! lặp cho tới khi appid == 0:
//!     appid          u32
//!     size           u32     số byte còn lại của app này, tính TỪ SAU trường size
//!     infoState      u32
//!     lastUpdated    u32
//!     picsToken      u64
//!     sha1Text       [u8;20]
//!     changeNumber   u32
//!     sha1Binary     [u8;20] (chỉ v28+)
//!     data           binary VDF, dài (size - 60)
//! ```
//!
//! Ở v29, khoá của mỗi node là **u32 index vào string table**, không phải
//! chuỗi NUL-terminated như các bản cũ. String table nằm ở `stringTableOffset`:
//! `u32 count` rồi `count` chuỗi NUL-terminated liên tiếp.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::error::{Error, Result};

const MAGIC_V27: u32 = 0x0756_4427;
const MAGIC_V28: u32 = 0x0756_4428;
const MAGIC_V29: u32 = 0x0756_4429;

/// Một node trong cây VDF. Ta chỉ giữ những kiểu thực sự gặp trong appinfo.
#[derive(Debug, Clone)]
pub enum Vdf {
    Obj(BTreeMap<String, Vdf>),
    Str(String),
    Int(i64),
}

fn empty_map() -> &'static BTreeMap<String, Vdf> {
    static EMPTY: OnceLock<BTreeMap<String, Vdf>> = OnceLock::new();
    EMPTY.get_or_init(BTreeMap::new)
}

impl Vdf {
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            // Khoá trong VDF không phân biệt hoa thường.
            Vdf::Obj(m) => m.get(key).or_else(|| {
                m.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(key))
                    .map(|(_, v)| v)
            }),
            _ => None,
        }
    }

    /// Đi theo một chuỗi khoá lồng nhau: `v.path(&["ufs", "savefiles"])`.
    pub fn path(&self, keys: &[&str]) -> Option<&Vdf> {
        keys.iter().try_fold(self, |cur, k| cur.get(k))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Vdf::Int(i) => Some(*i),
            // Nhiều trường trong appinfo là chuỗi chứa số, vd recursive = "1".
            Vdf::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn entries(&self) -> std::collections::btree_map::Iter<'_, String, Vdf> {
        match self {
            Vdf::Obj(m) => m.iter(),
            _ => empty_map().iter(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Con trỏ đọc
// ─────────────────────────────────────────────────────────────────────────

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn need(&self, n: usize) -> Result<()> {
        if self.pos + n > self.buf.len() {
            return Err(Error::Parse(format!(
                "appinfo.vdf: đọc quá cuối vùng dữ liệu tại offset {} (cần {} byte)",
                self.pos, n
            )));
        }
        Ok(())
    }

    fn u8(&mut self) -> Result<u8> {
        self.need(1)?;
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn u32(&mut self) -> Result<u32> {
        self.need(4)?;
        let v = u32::from_le_bytes(self.buf[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        Ok(v)
    }

    fn i64v(&mut self) -> Result<i64> {
        self.need(8)?;
        let v = i64::from_le_bytes(self.buf[self.pos..self.pos + 8].try_into().unwrap());
        self.pos += 8;
        Ok(v)
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.need(n)?;
        self.pos += n;
        Ok(())
    }

    /// Chuỗi NUL-terminated. Dữ liệu Steam thường là UTF-8 nhưng không phải
    /// lúc nào cũng hợp lệ, nên dùng lossy thay vì làm hỏng cả file.
    fn cstr(&mut self) -> Result<String> {
        let start = self.pos;
        while self.pos < self.buf.len() && self.buf[self.pos] != 0 {
            self.pos += 1;
        }
        if self.pos >= self.buf.len() {
            return Err(Error::Parse("appinfo.vdf: chuỗi không kết thúc".into()));
        }
        let s = String::from_utf8_lossy(&self.buf[start..self.pos]).into_owned();
        self.pos += 1; // nuốt NUL
        Ok(s)
    }

    /// Chuỗi UTF-16LE kết thúc bằng 0x0000.
    fn wstr(&mut self) -> Result<String> {
        let mut units = Vec::new();
        loop {
            self.need(2)?;
            let u = u16::from_le_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
            self.pos += 2;
            if u == 0 {
                break;
            }
            units.push(u);
        }
        Ok(String::from_utf16_lossy(&units))
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Binary VDF
// ─────────────────────────────────────────────────────────────────────────

const T_OBJ: u8 = 0x00;
const T_STR: u8 = 0x01;
const T_I32: u8 = 0x02;
const T_F32: u8 = 0x03;
const T_PTR: u8 = 0x04;
const T_WSTR: u8 = 0x05;
const T_COLOR: u8 = 0x06;
const T_U64: u8 = 0x07;
const T_END: u8 = 0x08;
const T_I64: u8 = 0x0A;
const T_END_ALT: u8 = 0x0B;

/// Bảng chuỗi của v29, hoặc None với v27/v28 (khoá là cstring trực tiếp).
type StringTable<'a> = Option<&'a [String]>;

fn read_key(c: &mut Cursor, table: StringTable) -> Result<String> {
    match table {
        Some(t) => {
            let idx = c.u32()? as usize;
            t.get(idx).cloned().ok_or_else(|| {
                Error::Parse(format!(
                    "appinfo.vdf: string table index {idx} vượt phạm vi"
                ))
            })
        }
        None => c.cstr(),
    }
}

fn read_obj(c: &mut Cursor, table: StringTable, depth: u32) -> Result<Vdf> {
    if depth > 64 {
        return Err(Error::Parse("appinfo.vdf: lồng quá sâu".into()));
    }
    let mut map = BTreeMap::new();
    loop {
        let ty = c.u8()?;
        if ty == T_END || ty == T_END_ALT {
            break;
        }
        let key = read_key(c, table)?;
        let val = match ty {
            T_OBJ => read_obj(c, table, depth + 1)?,
            T_STR => Vdf::Str(c.cstr()?),
            T_WSTR => Vdf::Str(c.wstr()?),
            T_I32 | T_PTR | T_COLOR => Vdf::Int(c.u32()? as i32 as i64),
            T_F32 => {
                let bits = c.u32()?;
                Vdf::Str(f32::from_bits(bits).to_string())
            }
            T_U64 | T_I64 => Vdf::Int(c.i64v()?),
            other => {
                return Err(Error::Parse(format!(
                    "appinfo.vdf: kiểu node lạ 0x{other:02x} tại offset {}",
                    c.pos
                )))
            }
        };
        map.insert(key, val);
    }
    Ok(Vdf::Obj(map))
}

// ─────────────────────────────────────────────────────────────────────────
// File appinfo.vdf
// ─────────────────────────────────────────────────────────────────────────

/// Header cố định mỗi app, tính từ sau trường `size`:
/// infoState 4 + lastUpdated 4 + picsToken 8 + sha1Text 20 + changeNumber 4.
const APP_HEADER_V27: usize = 40;
/// v28+ có thêm sha1Binary 20 byte.
const APP_HEADER_V28: usize = APP_HEADER_V27 + 20;

pub struct AppInfo {
    apps: BTreeMap<u32, Vdf>,
}

impl AppInfo {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut c = Cursor::new(bytes);
        let magic = c.u32()?;
        if !matches!(magic, MAGIC_V27 | MAGIC_V28 | MAGIC_V29) {
            return Err(Error::Parse(format!(
                "appinfo.vdf: magic không nhận ra 0x{magic:08x}"
            )));
        }
        let _universe = c.u32()?;

        // v29 đặt string table ở cuối file và trỏ tới bằng offset 64-bit.
        let table: Option<Vec<String>> = if magic >= MAGIC_V29 {
            let off = c.i64v()?;
            if off <= 0 || off as usize >= bytes.len() {
                return Err(Error::Parse(format!(
                    "appinfo.vdf: stringTableOffset {off} không hợp lệ"
                )));
            }
            let mut tc = Cursor::new(bytes);
            tc.pos = off as usize;
            let count = tc.u32()? as usize;
            let mut v = Vec::with_capacity(count.min(100_000));
            for _ in 0..count {
                v.push(tc.cstr()?);
            }
            Some(v)
        } else {
            None
        };
        let table_ref: StringTable = table.as_deref();

        let app_header = if magic >= MAGIC_V28 {
            APP_HEADER_V28
        } else {
            APP_HEADER_V27
        };

        let mut apps = BTreeMap::new();
        loop {
            let app_id = c.u32()?;
            if app_id == 0 {
                break; // sentinel kết thúc
            }
            let size = c.u32()? as usize;
            if size < app_header {
                return Err(Error::Parse(format!(
                    "appinfo.vdf: app {app_id} khai size {size} nhỏ hơn header {app_header}"
                )));
            }
            let body_start = c.pos;
            let data_end = body_start + size;
            if data_end > bytes.len() {
                return Err(Error::Parse(format!(
                    "appinfo.vdf: app {app_id} tràn ra ngoài file"
                )));
            }
            c.skip(app_header)?;

            // Parse trong một cursor riêng giới hạn đúng vùng của app này.
            // Nếu một app lỗi ta bỏ qua nó thay vì vứt cả file: appinfo chứa
            // hàng chục nghìn app, chỉ cần một app dị dạng là mất sạch.
            let mut ac = Cursor::new(&bytes[c.pos..data_end]);
            match read_obj(&mut ac, table_ref, 0) {
                Ok(v) => {
                    // Lớp ngoài cùng luôn là node tên "appinfo"; bóc ra cho gọn.
                    let inner = v.get("appinfo").cloned().unwrap_or(v);
                    apps.insert(app_id, inner);
                }
                Err(e) => log::warn!("appinfo.vdf: bỏ qua app {app_id}: {e}"),
            }

            c.pos = data_end;
        }

        Ok(Self { apps })
    }

    pub fn app(&self, app_id: u32) -> Option<&Vdf> {
        self.apps.get(&app_id)
    }

    pub fn app_name(&self, app_id: u32) -> Option<String> {
        self.app(app_id)?
            .path(&["common", "name"])?
            .as_str()
            .map(str::to_owned)
    }

    /// Tên thư mục cài, để ghép thành `steamapps/common/<installdir>`.
    pub fn install_dir(&self, app_id: u32) -> Option<String> {
        self.app(app_id)?
            .path(&["config", "installdir"])?
            .as_str()
            .map(str::to_owned)
    }

    pub fn app_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.apps.keys().copied()
    }

    /// Các quy tắc save trong `ufs.savefiles`.
    pub fn save_rules(&self, app_id: u32) -> Vec<UfsRule> {
        let Some(app) = self.app(app_id) else {
            return Vec::new();
        };
        let Some(savefiles) = app.path(&["ufs", "savefiles"]) else {
            return Vec::new();
        };
        savefiles
            .entries()
            .filter_map(|(_idx, rule)| UfsRule::from_vdf(rule))
            .collect()
    }
}

/// Một mục trong `ufs.savefiles`.
#[derive(Debug, Clone)]
pub struct UfsRule {
    pub root: String,
    /// Thư mục con so với root. Rỗng nghĩa là ngay tại root.
    pub path: String,
    /// Glob khớp tên file, vd `*.sav`. Rỗng coi như `*`.
    pub pattern: String,
    pub recursive: bool,
    /// Nền tảng áp dụng; rỗng nghĩa là mọi nền tảng.
    pub platforms: Vec<String>,
}

impl UfsRule {
    fn from_vdf(v: &Vdf) -> Option<Self> {
        let root = v.get("root")?.as_str()?.to_owned();
        let path = v
            .get("path")
            .and_then(Vdf::as_str)
            .unwrap_or_default()
            .replace('\\', "/")
            .trim_matches('/')
            .to_owned();
        let pattern = v
            .get("pattern")
            .and_then(Vdf::as_str)
            .unwrap_or("*")
            .to_owned();
        let recursive = v.get("recursive").and_then(Vdf::as_i64).unwrap_or(0) != 0;
        let platforms = v
            .get("platforms")
            .map(|p| {
                p.entries()
                    .filter_map(|(_, x)| x.as_str().map(|s| s.to_ascii_lowercase()))
                    .collect()
            })
            .unwrap_or_default();

        Some(Self {
            root,
            path,
            pattern: if pattern.is_empty() {
                "*".into()
            } else {
                pattern
            },
            recursive,
            platforms,
        })
    }

    /// Các giá trị `platforms` thực tế trong appinfo.vdf là `Windows`,
    /// `MacOS`, `Linux`, `all` — ta đã hạ về chữ thường lúc parse.
    pub fn applies_to_windows(&self) -> bool {
        self.platforms.is_empty() || self.platforms.iter().any(|p| p == "windows" || p == "all")
    }

    /// Thay placeholder trong `path` bằng giá trị của tài khoản hiện tại.
    ///
    /// Steam nhúng id người chơi thẳng vào đường dẫn save. Đếm trên
    /// `appinfo.vdf` thật: `{64BitSteamID}` xuất hiện 71 lần và
    /// `{Steam3AccountID}` 11 lần trên tổng 422 quy tắc — bỏ qua chúng là mất
    /// gần một phần năm số quy tắc, gồm cả những game lớn như Stellar Blade,
    /// Black Myth: Wukong và Elden Ring Nightreign.
    ///
    /// Trả `None` khi còn placeholder lạ: thà bỏ sót còn hơn đi mở một thư mục
    /// tên đúng bằng chuỗi `{SomethingElse}`.
    pub fn expanded_path(&self, account_id: u32) -> Option<String> {
        let steam_id64 = STEAMID64_BASE + account_id as u64;
        let expanded = self
            .path
            .replace("{64BitSteamID}", &steam_id64.to_string())
            .replace("{Steam3AccountID}", &account_id.to_string());

        if expanded.contains('{') || expanded.contains('%') {
            log::debug!(
                "UFS: còn placeholder chưa giải trong '{}', bỏ qua",
                self.path
            );
            return None;
        }
        Some(expanded)
    }
}

/// Offset chuyển account id 32-bit sang SteamID64.
const STEAMID64_BASE: u64 = 76_561_197_960_265_728;

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(path: &str) -> UfsRule {
        UfsRule {
            root: "WindowsHome".into(),
            path: path.into(),
            pattern: "*.sav".into(),
            recursive: false,
            platforms: vec![],
        }
    }

    #[test]
    fn expands_steamid64() {
        // Stellar Blade: "Documents/StellarBlade/{64BitSteamID}"
        let r = rule("Documents/StellarBlade/{64BitSteamID}");
        assert_eq!(
            r.expanded_path(123_456_789).unwrap(),
            "Documents/StellarBlade/76561198083722517"
        );
    }

    #[test]
    fn expands_account_id() {
        let r = rule("Saves/{Steam3AccountID}");
        assert_eq!(r.expanded_path(987_654_321).unwrap(), "Saves/987654321");
    }

    #[test]
    fn leaves_plain_path_alone() {
        assert_eq!(
            rule("StardewValley/Saves").expanded_path(1).unwrap(),
            "StardewValley/Saves"
        );
    }

    #[test]
    fn rejects_unknown_placeholder() {
        assert!(rule("Saves/{SomethingElse}").expanded_path(1).is_none());
    }

    #[test]
    fn windows_platform_filter() {
        let mut r = rule("x");
        r.platforms = vec!["windows".into()];
        assert!(r.applies_to_windows());
        r.platforms = vec!["macos".into()];
        assert!(!r.applies_to_windows());
        r.platforms = vec!["all".into()];
        assert!(r.applies_to_windows());
        r.platforms = vec![];
        assert!(r.applies_to_windows());
    }
}
