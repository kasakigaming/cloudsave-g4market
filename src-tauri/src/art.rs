//! Ảnh game: icon vuông, bìa đứng, ảnh ngang.
//!
//! Thứ tự nguồn:
//!
//! 1. **Cache của Steam trên đĩa** — `<steam>/appcache/librarycache/<appid>/`.
//!    Tức thì, không cần mạng, có cho mọi game từng hiện trong thư viện Steam.
//!    Hai kiểu thư mục (tuỳ đời client): file nằm thẳng trong `<appid>/`
//!    (`header.jpg`, `library_600x900.jpg`, `<hash>.jpg` = icon 32×32), hoặc
//!    trong một thư mục con tên hash (`library_header.jpg`,
//!    `library_capsule.jpg`).
//! 2. **Ảnh đã tải về trước đó** — `%LOCALAPPDATA%\cloudsave-g4market\art\<appid>\`.
//! 3. **CDN của Steam** cho phần còn thiếu (game chỉ có trên cloud, máy mới).
//!    `appinfo.vdf` ghi sẵn đường dẫn chính xác (có hash) của bìa và ảnh ngang
//!    — game mới không có ảnh ở đường dẫn "đoán được". Không có thì thử đường
//!    dẫn kiểu cũ, rồi `store/api/appdetails`. Tải một lần rồi giữ; ảnh không
//!    có thì ghi nhớ, 7 ngày sau mới hỏi lại.
//!
//! Icon: icon JPEG trong cache Steam chỉ 32×32 — phóng lên là mờ. File `.ico`
//! của game (`steam/games/<clienticon>.ico` khi đã cài, hoặc trên CDN) thường
//! có khung 256×256 dạng PNG; tách khung đó ra làm `icon_large`. Icon 32px chỉ
//! dùng cho ô nhỏ trong danh sách.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::error::{Error, Result};

/// Không hỏi lại ảnh đã biết là không có trong khoảng này.
const RETRY_MISSING: Duration = Duration::from_secs(7 * 24 * 3600);
/// Ảnh lớn hơn thế này là bất thường (bìa 600×900 cỡ 100–300 KB).
const MAX_IMAGE: usize = 5 * 1024 * 1024;

/// Đường dẫn file ảnh trên máy; giao diện đổi sang URL bằng `convertFileSrc`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GameArt {
    /// Icon vuông nhỏ (thường 32×32) — chỉ đủ nét cho ô nhỏ.
    pub icon: Option<PathBuf>,
    /// Icon vuông lớn (≥ 64px, thường 256) tách từ `.ico` của game.
    pub icon_large: Option<PathBuf>,
    /// Bìa đứng 600×900.
    pub cover: Option<PathBuf>,
    /// Ảnh ngang 460×215.
    pub header: Option<PathBuf>,
    /// Tên game lấy từ API store, chỉ có khi Steam trên máy không biết tên.
    pub name: Option<String>,
}

impl GameArt {
    fn complete(&self) -> bool {
        self.icon_large.is_some() && self.cover.is_some() && self.header.is_some()
    }

    /// Lấy phần còn thiếu từ `other`.
    fn fill_from(&mut self, other: GameArt) {
        self.icon = self.icon.take().or(other.icon);
        self.icon_large = self.icon_large.take().or(other.icon_large);
        self.cover = self.cover.take().or(other.cover);
        self.header = self.header.take().or(other.header);
        self.name = self.name.take().or(other.name);
    }
}

