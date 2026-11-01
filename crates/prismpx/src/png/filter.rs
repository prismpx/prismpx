//! PNG scanline filters (prediction) and per-row filter selection heuristics.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Strategy {
    /// The same filter type (0..=4) on every row.
    Fixed(u8),
    /// Per row, the filter with the smallest sum of absolute signed residuals.
    MinSum,
    /// Per row, the filter whose residuals have the lowest Shannon entropy.
    Entropy,
}

impl Strategy {
    pub fn name(self) -> &'static str {
        match self {
            Strategy::Fixed(0) => "none",
            Strategy::Fixed(1) => "sub",
            Strategy::Fixed(2) => "up",
            Strategy::Fixed(3) => "average",
            Strategy::Fixed(_) => "paeth",
            Strategy::MinSum => "adaptive-minsum",
            Strategy::Entropy => "adaptive-entropy",
        }
    }
}

#[inline]
pub(crate) fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let pa = (p - a as i16).abs();
    let pb = (p - b as i16).abs();
    let pc = (p - c as i16).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Appends the filtered version of `cur` (without the type byte) to `out`.
fn filter_row(ft: u8, cur: &[u8], prev: &[u8], bpp: usize, out: &mut Vec<u8>) {
    for i in 0..cur.len() {
        let a = if i >= bpp { cur[i - bpp] } else { 0 };
        let b = prev[i];
        let c = if i >= bpp { prev[i - bpp] } else { 0 };
        let pred = match ft {
            0 => 0,
            1 => a,
            2 => b,
            3 => ((a as u16 + b as u16) / 2) as u8,
            _ => paeth(a, b, c),
        };
        out.push(cur[i].wrapping_sub(pred));
    }
}

/// Reverses a filter in place. `prev` is the previous reconstructed row.
pub(crate) fn unfilter_row(ft: u8, cur: &mut [u8], prev: &[u8], bpp: usize) -> bool {
    match ft {
        0 => {}
        1 => {
            for i in bpp..cur.len() {
                cur[i] = cur[i].wrapping_add(cur[i - bpp]);
            }
        }
        2 => {
            for i in 0..cur.len() {
                cur[i] = cur[i].wrapping_add(prev[i]);
            }
        }
        3 => {
            for i in 0..cur.len() {
                let a = if i >= bpp { cur[i - bpp] } else { 0 };
                cur[i] = cur[i].wrapping_add(((a as u16 + prev[i] as u16) / 2) as u8);
            }
        }
        4 => {
            for i in 0..cur.len() {
                let (a, c) = if i >= bpp {
                    (cur[i - bpp], prev[i - bpp])
                } else {
                    (0, 0)
                };
                cur[i] = cur[i].wrapping_add(paeth(a, prev[i], c));
            }
        }
        _ => return false,
    }
    true
}

fn minsum_score(row: &[u8]) -> u64 {
    row.iter().map(|&b| (b as i8).unsigned_abs() as u64).sum()
}

fn entropy_score(row: &[u8]) -> f64 {
    let mut hist = [0u32; 256];
    for &b in row {
        hist[b as usize] += 1;
    }
    let n = row.len() as f64;
    let mut bits = 0.0;
    for &c in hist.iter().filter(|&&c| c > 0) {
        let c = c as f64;
        bits -= c * (c / n).log2();
    }
    bits
}

/// Filters all rows of `raw` (rows of `row_len` bytes) into a PNG IDAT payload
/// (each row prefixed with its filter type byte).
pub(crate) fn apply(raw: &[u8], row_len: usize, bpp: usize, strategy: Strategy) -> Vec<u8> {
    let rows = raw.len().checked_div(row_len).unwrap_or(0);
    let mut out = Vec::with_capacity(rows * (row_len + 1));
    let zeros = vec![0u8; row_len];
    let mut trial: [Vec<u8>; 5] = Default::default();
    for y in 0..rows {
        let cur = &raw[y * row_len..(y + 1) * row_len];
        let prev = if y == 0 {
            &zeros[..]
        } else {
            &raw[(y - 1) * row_len..y * row_len]
        };
        match strategy {
            Strategy::Fixed(ft) => {
                out.push(ft);
                filter_row(ft, cur, prev, bpp, &mut out);
            }
            Strategy::MinSum | Strategy::Entropy => {
                for (ft, buf) in trial.iter_mut().enumerate() {
                    buf.clear();
                    filter_row(ft as u8, cur, prev, bpp, buf);
                }
                let best = if strategy == Strategy::MinSum {
                    (0..5).min_by_key(|&ft| minsum_score(&trial[ft])).unwrap()
                } else {
                    (0..5)
                        .min_by(|&x, &y| {
                            entropy_score(&trial[x]).total_cmp(&entropy_score(&trial[y]))
                        })
                        .unwrap()
                };
                out.push(best as u8);
                out.extend_from_slice(&trial[best]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_roundtrip() {
        let row_len = 13;
        let raw: Vec<u8> = (0..row_len * 7).map(|i| (i * 37 % 251) as u8).collect();
        for s in [
            Strategy::Fixed(0),
            Strategy::Fixed(1),
            Strategy::Fixed(2),
            Strategy::Fixed(3),
            Strategy::Fixed(4),
            Strategy::MinSum,
            Strategy::Entropy,
        ] {
            let f = apply(&raw, row_len, 3, s);
            let mut prev = vec![0u8; row_len];
            for y in 0..7 {
                let start = y * (row_len + 1);
                let mut cur = f[start + 1..start + 1 + row_len].to_vec();
                assert!(unfilter_row(f[start], &mut cur, &prev, 3));
                assert_eq!(cur, &raw[y * row_len..(y + 1) * row_len]);
                prev = cur;
            }
        }
    }
}
