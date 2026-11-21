//! Đổi id tài khoản Steam **bên trong** save FromSoftware (Sekiro, Elden Ring,
//! Dark Souls II / III…), để save của tài khoản này chơi được bằng tài khoản khác.
//!
//! Các game này ghi SteamID64 của người chơi vào chính file `.sl2` và từ chối
//! đọc khi id đó khác tài khoản đang chơi ("Failed to load save data"), dù file
//! nằm đúng thư mục. Đổi đường dẫn như `restore` vẫn làm là chưa đủ.
//!
//! Một logic cho mọi game, không tách theo tên game hay tên file:
//!
//!  1. Đọc bảng mục của BND4 và chứng minh mỗi mục mở đầu bằng MD5 của phần còn
//!     lại. Không đúng thế thì đây không phải loại file này — không đụng vào.
//!  2. Lấy bản rõ của từng mục: chính thân mục, hoặc — khi thân mục là IV +
//!     AES-128-CBC — bản giải mã bằng đúng cái khoá trong bảng cho kết quả hợp lý.
//!  3. Tìm tài khoản mà bản rõ ghi (SteamID64 dạng 8 byte little-endian, hoặc
//!     16 ký tự hex) và xác định ai là chủ.
//!  4. Thay id của chủ, mã hoá lại bằng **cùng IV** nếu mục có mã hoá, tính lại
//!     MD5 của mục vừa đổi.
//!  5. Đọc lại kết quả qua đúng các bước 1–3 và đòi nó khớp từng chỗ với dự
//!     tính. Lệch một chỗ là bỏ, trả về "không sửa" — không bao giờ đưa ra một
//!     file sửa dở.
//!
//! Hàm ở đây không đọc đĩa, không ghi đĩa: nhận dãy byte, trả dãy byte. Kho
//! local và cloud đặt tên blob theo sha256 của nội dung **gốc**, nên bản đã sửa
//! không bao giờ được ghi ngược vào kho — nó chỉ tồn tại ở bước ghi ra thư mục
//! save (xem `restore`).
//!
//! Đã đối chiếu từng byte với công cụ đã tạo ra các file Sekiro và Elden Ring mà
//! game nhận. Dark Souls II / III mới kiểm ở mức file (giải mã, sửa, mã hoá lại,
//! đọc lại được), chưa nạp trong game.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use md5::{Digest, Md5};

use crate::steam::roots;

const MAGIC: &[u8; 4] = b"BND4";
const MD5_LEN: usize = 16;
const BLOCK: usize = 16;
/// Bốn byte cao của mọi SteamID64 tài khoản cá nhân, theo thứ tự little-endian.
const HIGH_LE: [u8; 4] = [0x01, 0x00, 0x10, 0x01];
/// Cũng bốn byte đó, viết thành hex. Toàn chữ số nên không có chuyện hoa/thường.
const HIGH_HEX: &[u8; 8] = b"01100001";

/// Khoá tĩnh của từng game, công bố trong SoulsFormats (`SFUtil.cs`) và nhiều
/// công cụ mã nguồn mở khác. Không cần biết file của game nào: khoá đúng là
/// khoá mà **mọi** mục giải ra đều hợp lý.
const KEYS: [(&str, [u8; 16]); 4] = [
    (
        "Dark Souls III",
        [
            0xFD, 0x46, 0x4D, 0x69, 0x5E, 0x69, 0xA3, 0x9A, 0x10, 0xE3, 0x19, 0xA7, 0xAC, 0xE8,
            0xB7, 0xFA,
        ],
    ),
    (
        "Dark Souls II: Scholar of the First Sin",
        [
            0x59, 0x9F, 0x9B, 0x69, 0x96, 0x40, 0xA5, 0x52, 0x36, 0xEE, 0x2D, 0x70, 0x83, 0x5E,
            0xC7, 0x44,
        ],
    ),
    (
        "Dark Souls II",
        [
            0xB7, 0xFD, 0x46, 0x3E, 0x4A, 0x9C, 0x11, 0x02, 0xDF, 0x17, 0x39, 0xE5, 0xF3, 0xB2,
            0xA5, 0x0F,
        ],
    ),
    (
        "Dark Souls Remastered",
        [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54,
            0x32, 0x10,
        ],
    ),
];

