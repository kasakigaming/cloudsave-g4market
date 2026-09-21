//! Client Supabase: GoTrue (đăng nhập) + PostgREST (bảng và RPC).
//!
//! Cố tình viết tay thay vì kéo một SDK: ta chỉ cần vài endpoint, và phần
//! bytea-qua-base64 dù sao cũng phải đi qua RPC tự định nghĩa.

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::blob::{Chunk, Codec};
use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct Config {
    /// vd `https://abcdefgh.supabase.co`
    pub url: String,
    pub anon_key: String,
}

impl Config {
    /// Đọc từ biến môi trường hoặc file `.env` cạnh binary.
    ///
    /// Anon / publishable key là khoá công khai (RLS mới là thứ bảo vệ dữ
    /// liệu), nên để ở đây là đúng. Secret / service-role key thì tuyệt đối
    /// không được có mặt trong app.
    pub fn from_env() -> Result<Self> {
        // Biến môi trường / `.env` lúc chạy trước, rồi mới tới giá trị build.rs
        // nhúng lúc biên dịch (để bản cài không cần `.env` vẫn lên được cloud).
        let url = std::env::var("SUPABASE_URL")
            .ok()
            .or_else(|| option_env!("CS_BUILD_SUPABASE_URL").map(str::to_owned));
        let key = std::env::var("SUPABASE_ANON_KEY")
            .ok()
            .or_else(|| option_env!("CS_BUILD_SUPABASE_ANON_KEY").map(str::to_owned));
        let (Some(url), Some(anon_key)) = (url, key) else {
            return Err(Error::NotConfigured);
        };
        if url.is_empty() || anon_key.is_empty() {
            return Err(Error::NotConfigured);
        }

        // App desktop phát tán `.env` cùng binary, nên một secret key lọt vào
        // đây là trao toàn quyền database cho bất kỳ ai cầm bản build. Nó cũng
        // làm app hỏng ngay: service_role bypass RLS nên `auth.uid()` trả NULL
        // và `snapshots.user_id NOT NULL` fail với 23502. Chặn thẳng, đừng để
        // người dùng phát hiện ra bằng một thông báo lỗi khó hiểu.
        if is_privileged_key(&anon_key) {
            return Err(Error::Other(
                "SUPABASE_ANON_KEY đang chứa secret / service_role key. \
                 Dùng publishable (anon) key — key kia bypass RLS và sẽ bị \
                 phát tán cùng app."
                    .into(),
            ));
        }

        Ok(Self {
            url: url.trim_end_matches('/').to_string(),
            anon_key,
        })
    }
}

/// Giải base64 do Postgres sinh ra.
///
/// `encode(bytea, 'base64')` của Postgres xuất theo RFC 2045 (MIME), tức là
/// **chèn xuống dòng sau mỗi 76 ký tự**. Engine `STANDARD` của crate base64
/// tuân thủ RFC 4648 nên coi `\n` là ký tự lạ và từ chối với
/// `Invalid symbol 10, offset 76`.
///
/// Lọc mọi khoảng trắng trước khi giải. Làm ở phía client thay vì sửa SQL để
/// hàm này chạy đúng với cả database đã triển khai lẫn database mới.
fn decode_pg_base64(s: &str) -> std::result::Result<Vec<u8>, base64::DecodeError> {
    if s.bytes().any(|b| b.is_ascii_whitespace()) {
        let cleaned: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        return B64.decode(cleaned);
    }
    // Đường phổ biến: không có khoảng trắng thì khỏi cấp phát thêm chuỗi mới.
    B64.decode(s)
}

/// Nhận diện khoá có đặc quyền, cả định dạng mới (`sb_secret_…`) lẫn JWT cũ
/// (payload chứa `"role":"service_role"`).
fn is_privileged_key(key: &str) -> bool {
    let k = key.trim();
    if k.starts_with("sb_secret_") || k.starts_with("service_role") {
        return true;
    }
    // JWT cũ: header.payload.signature, payload là base64url không padding.
    let Some(payload) = k.split('.').nth(1) else {
        return false;
    };
    let Ok(raw) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&raw) else {
        return false;
    };
    text.replace(char::is_whitespace, "")
        .contains("\"role\":\"service_role\"")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    pub user_id: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignUpOutcome {
    /// Project tắt xác nhận email: đã có session, đăng nhập luôn.
    SignedIn { session: Session },
    /// Đã gửi thư xác nhận; phải bấm link rồi mới đăng nhập được.
    NeedsConfirmation { email: String },
    /// Email này đã có tài khoản.
    AlreadyRegistered,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    user: TokenUser,
}

