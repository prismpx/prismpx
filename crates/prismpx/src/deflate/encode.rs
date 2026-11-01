use super::huffman::{canonical_codes, length_limited};
use super::lz77::{self, Token};
use super::tables::*;
use crate::checksum::adler32;

/// Compression level parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    /// Maximum hash-chain candidates examined per position.
    pub max_chain: u32,
    /// Stop searching once a match this long is found.
    pub nice_len: usize,
    /// Use lazy matching (look one byte ahead for a longer match).
    pub lazy: bool,
}

impl Level {
    pub const FAST: Level = Level {
        max_chain: 16,
        nice_len: 32,
        lazy: false,
    };
    pub const DEFAULT: Level = Level {
        max_chain: 256,
        nice_len: 128,
        lazy: true,
    };
    pub const MAX: Level = Level {
        max_chain: 4096,
        nice_len: 258,
        lazy: true,
    };
}

/// Tokens per block. Smaller blocks adapt Huffman tables to local statistics
/// at the cost of more headers.
const BLOCK_TOKENS: usize = 1 << 14;

struct BitWriter {
    out: Vec<u8>,
    buf: u64,
    cnt: u32,
}

impl BitWriter {
    #[inline]
    fn write(&mut self, value: u32, n: u32) {
        self.buf |= (value as u64) << self.cnt;
        self.cnt += n;
        while self.cnt >= 8 {
            self.out.push(self.buf as u8);
            self.buf >>= 8;
            self.cnt -= 8;
        }
    }

    fn align(&mut self) {
        if self.cnt > 0 {
            self.out.push(self.buf as u8);
            self.buf = 0;
            self.cnt = 0;
        }
    }

    fn bit_pos(&self) -> u64 {
        self.out.len() as u64 * 8 + self.cnt as u64
    }
}

/// Compresses into a raw DEFLATE stream.
pub fn deflate(data: &[u8], level: &Level) -> Vec<u8> {
    let tokens = lz77::parse(data, level);
    let mut w = BitWriter {
        out: Vec::with_capacity(data.len() / 2 + 64),
        buf: 0,
        cnt: 0,
    };
    if tokens.is_empty() {
        write_block(&mut w, &[], &[], true);
    }
    let mut offset = 0;
    let blocks = tokens.len().div_ceil(BLOCK_TOKENS);
    for (i, chunk) in tokens.chunks(BLOCK_TOKENS).enumerate() {
        let span: usize = chunk.iter().map(|t| t.span()).sum();
        write_block(&mut w, chunk, &data[offset..offset + span], i + 1 == blocks);
        offset += span;
    }
    w.align();
    w.out
}

/// Compresses into a zlib stream (header + DEFLATE + Adler-32).
pub fn zlib_compress(data: &[u8], level: &Level) -> Vec<u8> {
    let mut out = vec![0x78, 0xDA];
    out.extend(deflate(data, level));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

struct Stats {
    lit: [u32; 286],
    dist: [u32; 30],
}

impl Stats {
    fn of(tokens: &[Token]) -> Stats {
        let mut s = Stats {
            lit: [0; 286],
            dist: [0; 30],
        };
        for t in tokens {
            if t.is_literal() {
                s.lit[t.len as usize] += 1;
            } else {
                s.lit[257 + len_code(t.len).0] += 1;
                s.dist[dist_code(t.dist).0] += 1;
            }
        }
        s.lit[256] = 1;
        s
    }

    fn data_bits(&self, ll: &[u8], dl: &[u8]) -> u64 {
        let mut bits = 0u64;
        for (i, &f) in self.lit.iter().enumerate() {
            let extra = if i > 256 {
                LEN_EXTRA[i - 257] as u64
            } else {
                0
            };
            bits += f as u64 * (ll[i] as u64 + extra);
        }
        for (i, &f) in self.dist.iter().enumerate() {
            bits += f as u64 * (dl[i] as u64 + DIST_EXTRA[i] as u64);
        }
        bits
    }
}

/// Run-length encodes code lengths into code-length alphabet symbols.
fn rle_lengths(lens: &[u8]) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lens.len() {
        let v = lens[i];
        let mut run = 1;
        while i + run < lens.len() && lens[i + run] == v {
            run += 1;
        }
        if v == 0 {
            let mut r = run;
            while r >= 11 {
                let k = r.min(138);
                out.push((18, (k - 11) as u8));
                r -= k;
            }
            if r >= 3 {
                out.push((17, (r - 3) as u8));
                r = 0;
            }
            out.extend(std::iter::repeat_n((0, 0), r));
        } else {
            out.push((v, 0));
            let mut r = run - 1;
            while r >= 3 {
                let k = r.min(6);
                out.push((16, (k - 3) as u8));
                r -= k;
            }
            out.extend(std::iter::repeat_n((v, 0), r));
        }
        i += run;
    }
    out
}

const CL_EXTRA: [u32; 19] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];

struct Dynamic {
    ll: Vec<u8>,
    dl: Vec<u8>,
    hlit: usize,
    hdist: usize,
    rle: Vec<(u8, u8)>,
    cl: Vec<u8>,
    hclen: usize,
}

impl Dynamic {
    fn build(stats: &Stats) -> Dynamic {
        let ll = length_limited(&stats.lit, 15);
        let mut dl = length_limited(&stats.dist, 15);
        if dl.iter().all(|&l| l == 0) {
            dl[0] = 1;
            dl[1] = 1;
        }
        let hlit = (ll.iter().rposition(|&l| l != 0).unwrap_or(0) + 1).max(257);
        let hdist = (dl.iter().rposition(|&l| l != 0).unwrap_or(0) + 1).max(1);
        let mut all = ll[..hlit].to_vec();
        all.extend_from_slice(&dl[..hdist]);
        let rle = rle_lengths(&all);
        let mut cf = [0u32; 19];
        for &(s, _) in &rle {
            cf[s as usize] += 1;
        }
        let cl = length_limited(&cf, 7);
        let mut hclen = 19;
        while hclen > 4 && cl[CL_ORDER[hclen - 1]] == 0 {
            hclen -= 1;
        }
        Dynamic {
            ll,
            dl,
            hlit,
            hdist,
            rle,
            cl,
            hclen,
        }
    }