/// Kết quả của `rebind`.
#[derive(Debug)]
pub enum Outcome {
    /// Không phải BND4, hoặc là BND4 kiểu khác (mục không mở đầu bằng MD5 của
    /// chính nó), hoặc file hỏng. Không có gì để làm ở đây.
    NotThisFormat,
    /// Đúng định dạng nhưng không ghi tài khoản nào bên trong (Dark Souls
    /// Remastered): chỉ thư mục gắn save với tài khoản.
    NoAccount,
    /// Chủ của save đã là tài khoản đích.
    AlreadyOwned,
    Rebound(Rebound),
    /// Đúng định dạng, có ghi tài khoản, nhưng không sửa được một cách chắc
    /// chắn. Chuỗi là lý do, để đưa vào cảnh báo.
    Refused(String),
}

#[derive(Debug)]
pub struct Rebound {
    /// Nội dung đã sửa: cùng độ dài với bản gốc, mọi MD5 hợp lệ.
    pub bytes: Vec<u8>,
    /// Tài khoản là chủ của save trước khi sửa.
    pub from: u32,
    /// Số chỗ ghi id đã thay.
    pub replaced: usize,
    /// Số mục phải ghi lại (và tính lại MD5).
    pub entries: usize,
    /// Tên bộ khoá nếu các mục có mã hoá.
    pub cipher: Option<&'static str>,
}

/// Kiểm nhanh bốn byte đầu, để khỏi phân tích mọi file save của mọi game.
pub fn looks_like(data: &[u8]) -> bool {
    data.len() >= 0x40 && data[..4] == MAGIC[..]
}

/// Đổi chủ của save sang tài khoản `to`.
///
/// `hints`: các tài khoản mà bên gọi tin là chủ, đáng tin nhất trước — tài khoản
/// của thư mục chứa file, rồi tài khoản đã chụp bản lưu. Cần vì một giá trị
/// *trông như* SteamID64 chưa chắc là của chủ: Dark Souls III ghi cả id của
/// những người chơi đã gặp, và dữ liệu game có thể tình cờ chứa đúng bốn byte
/// cao đó (một save Dark Souls II đo được 294 chỗ như vậy, gọi tên mười "tài
/// khoản"). Thứ tự xét: tài khoản trong `hints` mà file có ghi; tài khoản duy
/// nhất file ghi; tài khoản duy nhất được ghi ở dạng chữ.
pub fn rebind(data: &[u8], hints: &[u32], to: u32) -> Outcome {
    let Some(save) = Save::parse(data) else {
        return Outcome::NotThisFormat;
    };
    if to == 0 {
        return Outcome::Refused("tài khoản đích không hợp lệ".into());
    }
    let (before, case) = save.ids();
    let from = match owner(&before, hints) {
        Ok(Some(a)) => a,
        Ok(None) => return Outcome::NoAccount,
        Err(why) => return Outcome::Refused(why),
    };
    if from == to {
        return Outcome::AlreadyOwned;
    }

    // Thay đúng tại những chỗ đã tìm thấy — không tìm-và-thay lần hai — nên số
    // chỗ thay và danh sách dự tính bên dưới không thể lệch nhau.
    let mut per_entry: BTreeMap<usize, Vec<&Hit>> = BTreeMap::new();
    for h in before.iter().filter(|h| h.account == from) {
        per_entry.entry(h.entry).or_default().push(h);
    }
    let to_le = roots::steam_id64(to).to_le_bytes();
    let mut out = data.to_vec();
    let mut replaced = 0;
    for (&idx, hits) in &per_entry {
        let e = save.entries[idx];
        let mut plain = save.plain(e).into_owned();
        for h in hits {
            match h.form {
                Form::Le => plain[h.offset..h.offset + 8].copy_from_slice(&to_le),
                Form::Hex => {
                    let old = &plain[h.offset..h.offset + 16];
                    let upper = match letter_case(old) {
                        Some(upper) => upper,
                        None => case.prefers_upper(),
                    };
                    let text = hex_id(to, upper);
                    plain[h.offset..h.offset + 16].copy_from_slice(&text);
                }
            }
            replaced += 1;
        }
        let body = &mut out[e.off + MD5_LEN..e.off + e.size];
        match &save.key {
            // Cùng IV: IV là 16 byte đầu của thân mục và không bị đụng tới.
            Some(k) => {
                let (iv, cipher) = body.split_at_mut(BLOCK);
                encrypt_into(&k.aes, iv, &plain, cipher);
            }
            None => body.copy_from_slice(&plain),
        }
        let sum = Md5::digest(&out[e.off + MD5_LEN..e.off + e.size]);
        out[e.off..e.off + MD5_LEN].copy_from_slice(&sum);
    }

    // Đọc lại như một lần khôi phục sẽ đọc, và đòi khớp từng chỗ: cùng bộ khoá,
    // cùng số chỗ, cùng vị trí, chỉ khác là chủ cũ giờ là `to`.
    let Some(check) = Save::parse(&out) else {
        return Outcome::Refused("file sau khi sửa không còn đọc lại được".into());
    };
    let expected: Vec<Hit> = before
        .iter()
        .map(|h| Hit {
            account: if h.account == from { to } else { h.account },
            ..*h
        })
        .collect();
    let same_key = check.key.as_ref().map(|k| k.name) == save.key.as_ref().map(|k| k.name);
    if !same_key || check.ids().0 != expected {
        return Outcome::Refused(
            "sau khi sửa, các chỗ ghi tài khoản trong file không khớp với dự tính".into(),
        );
    }

    Outcome::Rebound(Rebound {
        bytes: out,
        from,
        replaced,
        entries: per_entry.len(),
        cipher: save.key.as_ref().map(|k| k.name),
    })
}

