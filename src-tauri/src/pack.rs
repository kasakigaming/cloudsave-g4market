//! Mã hoá blob để lưu trên cloud — nén tới giới hạn thực tế.
//!
//! Đo trên save thật (Stardew, Stellar Blade, DAVE THE DIVER, Elden Ring,
//! Nightreign), **không có thuật toán nào thắng mọi file**:
//!
//! | File                       | Thắng      | So với zstd-19 |
//! |----------------------------|------------|----------------|
//! | Stardew main (XML 3,7 MB)  | xz -9e     | −10%           |
//! | Stardew info (XML 91 KB)   | brotli-11  | −10%           |
//! | DAVE THE DIVER PZ (676 KB) | bzip2 -9   | −28%           |
//! | Elden Ring (29 MB)         | ngang nhau | ~0%            |
//! | Nightreign (19,5 MB, mã hoá)| lưu thô   | không nén được |
//!
//! Nên mỗi file được thử **song song** bằng mọi cách dưới đây, giữ cái nhỏ nhất:
//!
//!   - Độc lập: `zstd` (22), `xz` (-9e), `brotli` (11, cửa sổ 16 MiB), `bzip2`
//!     (-9), và `store` (thô) cho file không nén được.
//!   - **Delta** `zstd-delta` (22, tương đương `zstd --patch-from`) so với từng
//!     bản gốc ứng viên: phiên bản trước của chính file đó, và các file anh em
//!     trong cùng snapshot. Đo: save Stardew hôm nay so với hôm qua 119,7 KB →
//!     3,4 KB; SaveGameInfo dùng save chính làm tham chiếu 10,2 KB → **46 B**.
//!
//! Kết quả được chọn luôn được **giải ngược lại và so từng byte** với bản gốc
//! trước khi chấp nhận; sai thì loại và lấy cách tốt kế tiếp. Một lỗi trong
//! bất kỳ thư viện nén nào vì vậy không thể biến thành một bản sao lưu hỏng.
//!
//! Delta tạo thành chuỗi; chuỗi bị giới hạn ở `MAX_DEPTH` để khôi phục không
//! bao giờ phải tải quá `MAX_DEPTH + 1` blob.

use std::io::{Read, Write};

use crate::error::{Error, Result};

pub const ZSTD_LEVEL: i32 = 22;
/// Độ sâu tối đa của chuỗi delta. Bản độc lập có độ sâu 0.
pub const MAX_DEPTH: u32 = 8;
/// Cửa sổ zstd tối đa. Giới hạn 64-bit là 31; 30 là dư cho mọi save hợp lý
/// (file > 64 MB đã bị loại từ lúc chụp).
const MAX_WINDOW_LOG: u32 = 30;
/// Bằng hằng `LZMA_PRESET_EXTREME` của liblzma.
const XZ_PRESET: u32 = 9 | 0x8000_0000;
/// Từ điển xz lớn nhất (bằng preset 9). Từ điển lớn hơn file là phí bộ nhớ
/// mà không lợi byte nào, nên thực tế dùng lũy thừa 2 kế tiếp của kích thước.
const XZ_MAX_DICT: u32 = 64 * 1024 * 1024;
/// File lớn hơn mức này thì thử lần lượt từng codec thay vì song song, để bộ
/// nhớ đỉnh không nhân lên theo số codec (một bộ nén zstd-22 hay xz -9e cho
/// file vài chục MB có thể chiếm vài trăm MB).
const PARALLEL_LIMIT: usize = 8 * 1024 * 1024;
const BROTLI_QUALITY: u32 = 11;
/// 24 là cửa sổ lớn nhất của brotli chuẩn (16 MiB).
const BROTLI_LGWIN: u32 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    Store,
    Zstd,
    Xz,
    Brotli,
    Bzip2,
    ZstdDelta,
}

