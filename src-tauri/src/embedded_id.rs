//! Dò id tài khoản Steam nằm **bên trong nội dung** file save.
//!
//! Đổi đường dẫn và dán lại `steam_autocloud.vdf` là đủ với phần lớn game,
//! nhưng một số game còn ghi id tài khoản vào chính file save và từ chối đọc
//! khi id đó khác tài khoản đang chơi. Đo trên máy thật: `S0000.sl2` của Sekiro
//! mang SteamID64 của người chơi ở 4 chỗ, khôi phục sang tài khoản khác thì game
//! báo không đọc được save dù file nằm đúng thư mục.
//!
//! Hàm ở đây **chỉ dò, không sửa**, và không biết gì về định dạng của game nào:
//! nó tìm đúng id của một tài khoản đã biết trong dãy byte, ở các cách ghi mà
//! game hay dùng. Sửa thì khác — thay id xong thường phải tính lại mã kiểm, mà
//! mã kiểm là chuyện riêng của từng định dạng — nên việc đó không nằm ở đây.
//! Định dạng app biết sửa (BND4 của FromSoftware) nằm ở `bnd4`; file được sửa ở
//! đó thì không còn mang id của tài khoản nguồn và không bị báo ở đây nữa.
//!
//! Giới hạn cần nhớ khi đọc kết quả: file mã hoá hoặc nén thì id nằm trong đó
//! nhưng không lộ ra dưới dạng nào bên dưới. "Không thấy" nghĩa là không thấy,
//! không phải là chắc chắn không có.

use serde::{Deserialize, Serialize};

use crate::steam::roots;

/// Id được ghi theo cách nào.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Form {
    /// SteamID64, số 8 byte, byte thấp trước — cách phổ biến nhất trên PC.
    Id64Le,
    /// SteamID64, số 8 byte, byte cao trước.
    Id64Be,
    /// SteamID64 viết thành 17 chữ số (JSON, XML, INI…).
    Id64Text,
    /// Như trên nhưng UTF-16.
    Id64Utf16,
    /// Account id 32-bit viết thành chữ số, không dính chữ số nào hai bên.
    AccountText,
}

/// Một cách ghi tìm thấy trong file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub form: Form,
    pub count: usize,
    /// Vị trí lần đầu, để người đọc log mở file ra đối chiếu được.
    pub first_offset: u64,
}

/// Account id ngắn hơn ngần này chữ số thì không dò dạng chữ: dãy quá ngắn dễ
/// trùng với số liệu bình thường của game.
const MIN_ACCOUNT_TEXT_DIGITS: usize = 7;

/// Mọi chỗ `bytes` có ghi id của `account_id`. Rỗng = không thấy.
///
/// Không dò account id 32-bit ở dạng nhị phân: 4 byte trùng ngẫu nhiên là
/// chuyện thường trong file vài MB. 8 byte trùng đúng một SteamID64 thì không.
pub fn scan(bytes: &[u8], account_id: u32) -> Vec<Hit> {
    if account_id == 0 || bytes.is_empty() {
        return Vec::new();
    }
    let id64 = roots::steam_id64(account_id);
    let text64 = id64.to_string();
    let utf16: Vec<u8> = text64.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let text32 = account_id.to_string();

    let mut hits = Vec::new();
    let mut push = |form: Form, found: (usize, Option<usize>)| {
        if let (count, Some(first)) = found {
            hits.push(Hit {
                form,
                count,
                first_offset: first as u64,
            });
        }
    };
    push(Form::Id64Le, count(bytes, &id64.to_le_bytes(), false));
    push(Form::Id64Be, count(bytes, &id64.to_be_bytes(), false));
    push(Form::Id64Text, count(bytes, text64.as_bytes(), true));
    push(Form::Id64Utf16, count(bytes, &utf16, false));
    if text32.len() >= MIN_ACCOUNT_TEXT_DIGITS {
        push(Form::AccountText, count(bytes, text32.as_bytes(), true));
    }
    hits
}

/// Tổng số chỗ, cho câu cảnh báo.
pub fn total(hits: &[Hit]) -> usize {
    hits.iter().map(|h| h.count).sum()
}

