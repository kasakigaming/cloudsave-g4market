//! Đưa blob lên và lấy blob về từ Supabase, cho cả hai định dạng.
//!
//!   - **Kiểu cũ**: chunk 512 KiB nén riêng lẻ, không có dòng `blobs`.
//!   - **Kiểu mới** (`pack.rs`): một luồng nén cả file, cắt thành lát
//!     `codec = 'part'`, kèm dòng `blobs` ghi kiểu mã hoá và bản gốc delta.
//!
//! Lấy về một blob delta nghĩa là đi ngược chuỗi tới một bản đầy đủ (hoặc một
//! blob kiểu cũ), rồi giải xuôi từng mắt. Mọi mắt đều được kiểm sha256 — một
//! mắt sai là dừng, không bao giờ trả dữ liệu hỏng lên tầng trên.

use std::collections::HashMap;

use crate::blob::{self, Chunk, Codec, CHUNK_SIZE};
use crate::error::{Error, Result};
use crate::pack::{self, Packed, MAX_DEPTH};
use crate::supabase::{BlobMeta, Supabase};

/// Upload một blob đã mã hoá. Trả về số lát đã gửi.
///
/// Thứ tự cố định: xoá lát mồ côi → gửi lát → ghi metadata. Có dòng metadata
/// nghĩa là blob hoàn chỉnh; đứt ở bất kỳ bước nào trước đó chỉ để lại lát cắt
/// mồ côi, sẽ bị dọn ở lần upload sau hoặc bởi `cs_gc_blobs`.
pub async fn upload(sb: &Supabase, hash: &str, packed: &Packed) -> Result<u32> {
    sb.reset_blob(hash).await?;

    let pieces: Vec<&[u8]> = if packed.bytes.is_empty() {
        vec![&[][..]]
    } else {
        packed.bytes.chunks(CHUNK_SIZE).collect()
    };
    for (idx, piece) in pieces.iter().enumerate() {
        let chunk = Chunk {
            idx: idx as u32,
            codec: Codec::Part,
            data: piece.to_vec(),
        };
        sb.put_chunk(hash, &chunk).await?;
    }

    let n = pieces.len() as u32;
    sb.put_blob(hash, packed, n).await?;
    Ok(n)
}

/// Bộ nhớ đệm nội dung gốc đã giải trong một lần khôi phục. Nhiều file cùng
/// chung tổ tiên delta thì mỗi tổ tiên chỉ tải và giải một lần.
#[derive(Default)]
pub struct Cache {
    plain: HashMap<String, Vec<u8>>,
}

/// Lấy nội dung gốc của một blob, bất kể định dạng.
///
/// `legacy_chunks` là số chunk kiểu cũ nếu đã biết (từ `snapshot_files`); để
/// `None` thì hỏi server.
pub async fn fetch(
    sb: &Supabase,
    hash: &str,
    legacy_chunks: Option<u32>,
    cache: &mut Cache,
) -> Result<Vec<u8>> {
    if let Some(p) = cache.plain.get(hash) {
        return Ok(p.clone());
    }

    // ── Đi ngược chuỗi ───────────────────────────────────────────────────
    // `chain[0]` là blob cần lấy; phần tử cuối là mắt dưới cùng cần giải trước.
    let mut chain: Vec<BlobMeta> = Vec::new();
    let mut floor: Option<Vec<u8>> = None; // nội dung gốc của mắt ngay dưới chuỗi
    let mut cur = hash.to_string();

    loop {
        if let Some(p) = cache.plain.get(&cur) {
            floor = Some(p.clone());
            break;
        }
        // Chuỗi dài hơn giới hạn nghĩa là dữ liệu trên server bất thường (vd
        // vòng lặp). Server đã chặn khi ghi, đây là lớp phòng vệ thứ hai.
        if chain.len() as u32 > MAX_DEPTH + 2 {
            return Err(Error::Parse(format!(
                "chuỗi delta của blob {hash} dài bất thường"
            )));
        }

        let meta = sb.blob_meta(std::slice::from_ref(&cur)).await?.remove(&cur);
        match meta {
            None => {
                // Blob kiểu cũ: luôn là đáy chuỗi.
                let hint = if cur == hash { legacy_chunks } else { None };
                let plain = fetch_legacy(sb, &cur, hint).await?;
                cache.plain.insert(cur.clone(), plain.clone());
                floor = Some(plain);
                break;
            }
            Some(m) => {
                let next = m.base_hash.clone();
                // Mọi kiểu trừ delta đều tự giải được — đó là đáy chuỗi.
                let is_full = !m.encoding.is_delta();
                chain.push(m);
                if is_full {
                    break;
                }
                cur =
                    next.ok_or_else(|| Error::Parse(format!("blob delta {cur} không có bản gốc")))?;
            }
        }
    }

    // ── Giải xuôi từ đáy lên ─────────────────────────────────────────────
    let mut below = floor;
    for m in chain.iter().rev() {
        let stream = fetch_parts(sb, &m.hash, m.chunk_count).await?;
        let plain = pack::unpack(m.encoding, &stream, below.as_deref())?;
        let actual = blob::sha256_hex(&plain);
        if actual != m.hash {
            return Err(Error::ChecksumMismatch {
                expected: m.hash.clone(),
                actual,
            });
        }
        cache.plain.insert(m.hash.clone(), plain.clone());
        below = Some(plain);
    }

    below.ok_or_else(|| Error::Parse(format!("không dựng lại được blob {hash}")))
}

async fn fetch_parts(sb: &Supabase, hash: &str, n: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for idx in 0..n {
        let c = sb.get_chunk(hash, idx).await?;
        if c.codec != Codec::Part {
            return Err(Error::Parse(format!(
                "blob {hash}: chunk {idx} không phải lát cắt định dạng mới"
            )));
        }
        out.extend_from_slice(&c.data);
    }
    Ok(out)
}

async fn fetch_legacy(sb: &Supabase, hash: &str, hint: Option<u32>) -> Result<Vec<u8>> {
    let n = match hint {
        Some(n) => n,
        None => sb
            .have_blobs(&[hash.to_string()])
            .await?
            .into_iter()
            .find(|(h, _)| h == hash)
            .map(|(_, n)| n as u32)
            .ok_or_else(|| Error::Other(format!("blob {hash} không có trên cloud")))?,
    };
    let mut chunks = Vec::with_capacity(n as usize);
    for idx in 0..n {
        chunks.push(sb.get_chunk(hash, idx).await?);
    }
    // `assemble` kiểm sha256 và thiếu chunk.
    blob::assemble(chunks, hash)
}
