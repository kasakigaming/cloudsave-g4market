//! Tắt Steam Cloud bằng chính giao diện Steam: mở Settings → Cloud, đọc màn
//! hình bằng OCR của Windows, bấm công tắc "Enable Steam Cloud".
//!
//! Vì sao phải "nhìn" màn hình:
//!
//! * Steam chỉ đọc `sharedconfig.vdf` lúc đăng nhập — sửa file không có tác
//!   dụng với phiên đang chạy (xem `steam_cloud.rs`).
//! * Giao diện Steam là Chromium (CEF) và Steam tắt accessibility: UI
//!   Automation chỉ thấy 3 khung trống, không tìm được nút theo tên.
//! * Bấm trong Settings có hiệu lực ngay và Steam tự đẩy lên server, nên không
//!   bị bản trên server đè lại như khi sửa file.
//!
//! Không dựa vào toạ độ cố định: chữ được tìm bằng OCR trên ảnh chụp cửa sổ
//! (cửa sổ nhỏ hay phóng to đều được), công tắc được nhận ra bằng màu (xanh
//! Steam = bật). Mỗi lần bấm đều chụp lại để kiểm tra; không chắc thì dừng.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::{BOOL, HSTRING};
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
    GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT, VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetClientRect, GetCursorPos, GetForegroundWindow,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostMessageW,
    SetCursorPos, SetForegroundWindow, ShowWindow, SW_RESTORE, WM_CLOSE,
};

use crate::error::{Error, Result};