#[derive(Debug, Deserialize)]
struct TokenUser {
    id: String,
    email: Option<String>,
}

pub struct Supabase {
    cfg: Config,
    http: Client,
    session: Arc<RwLock<Option<Session>>>,
}

impl Supabase {
    pub fn new(cfg: Config) -> Result<Self> {
        Ok(Self {
            cfg,
            http: Client::builder()
                .user_agent("cloudsave-g4market")
                .timeout(std::time::Duration::from_secs(60))
                .build()?,
            session: Arc::new(RwLock::new(None)),
        })
    }

    pub async fn session(&self) -> Option<Session> {
        self.session.read().await.clone()
    }

    async fn token(&self) -> Result<String> {
        self.session
            .read()
            .await
            .as_ref()
            .map(|s| s.access_token.clone())
            .ok_or(Error::NotAuthenticated)
    }

    // ── Auth ─────────────────────────────────────────────────────────────

    pub async fn sign_in(&self, email: &str, password: &str) -> Result<Session> {
        let res = self
            .http
            .post(format!(
                "{}/auth/v1/token?grant_type=password",
                self.cfg.url
            ))
            .header("apikey", &self.cfg.anon_key)
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await?;

        let tr: TokenResponse = self.decode(res).await?;
        let s = Session {
            access_token: tr.access_token,
            refresh_token: tr.refresh_token,
            user_id: tr.user.id,
            email: tr.user.email,
        };
        *self.session.write().await = Some(s.clone());
        Ok(s)
    }

    /// Đăng ký. Kết quả phụ thuộc cấu hình project:
    ///
    ///   - Tắt "Confirm email" → GoTrue trả luôn session → đăng nhập ngay.
    ///   - Bật → chỉ trả user, phải bấm link trong thư rồi mới đăng nhập được.
    ///     (Máy chủ email có sẵn của Supabase chỉ gửi ~2 thư/giờ cho cả project;
    ///     vượt thì GoTrue trả 429 `over_email_send_rate_limit`.)
    ///   - Email đã có tài khoản mà project bật xác nhận → GoTrue KHÔNG báo lỗi
    ///     (để khỏi lộ email nào đã đăng ký) mà trả một user giả với
    ///     `identities` rỗng. Ta nhận diện ca này để báo đúng cho người dùng.
    pub async fn sign_up(&self, email: &str, password: &str) -> Result<SignUpOutcome> {
        let res = self
            .http
            .post(format!("{}/auth/v1/signup", self.cfg.url))
            .header("apikey", &self.cfg.anon_key)
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await?;
        let v: Value = self.decode(res).await?;

        if let Ok(tr) = serde_json::from_value::<TokenResponse>(v.clone()) {
            let s = Session {
                access_token: tr.access_token,
                refresh_token: tr.refresh_token,
                user_id: tr.user.id,
                email: tr.user.email,
            };
            *self.session.write().await = Some(s.clone());
            return Ok(SignUpOutcome::SignedIn { session: s });
        }

        let identities_empty = v
            .get("identities")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
        if identities_empty {
            return Ok(SignUpOutcome::AlreadyRegistered);
        }
        Ok(SignUpOutcome::NeedsConfirmation {
            email: email.to_string(),
        })
    }

    /// Làm mới access token. Gọi khi PostgREST trả 401 giữa chừng một lần
    /// upload dài — token GoTrue mặc định chỉ sống 1 giờ.
    pub async fn refresh(&self) -> Result<Session> {
        let refresh_token = {
            let g = self.session.read().await;
            g.as_ref()
                .map(|s| s.refresh_token.clone())
                .ok_or(Error::NotAuthenticated)?
        };
        let res = self
            .http
            .post(format!(
                "{}/auth/v1/token?grant_type=refresh_token",
                self.cfg.url
            ))
            .header("apikey", &self.cfg.anon_key)
            .json(&json!({ "refresh_token": refresh_token }))
            .send()
            .await?;
        let tr: TokenResponse = self.decode(res).await?;
        let s = Session {
            access_token: tr.access_token,
            refresh_token: tr.refresh_token,
            user_id: tr.user.id,
            email: tr.user.email,
        };
        *self.session.write().await = Some(s.clone());
        Ok(s)
    }

