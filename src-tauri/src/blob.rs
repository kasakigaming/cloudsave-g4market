//! Băm, nén và cắt chunk nội dung file save.
//!
//! Đây là tầng quyết định tính đúng đắn của toàn bộ app: **bytes là nguồn chân
//! lý duy nhất**. Ta không bao giờ diễn giải nội dung save để dựng lại nó —
//! save game thường có checksum nội bộ, mã hoá theo máy, hoặc padding, nên chỉ
//! cần sai một byte là game từ chối nạp. Mọi thứ ở đây phải lossless tuyệt đối.

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Cắt theo plaintext 512 KiB. Đủ nhỏ để một chunk base64 (~700 KB) lọt giới
/// hạn request của Supabase, đủ lớn để không tạo hàng nghìn round-trip.
pub const CHUNK_SIZE: usize = 512 * 1024;

const ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Codec {
    Raw,
    Zstd,
    /// Lát cắt của một luồng nén cả file (định dạng mới, xem `pack.rs`). Một
    /// lát riêng lẻ không giải được — phải ghép đủ rồi giải cả luồng.
    Part,
}

impl Codec {
    pub fn as_str(&self) -> &'static str {
        match self {
            Codec::Raw => "raw",
            Codec::Zstd => "zstd",
            Codec::Part => "part",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "raw" => Ok(Codec::Raw),
            "zstd" => Ok(Codec::Zstd),
            "part" => Ok(Codec::Part),
            other => Err(Error::Parse(format!("codec không nhận ra: {other}"))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub idx: u32,
    pub codec: Codec,
    /// Đã nén nếu `codec == Zstd`.
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Prepared {
    /// SHA-256 hex của **toàn bộ nội dung gốc**, trước khi nén. Đây là khoá
    /// content-addressed: cùng nội dung ở nhiều snapshot chỉ lưu một lần.
    pub hash: String,
    pub plain_size: u64,
    pub chunks: Vec<Chunk>,
}

/// Số chunk mà `prepare` sẽ sinh ra cho một file `size` byte, tính mà không
/// cần đọc file. Dùng để hỏi server "blob này đã đủ chunk chưa" trước khi đọc.
pub fn chunk_count(size: u64) -> u32 {
    (size.div_ceil(CHUNK_SIZE as u64)).max(1) as u32
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Chuẩn bị nội dung một file để upload.
pub fn prepare(bytes: &[u8]) -> Prepared {
    let hash = sha256_hex(bytes);

    let mut chunks = Vec::new();
    for (idx, piece) in bytes.chunks(CHUNK_SIZE).enumerate() {
        chunks.push(compress_piece(idx as u32, piece));
    }

    // File rỗng vẫn cần một chunk, nếu không `assemble` không có gì để ghép
    // và ta mất mất chính cái file rỗng đó.
    if chunks.is_empty() {
        chunks.push(Chunk {
            idx: 0,
            codec: Codec::Raw,
            data: Vec::new(),
        });
    }

    Prepared {
        hash,
        plain_size: bytes.len() as u64,
        chunks,
    }
}

fn compress_piece(idx: u32, piece: &[u8]) -> Chunk {
    match zstd::encode_all(piece, ZSTD_LEVEL) {
        // Nhiều save đã nén sẵn (zip, png, dữ liệu mã hoá). Nén lại chỉ làm
        // phình ra, nên giữ raw khi không lợi.
        Ok(z) if z.len() < piece.len() => Chunk {
            idx,
            codec: Codec::Zstd,
            data: z,
        },
        _ => Chunk {
            idx,
            codec: Codec::Raw,
            data: piece.to_vec(),
        },
    }
}

/// Ghép các chunk (đã sắp theo `idx`) trở lại nội dung gốc và **kiểm chứng
/// checksum**. Không bao giờ ghi ra đĩa thứ chưa qua hàm này.
pub fn assemble(mut chunks: Vec<Chunk>, expected_hash: &str) -> Result<Vec<u8>> {
    chunks.sort_by_key(|c| c.idx);

    // Thiếu hay trùng chunk nghĩa là upload dở dang. Phát hiện ở đây rẻ hơn
    // nhiều so với việc ghi đè một save tốt bằng dữ liệu cụt.
    for (want, c) in chunks.iter().enumerate() {
        if c.idx as usize != want {
            return Err(Error::Parse(format!(
                "thiếu chunk {want} của blob {expected_hash}"
            )));
        }
    }

    let mut out = Vec::new();
    for c in &chunks {
        match c.codec {
            Codec::Raw => out.extend_from_slice(&c.data),
            // Lát cắt định dạng mới lọt vào đường giải kiểu cũ là dữ liệu bị
            // nhầm định dạng; báo lỗi rõ ràng thay vì để sha256 báo sai sau đó.
            Codec::Part => {
                return Err(Error::Parse(format!(
                    "blob {expected_hash} là định dạng mới, không giải từng chunk được"
                )))
            }
            Codec::Zstd => {
                let d = zstd::decode_all(c.data.as_slice())
                    .map_err(|e| Error::Parse(format!("giải nén chunk {}: {e}", c.idx)))?;
                out.extend_from_slice(&d);
            }
        }
    }

    let actual = sha256_hex(&out);
    if actual != expected_hash {
        return Err(Error::ChecksumMismatch {
            expected: expected_hash.to_string(),
            actual,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(data: &[u8]) {
        let p = prepare(data);
        let got = assemble(p.chunks.clone(), &p.hash).unwrap();
        assert_eq!(got, data, "round-trip phải byte-exact");
        assert_eq!(p.plain_size, data.len() as u64);
    }

    #[test]
    fn chunk_count_matches_prepare() {
        for size in [
            0usize,
            1,
            CHUNK_SIZE - 1,
            CHUNK_SIZE,
            CHUNK_SIZE + 1,
            CHUNK_SIZE * 3,
        ] {
            let data = vec![7u8; size];
            assert_eq!(
                chunk_count(size as u64) as usize,
                prepare(&data).chunks.len(),
                "size {size}"
            );
        }
    }

    #[test]
    fn roundtrips_empty() {
        roundtrip(b"");
    }

    #[test]
    fn roundtrips_small_text() {
        roundtrip(b"{\"level\":42,\"gold\":1000}");
    }

    #[test]
    fn roundtrips_multi_chunk_binary() {
        // Dữ liệu giả nhị phân, dài hơn nhiều chunk, có cả vùng lặp lẫn vùng
        // gần ngẫu nhiên để thử cả hai nhánh raw/zstd.
        let mut data = Vec::new();
        for i in 0..(CHUNK_SIZE * 2 + 1234) {
            data.push(((i * 31 + i / 7) % 251) as u8);
        }
        roundtrip(&data);
        assert!(prepare(&data).chunks.len() >= 3);
    }

    #[test]
    fn roundtrips_incompressible() {
        // Mô phỏng save đã nén sẵn: byte gần như không lặp.
        let data: Vec<u8> = (0..200_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 16) as u8)
            .collect();
        roundtrip(&data);
    }

    #[test]
    fn detects_missing_chunk() {
        let mut data = vec![0u8; CHUNK_SIZE * 2];
        data[CHUNK_SIZE] = 9;
        let p = prepare(&data);
        let mut broken = p.chunks.clone();
        broken.remove(0);
        assert!(assemble(broken, &p.hash).is_err());
    }

    #[test]
    fn detects_corrupted_content() {
        let p = prepare(b"nguyen ven");
        let wrong = sha256_hex(b"khac roi");
        assert!(matches!(
            assemble(p.chunks, &wrong),
            Err(Error::ChecksumMismatch { .. })
        ));
    }
}