impl Encoding {
    pub fn as_str(self) -> &'static str {
        match self {
            Encoding::Store => "store",
            Encoding::Zstd => "zstd",
            Encoding::Xz => "xz",
            Encoding::Brotli => "brotli",
            Encoding::Bzip2 => "bzip2",
            Encoding::ZstdDelta => "zstd-delta",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "store" => Encoding::Store,
            "zstd" => Encoding::Zstd,
            "xz" => Encoding::Xz,
            "brotli" => Encoding::Brotli,
            "bzip2" => Encoding::Bzip2,
            "zstd-delta" => Encoding::ZstdDelta,
            other => {
                return Err(Error::Parse(format!(
                    "kiểu mã hoá blob không nhận ra: {other}"
                )))
            }
        })
    }

    pub fn is_delta(self) -> bool {
        self == Encoding::ZstdDelta
    }
}

/// Một bản gốc ứng viên cho delta.
pub struct Base<'a> {
    pub hash: &'a str,
    pub plain: &'a [u8],
    pub depth: u32,
}

#[derive(Debug, Clone)]
pub struct Packed {
    pub encoding: Encoding,
    pub base_hash: Option<String>,
    pub depth: u32,
    pub bytes: Vec<u8>,
}

/// Mã hoá một file bằng cách nhỏ nhất trong mọi cách được phép.
pub fn pack(plain: &[u8], bases: &[Base<'_>]) -> Result<Packed> {
    // Thăm dò nhanh: file đã mã hoá / đã nén sẵn thì các codec độc lập nặng
    // chỉ tốn CPU mà không lợi byte nào (Nightreign: 100,00% ở mọi codec).
    let incompressible = zstd::bulk::compress(plain, 1)
        .map(|p| p.len() as u64 * 100 >= plain.len() as u64 * 99)
        .unwrap_or(false);

    let usable_bases: Vec<&Base<'_>> = bases.iter().filter(|b| b.depth < MAX_DEPTH).collect();

    // Mỗi ứng viên là một việc độc lập. Việc nào lỗi thì chỉ mất ứng viên đó
    // — `store` luôn có.
    type Job<'j> = Box<dyn FnOnce() -> Option<Packed> + Send + 'j>;
    let mut jobs: Vec<Job<'_>> = Vec::new();
    if !incompressible {
        for enc in [
            Encoding::Zstd,
            Encoding::Xz,
            Encoding::Brotli,
            Encoding::Bzip2,
        ] {
            jobs.push(Box::new(move || {
                encode(enc, plain).ok().map(|bytes| Packed {
                    encoding: enc,
                    base_hash: None,
                    depth: 0,
                    bytes,
                })
            }));
        }
    }
    for b in &usable_bases {
        let (hash, base, depth) = (b.hash, b.plain, b.depth);
        jobs.push(Box::new(move || {
            encode_delta(plain, base).ok().map(|bytes| Packed {
                encoding: Encoding::ZstdDelta,
                base_hash: Some(hash.to_string()),
                depth: depth + 1,
                bytes,
            })
        }));
    }

    let mut candidates: Vec<Packed> = if plain.len() <= PARALLEL_LIMIT {
        std::thread::scope(|s| {
            let handles: Vec<_> = jobs.into_iter().map(|j| s.spawn(j)).collect();
            handles
                .into_iter()
                .filter_map(|h| h.join().ok().flatten())
                .collect()
        })
    } else {
        jobs.into_iter().filter_map(|j| j()).collect()
    };
    candidates.push(Packed {
        encoding: Encoding::Store,
        base_hash: None,
        depth: 0,
        bytes: plain.to_vec(),
    });

    // Nhỏ nhất thắng. Bằng nhau thì ưu tiên bản độc lập, rồi chuỗi ngắn hơn —
    // mỗi mắt delta là thêm một thứ phải còn nguyên lúc khôi phục.
    candidates.sort_by_key(|c| (c.bytes.len(), c.encoding.is_delta(), c.depth));

    for c in candidates {
        let base = c
            .base_hash
            .as_deref()
            .and_then(|h| usable_bases.iter().find(|b| b.hash == h))
            .map(|b| b.plain);
        match unpack(c.encoding, &c.bytes, base) {
            Ok(out) if out == plain => return Ok(c),
            _ => log::error!(
                "pack: {} cho kết quả không khớp khi giải ngược, loại",
                c.encoding.as_str()
            ),
        }
    }
    // Không thể xảy ra: `store` luôn giải ngược đúng.
    Err(Error::Other("không mã hoá được blob".into()))
}

pub fn unpack(encoding: Encoding, bytes: &[u8], base: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    match encoding {
        Encoding::Store => out.extend_from_slice(bytes),
        Encoding::Zstd => {
            let mut d = zstd::stream::read::Decoder::new(bytes).map_err(io)?;
            d.window_log_max(MAX_WINDOW_LOG).map_err(io)?;
            d.read_to_end(&mut out).map_err(io)?;
        }
        Encoding::Xz => {
            xz2::read::XzDecoder::new(bytes)
                .read_to_end(&mut out)
                .map_err(io)?;
        }
        Encoding::Brotli => {
            brotli::Decompressor::new(bytes, 64 * 1024)
                .read_to_end(&mut out)
                .map_err(io)?;
        }
        Encoding::Bzip2 => {
            bzip2::read::MultiBzDecoder::new(bytes)
                .read_to_end(&mut out)
                .map_err(io)?;
        }
        Encoding::ZstdDelta => {
            let base = base.ok_or_else(|| Error::Parse("blob delta thiếu bản gốc".into()))?;
            let mut d = zstd::stream::read::Decoder::with_ref_prefix(bytes, base).map_err(io)?;
            d.window_log_max(MAX_WINDOW_LOG).map_err(io)?;
            d.read_to_end(&mut out).map_err(io)?;
        }
    }
    Ok(out)
}

/// Cửa sổ đủ bao trọn mọi dữ liệu cần tham chiếu. Thiếu cửa sổ thì
/// long-distance matching không với tới phần đầu, và delta mất tác dụng với
/// file lớn.
fn window_for(total: usize) -> u32 {
    let bits = usize::BITS - total.max(1).leading_zeros();
    (bits + 1).clamp(20, MAX_WINDOW_LOG)
}

fn encode(enc: Encoding, plain: &[u8]) -> Result<Vec<u8>> {
    match enc {
        Encoding::Store => Ok(plain.to_vec()),
        Encoding::Zstd => {
            let mut e = zstd::stream::write::Encoder::new(Vec::new(), ZSTD_LEVEL).map_err(io)?;
            // Báo trước kích thước: zstd chọn cửa sổ và bảng vừa đủ cho file
            // này thay vì bảng ~640 MB mặc định của mức 22 — cùng tỷ lệ nén,
            // bộ nhớ nhỏ hơn hàng chục lần với file nhỏ.
            e.set_pledged_src_size(Some(plain.len() as u64))
                .map_err(io)?;
            e.write_all(plain).map_err(io)?;
            e.finish().map_err(io)
        }
        Encoding::Xz => {
            // Preset 9e nhưng từ điển vừa đủ bao file: bộ nhớ nén ~10× từ điển,
            // nên từ điển 64 MiB cố định là ~674 MB cho cả file vài KB.
            let dict = (plain.len() as u32)
                .max(64 * 1024)
                .checked_next_power_of_two()
                .unwrap_or(XZ_MAX_DICT)
                .min(XZ_MAX_DICT);
            let mut opts = xz2::stream::LzmaOptions::new_preset(XZ_PRESET)
                .map_err(|e| Error::Other(format!("xz: {e}")))?;
            opts.dict_size(dict);
            let mut filters = xz2::stream::Filters::new();
            filters.lzma2(&opts);
            // Không cần CRC của xz: mọi blob đã được kiểm sha256 lúc khôi phục.
            let stream =
                xz2::stream::Stream::new_stream_encoder(&filters, xz2::stream::Check::None)
                    .map_err(|e| Error::Other(format!("xz: {e}")))?;
            let mut e = xz2::write::XzEncoder::new_stream(Vec::new(), stream);
            e.write_all(plain).map_err(io)?;
            e.finish().map_err(io)
        }
        Encoding::Brotli => {
            let mut e =
                brotli::CompressorWriter::new(Vec::new(), 64 * 1024, BROTLI_QUALITY, BROTLI_LGWIN);
            e.write_all(plain).map_err(io)?;
            e.flush().map_err(io)?;
            Ok(e.into_inner())
        }
        Encoding::Bzip2 => {
            let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
            e.write_all(plain).map_err(io)?;
            e.finish().map_err(io)
        }
        Encoding::ZstdDelta => Err(Error::Other("delta cần bản gốc".into())),
    }
}

fn encode_delta(plain: &[u8], base: &[u8]) -> Result<Vec<u8>> {
    let mut e =
        zstd::stream::write::Encoder::with_ref_prefix(Vec::new(), ZSTD_LEVEL, base).map_err(io)?;
    e.set_pledged_src_size(Some(plain.len() as u64))
        .map_err(io)?;
    // Cửa sổ phải bao trọn cả bản gốc lẫn bản mới, nếu không bản mới không
    // với tới được phần đầu của bản gốc và delta mất tác dụng với file lớn.
    e.long_distance_matching(true).map_err(io)?;
    e.window_log(window_for(base.len() + plain.len()))
        .map_err(io)?;
    e.write_all(plain).map_err(io)?;
    e.finish().map_err(io)
}

fn io(e: std::io::Error) -> Error {
    Error::Parse(format!("nén/giải nén: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::sha256_hex;

    /// Giả một save XML: phần lớn nội dung giữ nguyên giữa hai ngày, chỉ vài
    /// giá trị đổi — đúng như save thật.
    fn fake_save(day: u32, money: u32) -> Vec<u8> {
        let mut s = String::from("<?xml version=\"1.0\"?><SaveGame>");
        s.push_str(&format!("<day>{day}</day><money>{money}</money>"));
        for i in 0..5_000u32 {
            s.push_str(&format!(
                "<item id=\"{i}\"><name>Parsnip Seeds {}</name><stack>{}</stack></item>",
                i % 37,
                if i % 1000 == 0 {
                    (i + day) % 99
                } else {
                    (i * 7) % 99
                }
            ));
        }
        s.push_str("</SaveGame>");
        s.into_bytes()
    }

    fn noise(seed: u32, n: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761) | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn every_codec_roundtrips_exactly() {
        let data = fake_save(3, 1234);
        for enc in [
            Encoding::Store,
            Encoding::Zstd,
            Encoding::Xz,
            Encoding::Brotli,
            Encoding::Bzip2,
        ] {
            let bytes = encode(enc, &data).unwrap();
            assert_eq!(unpack(enc, &bytes, None).unwrap(), data, "{}", enc.as_str());
        }
    }

    #[test]
    fn picks_the_smallest_candidate() {
        let data = fake_save(3, 1234);
        let p = pack(&data, &[]).unwrap();
        for enc in [
            Encoding::Zstd,
            Encoding::Xz,
            Encoding::Brotli,
            Encoding::Bzip2,
        ] {
            let other = encode(enc, &data).unwrap();
            assert!(
                p.bytes.len() <= other.len(),
                "chọn {} ({} B) nhưng {} chỉ {} B",
                p.encoding.as_str(),
                p.bytes.len(),
                enc.as_str(),
                other.len()
            );
        }
        assert_eq!(unpack(p.encoding, &p.bytes, None).unwrap(), data);
    }

    #[test]
    fn delta_against_previous_version_wins_and_roundtrips() {
        let old = fake_save(1, 500);
        let new = fake_save(2, 750);
        let h = sha256_hex(&old);
        let p = pack(
            &new,
            &[Base {
                hash: &h,
                plain: &old,
                depth: 0,
            }],
        )
        .unwrap();
        assert_eq!(p.encoding, Encoding::ZstdDelta);
        assert_eq!(p.base_hash.as_deref(), Some(h.as_str()));
        assert_eq!(p.depth, 1);
        assert_eq!(unpack(p.encoding, &p.bytes, Some(&old)).unwrap(), new);
        assert!(p.bytes.len() * 3 < pack(&new, &[]).unwrap().bytes.len());
    }

    #[test]
    fn best_of_several_bases_is_chosen() {
        // Bản gốc thứ nhất vô dụng (nhiễu), bản thứ hai là phiên bản trước.
        let new = fake_save(2, 750);
        let junk = noise(7, 100_000);
        let prev = fake_save(1, 500);
        let (hj, hp) = (sha256_hex(&junk), sha256_hex(&prev));
        let p = pack(
            &new,
            &[
                Base {
                    hash: &hj,
                    plain: &junk,
                    depth: 0,
                },
                Base {
                    hash: &hp,
                    plain: &prev,
                    depth: 2,
                },
            ],
        )
        .unwrap();
        assert_eq!(p.base_hash.as_deref(), Some(hp.as_str()));
        assert_eq!(p.depth, 3);
    }

    #[test]
    fn sibling_substring_compresses_to_almost_nothing() {
        // Như SaveGameInfo nằm gọn trong save chính của Stardew.
        let main = fake_save(5, 999);
        let info = main[1000..60_000].to_vec();
        let hm = sha256_hex(&main);
        let p = pack(
            &info,
            &[Base {
                hash: &hm,
                plain: &main,
                depth: 0,
            }],
        )
        .unwrap();
        assert_eq!(p.encoding, Encoding::ZstdDelta);
        assert!(
            p.bytes.len() < 200,
            "chỉ {} B mới đúng kỳ vọng",
            p.bytes.len()
        );
        assert_eq!(unpack(p.encoding, &p.bytes, Some(&main)).unwrap(), info);
    }

    #[test]
    fn chain_is_cut_at_max_depth() {
        let old = fake_save(1, 500);
        let new = fake_save(2, 750);
        let h = sha256_hex(&old);
        let p = pack(
            &new,
            &[Base {
                hash: &h,
                plain: &old,
                depth: MAX_DEPTH,
            }],
        )
        .unwrap();
        assert!(
            !p.encoding.is_delta(),
            "chuỗi đã đủ dài phải lưu bản độc lập"
        );
        assert_eq!(p.depth, 0);
    }

    #[test]
    fn incompressible_data_is_stored_raw() {
        // Như save Nightreign đã mã hoá: mọi codec đều phình ra, nên lưu thô.
        let data = noise(1, 300_000);
        let prev = noise(2, 300_000);
        let h = sha256_hex(&prev);
        let p = pack(
            &data,
            &[Base {
                hash: &h,
                plain: &prev,
                depth: 0,
            }],
        )
        .unwrap();
        assert_eq!(p.encoding, Encoding::Store);
        assert_eq!(p.bytes, data);
    }

    #[test]
    fn delta_with_wrong_base_does_not_reproduce_file() {
        let old = fake_save(1, 500);
        let other = fake_save(9, 1);
        let new = fake_save(2, 750);
        let h = sha256_hex(&old);
        let p = pack(
            &new,
            &[Base {
                hash: &h,
                plain: &old,
                depth: 0,
            }],
        )
        .unwrap();
        // Lỗi giải nén cũng chấp nhận được; chỉ cấm ra đúng file.
        if let Ok(out) = unpack(p.encoding, &p.bytes, Some(&other)) {
            assert_ne!(sha256_hex(&out), sha256_hex(&new));
        }
    }

    #[test]
    fn empty_file_roundtrips() {
        let p = pack(b"", &[]).unwrap();
        assert_eq!(unpack(p.encoding, &p.bytes, None).unwrap(), b"");
    }

    #[test]
    fn encoding_names_roundtrip() {
        for e in [
            Encoding::Store,
            Encoding::Zstd,
            Encoding::Xz,
            Encoding::Brotli,
            Encoding::Bzip2,
            Encoding::ZstdDelta,
        ] {
            assert_eq!(Encoding::parse(e.as_str()).unwrap(), e);
        }
        assert!(Encoding::parse("gzip").is_err());
    }
}