/// Chữ của mục "Cloud" ở menu trái, theo các ngôn ngữ hay gặp.
const NAV_WORDS: &[&str] = &["cloud", "đám mây", "云", "雲", "클라우드"];
/// Tiêu đề cửa sổ Settings theo ngôn ngữ (chỉ để ưu tiên, không bắt buộc).
const SETTINGS_TITLES: &[&str] = &["settings", "cài đặt", "设置", "設定", "설정"];
/// Số điểm ảnh xanh tối thiểu để coi là công tắc đang bật.
const BLUE_MIN: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Toggle {
    On,
    Off,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UiReport {
    pub before: Toggle,
    pub after: Toggle,
    pub clicked: bool,
    /// Các bước đã làm, để ghi log / gỡ lỗi.
    pub steps: Vec<String>,
}

/// Mở Settings → Cloud và tắt Steam Cloud nếu đang bật.
///
/// `click = false` chỉ đọc và báo trạng thái, không gạt công tắc (dùng để
/// thử). Vẫn bấm mục "Cloud" ở menu để chuyển trang — việc đó không đổi gì.
/// `debug_dir` có thì lưu ảnh chụp + chữ OCR đọc được vào đó.
pub fn disable_via_ui(steam_root: &Path, click: bool, debug_dir: Option<&Path>) -> Result<UiReport> {
    unsafe {
        // Toạ độ theo điểm ảnh thật, khớp với ảnh chụp dù màn hình scale 125–200%.
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let mut steps = Vec::new();
    let engine = ocr_engine()?;

    let before_windows = steam_windows();
    let hwnd = match find_settings(&before_windows) {
        Some(h) => {
            steps.push("cửa sổ Settings đã mở sẵn".into());
            h
        }
        None => {
            open_settings(steam_root)?;
            steps.push("đã mở steam://open/settings".into());
            wait_for(Duration::from_secs(10), || {
                let now = steam_windows();
                find_settings(&now).or_else(|| {
                    now.iter()
                        .find(|w| !before_windows.iter().any(|b| b.hwnd == w.hwnd) && w.title != "Steam")
                        .map(|w| w.hwnd)
                })
            })
            .ok_or_else(|| Error::Other("không thấy cửa sổ Settings của Steam sau 10 giây".into()))?
        }
    };
    let opened_by_us = !before_windows.iter().any(|w| w.hwnd == hwnd);
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
    }
    // Cửa sổ vừa mở cần chút thời gian để vẽ xong nội dung.
    std::thread::sleep(Duration::from_millis(if opened_by_us { 1500 } else { 300 }));

    let saved_cursor = cursor_pos();
    let saved_fg = unsafe { GetForegroundWindow() };
    let result = run(&engine, hwnd, click, debug_dir, &mut steps);
    {
        if let Some(p) = saved_cursor {
            unsafe {
                let _ = SetCursorPos(p.x, p.y);
            }
        }
    }
    if opened_by_us {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        steps.push("đã đóng cửa sổ Settings".into());
    }
    if !saved_fg.is_invalid() && saved_fg != hwnd {
        unsafe {
            let _ = SetForegroundWindow(saved_fg);
        }
    }
    let (before, after, clicked) = result?;
    Ok(UiReport {
        before,
        after,
        clicked,
        steps,
    })
}

fn run(
    engine: &OcrEngine,
    hwnd: HWND,
    click: bool,
    debug_dir: Option<&Path>,
    steps: &mut Vec<String>,
) -> Result<(Toggle, Toggle, bool)> {
    // 1. Tìm mục "Cloud" ở menu trái.
    let shot = capture(hwnd)?;
    let lines = recognize(engine, &shot)?;
    dump(debug_dir, "1-settings", &shot, &lines);
    let nav = lines
        .iter()
        .filter(|l| l.x < shot.w as f32 * 0.4)
        .find(|l| NAV_WORDS.iter().any(|w| l.text.trim().eq_ignore_ascii_case(w)))
        .cloned()
        .ok_or_else(|| {
            Error::Other(format!(
                "không tìm thấy mục \"Cloud\" trong Settings (OCR đọc được: {})",
                summary(&lines)
            ))
        })?;
    steps.push(format!("thấy mục \"{}\" ở ({:.0}, {:.0})", nav.text, nav.cx(), nav.cy()));

    // 2. Mở trang Cloud (bấm vào mục menu không đổi cài đặt gì).
    let page_left = nav.x + nav.w;
    let mut shot = shot;
    let mut row = find_cloud_row(&lines, page_left);
    if row.is_none() {
        click_at(hwnd, nav.cx(), nav.cy())?;
        steps.push("đã bấm mục Cloud".into());
        std::thread::sleep(Duration::from_millis(900));
        shot = capture(hwnd)?;
        let lines = recognize(engine, &shot)?;
        dump(debug_dir, "2-cloud", &shot, &lines);
        row = find_cloud_row(&lines, page_left);
    }
    let row = row.ok_or_else(|| Error::Other("không tìm thấy dòng \"Steam Cloud\" trong trang Cloud".into()))?;
    steps.push(format!("thấy dòng \"{}\"", row.text));

    // 3. Đọc công tắc bên phải dòng đó.
    let band = band_for(&row, &shot);
    let (blue, bbox) = blue_pixels(&shot, band);
    let before = if blue >= BLUE_MIN { Toggle::On } else { Toggle::Off };
    steps.push(format!("công tắc: {before:?} ({blue} điểm ảnh xanh)"));
    if before == Toggle::Off || !click {
        return Ok((before, before, false));
    }

    // 4. Bấm tắt, rồi chụp lại để chắc chắn.
    let (x0, y0, x1, y1) = bbox;
    click_at(hwnd, (x0 + x1) as f32 / 2.0, (y0 + y1) as f32 / 2.0)?;
    steps.push("đã bấm công tắc".into());
    std::thread::sleep(Duration::from_millis(900));
    let shot2 = capture(hwnd)?;
    let (blue2, _) = blue_pixels(&shot2, band);
    let lines2 = if debug_dir.is_some() { recognize(engine, &shot2)? } else { Vec::new() };
    dump(debug_dir, "3-after", &shot2, &lines2);
    let after = if blue2 >= BLUE_MIN { Toggle::On } else { Toggle::Off };
    steps.push(format!("sau khi bấm: {after:?} ({blue2} điểm ảnh xanh)"));
    if after != Toggle::Off {
        return Err(Error::Other("đã bấm nhưng công tắc vẫn bật — dừng, không bấm thêm".into()));
    }
    Ok((before, after, true))
}

// ── Cửa sổ Steam ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Win {
    hwnd: HWND,
    pid: u32,
    title: String,
}

