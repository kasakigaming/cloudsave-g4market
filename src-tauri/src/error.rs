use serde::{Serialize, Serializer};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("không tìm thấy cài đặt Steam trên máy này")]
    SteamNotFound,

    #[error("lỗi đọc dữ liệu: {0}")]
    Parse(String),

    #[error("lỗi I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("lỗi mạng: {0}")]
    Http(#[from] reqwest::Error),

    #[error("máy chủ cloud trả lỗi {status}: {body}")]
    Supabase { status: u16, body: String },

    #[error("chưa đăng nhập cloud")]
    NotAuthenticated,

    #[error("bản này chưa bật cloud")]
    NotConfigured,

    /// Snapshot định upload không nối tiếp head hiện tại: hai máy cùng sửa.
    /// Save game không merge được nên ta dừng và để người dùng quyết định.
    #[error("xung đột: máy '{other_device}' đã tạo bản sao lưu mới hơn")]
    Conflict {
        other_device: String,
        remote_head: String,
    },

    #[error("dữ liệu tải về không khớp checksum (mong đợi {expected}, nhận {actual})")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("{0}")]
    Other(String),
}

/// Tauri cần lỗi serialize được để trả về cho frontend.
impl Serialize for Error {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Parse(format!("JSON: {e}"))
    }
}

impl From<serde_yaml::Error> for Error {
    fn from(e: serde_yaml::Error) -> Self {
        Error::Parse(format!("YAML: {e}"))
    }
}
