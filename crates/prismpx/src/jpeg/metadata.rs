//! Metadata (APPn / COM segment) policy for JPEG.

use crate::options::MetadataPolicy;

const APP0: u8 = 0xE0;
const APP1: u8 = 0xE1;
const APP2: u8 = 0xE2;
const APP14: u8 = 0xEE;

fn is_jfif(m: u8, p: &[u8]) -> bool {
    m == APP0 && p.starts_with(b"JFIF\0")
}

fn is_icc(m: u8, p: &[u8]) -> bool {
    m == APP2 && p.starts_with(b"ICC_PROFILE\0")
}

fn is_adobe(m: u8, p: &[u8]) -> bool {
    m == APP14 && p.starts_with(b"Adobe")
}

fn is_exif(m: u8, p: &[u8]) -> bool {
    m == APP1 && p.starts_with(b"Exif\0\0")
}

fn is_mpf(m: u8, p: &[u8]) -> bool {
    m == APP2 && p.starts_with(b"MPF\0")
}

/// Reads the EXIF orientation tag (1..=8) from IFD0, if present.
pub(crate) fn exif_orientation(p: &[u8]) -> Option<u16> {
    let t = p.get(6..)?;
    let le = match t.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let b = t.get(o..o + 2)?;
        Some(if le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let b = t.get(o..o + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    };
    let ifd = u32_at(4)? as usize;
    let n = u16_at(ifd)? as usize;
    for i in 0..n.min(512) {
        let e = ifd + 2 + 12 * i;
        if u16_at(e)? == 0x0112 && u16_at(e + 2)? == 3 {
            let v = u16_at(e + 8)?;
            return (1..=8).contains(&v).then_some(v);
        }
    }
    None
}

/// A minimal EXIF payload carrying only the orientation tag.
pub(crate) fn orientation_only_exif(orientation: u16) -> Vec<u8> {
    let mut v = b"Exif\0\0MM\0\x2a\0\0\0\x08".to_vec();
    v.extend_from_slice(&1u16.to_be_bytes()); // one entry
    v.extend_from_slice(&0x0112u16.to_be_bytes()); // Orientation
    v.extend_from_slice(&3u16.to_be_bytes()); // SHORT
    v.extend_from_slice(&1u32.to_be_bytes()); // count
    v.extend_from_slice(&orientation.to_be_bytes());
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(&0u32.to_be_bytes()); // no next IFD
    v
}

/// Applies the policy. Returns the kept segments and a short note on what changed.
///
/// Always kept: JFIF, ICC profiles and the Adobe segment (which decides how
/// color components are interpreted). EXIF is kept whole with `KeepAll`;
/// otherwise it is reduced to its orientation tag, because orientation changes
/// how the image is displayed. Multi-picture (MPF) data is always dropped,
/// since the extra images after EOI are not carried over.
pub(crate) fn filter(
    segments: &[(u8, Vec<u8>)],
    policy: MetadataPolicy,
) -> (Vec<(u8, Vec<u8>)>, String) {
    let mut kept = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    for (m, p) in segments {
        let (m, p) = (*m, p);
        if is_mpf(m, p) {
            removed.push("MPF".into());
        } else if is_jfif(m, p)
            || is_icc(m, p)
            || is_adobe(m, p)
            || policy == MetadataPolicy::KeepAll
        {
            kept.push((m, p.clone()));
        } else if is_exif(m, p) {
            match exif_orientation(p) {
                Some(o) if o != 1 => {
                    kept.push((APP1, orientation_only_exif(o)));
                    removed.push("EXIF (kept orientation)".into());
                }
                _ => removed.push("EXIF".into()),
            }
        } else {
            removed.push(match m {
                0xFE => "COM".into(),
                APP1 if p.starts_with(b"http://ns.adobe.com/xap/") => "XMP".into(),
                0xED => "IPTC/Photoshop".into(),
                _ => format!("APP{}", m - 0xE0),
            });
        }
    }
    removed.dedup();
    let note = if removed.is_empty() {
        String::new()
    } else {
        format!("removed: {}", removed.join(" "))
    };
    (kept, note)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_roundtrip() {
        for o in 1..=8 {
            assert_eq!(exif_orientation(&orientation_only_exif(o)), Some(o));
        }
        // Little-endian EXIF with two entries, orientation second.
        let mut le = b"Exif\0\0II\x2a\0\x08\0\0\0".to_vec();
        le.extend_from_slice(&2u16.to_le_bytes());
        le.extend_from_slice(&[0x0f, 0x01, 2, 0, 4, 0, 0, 0, 0, 0, 0, 0]); // Make (ASCII)
        le.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
        le.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(exif_orientation(&le), Some(6));
    }
}