/// Tên tạm khi không biết tên game: `App 1955830`.
pub fn is_placeholder(title: &str) -> bool {
    title
        .strip_prefix("App ")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Tên game đã lấy từ API store trước đó.
pub fn cached_name(app_id: u32) -> Option<String> {
    let s = std::fs::read_to_string(cache_dir().join(app_id.to_string()).join(Kind::Name.file())).ok()?;
    let s = s.trim();
    (!s.is_empty() && s.chars().count() <= 200).then(|| s.to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Icon,
    IconLarge,
    Cover,
    Header,
    Name,
}

impl Kind {
    fn file(self) -> &'static str {
        match self {
            Kind::Icon => "icon.jpg",
            Kind::IconLarge => "icon.png",
            Kind::Cover => "cover.jpg",
            Kind::Header => "header.jpg",
            Kind::Name => "name.txt",
        }
    }
}

/// Gợi ý từ `appinfo.vdf` để tải đúng ảnh.
#[derive(Debug, Clone, Default)]
pub struct Hints {
    /// `common.icon` — tên icon JPEG 32px trên CDN community.
    pub icon: Option<String>,
    /// `common.clienticon` — tên file `.ico` (thường có khung 256px).
    pub clienticon: Option<String>,
    /// Đường dẫn bìa trên CDN store, vd `<hash>/library_capsule.jpg`.
    pub cover: Option<String>,
    /// Đường dẫn ảnh ngang trên CDN store, vd `<hash>/header.jpg`.
    pub header: Option<String>,
    /// Steam trên máy và danh sách cộng đồng đều không biết tên game: hỏi
    /// API store.
    pub need_name: bool,
}

fn is_hex40(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Đường dẫn tương đối an toàn để ghép vào URL CDN: `[hash/]tên.jpg|png`.
fn safe_cdn_path(p: &str) -> bool {
    let parts: Vec<&str> = p.split('/').collect();
    let name_ok = |n: &str| {
        !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
            && (n.ends_with(".jpg") || n.ends_with(".png"))
            && !n.starts_with('.')
    };
    match parts.as_slice() {
        [name] => name_ok(name),
        [hash, name] => is_hex40(hash) && name_ok(name),
        _ => false,
    }
}

/// Khung PNG lớn nhất (≥ 64px) trong một file `.ico`. Khung dạng BMP thì bỏ
/// qua — dựng lại PNG từ BMP cần thư viện ảnh, mà khung 256px gần như luôn là
/// PNG.
pub fn largest_png_in_ico(b: &[u8]) -> Option<Vec<u8>> {
    let u16le = |o: usize| b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]]));
    let u32le = |o: usize| b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]));
    if u16le(0)? != 0 || u16le(2)? != 1 {
        return None;
    }
    let n = u16le(4)? as usize;
    let mut best: Option<(u32, &[u8])> = None;
    for i in 0..n.min(64) {
        let e = 6 + 16 * i;
        let w = match *b.get(e)? {
            0 => 256,
            x => x as u32,
        };
        let size = u32le(e + 8)? as usize;
        let off = u32le(e + 12)? as usize;
        let data = b.get(off..off.checked_add(size)?)?;
        if w >= 64 && data.starts_with(b"\x89PNG\r\n\x1a\n") && best.map_or(true, |(bw, _)| w > bw) {
            best = Some((w, data));
        }
    }
    best.map(|(_, d)| d.to_vec())
}

pub fn cache_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("cloudsave-g4market")
        .join("art")
}

pub fn steam_cache_dir(steam_root: &Path) -> PathBuf {
    steam_root.join("appcache").join("librarycache")
}

// ─────────────────────────────────────────────────────────────────────────
// Ảnh có sẵn trên đĩa
// ─────────────────────────────────────────────────────────────────────────