/// Mọi cửa sổ giao diện của Steam (lớp `SDL_app`, cùng tiến trình với cửa sổ
/// chính tên "Steam").
fn steam_windows() -> Vec<Win> {
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<Win>);
        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }
        let mut class = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut class) as usize;
        if String::from_utf16_lossy(&class[..n]) != "SDL_app" {
            return true.into();
        }
        let mut title = [0u16; 256];
        let n = GetWindowTextW(hwnd, &mut title) as usize;
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        out.push(Win {
            hwnd,
            pid,
            title: String::from_utf16_lossy(&title[..n]),
        });
        true.into()
    }
    let mut all: Vec<Win> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut all as *mut _ as isize));
    }
    let Some(pid) = all.iter().find(|w| w.title == "Steam").map(|w| w.pid) else {
        return Vec::new();
    };
    all.retain(|w| w.pid == pid);
    all
}

fn find_settings(wins: &[Win]) -> Option<HWND> {
    wins.iter()
        .find(|w| {
            let t = w.title.to_lowercase();
            SETTINGS_TITLES.iter().any(|k| t.contains(k))
        })
        .map(|w| w.hwnd)
}

fn open_settings(steam_root: &Path) -> Result<()> {
    // steam.exe chuyển URL cho tiến trình Steam đang chạy rồi thoát ngay.
    std::process::Command::new(steam_root.join("steam.exe"))
        .arg("steam://open/settings")
        .spawn()
        .map_err(|e| Error::Other(format!("không mở được Settings của Steam: {e}")))?;
    Ok(())
}

fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if start.elapsed() > timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

// ── Chụp ─────────────────────────────────────────────────────────────────

struct Shot {
    w: usize,
    h: usize,
    /// BGRA, từ trên xuống.
    px: Vec<u8>,
}

impl Shot {
    /// Gần như toàn điểm đen (ảnh chụp hỏng). Lấy mẫu thưa cho nhanh.
    fn is_blank(&self) -> bool {
        let total = self.w * self.h;
        let step = (total / 5000).max(1);
        let (mut seen, mut dark) = (0usize, 0usize);
        for i in (0..total).step_by(step) {
            seen += 1;
            let j = i * 4;
            if self.px[j] < 8 && self.px[j + 1] < 8 && self.px[j + 2] < 8 {
                dark += 1;
            }
        }
        dark * 100 >= seen * 98
    }

    fn rgb(&self, x: usize, y: usize) -> (u8, u8, u8) {
        let i = (y * self.w + x) * 4;
        (self.px[i + 2], self.px[i + 1], self.px[i])
    }
}

/// Chụp vùng client của cửa sổ.
///
/// Thử `PrintWindow` trước (chụp được cả khi bị cửa sổ khác che). Chromium đôi
/// khi trả ảnh đen — nhất là khi cửa sổ vừa phóng to — thì đợi rồi thử lại, và
/// cuối cùng đưa cửa sổ lên trước rồi chụp thẳng từ màn hình.
fn capture(hwnd: HWND) -> Result<Shot> {
    for attempt in 0..4 {
        let shot = capture_with(hwnd, false)?;
        if !shot.is_blank() {
            return Ok(shot);
        }
        std::thread::sleep(Duration::from_millis(400 + attempt * 300));
    }
    bring_to_front(hwnd)?;
    std::thread::sleep(Duration::from_millis(400));
    let shot = capture_with(hwnd, true)?;
    if shot.is_blank() {
        return Err(Error::Other("ảnh chụp cửa sổ Settings toàn màu đen".into()));
    }
    Ok(shot)
}