// ─────────────────────────────────────────────────────────────────────────
// Đọc BND4
// ─────────────────────────────────────────────────────────────────────────

/// Một mục: `off` trỏ vào MD5 mở đầu, `size` tính cả MD5.
#[derive(Debug, Clone, Copy)]
struct Entry {
    off: usize,
    size: usize,
}

struct Key {
    name: &'static str,
    aes: Aes128,
}

struct Save<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
    /// `None` khi các mục là bản rõ (Sekiro, Elden Ring).
    key: Option<Key>,
}

fn u32_at(data: &[u8], at: usize) -> Option<u64> {
    let b = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(b.try_into().ok()?) as u64)
}

fn u64_at(data: &[u8], at: usize) -> Option<u64> {
    let b = data.get(at..at.checked_add(8)?)?;
    Some(u64::from_le_bytes(b.try_into().ok()?))
}

impl<'a> Save<'a> {
    /// `None` = không phải file loại này. Mọi phép tính vị trí đều kiểm tràn:
    /// file tới từ đĩa và từ cloud, không được tin.
    fn parse(data: &'a [u8]) -> Option<Self> {
        if !looks_like(data) {
            return None;
        }
        let size = data.len() as u64;
        let count = u32_at(data, 0x0C)?;
        let header = u64_at(data, 0x10)?;
        let entry_header = u64_at(data, 0x20)?;
        if count == 0
            || count > 4096
            || entry_header < 0x18
            || header > size
            || entry_header > (size - header) / count
        {
            return None;
        }
        let mut entries = Vec::with_capacity(count as usize);
        for i in 0..count {
            let base = usize::try_from(header + i * entry_header).ok()?;
            let e_size = u64_at(data, base + 0x08)?;
            let e_off = u32_at(data, base + 0x10)?;
            if e_size < MD5_LEN as u64 || e_off > size || e_size > size - e_off {
                return None;
            }
            let (off, len) = (e_off as usize, e_size as usize);
            let sum = Md5::digest(&data[off + MD5_LEN..off + len]);
            if sum[..] != data[off..off + MD5_LEN] {
                return None;
            }
            entries.push(Entry { off, size: len });
        }
        let mut save = Save {
            data,
            entries,
            key: None,
        };
        save.key = save.pick_key();
        Some(save)
    }

