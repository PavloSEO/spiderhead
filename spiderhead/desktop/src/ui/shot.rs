//! `ui.screenshot`: encode the egui `ColorImage` the viewport returns as PNG —
//! a tiny zlib+CRC32 writer, no image crate needed.

use egui::ColorImage;
use std::io::Write;

fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *e = c;
        }
        t
    });
    data.iter().fold(0xFFFF_FFFF, |c, &b| {
        table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8)
    }) ^ 0xFFFF_FFFF
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_data = Vec::with_capacity(4 + data.len());
    crc_data.extend_from_slice(kind);
    crc_data.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_data).to_be_bytes());
}

/// Encode an RGBA `ColorImage` as a PNG byte string (filter byte 0 per row).
/// Compression errors propagate: a truncated IDAT written as a valid PNG is
/// worse than no PNG at all.
pub fn encode_png(image: &ColorImage) -> std::io::Result<Vec<u8>> {
    let (w, h) = (image.size[0] as u32, image.size[1] as u32);
    let mut raw = Vec::with_capacity((w as usize + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0);
        for x in 0..w as usize {
            let p = image[(x, y)];
            raw.extend_from_slice(&[p.r(), p.g(), p.b(), p.a()]);
        }
    }
    let mut compressed = Vec::new();
    {
        let mut enc = flate2::write::ZlibEncoder::new(&mut compressed, flate2::Compression::fast());
        enc.write_all(&raw)?;
        enc.finish()?;
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &compressed);
    chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_header_and_chunks() {
        let image = ColorImage::new([2, 2], vec![egui::Color32::RED; 4]);
        let png = encode_png(&image).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(png.windows(4).any(|w| w == b"IHDR"));
        assert!(png.windows(4).any(|w| w == b"IDAT"));
        assert!(png.windows(4).any(|w| w == b"IEND"));
    }

    // regression (review 2026-10-27): the zlib stream was never finished, so
    // IDAT could hold a truncated stream still wrapped in a valid PNG.
    #[test]
    fn idat_is_a_complete_zlib_stream() {
        let image = ColorImage::new([2, 2], vec![egui::Color32::RED; 4]);
        let png = encode_png(&image).unwrap();
        // Walk the chunks to the IDAT payload.
        let mut at = 8;
        let idat = loop {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind = &png[at + 4..at + 8];
            let data = &png[at + 8..at + 8 + len];
            if kind == b"IDAT" {
                break data.to_vec();
            }
            at += 8 + len + 4;
        };
        let mut raw = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&idat[..]), &mut raw)
            .unwrap();
        // 2 rows × (filter byte + 2 RGBA pixels).
        assert_eq!(raw.len(), 2 * (1 + 2 * 4));
        for row in raw.as_chunks::<{ 1 + 2 * 4 }>().0 {
            assert_eq!(row[0], 0);
            assert!(row[1..]
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| *px == [255, 0, 0, 255]));
        }
    }
}