    pub async fn sign_out(&self) {
        *self.session.write().await = None;
    }

    // ── Lõi HTTP ─────────────────────────────────────────────────────────

    async fn decode<T: for<'de> Deserialize<'de>>(&self, res: reqwest::Response) -> Result<T> {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::Supabase {
                status: status.as_u16(),
                body,
            });
        }
        serde_json::from_str(&body)
            .map_err(|e| Error::Parse(format!("phản hồi Supabase không đọc được: {e} — {body}")))
    }

    /// Gửi một request tới PostgREST, tự refresh token đúng một lần khi gặp 401.
    async fn rest(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        prefer: Option<&str>,
    ) -> Result<Value> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let mut req = self
                .http
                .request(method.clone(), format!("{}/rest/v1/{path}", self.cfg.url))
                .header("apikey", &self.cfg.anon_key)
                .header("Authorization", format!("Bearer {token}"));
            if let Some(p) = prefer {
                req = req.header("Prefer", p);
            }
            if let Some(b) = &body {
                req = req.json(b);
            }

            let res = req.send().await?;
            let status = res.status();

            if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.refresh().await?;
                continue;
            }

            let text = res.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(Error::Supabase {
                    status: status.as_u16(),
                    body: text,
                });
            }
            // 204 No Content khi Prefer: return=minimal.
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text).map_err(Into::into);
        }
        Err(Error::NotAuthenticated)
    }

    pub async fn rpc(&self, func: &str, args: Value) -> Result<Value> {
        self.rest(Method::POST, &format!("rpc/{func}"), Some(args), None)
            .await
    }

    // ── Blob ─────────────────────────────────────────────────────────────

    /// Hỏi trước xem hash nào đã đủ chunk trên server → khỏi upload lại.
    /// Đây là chỗ tiết kiệm nhiều nhất: sao lưu lần thứ hai của cùng một game
    /// thường chỉ có một hai file thực sự đổi.
    pub async fn have_blobs(&self, hashes: &[String]) -> Result<Vec<(String, i64)>> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let v = self
            .rpc("cs_have_blobs", json!({ "p_hashes": hashes }))
            .await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|row| {
                        Some((
                            row.get("hash")?.as_str()?.to_string(),
                            row.get("chunk_count")?.as_i64()?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn put_chunk(&self, hash: &str, chunk: &Chunk) -> Result<()> {
        self.rpc(
            "cs_put_chunk",
            json!({
                "p_hash":  hash,
                "p_idx":   chunk.idx,
                "p_codec": chunk.codec.as_str(),
                "p_b64":   B64.encode(&chunk.data),
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn get_chunk(&self, hash: &str, idx: u32) -> Result<Chunk> {
        let v = self
            .rpc("cs_get_chunk", json!({ "p_hash": hash, "p_idx": idx }))
            .await?;
        let row = v
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| Error::Parse(format!("không tìm thấy chunk {idx} của blob {hash}")))?;

        let codec = Codec::parse(
            row.get("codec")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Parse("chunk thiếu trường codec".into()))?,
        )?;
        let b64 = row
            .get("b64")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Parse("chunk thiếu dữ liệu".into()))?;
        let data = decode_pg_base64(b64)
            .map_err(|e| Error::Parse(format!("base64 hỏng ở chunk {idx}: {e}")))?;

        Ok(Chunk { idx, codec, data })
    }

    // ── Snapshot ─────────────────────────────────────────────────────────

    pub async fn head_snapshot(&self, game_slug: &str) -> Result<Option<Value>> {
        let v = self
            .rpc("cs_head", json!({ "p_game_slug": game_slug }))
            .await?;
        Ok(v.as_array().and_then(|a| a.first()).cloned())
    }

    pub async fn insert_snapshot(&self, row: Value) -> Result<String> {
        let v = self
            .rest(
                Method::POST,
                "snapshots",
                Some(json!([row])),
                Some("return=representation"),
            )
            .await?;
        v.as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::Parse("không nhận được id snapshot".into()))
    }

    pub async fn insert_files(&self, rows: Vec<Value>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        // Chèn theo lô để một game nhiều file không tạo ra một request khổng lồ.
        for batch in rows.chunks(200) {
            self.rest(
                Method::POST,
                "snapshot_files",
                Some(Value::Array(batch.to_vec())),
                Some("return=minimal"),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn finalize_snapshot(&self, id: &str, preview: Option<Value>) -> Result<()> {
        let mut patch = json!({
            "status": "complete",
            "completed_at": chrono::Utc::now().to_rfc3339(),
        });
        if let Some(p) = preview {
            patch["preview"] = p;
        }
        self.rest(
            Method::PATCH,
            &format!("snapshots?id=eq.{id}"),
            Some(patch),
            Some("return=minimal"),
        )
        .await?;
        Ok(())
    }

    pub async fn delete_snapshot(&self, id: &str) -> Result<()> {
        self.rest(
            Method::DELETE,
            &format!("snapshots?id=eq.{id}"),
            None,
            Some("return=minimal"),
        )
        .await?;
        Ok(())
    }

    pub async fn list_snapshots(&self, game_slug: Option<&str>) -> Result<Value> {
        let mut q = String::from(
            "snapshots?select=id,game_slug,game_title,steam_appid,device_name,device_id,\
             file_count,total_bytes,preview,status,label,created_at,parent_id\
             &status=eq.complete&order=created_at.desc&limit=200",
        );
        if let Some(slug) = game_slug {
            q.push_str(&format!("&game_slug=eq.{slug}"));
        }
        self.rest(Method::GET, &q, None, None).await
    }

    pub async fn snapshot_files(&self, snapshot_id: &str) -> Result<Value> {
        self.rest(
            Method::GET,
            &format!(
                "snapshot_files?select=root_token,rel_path,size,mtime_utc,blob_hash,chunk_count\
                 &snapshot_id=eq.{snapshot_id}&order=rel_path.asc"
            ),
            None,
            None,
        )
        .await
    }

    /// Dọn blob không còn snapshot nào tham chiếu.
    pub async fn gc(&self) -> Result<i64> {
        let v = self.rpc("cs_gc_blobs", json!({})).await?;
        Ok(v.as_i64().unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt_with_role(role: &str) -> String {
        let payload = format!(r#"{{"iss":"supabase","role":"{role}","exp":9999999999}}"#);
        format!(
            "eyJhbGciOiJIUzI1NiJ9.{}.c2ln",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload)
        )
    }

    #[test]
    fn rejects_new_style_secret_key() {
        assert!(is_privileged_key("sb_secret_FAKEexample0000"));
    }

    #[test]
    fn accepts_new_style_publishable_key() {
        assert!(!is_privileged_key("sb_publishable_aRJxYz"));
    }

    #[test]
    fn rejects_legacy_service_role_jwt() {
        assert!(is_privileged_key(&jwt_with_role("service_role")));
    }

    #[test]
    fn accepts_legacy_anon_jwt() {
        assert!(!is_privileged_key(&jwt_with_role("anon")));
    }

    #[test]
    fn decodes_postgres_mime_wrapped_base64() {
        // Postgres xuống dòng mỗi 76 ký tự. Đây đúng là dạng chuỗi đã làm
        // hỏng lần khôi phục đầu tiên với "Invalid symbol 10, offset 76".
        let data: Vec<u8> = (0..200u16).map(|i| (i % 251) as u8).collect();
        let flat = B64.encode(&data);
        assert!(flat.len() > 76, "mẫu phải đủ dài để bị ngắt dòng");

        let wrapped = flat
            .as_bytes()
            .chunks(76)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(wrapped.contains('\n'));

        assert_eq!(decode_pg_base64(&wrapped).unwrap(), data);
        assert_eq!(decode_pg_base64(&flat).unwrap(), data);
        // Xuống dòng ở cuối cũng phải chấp nhận.
        assert_eq!(decode_pg_base64(&format!("{wrapped}\n")).unwrap(), data);
        // Kiểu CRLF, phòng khi đi qua một tầng trung gian nào đó.
        assert_eq!(
            decode_pg_base64(&wrapped.replace('\n', "\r\n")).unwrap(),
            data
        );
    }

    #[test]
    fn decodes_empty_and_rejects_invalid() {
        assert_eq!(decode_pg_base64("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_pg_base64("   \n ").unwrap(), Vec::<u8>::new());
        assert!(decode_pg_base64("!!!not base64!!!").is_err());
    }

    #[test]
    fn tolerates_garbage_without_panicking() {
        for k in ["", "not-a-key", "a.b", "a.!!!.c", "....."] {
            let _ = is_privileged_key(k);
        }
    }
}