/// Số lần `pat` xuất hiện và vị trí lần đầu. `whole_number`: bỏ qua lần nào có
/// chữ số ASCII dính ngay trước hoặc sau, tức `pat` chỉ là một khúc của số khác.
fn count(hay: &[u8], pat: &[u8], whole_number: bool) -> (usize, Option<usize>) {
    let (mut n, mut first) = (0, None);
    if pat.is_empty() || hay.len() < pat.len() {
        return (n, first);
    }
    let last_start = hay.len() - pat.len();
    let mut i = 0;
    while i <= last_start {
        // Nhảy thẳng tới byte đầu kế tiếp: file save phần lớn là byte 0, nên
        // gần như không phải so cả mẫu.
        let Some(step) = hay[i..=last_start].iter().position(|&b| b == pat[0]) else {
            break;
        };
        i += step;
        if hay[i..i + pat.len()] == *pat {
            let before = i > 0 && hay[i - 1].is_ascii_digit();
            let after = hay.get(i + pat.len()).is_some_and(u8::is_ascii_digit);
            if !(whole_number && (before || after)) {
                n += 1;
                first.get_or_insert(i);
            }
        }
        i += 1;
    }
    (n, first)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACC: u32 = 111_111_111;
    const OTHER: u32 = 222_222_222;

    fn forms(hits: &[Hit]) -> Vec<Form> {
        hits.iter().map(|h| h.form).collect()
    }

    #[test]
    fn finds_binary_id_in_both_byte_orders() {
        let id = roots::steam_id64(ACC);
        let mut data = vec![0u8; 64];
        data[10..18].copy_from_slice(&id.to_le_bytes());
        data[40..48].copy_from_slice(&id.to_le_bytes());
        data.extend_from_slice(&id.to_be_bytes());
        let hits = scan(&data, ACC);
        assert_eq!(forms(&hits), vec![Form::Id64Le, Form::Id64Be]);
        assert_eq!((hits[0].count, hits[0].first_offset), (2, 10));
        assert_eq!((hits[1].count, hits[1].first_offset), (1, 64));
        assert_eq!(total(&hits), 3);
    }

    #[test]
    fn finds_text_ids() {
        let id = roots::steam_id64(ACC).to_string();
        let json = format!(r#"{{"owner":"{id}","account":{ACC}}}"#);
        assert_eq!(
            forms(&scan(json.as_bytes(), ACC)),
            vec![Form::Id64Text, Form::AccountText]
        );
        let wide: Vec<u8> = id.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(forms(&scan(&wide, ACC)), vec![Form::Id64Utf16]);
    }

    #[test]
    fn another_accounts_id_is_not_a_hit() {
        let mut data = vec![0u8; 32];
        data[8..16].copy_from_slice(&roots::steam_id64(OTHER).to_le_bytes());
        data.extend_from_slice(roots::steam_id64(OTHER).to_string().as_bytes());
        assert!(scan(&data, ACC).is_empty());
    }

    #[test]
    fn a_number_that_only_contains_the_id_is_not_a_hit() {
        // Tên save kiểu `farm_1111111119` hay một timestamp dài: có chứa dãy số
        // của account id nhưng không phải nó.
        assert!(scan(format!("seed={ACC}9").as_bytes(), ACC).is_empty());
        assert!(scan(format!("seed=7{ACC}").as_bytes(), ACC).is_empty());
        assert!(scan(format!("x{}0", roots::steam_id64(ACC)).as_bytes(), ACC).is_empty());
    }

    #[test]
    fn short_account_ids_are_only_matched_as_id64() {
        // Account id 22202 viết thành chữ thì trùng với đủ thứ số khác.
        let hits = scan(b"level 22202 gold 22202", 22_202);
        assert!(hits.is_empty(), "{hits:?}");
        let id = roots::steam_id64(22_202);
        assert_eq!(forms(&scan(&id.to_le_bytes(), 22_202)), vec![Form::Id64Le]);
    }

    #[test]
    fn empty_input_and_account_zero_find_nothing() {
        assert!(scan(b"", ACC).is_empty());
        assert!(scan(&roots::steam_id64(0).to_le_bytes(), 0).is_empty());
    }

    #[test]
    fn pattern_at_the_very_end_is_found() {
        let id = roots::steam_id64(ACC).to_le_bytes();
        let mut data = vec![id[0]; 5]; // nhiều byte đầu giả ngay trước mẫu thật
        data.extend_from_slice(&id);
        let hits = scan(&data, ACC);
        assert_eq!((hits[0].count, hits[0].first_offset), (1, 5));
    }
}