fn capture_with(hwnd: HWND, from_screen: bool) -> Result<Shot> {
    unsafe {
        let mut rc = RECT::default();
        GetClientRect(hwnd, &mut rc).map_err(|e| Error::Other(format!("GetClientRect: {e}")))?;
        let (w, h) = ((rc.right - rc.left) as i32, (rc.bottom - rc.top) as i32);
        if w <= 0 || h <= 0 {
            return Err(Error::Other("cửa sổ Settings có kích thước 0".into()));
        }
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let ok = if from_screen {
            let mut p = POINT::default();
            let _ = ClientToScreen(hwnd, &mut p);
            BitBlt(mem, 0, 0, w, h, Some(screen), p.x, p.y, SRCCOPY).is_ok()
        } else {
            // PW_CLIENTONLY | PW_RENDERFULLCONTENT: chụp được cả nội dung Chromium.
            PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(1 | 2)).as_bool()
        };
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let rows = GetDIBits(mem, bmp, 0, h as u32, Some(px.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !ok || rows == 0 {
            return Err(Error::Other("không chụp được cửa sổ Settings".into()));
        }
        Ok(Shot {
            w: w as usize,
            h: h as usize,
            px,
        })
    }
}

// ── OCR ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Line {
    text: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Line {
    fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
    fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
}

fn ocr_engine() -> Result<OcrEngine> {
    // Chữ trong Steam hay là tiếng Anh; không có gói OCR tiếng Anh thì dùng
    // ngôn ngữ của người dùng.
    let en = Language::CreateLanguage(&HSTRING::from("en-US")).ok();
    if let Some(l) = en {
        if OcrEngine::IsLanguageSupported(&l).unwrap_or(false) {
            if let Ok(e) = OcrEngine::TryCreateFromLanguage(&l) {
                return Ok(e);
            }
        }
    }
    OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|e| Error::Other(format!("Windows không có OCR cho ngôn ngữ nào: {e}")))
}

fn recognize(engine: &OcrEngine, shot: &Shot) -> Result<Vec<Line>> {
    let err = |e: windows::core::Error| Error::Other(format!("OCR lỗi: {e}"));
    // OCR của Windows giới hạn kích thước ảnh; cửa sổ quá to thì thu nhỏ.
    let max = OcrEngine::MaxImageDimension().unwrap_or(2600) as usize;
    let scale = ((shot.w.max(shot.h) + max - 1) / max).max(1);
    let (w, h) = (shot.w / scale, shot.h / scale);
    let px: Vec<u8> = if scale == 1 {
        shot.px.clone()
    } else {
        let mut v = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * scale) * shot.w + x * scale) * 4;
                v.extend_from_slice(&shot.px[i..i + 4]);
            }
        }
        v
    };
    let writer = DataWriter::new().map_err(err)?;
    writer.WriteBytes(&px).map_err(err)?;
    let buf = writer.DetachBuffer().map_err(err)?;
    let bmp = SoftwareBitmap::CreateCopyFromBuffer(&buf, BitmapPixelFormat::Bgra8, w as i32, h as i32)
        .map_err(err)?;
    let result = engine.RecognizeAsync(&bmp).map_err(err)?.get().map_err(err)?;
    let lines = result.Lines().map_err(err)?;
    let mut out = Vec::new();
    for i in 0..lines.Size().map_err(err)? {
        let line = lines.GetAt(i).map_err(err)?;
        let words = line.Words().map_err(err)?;
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, 0f32, 0f32);
        for j in 0..words.Size().map_err(err)? {
            let r = words.GetAt(j).map_err(err)?.BoundingRect().map_err(err)?;
            x0 = x0.min(r.X);
            y0 = y0.min(r.Y);
            x1 = x1.max(r.X + r.Width);
            y1 = y1.max(r.Y + r.Height);
        }
        if x1 <= x0 {
            continue;
        }
        let s = scale as f32;
        out.push(Line {
            text: line.Text().map_err(err)?.to_string(),
            x: x0 * s,
            y: y0 * s,
            w: (x1 - x0) * s,
            h: (y1 - y0) * s,
        });
    }
    Ok(out)
}