fn is_hash_icon(name: &str) -> bool {
    name.len() == 44 && name.ends_with(".jpg") && name[..40].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Ảnh Steam đã cache cho một game. Có nhiều bản (đổi ảnh theo mùa sale…) thì
/// lấy bản mới nhất.
pub fn from_steam_cache(steam_root: &Path, app_id: u32) -> GameArt {
    let dir = steam_cache_dir(steam_root).join(app_id.to_string());
    let mut files: Vec<(String, PathBuf, SystemTime, bool)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return GameArt::default();
    };
    for e in entries.flatten() {
        let path = e.path();
        let Ok(meta) = e.metadata() else { continue };
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if meta.is_dir() {
            for f in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                if let (Some(name), Ok(m)) = (f.file_name().to_str().map(str::to_owned), f.metadata()) {
                    if m.is_file() && m.len() > 0 {
                        files.push((name, f.path(), m.modified().unwrap_or(mtime), false));
                    }
                }
            }
        } else if let Some(name) = e.file_name().to_str() {
            if meta.len() > 0 {
                files.push((name.to_owned(), path, mtime, true));
            }
        }
    }
    let newest = |names: &[&str]| {
        files
            .iter()
            .filter(|(n, ..)| names.contains(&n.as_str()))
            .max_by_key(|(_, _, t, _)| *t)
            .map(|(_, p, ..)| p.clone())
    };
    GameArt {
        icon: files
            .iter()
            .filter(|(n, _, _, top)| *top && is_hash_icon(n))
            .max_by_key(|(_, _, t, _)| *t)
            .map(|(_, p, ..)| p.clone()),
        icon_large: None,
        cover: newest(&["library_600x900.jpg", "library_capsule.jpg"]),
        header: newest(&["header.jpg", "library_header.jpg"]),
        name: None,
    }
}

/// Ảnh app đã tải về trước đó.
fn from_download_cache(dir: &Path, app_id: u32) -> GameArt {
    let d = dir.join(app_id.to_string());
    let have = |k: Kind| {
        let p = d.join(k.file());
        p.is_file().then_some(p)
    };
    GameArt {
        icon: have(Kind::Icon),
        icon_large: have(Kind::IconLarge),
        cover: have(Kind::Cover),
        header: have(Kind::Header),
        name: cached_name(app_id),
    }
}

/// Mọi ảnh có sẵn trên máy, không chạm mạng.
pub fn local(steam_root: Option<&Path>, app_id: u32) -> GameArt {
    let mut art = steam_root.map(|r| from_steam_cache(r, app_id)).unwrap_or_default();
    art.fill_from(from_download_cache(&cache_dir(), app_id));
    art
}

// ─────────────────────────────────────────────────────────────────────────
// Tải phần còn thiếu
// ─────────────────────────────────────────────────────────────────────────

/// Ghi nhớ ảnh không có trên Steam, để không hỏi lại liên tục.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Missing {
    #[serde(default)]
    kinds: Vec<Kind>,
    #[serde(default)]
    checked_at: u64,
    /// Cách tìm ảnh đổi (thêm nguồn mới) thì tăng số này: ghi nhớ "không có"
    /// của cách cũ không còn đúng.
    #[serde(default)]
    v: u32,
}

