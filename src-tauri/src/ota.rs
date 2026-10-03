//! Cập nhật nóng giao diện: thay HTML/CSS/JS mà không tắt app.
//!
//! Giao diện của app là một thư mục `dist/` nhúng trong exe. Module này thay
//! nguồn asset của Tauri (`Context::set_assets`) bằng `OtaAssets`: có gói giao
//! diện mới đã tải về thì phát file từ gói đó, không thì phát bản nhúng. Nạp lại
//! WebView là thấy giao diện mới — cùng origin, cùng IPC, cùng CSP, tiến trình
//! không tắt nên watcher, game đang theo dõi, phiên đăng nhập vẫn nguyên.
//!
//! Code Rust thì KHÔNG thay nóng được (nó là chính file exe đang chạy); đổi
//! backend vẫn phải qua `updater.rs` (khởi động lại vài giây).
//!
//! Gói giao diện chạy với toàn quyền IPC của app (khôi phục, xoá file save…),
//! nên phải chặn chặt:
//!
//! 1. **Chữ ký** minisign bằng cùng khoá công khai với bộ cập nhật; sai là bỏ.
//! 2. **Đúng backend**: gói build cho backend `native` nào chỉ nạp vào đúng
//!    phiên bản đó — giao diện mới gọi lệnh mà backend cũ không có là hỏng.
//! 3. **Không phát lại gói cũ**: `native` + `ui_version` nằm trong `ota.json`
//!    BÊN TRONG gói đã ký, phải khớp manifest; `ui_version` phải lớn hơn bản
//!    đang chạy.
//! 4. **Tự quay về**: nạp xong mà giao diện mới không gọi `ui_ready` trong
//!    `CONFIRM_WINDOW` thì trở lại bản trước và nhớ không nạp gói đó nữa.
//! 5. **Chỉ tải từ GitHub Releases của repo**: link trong bảng phải bắt đầu
//!    bằng `GITHUB_DOWNLOADS` (bảng cũng có ràng buộc này).
//!
//! Danh sách gói nằm trong bảng `ui_releases` trên Supabase (đọc công khai,
//! chỉ admin ghi được). Phát hành cập nhật nóng = upload gói lên GitHub
//! Releases + thêm một dòng vào bảng; tắt một gói = đặt `enabled = false`.
//! Ai sửa được bảng cũng không đẩy được code lạ: không có khoá bí mật thì
//! không ký được gói.

use std::borrow::Cow;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use tauri::utils::assets::{AssetKey, AssetsIter, CspHash};
use tauri::{App, AppHandle, Assets, Emitter, Manager, Runtime};

use crate::error::{Error, Result};
use crate::supabase::Supabase;

/// Gói giao diện chỉ được tải từ đây — link tải trực tiếp của GitHub Releases.
pub const GITHUB_DOWNLOADS: &str = "https://github.com/kasakigaming/cloudsave-g4market/releases/download/";

/// Giao diện mới phải báo "đã chạy" trong khoảng này, không thì quay về.
const CONFIRM_WINDOW: Duration = Duration::from_secs(20);
/// Gói giao diện lớn hơn thế này là bất thường (dist hiện ~3 MB).
const MAX_BUNDLE: u64 = 50 * 1024 * 1024;
const MAX_FILES: usize = 2_000;

/// Gói giao diện đang dùng, ghi trong `ui/current.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Active {
    native: String,
    ui_version: u32,
    dir: PathBuf,
}

/// `ota.json` nằm trong gói, và cũng là phần chính của manifest `ui.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Stamp {
    native: String,
    ui_version: u32,
}

