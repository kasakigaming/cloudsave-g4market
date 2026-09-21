//! Ảnh nền lấy từ web cho chế độ xoay vòng — nguồn không giới hạn.
//!
//! Nguồn: các danh mục ảnh tuyển chọn về phong cảnh trên Wikimedia Commons
//! (hàng trăm ảnh "Featured" / "Quality", giấy phép tự do như CC BY-SA). API
//! công khai, không cần khoá. Giấy phép đòi ghi tác giả, nên mỗi ảnh trả về kèm
//! tác giả + giấy phép + trang gốc để giao diện hiện dòng ghi công.
//!
//! Mỗi lần gọi lấy một loạt ảnh từ một danh mục và một điểm bắt đầu ngẫu nhiên,
//! lọc ảnh ngang đủ nét, xáo trộn — giao diện gọi tiếp khi sắp hết.

use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};

const API: &str = "https://commons.wikimedia.org/w/api.php";
/// Wikimedia yêu cầu User-Agent mô tả được ứng dụng và cách liên hệ.
const USER_AGENT: &str = concat!(
    "CloudSaveG4Market/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/kasakigaming/cloudsave-g4market)"
);
const CATEGORIES: &[&str] = &[
    "Category:Featured_pictures_of_landscapes",
    "Category:Featured_pictures_of_mountains",
    "Category:Quality_images_of_mountains",
];
/// Chiều rộng ảnh xin về (Wikimedia tự thu nhỏ từ ảnh gốc).
const WIDTH: u32 = 2560;

#[derive(Debug, Clone, Serialize)]
pub struct WebPhoto {
    pub url: String,
    pub author: String,
    pub license: String,
    /// Trang của ảnh trên Wikimedia Commons.
    pub page: String,
}

/// Một loạt ảnh ngẫu nhiên (thường 15–40 ảnh sau khi lọc).
pub async fn fetch_batch() -> Result<Vec<WebPhoto>> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| Error::Other(e.to_string()))?;

    let seed = uuid::Uuid::new_v4().as_u128();
    let category = CATEGORIES[(seed % CATEGORIES.len() as u128) as usize];
    // Nhảy tới một chữ cái ngẫu nhiên trong danh mục để mỗi loạt một khác.
    let prefixes = b"ABCDEFGHIJKLMNOPRSTUVW0123456789";
    let prefix = (prefixes[((seed >> 8) % prefixes.len() as u128) as usize] as char).to_string();
    let width = WIDTH.to_string();

    let query = [
        ("action", "query"),
        ("format", "json"),
        ("formatversion", "2"),
        ("generator", "categorymembers"),
        ("gcmtitle", category),
        ("gcmtype", "file"),
        ("gcmlimit", "50"),
        ("gcmstartsortkeyprefix", prefix.as_str()),
        ("prop", "imageinfo"),
        ("iiprop", "url|size|extmetadata"),
        ("iiurlwidth", width.as_str()),
        ("iiextmetadatafilter", "Artist|LicenseShortName"),
    ];
    let resp = client
        .get(API)
        .query(&query)
        .send()
        .await
        .map_err(|e| Error::Other(format!("không tải được danh sách ảnh: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!("Wikimedia trả về {}", resp.status())));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| Error::Other(format!("danh sách ảnh hỏng: {e}")))?;

    let mut out = parse(&body);
    shuffle(&mut out, seed);
    Ok(out)
}

fn parse(body: &Value) -> Vec<WebPhoto> {
    let Some(pages) = body.pointer("/query/pages").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    pages
        .iter()
        .filter_map(|p| {
            let ii = p.pointer("/imageinfo/0")?;
            let (w, h) = (ii.get("width")?.as_f64()?, ii.get("height")?.as_f64()?);
            // Ảnh ngang, không quá dài (panorama bị cắt mất gần hết), đủ nét.
            let ratio = w / h;
            if !(1.3..=2.2).contains(&ratio) || w < 1920.0 {
                return None;
            }
            let url = ii.get("thumburl").or_else(|| ii.get("url"))?.as_str()?.to_string();
            if !url.starts_with("https://") {
                return None;
            }
            let meta = ii.get("extmetadata");
            let field = |k: &str| {
                meta.and_then(|m| m.pointer(&format!("/{k}/value")))
                    .and_then(|v| v.as_str())
                    .map(strip_html)
                    .unwrap_or_default()
            };
            Some(WebPhoto {
                url,
                author: field("Artist"),
                license: field("LicenseShortName"),
                page: ii
                    .get("descriptionurl")
                    .and_then(|v| v.as_str())
                    .unwrap_or("https://commons.wikimedia.org")
                    .to_string(),
            })
        })
        .collect()
}

/// Tên tác giả trong extmetadata thường là HTML (`<a href=…>Tên</a>`).
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&nbsp;", " ");
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > 60 {
        out.chars().take(57).collect::<String>() + "…"
    } else {
        out
    }
}

/// Xáo trộn không cần thêm crate `rand`.
fn shuffle<T>(v: &mut [T], mut seed: u128) {
    for i in (1..v.len()).rev() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let j = ((seed >> 64) % (i as u128 + 1)) as usize;
        v.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_wide_sharp_photos_with_credit() {
        let body: Value = serde_json::from_str(
            r#"{"query":{"pages":[
              {"title":"File:ok.jpg","imageinfo":[{"width":4000,"height":2250,
                "thumburl":"https://upload.wikimedia.org/a/2560px-ok.jpg","descriptionurl":"https://commons.wikimedia.org/wiki/File:ok.jpg",
                "extmetadata":{"Artist":{"value":"<a href=\"x\">Nguyễn &amp; Co</a>"},"LicenseShortName":{"value":"CC BY-SA 4.0"}}}]},
              {"title":"File:pano.jpg","imageinfo":[{"width":13000,"height":1486,"thumburl":"https://upload.wikimedia.org/b.jpg"}]},
              {"title":"File:portrait.jpg","imageinfo":[{"width":2000,"height":3000,"thumburl":"https://upload.wikimedia.org/c.jpg"}]},
              {"title":"File:small.jpg","imageinfo":[{"width":1200,"height":800,"thumburl":"https://upload.wikimedia.org/d.jpg"}]}
            ]}}"#,
        )
        .unwrap();
        let v = parse(&body);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].author, "Nguyễn & Co");
        assert_eq!(v[0].license, "CC BY-SA 4.0");
        assert!(v[0].page.ends_with("File:ok.jpg"));
    }

    #[test]
    fn shuffle_keeps_every_item() {
        let mut v: Vec<u32> = (0..50).collect();
        shuffle(&mut v, 12345);
        let mut s = v.clone();
        s.sort();
        assert_eq!(s, (0..50).collect::<Vec<_>>());
        assert_ne!(v, s, "thứ tự đã đổi");
    }

    /// Gọi Wikimedia thật — chạy tay: `cargo test --lib web_backgrounds -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn fetches_real_batch() {
        let v = fetch_batch().await.unwrap();
        assert!(!v.is_empty());
        for p in v.iter().take(3) {
            println!("{} | {} | {}", p.url, p.author, p.license);
        }
    }
}