fn summary(lines: &[Line]) -> String {
    let s: Vec<&str> = lines.iter().take(12).map(|l| l.text.as_str()).collect();
    if s.is_empty() {
        "không có chữ nào".into()
    } else {
        s.join(" | ")
    }
}

/// Dòng tiêu đề của công tắc: dòng trên cùng ở phần trang (bên phải menu) có
/// chữ "Steam Cloud" và không chỉ là mỗi chữ "Cloud" (tiêu đề trang).
fn find_cloud_row(lines: &[Line], page_left: f32) -> Option<Line> {
    lines
        .iter()
        .filter(|l| l.x > page_left)
        .filter(|l| l.text.to_lowercase().contains("steam cloud"))
        .min_by(|a, b| a.y.total_cmp(&b.y))
        .cloned()
}

// ── Công tắc ─────────────────────────────────────────────────────────────

/// Vùng tìm công tắc: cùng hàng với dòng chữ, từ cuối dòng chữ tới mép phải.
fn band_for(row: &Line, shot: &Shot) -> (usize, usize, usize, usize) {
    let x0 = (row.x + row.w) as usize;
    let y0 = (row.y - row.h * 1.2).max(0.0) as usize;
    let y1 = ((row.y + row.h * 3.0) as usize).min(shot.h);
    (x0.min(shot.w), y0, shot.w, y1)
}

/// Xanh của công tắc Steam khi bật (#1a9fff và các sắc gần đó).
fn is_toggle_blue(r: u8, g: u8, b: u8) -> bool {
    b >= 200 && r <= 90 && (110..=210).contains(&g)
}

/// Đếm điểm ảnh xanh trong vùng, kèm khung bao của chúng.
fn blue_pixels(shot: &Shot, (x0, y0, x1, y1): (usize, usize, usize, usize)) -> (usize, (usize, usize, usize, usize)) {
    let (mut n, mut bx0, mut by0, mut bx1, mut by1) = (0, usize::MAX, usize::MAX, 0, 0);
    for y in y0..y1 {
        for x in x0..x1 {
            let (r, g, b) = shot.rgb(x, y);
            if is_toggle_blue(r, g, b) {
                n += 1;
                bx0 = bx0.min(x);
                by0 = by0.min(y);
                bx1 = bx1.max(x);
                by1 = by1.max(y);
            }
        }
    }
    (n, (bx0, by0, bx1, by1))
}

// ── Chuột ────────────────────────────────────────────────────────────────

fn cursor_pos() -> Option<POINT> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok().map(|_| p) }
}

/// Đưa cửa sổ lên trước; không được thì báo lỗi (không bấm vào cửa sổ khác).
fn bring_to_front(hwnd: HWND) -> Result<()> {
    unsafe {
        if GetForegroundWindow() != hwnd {
            // Windows chỉ cho đổi cửa sổ trước khi vừa có phím bấm: gửi một
            // lần nhấn-nhả Alt (không gây tác dụng gì) rồi mới đổi.
            let alt = |flags| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_MENU,
                        dwFlags: flags,
                        ..Default::default()
                    },
                },
            };
            SendInput(&[alt(Default::default()), alt(KEYEVENTF_KEYUP)], std::mem::size_of::<INPUT>() as i32);
            let _ = SetForegroundWindow(hwnd);
            std::thread::sleep(Duration::from_millis(250));
        }
        if GetForegroundWindow() != hwnd {
            return Err(Error::Other(
                "không đưa được cửa sổ Settings của Steam lên trước — không bấm".into(),
            ));
        }
    }
    Ok(())
}