    fn header_bits(&self) -> u64 {
        let mut bits = 3 + 5 + 5 + 4 + 3 * self.hclen as u64;
        for &(s, _) in &self.rle {
            bits += self.cl[s as usize] as u64 + CL_EXTRA[s as usize] as u64;
        }
        bits
    }
}

fn write_block(w: &mut BitWriter, tokens: &[Token], raw: &[u8], last: bool) {
    let stats = Stats::of(tokens);
    let dynamic = Dynamic::build(&stats);
    let fixed_ll = fixed_lit_lengths();
    let fixed_dl = fixed_dist_lengths();

    let dyn_bits = dynamic.header_bits() + stats.data_bits(&dynamic.ll, &dynamic.dl);
    let fixed_bits = 3 + stats.data_bits(&fixed_ll[..286], &fixed_dl);
    let stored_bits = {
        let pos = w.bit_pos();
        let mut p = pos;
        let pieces = raw.len().div_ceil(65535).max(1);
        for k in 0..pieces {
            let len = (raw.len() - k * 65535).min(65535) as u64;
            p += 3;
            p = p.div_ceil(8) * 8;
            p += 32 + 8 * len;
        }
        p - pos
    };

    if stored_bits <= dyn_bits.min(fixed_bits) {
        write_stored(w, raw, last);
    } else if fixed_bits <= dyn_bits {
        w.write(last as u32, 1);
        w.write(1, 2);
        write_tokens(w, tokens, &fixed_ll, &fixed_dl);
    } else {
        w.write(last as u32, 1);
        w.write(2, 2);
        w.write((dynamic.hlit - 257) as u32, 5);
        w.write((dynamic.hdist - 1) as u32, 5);
        w.write((dynamic.hclen - 4) as u32, 4);
        for &i in &CL_ORDER[..dynamic.hclen] {
            w.write(dynamic.cl[i] as u32, 3);
        }
        let cl_codes = canonical_codes(&dynamic.cl);
        for &(s, extra) in &dynamic.rle {
            let s = s as usize;
            w.write(cl_codes[s] as u32, dynamic.cl[s] as u32);
            if CL_EXTRA[s] > 0 {
                w.write(extra as u32, CL_EXTRA[s]);
            }
        }
        write_tokens(w, tokens, &dynamic.ll, &dynamic.dl);
    }
}

fn write_stored(w: &mut BitWriter, raw: &[u8], last: bool) {
    let pieces = raw.len().div_ceil(65535).max(1);
    for k in 0..pieces {
        let piece = &raw[k * 65535..((k + 1) * 65535).min(raw.len())];
        w.write((last && k + 1 == pieces) as u32, 1);
        w.write(0, 2);
        w.align();
        let len = piece.len() as u16;
        w.out.extend_from_slice(&len.to_le_bytes());
        w.out.extend_from_slice(&(!len).to_le_bytes());
        w.out.extend_from_slice(piece);
    }
}

fn write_tokens(w: &mut BitWriter, tokens: &[Token], ll: &[u8], dl: &[u8]) {
    let lc = canonical_codes(ll);
    let dc = canonical_codes(dl);
    for t in tokens {
        if t.is_literal() {
            let s = t.len as usize;
            w.write(lc[s] as u32, ll[s] as u32);
        } else {
            let (li, lx, lv) = len_code(t.len);
            w.write(lc[257 + li] as u32, ll[257 + li] as u32);
            if lx > 0 {
                w.write(lv as u32, lx as u32);
            }
            let (di, dx, dv) = dist_code(t.dist);
            w.write(dc[di] as u32, dl[di] as u32);
            if dx > 0 {
                w.write(dv as u32, dx as u32);
            }
        }
    }
    w.write(lc[256] as u32, ll[256] as u32);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deflate::{inflate, zlib_decompress};

    fn samples() -> Vec<Vec<u8>> {
        let mut rng = 0x1234_5678u32;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            rng
        };
        let random: Vec<u8> = (0..70_000).map(|_| next() as u8).collect();
        let text: Vec<u8> = b"the quick brown fox jumps over the lazy dog. "
            .iter()
            .cycle()
            .take(200_000)
            .copied()
            .collect();
        let skewed: Vec<u8> = (0..100_000).map(|_| (next() % 7) as u8).collect();
        vec![
            vec![],
            vec![42],
            vec![1, 2],
            vec![0; 1000],
            vec![7; 300_000],
            random,
            text,
            skewed,
        ]
    }

    #[test]
    fn roundtrip_all_levels() {
        for data in samples() {
            for level in [Level::FAST, Level::DEFAULT, Level::MAX] {
                let z = deflate(&data, &level);
                let (back, used) = inflate(&z, data.len()).unwrap();
                assert_eq!(back, data, "len {}", data.len());
                assert_eq!(used, z.len());
                let zz = zlib_compress(&data, &level);
                assert_eq!(zlib_decompress(&zz, data.len()).unwrap(), data);
            }
        }
    }

    #[test]
    fn rejects_oversized_output() {
        let z = deflate(&[0u8; 1000], &Level::DEFAULT);
        assert!(inflate(&z, 999).is_err());
    }
}