/// Một dòng của bảng `ui_releases`: gói nào, cho backend nào, tải ở đâu, chữ ký gì.
#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    native: String,
    ui_version: u32,
    url: String,
    signature: String,
    #[serde(default)]
    notes: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiUpdate {
    pub ui_version: u32,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiInfo {
    pub native: String,
    /// 0 = giao diện nhúng trong exe.
    pub ui_version: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct UiProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
}

/// Trạng thái dùng chung giữa nguồn asset và các lệnh.
pub struct UiState {
    base: PathBuf,
    native: String,
    active: RwLock<Option<Active>>,
    /// Giao diện vừa nạp đã báo chạy ổn chưa.
    confirmed: AtomicBool,
    /// Tăng mỗi lần nạp, để bộ canh giờ của lần nạp cũ không quay về nhầm.
    generation: AtomicU64,
    /// `ui_version` từng bị quay về — không nạp lại trong phiên này.
    rejected: RwLock<Vec<u32>>,
}

impl UiState {
    /// Đọc gói đang dùng; gói build cho backend khác (sau một lần cập nhật đầy
    /// đủ) thì bỏ — bản nhúng trong exe mới hơn nó.
    pub fn load(native: &str) -> Arc<Self> {
        let base = dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("cloudsave-g4market")
            .join("ui");
        let active = std::fs::read(base.join("current.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Active>(&b).ok())
            .filter(|a| a.native == native && a.dir.join("index.html").is_file());
        let state = Arc::new(Self {
            base,
            native: native.to_string(),
            active: RwLock::new(active),
            confirmed: AtomicBool::new(true),
            generation: AtomicU64::new(0),
            rejected: RwLock::new(Vec::new()),
        });
        state.cleanup();
        state
    }

    fn dir(&self) -> Option<PathBuf> {
        self.active.read().unwrap().as_ref().map(|a| a.dir.clone())
    }

    pub fn info(&self) -> UiInfo {
        UiInfo {
            native: self.native.clone(),
            ui_version: self.active.read().unwrap().as_ref().map_or(0, |a| a.ui_version),
        }
    }

    fn set_active(&self, a: Option<Active>) -> Result<()> {
        std::fs::create_dir_all(&self.base)?;
        match &a {
            Some(a) => std::fs::write(
                self.base.join("current.json"),
                serde_json::to_vec_pretty(a).map_err(|e| Error::Other(e.to_string()))?,
            )?,
            None => {
                let _ = std::fs::remove_file(self.base.join("current.json"));
            }
        }
        *self.active.write().unwrap() = a;
        Ok(())
    }

    /// Xoá các gói không còn dùng (giữ gói đang chạy).
    fn cleanup(&self) {
        let keep = self.dir();
        let Ok(entries) = std::fs::read_dir(&self.base) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() && Some(&p) != keep.as_ref() {
                let _ = std::fs::remove_dir_all(&p);
            }
        }
    }

    /// Giao diện mới báo đã khởi động xong.
    pub fn confirm(&self) {
        self.confirmed.store(true, Ordering::SeqCst);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Nguồn asset
// ─────────────────────────────────────────────────────────────────────────

pub struct OtaAssets<R: Runtime> {
    embedded: Box<dyn Assets<R>>,
    state: Arc<UiState>,
}

/// Chỗ giữ tạm khi tráo nguồn asset (`set_assets` trả về nguồn cũ).
struct Empty;

impl<R: Runtime> Assets<R> for Empty {
    fn get(&self, _: &AssetKey) -> Option<Cow<'_, [u8]>> {
        None
    }
    fn iter(&self) -> Box<AssetsIter<'_>> {
        Box::new(std::iter::empty())
    }
    fn csp_hashes(&self, _: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        Box::new(std::iter::empty())
    }
}

/// Đặt `OtaAssets` làm nguồn asset, bản nhúng trong exe làm dự phòng.
pub fn install<R: Runtime>(context: &mut tauri::Context<R>, state: Arc<UiState>) {
    let embedded = context.set_assets(Box::new(Empty));
    context.set_assets(Box::new(OtaAssets { embedded, state }));
}

/// Đường dẫn asset (`/assets/index-abc.js`) → file trong thư mục gói, chặn mọi
/// thứ có thể thoát ra ngoài.
fn asset_path(dir: &Path, key: &str) -> Option<PathBuf> {
    let rel = key.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if rel.split('/').any(|s| s.is_empty() || s == "." || s == "..") || rel.contains(['\\', ':']) {
        return None;
    }
    Some(dir.join(rel))
}

impl<R: Runtime> Assets<R> for OtaAssets<R> {
    fn setup(&self, app: &App<R>) {
        self.embedded.setup(app);
    }

    fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        if let Some(dir) = self.state.dir() {
            if let Some(bytes) = asset_path(&dir, key.as_ref()).and_then(|p| std::fs::read(p).ok()) {
                return Some(Cow::Owned(bytes));
            }
        }
        self.embedded.get(key)
    }

    fn iter(&self) -> Box<AssetsIter<'_>> {
        self.embedded.iter()
    }

    fn csp_hashes(&self, html_path: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        // Giao diện không có <script>/<style> nội tuyến; hash (nếu có) của bản
        // nhúng không ảnh hưởng gì tới gói mới.
        self.embedded.csp_hashes(html_path)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Kiểm tra và nạp
// ─────────────────────────────────────────────────────────────────────────

/// Khoá công khai lấy từ cấu hình của bộ cập nhật — một khoá cho cả hai.
fn pubkey<R: Runtime>(app: &AppHandle<R>) -> Result<String> {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|c| c.get("pubkey"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| Error::Other("thiếu khoá công khai".into()))
}

fn check_url(url: &str) -> Result<()> {
    let rest = url.strip_prefix(GITHUB_DOWNLOADS).unwrap_or_default();
    // Phải có tag + tên file, không có `..` hay ký tự lạ để vòng ra chỗ khác.
    let ok = rest.split('/').count() == 2
        && !rest.split('/').any(|s| s.is_empty() || s == "." || s == "..")
        && !rest.contains(['?', '#', '\\', '@']);
    if ok {
        Ok(())
    } else {
        Err(Error::Other(format!(
            "từ chối tải gói giao diện từ nguồn không phải của app: {url}"
        )))
    }
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("CloudSaveG4Market/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| Error::Other(e.to_string()))
}

/// Gói mới nhất đang bật cho backend này, đọc từ bảng `ui_releases`.
async fn fetch_manifest(sb: &Supabase, native: &str) -> Result<Option<Manifest>> {
    let path = format!(
        "ui_releases?select=native,ui_version,url,signature,notes\
         &native=eq.{}&enabled=is.true&order=ui_version.desc&limit=1",
        urlencode(native)
    );
    let rows = match sb.public_get(&path).await {
        Ok(v) => v,
        // Bảng chưa tạo (chưa chạy migration): coi như chưa có gói nào.
        Err(Error::Supabase { status: 404, .. }) => return Ok(None),
        Err(e) => return Err(Error::Other(format!("không đọc được danh sách gói giao diện: {e}"))),
    };
    let Some(row) = rows.as_array().and_then(|a| a.first()).cloned() else {
        return Ok(None);
    };
    let m: Manifest =
        serde_json::from_value(row).map_err(|e| Error::Other(format!("dòng ui_releases hỏng: {e}")))?;
    Ok(Some(m))
}

/// Phiên bản backend chỉ gồm chữ số, chữ cái, `.` `-` `+`; mã hoá `+` cho URL.
fn urlencode(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        .map(|c| if c == '+' { "%2B".to_string() } else { c.to_string() })
        .collect()
}

/// Manifest có đáng nạp không, so với trạng thái hiện tại.
fn wanted(state: &UiState, m: &Manifest) -> bool {
    m.native == state.native
        && m.ui_version > state.info().ui_version
        && !state.rejected.read().unwrap().contains(&m.ui_version)
}

/// Không cấu hình cloud (thiếu `.env` lúc build) thì không có nguồn gói giao diện.
pub async fn check(sb: Option<&Supabase>, state: &UiState) -> Result<Option<UiUpdate>> {
    let Some(sb) = sb else {
        return Ok(None);
    };
    Ok(fetch_manifest(sb, &state.native)
        .await?
        .filter(|m| wanted(state, m))
        .map(|m| UiUpdate {
            ui_version: m.ui_version,
            notes: m.notes,
        }))
}

/// Kiểm chữ ký minisign, giống hệt cách `tauri-plugin-updater` làm.
fn verify(data: &[u8], signature_b64: &str, pubkey_b64: &str) -> Result<()> {
    use minisign_verify::{PublicKey, Signature};
    let b64 = |s: &str| -> Result<String> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .map_err(|e| Error::Other(format!("base64: {e}")))?;
        String::from_utf8(raw).map_err(|_| Error::Other("chữ ký không phải UTF-8".into()))
    };
    let pk = PublicKey::decode(&b64(pubkey_b64)?).map_err(|e| Error::Other(format!("khoá công khai: {e}")))?;
    let sig = Signature::decode(&b64(signature_b64)?).map_err(|e| Error::Other(format!("chữ ký: {e}")))?;
    pk.verify(data, &sig, true)
        .map_err(|e| Error::Other(format!("chữ ký KHÔNG hợp lệ, bỏ gói giao diện: {e}")))
}

/// Giải nén gói vào `dest`, kiểm từng đường dẫn. Trả về `ota.json` trong gói.
fn extract(bytes: &[u8], dest: &Path) -> Result<Stamp> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| Error::Other(format!("gói giao diện hỏng: {e}")))?;
    if zip.len() > MAX_FILES {
        return Err(Error::Other("gói giao diện có quá nhiều file".into()));
    }
    let mut total = 0u64;
    let mut stamp = None;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| Error::Other(e.to_string()))?;
        let Some(rel) = f.enclosed_name() else {
            return Err(Error::Other(format!("đường dẫn bất thường trong gói: {}", f.name())));
        };
        let out = dest.join(&rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        total += f.size();
        if total > MAX_BUNDLE {
            return Err(Error::Other("gói giao diện quá lớn khi giải nén".into()));
        }
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut buf)?;
        if rel == Path::new("ota.json") {
            stamp = Some(
                serde_json::from_slice::<Stamp>(&buf)
                    .map_err(|e| Error::Other(format!("ota.json hỏng: {e}")))?,
            );
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out, &buf)?;
    }
    if !dest.join("index.html").is_file() {
        return Err(Error::Other("gói giao diện thiếu index.html".into()));
    }
    stamp.ok_or_else(|| Error::Other("gói giao diện thiếu ota.json".into()))
}

