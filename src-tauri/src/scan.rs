//! Gộp ba nguồn thông tin thành một danh sách file save cụ thể trên đĩa.
//!
//! Thứ tự ưu tiên, tin cậy giảm dần:
//!
//!   1. `appinfo.vdf` → `ufs.savefiles` — chính Steam khai báo, chính xác nhất.
//!   2. `remotecache.vdf` — file Steam đã thực sự đồng bộ cho tài khoản này.
//!   3. ludusavi-manifest — cộng đồng đóng góp, phủ cả game không bật Cloud.
//!
//! Một file xuất hiện ở nhiều nguồn chỉ được tính một lần, và ta ghi lại nguồn
//! nào tìm ra nó để UI giải thích được vì sao file này có trong danh sách.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use globset::{Glob, GlobMatcher};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::manifest::ludusavi::{self, Manifest, Substitutions};
use crate::steam::{remotecache, AppInfo, RootContext, RootToken, SteamInstall};

/// Giới hạn an toàn khi quét đệ quy. Một quy tắc UFS trỏ nhầm vào thư mục cài
/// game có thể kéo theo hàng chục nghìn file asset; thà cắt còn hơn treo máy.
const MAX_WALK_DEPTH: usize = 12;
const MAX_FILES_PER_GAME: usize = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// `appinfo.vdf` → `ufs.savefiles`
    Ufs,
    /// `remotecache.vdf`
    RemoteCache,
    /// ludusavi-manifest
    Manifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveFile {
    pub root: RootToken,
    /// Tương đối so với root, luôn dùng `/`. Đây là thứ được lưu lên Supabase.
    pub rel_path: String,
    /// Đường dẫn thật trên máy này. KHÔNG upload — nó chỉ đúng với máy này.
    pub abs_path: PathBuf,
    pub size: u64,
    pub mtime: Option<DateTime<Utc>>,
    pub source: Source,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameScan {
    pub app_id: Option<u32>,
    pub slug: String,
    pub title: String,
    pub files: Vec<SaveFile>,
    pub total_bytes: u64,
    /// Nguồn nào đã đóng góp file, để hiển thị lên UI.
    pub sources: Vec<Source>,
    /// Vấn đề gặp phải khi quét (root lạ, thư mục không đọc được...).
    pub warnings: Vec<String>,
}

pub struct Scanner {
    pub steam: SteamInstall,
    /// Dùng Arc vì `appinfo.vdf` đã parse chiếm hàng chục MB — mỗi lần quét
    /// một game mà clone lại toàn bộ cây thì không chấp nhận được.
    pub appinfo: Arc<AppInfo>,
    pub manifest: Option<Arc<Manifest>>,
    pub account_id: u32,
}

impl Scanner {
    pub fn scan_app(&self, app_id: u32) -> GameScan {
        let installed = self.steam.installed_apps();
        let install_dir = installed.get(&app_id).map(|a| a.install_dir.clone());

        let title = self
            .appinfo
            .app_name(app_id)
            .or_else(|| installed.get(&app_id).map(|a| a.name.clone()))
            .unwrap_or_else(|| format!("App {app_id}"));

        let ctx = RootContext {
            steam_root: self.steam.root.clone(),
            account_id: self.account_id,
            app_id,
            game_install: install_dir.clone(),
        };

        // Khoá dedupe là (root, rel_path) — cùng một file có thể được cả ba
        // nguồn chỉ ra. Nguồn đầu tiên tìm ra nó thắng, theo thứ tự tin cậy.
        let mut found: BTreeMap<(RootToken, String), SaveFile> = BTreeMap::new();
        let mut warnings = Vec::new();

        self.collect_ufs(app_id, &ctx, &mut found, &mut warnings);
        self.collect_remote_cache(app_id, &ctx, &mut found, &mut warnings);
        self.collect_manifest(app_id, &ctx, &install_dir, &mut found, &mut warnings);

        let files: Vec<SaveFile> = found.into_values().collect();
        let total_bytes = files.iter().map(|f| f.size).sum();
        let mut sources: Vec<Source> = files.iter().map(|f| f.source).collect();
        sources.sort_by_key(|s| format!("{s:?}"));
        sources.dedup();

        GameScan {
            app_id: Some(app_id),
            slug: slugify(&title),
            title,
            files,
            total_bytes,
            sources,
            warnings,
        }
    }

    // ── nguồn 1: ufs.savefiles ───────────────────────────────────────────
    fn collect_ufs(
        &self,
        app_id: u32,
        ctx: &RootContext,
        out: &mut BTreeMap<(RootToken, String), SaveFile>,
        warnings: &mut Vec<String>,
    ) {
        for rule in self.appinfo.save_rules(app_id) {
            if !rule.applies_to_windows() {
                continue;
            }
            let Some(token) = RootToken::from_name(&rule.root) else {
                warnings.push(format!("UFS: root '{}' chưa hỗ trợ, bỏ qua", rule.root));
                continue;
            };
            let Some(base) = ctx.resolve(token) else {
                warnings.push(format!("UFS: không phân giải được root {token:?}"));
                continue;
            };
            // `path` có thể chứa `{64BitSteamID}` / `{Steam3AccountID}` —
            // khoảng 19% quy tắc UFS có placeholder, nên bước này bắt buộc.
            let Some(rule_path) = rule.expanded_path(self.account_id) else {
                warnings.push(format!(
                    "UFS: không giải được placeholder trong '{}'",
                    rule.path
                ));
                continue;
            };
            let dir = if rule_path.is_empty() {
                base.clone()
            } else {
                base.join(&rule_path)
            };
            if !dir.is_dir() {
                continue;
            }
            let Ok(matcher) = compile_glob(&rule.pattern) else {
                warnings.push(format!("UFS: pattern '{}' không hợp lệ", rule.pattern));
                continue;
            };

            let depth = if rule.recursive { MAX_WALK_DEPTH } else { 1 };
            walk(&dir, depth, |path, meta| {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if !matcher.is_match(name.as_ref()) {
                    return;
                }
                if let Some(rel) = rel_to(&base, path) {
                    insert(out, token, rel, path, meta, Source::Ufs);
                }
            });
        }
    }

    // ── nguồn 2: remotecache.vdf ─────────────────────────────────────────
    fn collect_remote_cache(
        &self,
        app_id: u32,
        ctx: &RootContext,
        out: &mut BTreeMap<(RootToken, String), SaveFile>,
        warnings: &mut Vec<String>,
    ) {
        let path = self.steam.remotecache_path(self.account_id, app_id);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let cache = match remotecache::parse(&text) {
            Ok(c) => c,
            Err(e) => {
                warnings.push(format!("remotecache.vdf: {e}"));
                return;
            }
        };

        for entry in cache.files {
            let Some(token) = entry.root_token else {
                continue;
            };
            let Some(abs) = entry.absolute(ctx) else {
                continue;
            };
            // Steam liệt kê cả file đã xoá khỏi máy; chỉ lấy cái còn tồn tại.
            let Ok(meta) = std::fs::metadata(&abs) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            insert(
                out,
                token,
                entry.rel_path.clone(),
                &abs,
                &meta,
                Source::RemoteCache,
            );
        }
    }

    // ── nguồn 3: ludusavi-manifest ───────────────────────────────────────
    fn collect_manifest(
        &self,
        app_id: u32,
        ctx: &RootContext,
        install_dir: &Option<PathBuf>,
        out: &mut BTreeMap<(RootToken, String), SaveFile>,
        warnings: &mut Vec<String>,
    ) {
        let Some(manifest) = &self.manifest else {
            return;
        };
        let Some((_name, entry)) = manifest.by_steam_app(app_id) else {
            return;
        };

        let subs = Substitutions {
            os_user_name: std::env::var("USERNAME").ok(),
            store_user_id: Some(self.account_id.to_string()),
            store_game_id: Some(app_id.to_string()),
            game_dir: install_dir
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned()),
        };

        for (raw, file) in &entry.files {
            if !file.is_save() || !file.applies_to_windows_steam() {
                continue;
            }
            let Some(resolved) = ludusavi::resolve_path(raw, &subs) else {
                continue;
            };
            let Some(base) = ctx.resolve(resolved.root) else {
                continue;
            };

            // Đường dẫn manifest có thể chứa `*` / `**` ở bất kỳ đâu, nên ta
            // đi từ phần cố định dài nhất rồi mới khớp glob phần còn lại.
            let (fixed, has_glob) = split_fixed_prefix(&resolved.rel_glob);
            let start = if fixed.is_empty() {
                base.clone()
            } else {
                base.join(&fixed)
            };

            if !has_glob {
                match std::fs::metadata(&start) {
                    Ok(m) if m.is_file() => {
                        insert(
                            out,
                            resolved.root,
                            resolved.rel_glob.clone(),
                            &start,
                            &m,
                            Source::Manifest,
                        );
                    }
                    Ok(m) if m.is_dir() => {
                        walk(&start, MAX_WALK_DEPTH, |p, meta| {
                            if let Some(rel) = rel_to(&base, p) {
                                insert(out, resolved.root, rel, p, meta, Source::Manifest);
                            }
                        });
                    }
                    _ => {}
                }
                continue;
            }

            let Ok(matcher) = compile_glob(&resolved.rel_glob) else {
                warnings.push(format!(
                    "manifest: glob '{}' không hợp lệ",
                    resolved.rel_glob
                ));
                continue;
            };
            let search_root = if start.is_dir() { start } else { base.clone() };
            walk(&search_root, MAX_WALK_DEPTH, |p, meta| {
                let Some(rel) = rel_to(&base, p) else { return };
                if matcher.is_match(&rel) {
                    insert(out, resolved.root, rel, p, meta, Source::Manifest);
                }
            });
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tiện ích
// ─────────────────────────────────────────────────────────────────────────

fn insert(
    out: &mut BTreeMap<(RootToken, String), SaveFile>,
    root: RootToken,
    rel_path: String,
    abs: &Path,
    meta: &std::fs::Metadata,
    source: Source,
) {
    if out.len() >= MAX_FILES_PER_GAME {
        return;
    }
    if !remotecache::is_safe_rel_path(&rel_path) {
        return;
    }
    out.entry((root, rel_path.clone()))
        // Nguồn tin cậy hơn chạy trước, nên đã có thì giữ nguyên.
        .or_insert_with(|| SaveFile {
            root,
            rel_path,
            abs_path: abs.to_path_buf(),
            size: meta.len(),
            mtime: meta.modified().ok().map(DateTime::<Utc>::from),
            source,
        });
}

fn rel_to(base: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    remotecache::is_safe_rel_path(&s).then_some(s)
}

fn walk<F: FnMut(&Path, &std::fs::Metadata)>(dir: &Path, max_depth: usize, mut f: F) {
    for entry in WalkDir::new(dir)
        .max_depth(max_depth)
        // Theo symlink có thể dẫn ra ngoài thư mục save hoặc thành vòng lặp.
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_file() {
            f(entry.path(), &meta);
        }
    }
}

fn compile_glob(pattern: &str) -> std::result::Result<GlobMatcher, globset::Error> {
    Ok(Glob::new(pattern)?.compile_matcher())
}

/// Tách phần đầu không chứa ký tự glob, để không phải quét cả cây thư mục.
/// `"Saves/*/player.dat"` → `("Saves", true)`.
fn split_fixed_prefix(glob: &str) -> (String, bool) {
    let mut fixed = Vec::new();
    let mut has_glob = false;
    for seg in glob.split('/') {
        if seg.contains(['*', '?', '[', '{']) {
            has_glob = true;
            break;
        }
        fixed.push(seg);
    }
    (fixed.join("/"), has_glob)
}

/// Khoá ổn định cho một game, dùng làm `game_slug` trên Supabase.
pub fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let s = out.trim_end_matches('-').to_string();
    if s.is_empty() {
        "unknown-game".into()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Stardew Valley"), "stardew-valley");
        assert_eq!(slugify("DAVE THE DIVER"), "dave-the-diver");
        assert_eq!(slugify("Stellar Blade™"), "stellar-blade");
        assert_eq!(slugify("!!!"), "unknown-game");
    }

    #[test]
    fn fixed_prefix() {
        assert_eq!(
            split_fixed_prefix("Saves/*/player.dat"),
            ("Saves".into(), true)
        );
        assert_eq!(
            split_fixed_prefix("Saves/slot1.sav"),
            ("Saves/slot1.sav".into(), false)
        );
        assert_eq!(split_fixed_prefix("*.sav"), (String::new(), true));
    }
}