/// Đưa cửa sổ lên trước rồi bấm chuột trái tại (x, y) — toạ độ client.
fn click_at(hwnd: HWND, x: f32, y: f32) -> Result<()> {
    bring_to_front(hwnd)?;
    unsafe {
        let mut p = POINT {
            x: x.round() as i32,
            y: y.round() as i32,
        };
        let _ = ClientToScreen(hwnd, &mut p);
        SetCursorPos(p.x, p.y).map_err(|e| Error::Other(format!("SetCursorPos: {e}")))?;
        std::thread::sleep(Duration::from_millis(60));
        let m = |flags| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        };
        SendInput(&[m(MOUSEEVENTF_LEFTDOWN), m(MOUSEEVENTF_LEFTUP)], std::mem::size_of::<INPUT>() as i32);
    }
    Ok(())
}

// ── Gỡ lỗi ───────────────────────────────────────────────────────────────

/// Lưu ảnh chụp (BMP) và chữ OCR đọc được, để xem app đã "thấy" gì.
fn dump(dir: Option<&Path>, name: &str, shot: &Shot, lines: &[Line]) {
    let Some(dir) = dir else { return };
    let _ = std::fs::create_dir_all(dir);
    let path: PathBuf = dir.join(format!("{name}.bmp"));
    let size = 54 + shot.px.len();
    let mut b = Vec::with_capacity(size);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(size as u32).to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(shot.w as i32).to_le_bytes());
    b.extend_from_slice(&(-(shot.h as i32)).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&[0; 24]);
    b.extend_from_slice(&shot.px);
    let _ = std::fs::write(&path, b);
    let text: String = lines
        .iter()
        .map(|l| format!("({:>5.0},{:>5.0} {:>4.0}x{:<3.0}) {}\n", l.x, l.y, l.w, l.h, l.text))
        .collect();
    let _ = std::fs::write(dir.join(format!("{name}.txt")), text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x: f32, y: f32) -> Line {
        Line {
            text: text.into(),
            x,
            y,
            w: 200.0,
            h: 14.0,
        }
    }

    #[test]
    fn picks_toggle_title_not_description_or_nav() {
        let lines = vec![
            line("Cloud", 55.0, 340.0),
            line("Cloud", 222.0, 45.0),
            line("Enable Steam Cloud", 233.0, 100.0),
            line("Enable Steam Cloud synchronization for applications", 233.0, 122.0),
        ];
        let row = find_cloud_row(&lines, 150.0).unwrap();
        assert_eq!(row.text, "Enable Steam Cloud");
    }

    #[test]
    fn steam_blue_is_detected_grey_is_not() {
        assert!(is_toggle_blue(26, 159, 255));
        assert!(is_toggle_blue(60, 180, 250));
        assert!(!is_toggle_blue(80, 80, 90), "rãnh xám khi tắt");
        assert!(!is_toggle_blue(230, 230, 230), "núm trắng");
        assert!(!is_toggle_blue(40, 50, 70), "nền tối");
    }

    #[test]
    fn counts_blue_inside_band_only() {
        let (w, h) = (100usize, 40usize);
        let mut px = vec![0u8; w * h * 4];
        let mut put = |x: usize, y: usize, (r, g, b): (u8, u8, u8)| {
            let i = (y * w + x) * 4;
            px[i] = b;
            px[i + 1] = g;
            px[i + 2] = r;
        };
        for y in 10..20 {
            for x in 80..95 {
                put(x, y, (26, 159, 255));
            }
        }
        // Xanh ngoài vùng tìm (bên trái) không được tính.
        for y in 10..20 {
            put(5, y, (26, 159, 255));
        }
        let shot = Shot { w, h, px };
        let (n, bbox) = blue_pixels(&shot, (50, 0, w, h));
        assert_eq!(n, 150);
        assert_eq!(bbox, (80, 10, 94, 19));
    }
}
