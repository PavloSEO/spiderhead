//! Bounded image-header reader: visually oriented dimensions without decoding pixels.

/// Largest pixel count accepted; anything bigger is treated as unreadable (decompression bomb).
const MAX_PIXELS: u64 = 89_478_485;

fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// `Some(Some(n))` for an EXIF orientation tag, `Some(None)` when absent or unreadable.
fn orientation(tiff: &[u8]) -> Option<u32> {
    let big = match tiff.get(..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |at: usize| if big { be16(tiff, at) } else { le16(tiff, at) };
    let u32_at = |at: usize| if big { be32(tiff, at) } else { le32(tiff, at) };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)? as usize;
    let count = u16_at(ifd)? as usize;
    for i in 0..count {
        let entry = ifd + 2 + i * 12;
        if u16_at(entry)? == 274 {
            // Only a single SHORT is a valid orientation; anything else is reported as 0.
            let ok = u16_at(entry + 2)? == 3 && u32_at(entry + 4)? == 1;
            return Some(if ok { u16_at(entry + 8)? } else { 0 });
        }
    }
    None
}

/// Walk PNG chunks to `IEND`; `None` when any chunk is cut short.
fn png(body: &[u8]) -> Option<(u32, u32, Option<u32>)> {
    if body.get(12..16)? != b"IHDR" || be32(body, 8)? != 13 {
        return None;
    }
    let (w, h) = (be32(body, 16)?, be32(body, 20)?);
    let mut exif = None;
    let mut at = 8;
    while at + 12 <= body.len() {
        let len = be32(body, at)? as usize;
        let end = at.checked_add(len)?.checked_add(12)?;
        if end > body.len() {
            return None;
        }
        let kind = &body[at + 4..at + 8];
        if kind == b"eXIf" {
            exif = orientation(&body[at + 8..end - 4]);
        }
        at = end;
        if kind == b"IEND" {
            return Some((w, h, exif));
        }
    }
    None
}

fn jpeg(body: &[u8]) -> Option<(u32, u32, Option<u32>)> {
    let (mut size, mut exif) = (None, None);
    let mut at = 2;
    while at + 4 <= body.len() {
        if body[at] != 0xFF {
            at += 1;
            continue;
        }
        let marker = body[at + 1];
        if marker == 0xFF || marker == 0x00 || (0xD0..=0xD8).contains(&marker) || marker == 0x01 {
            at += if marker == 0xFF { 1 } else { 2 };
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            break;
        }
        let len = be16(body, at + 2)? as usize;
        let seg = body.get(at + 4..(at + 2 + len).min(body.len()))?;
        if marker == 0xE1 && seg.starts_with(b"Exif\0\0") {
            exif = exif.or(orientation(&seg[6..]));
        }
        if (0xC0..=0xCF).contains(&marker)
            && ![0xC4, 0xC8, 0xCC].contains(&marker)
            && size.is_none()
        {
            size = Some((be16(seg, 3)?, be16(seg, 1)?));
        }
        at += 2 + len;
    }
    let (w, h) = size?;
    Some((w, h, exif))
}

fn webp(body: &[u8]) -> Option<(u32, u32, Option<u32>)> {
    let (mut size, mut exif) = (None, None);
    let mut at = 12;
    while at + 8 <= body.len() {
        let kind = &body[at..at + 4];
        let len = le32(body, at + 4)? as usize;
        let data = body.get(at + 8..(at + 8 + len).min(body.len()))?;
        match kind {
            b"VP8X" if data.len() >= 10 => {
                let w = u32::from_le_bytes([data[4], data[5], data[6], 0]) + 1;
                let h = u32::from_le_bytes([data[7], data[8], data[9], 0]) + 1;
                size = Some((w, h));
            }
            b"VP8 " if size.is_none() && data.get(3..6) == Some(&[0x9d, 0x01, 0x2a]) => {
                size = Some((le16(data, 6)? & 0x3fff, le16(data, 8)? & 0x3fff));
            }
            b"VP8L" if size.is_none() && data.first() == Some(&0x2f) => {
                let bits = le32(data, 1)?;
                size = Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1));
            }
            b"EXIF" => {
                let tiff = data.strip_prefix(b"Exif\0\0").unwrap_or(data);
                exif = exif.or(orientation(tiff));
            }
            _ => {}
        }
        at += 8 + len + (len & 1);
    }
    let (w, h) = size?;
    Some((w, h, exif))
}