/// Tải, kiểm, giải nén và bật gói giao diện mới. Giao diện gọi xong thì tự nạp
/// lại trang; nếu trang mới không gọi `ui_ready` kịp thì tự quay về.
pub async fn apply<R: Runtime>(
    app: &AppHandle<R>,
    sb: Option<&Supabase>,
    state: Arc<UiState>,
) -> Result<UiInfo> {
    let sb = sb.ok_or_else(|| Error::Other("bản build này không có cloud — không có nguồn gói giao diện".into()))?;
    let pubkey = pubkey(app)?;
    let m = fetch_manifest(sb, &state.native)
        .await?
        .filter(|m| wanted(&state, m))
        .ok_or_else(|| Error::Other("không có gói giao diện mới".into()))?;
    check_url(&m.url)?;

    let mut resp = client()?
        .get(&m.url)
        .send()
        .await
        .map_err(|e| Error::Other(format!("không tải được gói giao diện: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!("gói giao diện: {}", resp.status())));
    }
    let total = resp.content_length();
    if total.is_some_and(|t| t > MAX_BUNDLE) {
        return Err(Error::Other("gói giao diện quá lớn".into()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| Error::Other(format!("tải gói giao diện: {e}")))?
    {
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 > MAX_BUNDLE {
            return Err(Error::Other("gói giao diện quá lớn".into()));
        }
        let _ = app.emit(
            "ui-update-progress",
            UiProgress {
                downloaded: bytes.len() as u64,
                total,
            },
        );
    }

    // Chữ ký trước, rồi mới giải nén — không đụng tới đĩa với dữ liệu chưa kiểm.
    verify(&bytes, &m.signature, &pubkey)?;

    let want = Stamp {
        native: m.native.clone(),
        ui_version: m.ui_version,
    };
    let final_dir = state.base.join(format!("{}-{}", m.native, m.ui_version));
    let tmp = state.base.join(format!("{}-{}.tmp", m.native, m.ui_version));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let stamp = match extract(&bytes, &tmp) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };
    // Phần trong gói (đã ký) phải khớp manifest (chưa ký): chặn việc lấy một gói
    // cũ hợp lệ gắn vào manifest mới.
    if stamp != want {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(Error::Other(format!(
            "gói giao diện không khớp manifest (trong gói: {} #{}, manifest: {} #{})",
            stamp.native, stamp.ui_version, want.native, want.ui_version
        )));
    }
    let _ = std::fs::remove_dir_all(&final_dir);
    std::fs::rename(&tmp, &final_dir)?;

    let previous = state.active.read().unwrap().clone();
    state.set_active(Some(Active {
        native: m.native.clone(),
        ui_version: m.ui_version,
        dir: final_dir,
    }))?;
    log::info!("đã bật giao diện #{} cho backend {}", m.ui_version, m.native);

    // Bộ canh giờ: trang mới phải báo chạy ổn, không thì quay về bản trước.
    state.confirmed.store(false, Ordering::SeqCst);
    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let (app2, st, bad) = (app.clone(), state.clone(), m.ui_version);
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(CONFIRM_WINDOW).await;
        if st.generation.load(Ordering::SeqCst) != generation || st.confirmed.load(Ordering::SeqCst) {
            return;
        }
        log::warn!("giao diện #{bad} không báo chạy ổn sau {CONFIRM_WINDOW:?} — quay về bản trước");
        st.rejected.write().unwrap().push(bad);
        let _ = st.set_active(previous);
        st.confirmed.store(true, Ordering::SeqCst);
        if let Some(w) = app2.get_webview_window("main") {
            let _ = w.eval("location.reload()");
        }
        st.cleanup();
    });

    Ok(state.info())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_paths_cannot_escape_the_bundle() {
        let d = Path::new("C:/ui/0.1.5-1");
        assert_eq!(asset_path(d, "/"), Some(d.join("index.html")));
        assert_eq!(asset_path(d, "/assets/index-abc.js"), Some(d.join("assets/index-abc.js")));
        assert_eq!(asset_path(d, "/../secret"), None);
        assert_eq!(asset_path(d, "/assets/../../x"), None);
        assert_eq!(asset_path(d, "/a\\b"), None);
        assert_eq!(asset_path(d, "/C:/Windows/x"), None);
        // Dấu `/` thừa ở đầu bị cắt hết — vẫn nằm trong thư mục gói.
        assert_eq!(asset_path(d, "//double"), Some(d.join("double")));
        assert_eq!(asset_path(d, "/a//b"), None);
    }

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        // Dựng gói bằng định dạng "stored" (không nén) — bản zip trong app chỉ
        // bật phần giải nén, nên tự ghi header tay cho test.
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in files {
            let crc = crc32(data);
            let offset = out.len() as u32;
            let n = name.as_bytes();
            out.extend_from_slice(&0x04034b50u32.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(n.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(n);
            out.extend_from_slice(data);

            central.extend_from_slice(&0x02014b50u32.to_le_bytes());
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(n.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0; 12]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(n);
        }
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x06054b50u32.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("cs-ota-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn extracts_bundle_and_reads_stamp() {
        let z = zip_of(&[
            ("index.html", b"<html></html>"),
            ("assets/app.js", b"console.log(1)"),
            ("ota.json", br#"{"native":"0.1.5","ui_version":3}"#),
        ]);
        let d = tmp();
        let s = extract(&z, &d).unwrap();
        assert_eq!(s, Stamp { native: "0.1.5".into(), ui_version: 3 });
        assert_eq!(std::fs::read(d.join("assets/app.js")).unwrap(), b"console.log(1)");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn rejects_traversal_and_missing_parts() {
        let d = tmp();
        let evil = zip_of(&[("../evil.txt", b"x"), ("index.html", b"x"), ("ota.json", b"{}")]);
        assert!(extract(&evil, &d).is_err());
        assert!(!d.parent().unwrap().join("evil.txt").exists());

        let d2 = tmp();
        let no_stamp = zip_of(&[("index.html", b"x")]);
        assert!(extract(&no_stamp, &d2).is_err());

        let d3 = tmp();
        let no_index = zip_of(&[("ota.json", br#"{"native":"0.1.5","ui_version":1}"#)]);
        assert!(extract(&no_index, &d3).is_err());
        for p in [d, d2, d3] {
            std::fs::remove_dir_all(p).ok();
        }
    }

    #[test]
    fn downloads_only_from_this_repos_github_releases() {
        let ok = format!("{GITHUB_DOWNLOADS}v0.1.5/ui-0.1.5-2.zip");
        assert!(check_url(&ok).is_ok());
        for bad in [
            "https://github.com/someone-else/repo/releases/download/v1/ui.zip".to_string(),
            "http://github.com/kasakigaming/cloudsave-g4market/releases/download/v1/ui.zip".to_string(),
            "https://evil.example/ui.zip".to_string(),
            format!("{GITHUB_DOWNLOADS}v1/../../../x/ui.zip"),
            format!("{GITHUB_DOWNLOADS}ui.zip"),
            format!("{GITHUB_DOWNLOADS}v1/ui.zip?x=1"),
            format!("{GITHUB_DOWNLOADS}v1/a/ui.zip"),
        ] {
            assert!(check_url(&bad).is_err(), "phải từ chối {bad}");
        }
    }

    #[test]
    fn native_version_is_url_safe() {
        assert_eq!(urlencode("0.1.5"), "0.1.5");
        assert_eq!(urlencode("0.1.5-demo"), "0.1.5-demo");
        assert_eq!(urlencode("1.0.0+build.7"), "1.0.0%2Bbuild.7");
        assert_eq!(urlencode("0.1.5&enabled=is.false"), "0.1.5enabledis.false");
    }

    #[test]
    fn bad_signature_is_rejected() {
        // Khoá công khai thật của app, chữ ký rác.
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let pk = conf["plugins"]["updater"]["pubkey"].as_str().unwrap();
        let junk = base64::engine::general_purpose::STANDARD.encode("untrusted comment: x\nRWQ=\n");
        assert!(verify(b"data", &junk, pk).is_err());
    }
}