    fn body(&self, e: Entry) -> &'a [u8] {
        &self.data[e.off + MD5_LEN..e.off + e.size]
    }

    /// Các mục có mã hoá không, và bằng khoá nào: khoá mà dưới nó **mọi** mục
    /// giải ra đều hợp lý. Chỉ cần giải khối đầu của mỗi mục để biết.
    fn pick_key(&self) -> Option<Key> {
        KEYS.iter().find_map(|(name, raw)| {
            let aes = Aes128::new(GenericArray::from_slice(raw));
            self.entries
                .iter()
                .all(|e| sane_when_decrypted(&aes, self.body(*e)))
                .then_some(Key { name, aes })
        })
    }

    fn plain(&self, e: Entry) -> Cow<'a, [u8]> {
        match &self.key {
            None => Cow::Borrowed(self.body(e)),
            // `pick_key` đã bảo đảm mọi thân mục có dạng IV + các khối 16 byte.
            Some(k) => Cow::Owned(decrypt(&k.aes, self.body(e))),
        }
    }

    /// Mọi giá trị có hình dạng SteamID64 trong bản rõ của mọi mục, theo thứ tự
    /// mục rồi vị trí, kèm kiểu chữ của các id viết bằng hex.
    fn ids(&self) -> (Vec<Hit>, HexCase) {
        let mut out = Vec::new();
        let mut case = HexCase::default();
        for (i, e) in self.entries.iter().enumerate() {
            find_ids(&self.plain(*e), i, &mut out, &mut case);
        }
        (out, case)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// AES-128-CBC không đệm
// ─────────────────────────────────────────────────────────────────────────

/// Thân mục có thể là IV + bản mã không: ít nhất một khối, chia hết cho 16.
fn cipher_shaped(body: &[u8]) -> bool {
    body.len() >= 2 * BLOCK && body.len() % BLOCK == 0
}

/// Bản rõ thật mở đầu bằng chính độ dài của nó, và phần sau độ dài đó chỉ là
/// đệm cho tròn khối. Chỉ cần khối đầu để kiểm.
fn sane_when_decrypted(aes: &Aes128, body: &[u8]) -> bool {
    if !cipher_shaped(body) {
        return false;
    }
    let mut first = GenericArray::clone_from_slice(&body[BLOCK..2 * BLOCK]);
    aes.decrypt_block(&mut first);
    let n = u32::from_le_bytes([
        first[0] ^ body[0],
        first[1] ^ body[1],
        first[2] ^ body[2],
        first[3] ^ body[3],
    ]) as usize;
    let room = body.len() - BLOCK - 4;
    n > 0 && n <= room && room - n < 2 * BLOCK
}

fn decrypt(aes: &Aes128, body: &[u8]) -> Vec<u8> {
    debug_assert!(cipher_shaped(body));
    let mut out = Vec::with_capacity(body.len() - BLOCK);
    let mut prev = &body[..BLOCK];
    for chunk in body[BLOCK..].chunks_exact(BLOCK) {
        let mut block = GenericArray::clone_from_slice(chunk);
        aes.decrypt_block(&mut block);
        out.extend(block.iter().zip(prev).map(|(a, b)| a ^ b));
        prev = chunk;
    }
    out
}

fn encrypt_into(aes: &Aes128, iv: &[u8], plain: &[u8], out: &mut [u8]) {
    debug_assert!(plain.len() == out.len() && plain.len() % BLOCK == 0);
    let mut prev = [0u8; BLOCK];
    prev.copy_from_slice(iv);
    for (src, dst) in plain.chunks_exact(BLOCK).zip(out.chunks_exact_mut(BLOCK)) {
        let mut block = GenericArray::clone_from_slice(src);
        for (b, p) in block.iter_mut().zip(prev) {
            *b ^= p;
        }
        aes.encrypt_block(&mut block);
        dst.copy_from_slice(&block);
        prev.copy_from_slice(&block);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tìm id và xác định chủ
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// SteamID64, 8 byte, byte thấp trước.
    Le,
    /// SteamID64 viết thành 16 ký tự hex.
    Hex,
}

/// Một chỗ trong bản rõ có giá trị hình dạng SteamID64.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hit {
    account: u32,
    form: Form,
    entry: usize,
    /// Vị trí trong bản rõ của mục.
    offset: usize,
}

/// Các id viết bằng hex trong file dùng chữ hoa hay chữ thường.
#[derive(Debug, Default, Clone, Copy)]
struct HexCase {
    upper: bool,
    lower: bool,
}

impl HexCase {
    /// Dùng cho id mà bản thân nó toàn chữ số nên không tự nói được kiểu chữ:
    /// theo các id khác trong file, không có gì để theo thì chữ thường (kiểu
    /// của tên thư mục save).
    fn prefers_upper(self) -> bool {
        self.upper && !self.lower
    }
}

/// `Some(true)` = có chữ hoa, `Some(false)` = có chữ thường, `None` = toàn số.
fn letter_case(hex: &[u8]) -> Option<bool> {
    hex.iter().find_map(|b| match b {
        b'A'..=b'F' => Some(true),
        b'a'..=b'f' => Some(false),
        _ => None,
    })
}

fn hex_id(account: u32, upper: bool) -> [u8; 16] {
    let text = if upper {
        format!("{:016X}", roots::steam_id64(account))
    } else {
        format!("{:016x}", roots::steam_id64(account))
    };
    let mut out = [0u8; 16];
    out.copy_from_slice(text.as_bytes());
    out
}

/// Vị trí kế tiếp của `pat` trong `hay`, tính từ `from`.
fn find(hay: &[u8], pat: &[u8], from: usize) -> Option<usize> {
    let last = hay.len().checked_sub(pat.len())?;
    let mut i = from;
    while i <= last {
        // Nhảy thẳng tới byte đầu kế tiếp thay vì so cả mẫu ở từng vị trí.
        i += hay[i..=last].iter().position(|&b| b == pat[0])?;
        if hay[i..i + pat.len()] == *pat {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_ids(plain: &[u8], entry: usize, out: &mut Vec<Hit>, case: &mut HexCase) {
    let mut at = 0;
    while let Some(i) = find(plain, &HIGH_LE, at) {
        if i >= 4 {
            let account = u32::from_le_bytes([plain[i - 4], plain[i - 3], plain[i - 2], plain[i - 1]]);
            if account != 0 {
                out.push(Hit {
                    account,
                    form: Form::Le,
                    entry,
                    offset: i - 4,
                });
            }
        }
        at = i + 1;
    }

    let mut at = 0;
    while let Some(i) = find(plain, HIGH_HEX, at) {
        at = i + 1;
        let Some(low) = plain.get(i + 8..i + 16) else {
            continue;
        };
        // Phải là đúng 16 ký tự hex, không phải một khúc của dãy hex dài hơn.
        let glued = (i > 0 && plain[i - 1].is_ascii_hexdigit())
            || plain.get(i + 16).is_some_and(u8::is_ascii_hexdigit);
        if glued || !low.iter().all(u8::is_ascii_hexdigit) {
            continue;
        }
        let account = std::str::from_utf8(low)
            .ok()
            .and_then(|t| u32::from_str_radix(t, 16).ok())
            .unwrap_or(0);
        if account == 0 {
            continue;
        }
        match letter_case(low) {
            Some(true) => case.upper = true,
            Some(false) => case.lower = true,
            None => {}
        }
        out.push(Hit {
            account,
            form: Form::Hex,
            entry,
            offset: i,
        });
    }
}

/// Save này của ai. `Ok(None)` = file không ghi tài khoản nào.
fn owner(hits: &[Hit], hints: &[u32]) -> Result<Option<u32>, String> {
    let all: BTreeSet<u32> = hits.iter().map(|h| h.account).collect();
    let text: BTreeSet<u32> = hits
        .iter()
        .filter(|h| h.form == Form::Hex)
        .map(|h| h.account)
        .collect();
    if let Some(a) = hints.iter().find(|a| all.contains(a)) {
        return Ok(Some(*a));
    }
    match (all.len(), text.len()) {
        (0, _) => Ok(None),
        (1, _) => Ok(all.first().copied()),
        (_, 1) => Ok(text.first().copied()),
        (n, _) => Err(format!(
            "file ghi {n} tài khoản và không xác định được tài khoản nào là chủ"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACC_A: u32 = 111_111_111;
    const ACC_B: u32 = 222_222_222;
    const OTHER_1: u32 = 0x0A1B_2C3D;
    const OTHER_2: u32 = 0x00AB_CDEF;

    fn le(account: u32) -> [u8; 8] {
        roots::steam_id64(account).to_le_bytes()
    }

    /// Dựng một BND4 có bố cục như save thật: header 0x40, mỗi mục một header
    /// 0x20, rồi tới các mục (MD5 + thân). `key` = chỉ số trong `KEYS` nếu các
    /// mục được mã hoá; khi đó mỗi bản rõ được bọc thành "độ dài + nội dung +
    /// đệm", đúng như game ghi.
    fn build(payloads: &[Vec<u8>], key: Option<usize>) -> Vec<u8> {
        let bodies: Vec<Vec<u8>> = payloads
            .iter()
            .enumerate()
            .map(|(i, p)| match key {
                None => p.clone(),
                Some(k) => {
                    let mut plain = (p.len() as u32).to_le_bytes().to_vec();
                    plain.extend_from_slice(p);
                    plain.resize(plain.len().div_ceil(BLOCK) * BLOCK, 0);
                    let iv = [i as u8 + 1; BLOCK];
                    let aes = Aes128::new(GenericArray::from_slice(&KEYS[k].1));
                    let mut body = iv.to_vec();
                    body.resize(BLOCK + plain.len(), 0);
                    let (iv, cipher) = body.split_at_mut(BLOCK);
                    encrypt_into(&aes, iv, &plain, cipher);
                    body
                }
            })
            .collect();

        let count = bodies.len();
        let mut out = vec![0u8; 0x40 + count * 0x20];
        out[..4].copy_from_slice(MAGIC);
        out[0x0C..0x10].copy_from_slice(&(count as u32).to_le_bytes());
        out[0x10..0x18].copy_from_slice(&0x40u64.to_le_bytes());
        out[0x20..0x28].copy_from_slice(&0x20u64.to_le_bytes());
        for (i, body) in bodies.iter().enumerate() {
            let base = 0x40 + i * 0x20;
            let off = out.len();
            out[base + 0x08..base + 0x10].copy_from_slice(&((body.len() + MD5_LEN) as u64).to_le_bytes());
            out[base + 0x10..base + 0x14].copy_from_slice(&(off as u32).to_le_bytes());
            out.extend_from_slice(&Md5::digest(body));
            out.extend_from_slice(body);
        }
        out
    }

    /// Nội dung một mục: `fill` lặp lại, với các mẩu `parts` chèn vào đúng vị trí.
    fn payload(len: usize, fill: u8, parts: &[(usize, &[u8])]) -> Vec<u8> {
        let mut v = vec![fill; len];
        for (at, bytes) in parts {
            v[*at..*at + bytes.len()].copy_from_slice(bytes);
        }
        v
    }

    fn rebound(o: Outcome) -> Rebound {
        match o {
            Outcome::Rebound(r) => r,
            other => panic!("phải sửa được, nhận {other:?}"),
        }
    }

    /// Số byte khác nhau giữa hai file cùng độ dài.
    fn diff(a: &[u8], b: &[u8]) -> usize {
        assert_eq!(a.len(), b.len());
        a.iter().zip(b).filter(|(x, y)| x != y).count()
    }

    /// Kiểu Sekiro / Elden Ring: không mã hoá, id của chủ ở dạng 8 byte, nằm
    /// trong một mục "hồ sơ" và trong mục của nhân vật.
    fn plain_save(account: u32) -> Vec<u8> {
        build(
            &[
                payload(0x300, 0x11, &[(0x40, &le(account))]),
                payload(0x200, 0x22, &[]),
                payload(0x180, 0x33, &[(0x34, &le(account)), (0x100, &le(account))]),
            ],
            None,
        )
    }

    #[test]
    fn plain_save_is_rebound_and_stays_valid() {
        let original = plain_save(ACC_A);
        let r = rebound(rebind(&original, &[], ACC_B));
        assert_eq!((r.from, r.replaced, r.entries, r.cipher), (ACC_A, 3, 2, None));
        assert_eq!(r.bytes.len(), original.len());
        // Khớp từng byte với một save do chính tài khoản B tạo ra.
        assert_eq!(r.bytes, plain_save(ACC_B));
        // Mục không ghi tài khoản thì không bị đụng tới, kể cả MD5 của nó.
        let untouched = 0x40 + 3 * 0x20 + MD5_LEN + 0x300;
        assert_eq!(
            original[untouched..untouched + MD5_LEN + 0x200],
            r.bytes[untouched..untouched + MD5_LEN + 0x200]
        );
    }

    #[test]
    fn rebinding_back_gives_the_original_bytes() {
        let original = plain_save(ACC_A);
        let there = rebound(rebind(&original, &[ACC_A], ACC_B)).bytes;
        let back = rebound(rebind(&there, &[ACC_B], ACC_A)).bytes;
        assert_eq!(back, original);
    }

    #[test]
    fn already_owned_and_no_account_change_nothing() {
        assert!(matches!(rebind(&plain_save(ACC_A), &[], ACC_A), Outcome::AlreadyOwned));
        // Kiểu Dark Souls Remastered: đúng định dạng, không có id bên trong.
        let none = build(&[payload(0x100, 0x44, &[]), payload(0x80, 0x55, &[])], None);
        assert!(matches!(rebind(&none, &[ACC_A], ACC_B), Outcome::NoAccount));
    }

    #[test]
    fn other_files_are_not_this_format() {
        assert!(matches!(rebind(b"", &[], ACC_B), Outcome::NotThisFormat));
        assert!(matches!(rebind(&[0u8; 4096], &[], ACC_B), Outcome::NotThisFormat));
        // Đúng chữ ký BND4 nhưng một mục không khớp MD5: file hỏng, hoặc BND4
        // kiểu khác — không đoán, không sửa.
        let mut bad = plain_save(ACC_A);
        let last = bad.len() - 1;
        bad[last] ^= 0xFF;
        assert!(matches!(rebind(&bad, &[ACC_A], ACC_B), Outcome::NotThisFormat));
    }

    #[test]
    fn truncated_or_garbled_headers_never_panic() {
        let good = plain_save(ACC_A);
        for cut in 0..good.len().min(0x140) {
            let _ = rebind(&good[..cut], &[ACC_A], ACC_B);
        }
        // Mỗi trường của header và của bảng mục lần lượt mang giá trị cực đại.
        for at in (0x0C..0x40 + 3 * 0x20).step_by(4) {
            let mut bad = good.clone();
            bad[at..at + 4].copy_from_slice(&[0xFF; 4]);
            let _ = rebind(&bad, &[ACC_A], ACC_B);
        }
    }

    #[test]
    fn account_zero_is_refused() {
        assert!(matches!(rebind(&plain_save(ACC_A), &[], 0), Outcome::Refused(_)));
    }

    /// Kiểu Dark Souls III: mã hoá; chủ ở dạng 8 byte, kèm id của những người
    /// chơi khác đã gặp ở dạng hex.
    fn ds3_like(owner: u32, key: usize) -> Vec<u8> {
        let p1 = hex_id(OTHER_1, false);
        let p2 = hex_id(OTHER_2, false);
        build(
            &[
                payload(0x260, 0x70, &[(0x21, &le(owner)), (0x90, &p1), (0x130, &p2)]),
                payload(0x123, 0x71, &[]),
                payload(0x0F1, 0x72, &[(0x08, &le(owner))]),
            ],
            Some(key),
        )
    }

    #[test]
    fn encrypted_save_is_rebound_with_the_same_iv() {
        for (key, (name, _)) in KEYS.iter().enumerate() {
            let original = ds3_like(ACC_A, key);
            let r = rebound(rebind(&original, &[ACC_A], ACC_B));
            assert_eq!((r.from, r.replaced, r.entries), (ACC_A, 2, 2));
            assert_eq!(r.cipher, Some(*name));
            // Cùng IV, cùng khoá → khớp từng byte với save do B tạo ra.
            assert_eq!(r.bytes, ds3_like(ACC_B, key), "khoá {name}");
            // Bản mã không hề chứa id ở dạng nhìn thấy được.
            assert!(find(&original, &le(ACC_A), 0).is_none());
        }
    }

    #[test]
    fn owner_needs_a_hint_when_the_file_names_several_accounts() {
        let save = ds3_like(ACC_A, 0);
        // Không có gợi ý: ba tài khoản, hai cái ở dạng chữ → không đoán.
        match rebind(&save, &[], ACC_B) {
            Outcome::Refused(why) => assert!(why.contains("3 tài khoản"), "{why}"),
            other => panic!("phải từ chối, nhận {other:?}"),
        }
        // Gợi ý không có trong file thì bị bỏ qua, gợi ý kế tiếp được dùng.
        let r = rebound(rebind(&save, &[987_654, ACC_A], ACC_B));
        assert_eq!(r.from, ACC_A);
    }

    #[test]
    fn target_already_named_as_another_player_still_works() {
        // B từng gặp A trong game: save của A có ghi B ở danh sách người chơi.
        let other = hex_id(ACC_B, false);
        let make = |owner: u32| {
            build(
                &[payload(0x200, 0x71, &[(0x10, &le(owner)), (0x80, &other)])],
                Some(0),
            )
        };
        let r = rebound(rebind(&make(ACC_A), &[ACC_A], ACC_B));
        assert_eq!(r.replaced, 1);
        assert_eq!(r.bytes, make(ACC_B));
    }

    /// Kiểu Dark Souls II: chủ ở dạng 16 ký tự hex; dữ liệu game tình cờ chứa
    /// nhiều chỗ có hình dạng SteamID64 8 byte.
    fn ds2_like(owner_hex: &[u8; 16]) -> Vec<u8> {
        let mut noise = payload(0x400, 0x00, &[]);
        for (n, at) in (0x20..0x3C0).step_by(0x40).enumerate() {
            noise[at..at + 4].copy_from_slice(&(0x0100_0000u32 + n as u32).to_le_bytes());
            noise[at + 4..at + 8].copy_from_slice(&HIGH_LE);
        }
        build(&[payload(0x100, 0x20, &[(0x39, owner_hex)]), noise], Some(1))
    }

    #[test]
    fn text_owner_is_found_among_false_binary_shapes_and_keeps_its_case() {
        for upper in [false, true] {
            let original = ds2_like(&hex_id(OTHER_2, upper));
            let r = rebound(rebind(&original, &[], OTHER_1));
            assert_eq!((r.from, r.replaced, r.entries), (OTHER_2, 1, 1));
            assert_eq!(r.bytes, ds2_like(&hex_id(OTHER_1, upper)), "chữ hoa = {upper}");
        }
    }

    #[test]
    fn an_all_digit_text_id_follows_the_case_of_the_others() {
        // Id của chủ viết ra toàn chữ số: tự nó không nói được hoa hay thường.
        let digits: u32 = 0x1234_5678;
        let target: u32 = 0x00AB_CDEF;
        for upper in [false, true] {
            let neighbour = hex_id(OTHER_1, upper);
            let make = |owner: u32| {
                build(
                    &[payload(
                        0x100,
                        0x20,
                        &[(0x10, &hex_id(owner, upper)), (0x60, &neighbour)],
                    )],
                    None,
                )
            };
            let r = rebound(rebind(&make(digits), &[digits], target));
            assert_eq!(r.bytes, make(target), "chữ hoa = {upper}");
        }
    }

    #[test]
    fn a_longer_hex_run_is_not_an_id() {
        let mut run = hex_id(ACC_A, false).to_vec();
        run.push(b'f');
        let save = build(&[payload(0x100, 0x20, &[(0x10, &run)])], None);
        assert!(matches!(rebind(&save, &[ACC_A], ACC_B), Outcome::NoAccount));
    }

    #[test]
    fn only_the_id_and_the_checksums_change_in_a_plain_save() {
        let original = plain_save(ACC_A);
        let r = rebound(rebind(&original, &[], ACC_B));
        // 3 chỗ × tối đa 4 byte thấp của id, cộng MD5 (16 byte) của 2 mục.
        assert!(diff(&original, &r.bytes) <= 3 * 4 + 2 * MD5_LEN);
    }
}