fn gif(body: &[u8]) -> Option<(u32, u32, Option<u32>)> {
    Some((le16(body, 6)?, le16(body, 8)?, None))
}

fn bmp(body: &[u8]) -> Option<(u32, u32, Option<u32>)> {
    if le32(body, 14)? == 12 {
        return Some((le16(body, 18)?, le16(body, 20)?, None));
    }
    let w = le32(body, 18)? as i32;
    let h = le32(body, 22)? as i32;
    Some((w.unsigned_abs(), h.unsigned_abs(), None))
}

/// Visually oriented `(width, height)`, or `None` when the header is incomplete, unsupported,
/// oversized or carries an invalid EXIF orientation. Never decodes pixel data.
pub fn dimensions(body: &[u8]) -> Option<(i64, i64)> {
    let (mut w, mut h, exif) = if body.starts_with(b"\x89PNG\r\n\x1a\n") {
        png(body)?
    } else if body.starts_with(&[0xFF, 0xD8]) {
        jpeg(body)?
    } else if body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a") {
        gif(body)?
    } else if body.starts_with(b"RIFF") && body.get(8..12) == Some(b"WEBP") {
        webp(body)?
    } else if body.starts_with(b"BM") {
        bmp(body)?
    } else {
        return None;
    };
    let orient = exif.unwrap_or(1);
    if !(1..=8).contains(&orient) {
        return None;
    }
    if (5..=8).contains(&orient) {
        std::mem::swap(&mut w, &mut h);
    }
    if w < 1 || h < 1 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return None;
    }
    Some((i64::from(w), i64::from(h)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
        let mut v = (data.len() as u32).to_be_bytes().to_vec();
        v.extend(kind);
        v.extend(data);
        v.extend([0; 4]);
        v
    }

    fn png_with(w: u32, h: u32, extra: &[Vec<u8>], iend: bool) -> Vec<u8> {
        let mut ihdr = w.to_be_bytes().to_vec();
        ihdr.extend(h.to_be_bytes());
        ihdr.extend([8, 2, 0, 0, 0]);
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend(chunk(b"IHDR", &ihdr));
        for c in extra {
            v.extend(c);
        }
        if iend {
            v.extend(chunk(b"IEND", b""));
        }
        v
    }

    fn exif_orientation(value: u16) -> Vec<u8> {
        let mut t = b"MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
        t.extend(value.to_be_bytes());
        t.extend([0, 0, 0, 0, 0, 0]);
        t
    }

    #[test]
    fn complete_png_reports_its_size() {
        assert_eq!(dimensions(&png_with(1, 1, &[], true)), Some((1, 1)));
    }

    #[test]
    fn truncated_or_oversized_png_is_unreadable() {
        let full = png_with(1, 1, &[], true);
        assert_eq!(dimensions(&full[..20]), None);
        assert_eq!(dimensions(&png_with(100_000, 100_000, &[], false)), None);
        assert_eq!(dimensions(&png_with(100_000, 100_000, &[], true)), None);
        assert_eq!(dimensions(&[]), None);
    }

    #[test]
    fn exif_after_the_pixel_data_swaps_axes_for_rotated_orientations() {
        let rotated = png_with(3, 2, &[chunk(b"eXIf", &exif_orientation(6))], true);
        assert_eq!(dimensions(&rotated), Some((2, 3)));
        let upright = png_with(3, 2, &[chunk(b"eXIf", &exif_orientation(1))], true);
        assert_eq!(dimensions(&upright), Some((3, 2)));
        let bogus = png_with(3, 2, &[chunk(b"eXIf", &exif_orientation(9))], true);
        assert_eq!(dimensions(&bogus), None);
    }

    #[test]
    fn gif_and_jpeg_headers_are_read() {
        assert_eq!(dimensions(b"GIF89a\x05\0\x07\0rest"), Some((5, 7)));
        let jpg = [
            0xFF, 0xD8, 0xFF, 0xC0, 0, 11, 8, 0, 20, 0, 10, 1, 1, 0x11, 0, 0xFF, 0xD9,
        ];
        assert_eq!(dimensions(&jpg), Some((10, 20)));
    }
}