const MISSING_V: u32 = 2;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn read_missing(dir: &Path) -> Missing {
    std::fs::read(dir.join("missing.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Có đáng hỏi lại loại ảnh này không.
fn should_try(m: &Missing, k: Kind, now: u64) -> bool {
    m.v < MISSING_V || !m.kinds.contains(&k) || now.saturating_sub(m.checked_at) >= RETRY_MISSING.as_secs()
}

/// Byte đầu đúng là JPEG / PNG / WebP — chặn trang lỗi HTML trả về mã 200.
fn looks_like_image(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0xD8, 0xFF])
        || b.starts_with(b"\x89PNG\r\n\x1a\n")
        || (b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP")
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("CloudSaveG4Market/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| Error::Other(e.to_string()))
}

/// Tải một ảnh; `None` nếu không có (404, không phải ảnh, quá lớn).
async fn get_image(c: &reqwest::Client, url: &str) -> Option<Vec<u8>> {
    let resp = c.get(url).send().await.ok()?;
    if !resp.status().is_success() || resp.content_length().is_some_and(|n| n as usize > MAX_IMAGE) {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    (bytes.len() <= MAX_IMAGE && looks_like_image(&bytes)).then(|| bytes.to_vec())
}

/// Link ảnh ngang chính xác từ API store (game mới có đường dẫn kèm hash nên
/// đoán URL không ra).
async fn store_header_url(c: &reqwest::Client, app_id: u32) -> Option<String> {
    let url = format!("https://store.steampowered.com/api/appdetails?appids={app_id}&filters=basic");
    let v: serde_json::Value = c.get(url).send().await.ok()?.json().await.ok()?;
    let h = v.pointer(&format!("/{app_id}/data/header_image"))?.as_str()?;
    h.starts_with("https://").then(|| h.to_owned())
}

/// Tên game từ API store.
async fn store_name(c: &reqwest::Client, app_id: u32) -> Option<String> {
    let url = format!("https://store.steampowered.com/api/appdetails?appids={app_id}&filters=basic");
    let v: serde_json::Value = c.get(url).send().await.ok()?.json().await.ok()?;
    let n = v.pointer(&format!("/{app_id}/data/name"))?.as_str()?.trim();
    (!n.is_empty() && n.chars().count() <= 200).then(|| n.to_owned())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Lấy bytes `.ico` của game: trên máy (game đã cài) hoặc từ CDN.
async fn clienticon_bytes(c: &reqwest::Client, steam_root: Option<&Path>, app_id: u32, h: &str) -> Option<Vec<u8>> {
    if !is_hex40(h) {
        return None;
    }
    if let Some(root) = steam_root {
        if let Ok(b) = std::fs::read(root.join("steam").join("games").join(format!("{h}.ico"))) {
            return Some(b);
        }
    }
    let url = format!("https://cdn.cloudflare.steamstatic.com/steamcommunity/public/images/apps/{app_id}/{h}.ico");
    let resp = c.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let b = resp.bytes().await.ok()?;
    (b.len() <= MAX_IMAGE).then(|| b.to_vec())
}

/// Tải những loại ảnh `have` còn thiếu. Trả về ảnh đã có sau khi tải.
pub async fn fetch_missing(
    app_id: u32,
    steam_root: Option<&Path>,
    hints: &Hints,
    have: &GameArt,
) -> Result<GameArt> {
    let dir = cache_dir().join(app_id.to_string());
    std::fs::create_dir_all(&dir)?;
    let missing = read_missing(&dir);
    let now = now_secs();
    let c = client()?;
    let mut still_missing = Vec::new();
    let base = format!("https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/{app_id}");
    // Đường dẫn chính xác từ appinfo trước, rồi đường dẫn kiểu cũ.
    let urls = |hint: &Option<String>, fallback: &str| {
        let mut v = Vec::new();
        if let Some(h) = hint.as_deref().filter(|h| safe_cdn_path(h)) {
            v.push(format!("{base}/{h}"));
        }
        v.push(format!("{base}/{fallback}"));
        v
    };

    let mut want = Vec::new();
    if have.cover.is_none() {
        want.push(Kind::Cover);
    }
    if have.header.is_none() {
        want.push(Kind::Header);
    }
    if have.icon_large.is_none() {
        want.push(Kind::IconLarge);
    }
    if have.icon.is_none() {
        want.push(Kind::Icon);
    }
    if hints.need_name && have.name.is_none() {
        want.push(Kind::Name);
    }
    for k in want {
        if !should_try(&missing, k, now) {
            still_missing.push(k);
            continue;
        }
        let bytes = match k {
            Kind::Cover => {
                let mut got = None;
                for u in urls(&hints.cover, "library_600x900.jpg") {
                    got = get_image(&c, &u).await;
                    if got.is_some() {
                        break;
                    }
                }
                got
            }
            Kind::Header => {
                let mut got = None;
                for u in urls(&hints.header, "header.jpg") {
                    got = get_image(&c, &u).await;
                    if got.is_some() {
                        break;
                    }
                }
                if got.is_none() {
                    if let Some(u) = store_header_url(&c, app_id).await {
                        got = get_image(&c, &u).await;
                    }
                }
                got
            }
            Kind::IconLarge => match &hints.clienticon {
                Some(h) => clienticon_bytes(&c, steam_root, app_id, h)
                    .await
                    .and_then(|b| largest_png_in_ico(&b)),
                None => None,
            },
            Kind::Name => store_name(&c, app_id).await.map(String::into_bytes),
            Kind::Icon => match hints.icon.as_deref().filter(|h| is_hex40(h)) {
                Some(h) => {
                    get_image(
                        &c,
                        &format!("https://cdn.cloudflare.steamstatic.com/steamcommunity/public/images/apps/{app_id}/{h}.jpg"),
                    )
                    .await
                }
                None => None,
            },
        };
        match bytes {
            Some(b) => write_atomic(&dir.join(k.file()), &b)?,
            None => still_missing.push(k),
        }
    }
    let m = Missing {
        kinds: still_missing,
        checked_at: now,
        v: MISSING_V,
    };
    let _ = std::fs::write(dir.join("missing.json"), serde_json::to_vec(&m).unwrap_or_default());

    let mut art = have.clone();
    art.fill_from(from_download_cache(&cache_dir(), app_id));
    Ok(art)
}

/// Game đang được tải ảnh — không tải trùng khi giao diện hỏi lại.
static INFLIGHT: Mutex<Option<HashSet<u32>>> = Mutex::new(None);
/// Tối đa 4 game tải cùng lúc — đủ nhanh, không dội API của Steam.
static LIMIT: Semaphore = Semaphore::const_new(4);

/// Nhận một game vào hàng tải; `false` nếu nó đang được tải rồi.
pub fn claim(app_id: u32) -> bool {
    INFLIGHT.lock().unwrap().get_or_insert_with(HashSet::new).insert(app_id)
}

pub fn release(app_id: u32) {
    if let Some(s) = INFLIGHT.lock().unwrap().as_mut() {
        s.remove(&app_id);
    }
}

/// Tải ảnh còn thiếu trong nền, mỗi lúc tối đa 4 game.
pub async fn fetch_limited(
    app_id: u32,
    steam_root: Option<PathBuf>,
    hints: Hints,
    have: GameArt,
) -> Result<GameArt> {
    let _permit = LIMIT.acquire().await.map_err(|e| Error::Other(e.to_string()))?;
    fetch_missing(app_id, steam_root.as_deref(), &hints, &have).await
}

/// Có cần tải gì không (thiếu ảnh / tên và chưa ghi nhớ là không có).
pub fn needs_fetch(app_id: u32, art: &GameArt, need_name: bool) -> bool {
    let lack_name = need_name && art.name.is_none();
    if art.complete() && !lack_name {
        return false;
    }
    let m = read_missing(&cache_dir().join(app_id.to_string()));
    let now = now_secs();
    [
        (art.icon.is_none(), Kind::Icon),
        (art.icon_large.is_none(), Kind::IconLarge),
        (art.cover.is_none(), Kind::Cover),
        (art.header.is_none(), Kind::Header),
        (lack_name, Kind::Name),
    ]
    .into_iter()
    .any(|(lack, k)| lack && should_try(&m, k, now))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("cs-art-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, [0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
    }

    #[test]
    fn reads_old_steam_layout() {
        let root = tmp();
        let d = steam_cache_dir(&root).join("413150");
        touch(&d.join("header.jpg"));
        touch(&d.join("library_600x900.jpg"));
        touch(&d.join("35d1377200084a4034238c05b0c8930451e2fb40.jpg"));
        touch(&d.join("logo.png"));
        let a = from_steam_cache(&root, 413150);
        assert!(a.icon.unwrap().ends_with("35d1377200084a4034238c05b0c8930451e2fb40.jpg"));
        assert!(a.cover.unwrap().ends_with("library_600x900.jpg"));
        assert!(a.header.unwrap().ends_with("header.jpg"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn reads_new_steam_layout_in_hash_subfolder() {
        let root = tmp();
        let d = steam_cache_dir(&root).join("3489700").join("0a1b2c3d4e5f60718293a4b5c6d7e8f901234567");
        touch(&d.join("library_header.jpg"));
        touch(&d.join("library_capsule.jpg"));
        let a = from_steam_cache(&root, 3489700);
        assert!(a.icon.is_none(), "icon chỉ nằm ở cấp trên cùng");
        assert!(a.cover.unwrap().ends_with("library_capsule.jpg"));
        assert!(a.header.unwrap().ends_with("library_header.jpg"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unknown_game_has_no_art() {
        let root = tmp();
        assert_eq!(from_steam_cache(&root, 1), GameArt::default());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn only_real_images_are_kept() {
        assert!(looks_like_image(&[0xFF, 0xD8, 0xFF, 0xE0, 0]));
        assert!(looks_like_image(b"\x89PNG\r\n\x1a\nxxxx"));
        assert!(looks_like_image(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!looks_like_image(b"<!DOCTYPE html><html>"));
        assert!(!looks_like_image(b""));
    }

    #[test]
    fn missing_art_is_retried_only_after_a_week() {
        let m = Missing {
            kinds: vec![Kind::Cover],
            checked_at: 1_000_000,
            v: MISSING_V,
        };
        assert!(!should_try(&m, Kind::Cover, 1_000_000 + 3600));
        assert!(should_try(&m, Kind::Cover, 1_000_000 + RETRY_MISSING.as_secs()));
        assert!(should_try(&m, Kind::Header, 1_000_000 + 3600), "loại khác vẫn thử");
    }

    #[test]
    fn takes_largest_png_frame_from_ico() {
        // ICO tự dựng: khung 32px BMP, khung 128px PNG, khung 256px PNG.
        let png = |tag: u8| {
            let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
            v.push(tag);
            v
        };
        let frames: [(u8, Vec<u8>); 3] = [(32, vec![0x28, 0, 0, 0]), (128, png(1)), (0, png(2))];
        let mut b = vec![0, 0, 1, 0, frames.len() as u8, 0];
        let mut off = 6 + 16 * frames.len();
        let mut data = Vec::new();
        for (w, d) in &frames {
            b.extend_from_slice(&[*w, *w, 0, 0, 1, 0, 32, 0]);
            b.extend_from_slice(&(d.len() as u32).to_le_bytes());
            b.extend_from_slice(&(off as u32).to_le_bytes());
            off += d.len();
            data.extend_from_slice(d);
        }
        b.extend_from_slice(&data);
        assert_eq!(largest_png_in_ico(&b), Some(png(2)), "khung 256 (width 0) thắng");
        assert_eq!(largest_png_in_ico(b"not an ico"), None);
    }

    #[test]
    fn cdn_paths_from_appinfo_are_validated() {
        assert!(safe_cdn_path("library_600x900.jpg"));
        assert!(safe_cdn_path("f40c725261e949ef758d80bb067d09794635927e/library_capsule.jpg"));
        assert!(!safe_cdn_path("../x/library_capsule.jpg"));
        assert!(!safe_cdn_path("https://evil/x.jpg"));
        assert!(!safe_cdn_path("abc/library_capsule.jpg"), "thư mục phải là hash 40 ký tự");
        assert!(!safe_cdn_path("x.exe"));
    }

    #[test]
    fn old_missing_records_are_ignored_after_rules_change() {
        let old = Missing { kinds: vec![Kind::Cover], checked_at: 1_000_000, v: 0 };
        assert!(should_try(&old, Kind::Cover, 1_000_000 + 60));
    }

    #[test]
    fn placeholder_titles_are_recognised() {
        assert!(is_placeholder("App 1955830"));
        assert!(!is_placeholder("App"));
        assert!(!is_placeholder("App Store Tycoon"));
        assert!(!is_placeholder("Stardew Valley"));
    }

    #[test]
    fn hash_icon_name_is_strict() {
        assert!(is_hash_icon("35d1377200084a4034238c05b0c8930451e2fb40.jpg"));
        assert!(!is_hash_icon("header.jpg"));
        assert!(!is_hash_icon("35d1377200084a4034238c05b0c8930451e2fb4z.jpg"));
    }
}
