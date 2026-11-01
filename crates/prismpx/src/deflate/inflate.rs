use super::huffman::canonical_codes;
use super::tables::*;
use crate::checksum::adler32;
use crate::error::{malformed, Result};

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    cnt: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader {
            data,
            pos: 0,
            buf: 0,
            cnt: 0,
        }
    }

    #[inline]
    fn refill(&mut self) {
        while self.cnt <= 56 && self.pos < self.data.len() {
            self.buf |= (self.data[self.pos] as u64) << self.cnt;
            self.pos += 1;
            self.cnt += 8;
        }
    }

    #[inline]
    fn bits(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        if self.cnt < n {
            self.refill();
            if self.cnt < n {
                return Err(malformed("deflate stream ended unexpectedly"));
            }
        }
        let v = (self.buf & ((1u64 << n) - 1)) as u32;
        self.buf >>= n;
        self.cnt -= n;
        Ok(v)
    }

    /// Returns the next `n` bits without consuming them (zero-padded at the end).
    #[inline]
    fn peek(&mut self, n: u32) -> u32 {
        if self.cnt < n {
            self.refill();
        }
        (self.buf & ((1u64 << n) - 1)) as u32
    }

    #[inline]
    fn consume(&mut self, n: u32) -> Result<()> {
        if n > self.cnt {
            return Err(malformed("deflate stream ended unexpectedly"));
        }
        self.buf >>= n;
        self.cnt -= n;
        Ok(())
    }

    /// Discards bits up to the next byte boundary and returns the byte offset.
    fn align(&mut self) -> usize {
        let r = self.cnt % 8;
        self.buf >>= r;
        self.cnt -= r;
        self.pos - (self.cnt / 8) as usize
    }

    fn seek(&mut self, byte_pos: usize) {
        self.pos = byte_pos;
        self.buf = 0;
        self.cnt = 0;
    }
}

/// Single-level lookup table decoder: index by the next `bits` input bits.
struct Table {
    entries: Vec<u32>, // (symbol << 4) | length; length 0 marks an invalid code
    bits: u32,
}

impl Table {
    fn new(lengths: &[u8]) -> Result<Table> {
        let mut count = [0i32; 16];
        for &l in lengths {
            if l > 15 {
                return Err(malformed("code length over 15"));
            }
            count[l as usize] += 1;
        }
        count[0] = 0;
        let mut left = 1i32;
        for c in &count[1..] {
            left = (left << 1) - c;
            if left < 0 {
                return Err(malformed("over-subscribed Huffman code"));
            }
        }
        let max = lengths.iter().copied().max().unwrap_or(0).max(1) as u32;
        let size = 1usize << max;
        let mut entries = vec![0u32; size];
        let codes = canonical_codes(lengths);
        for (sym, (&l, &code)) in lengths.iter().zip(&codes).enumerate() {
            if l == 0 {
                continue;
            }
            let step = 1usize << l;
            let mut k = code as usize;
            while k < size {
                entries[k] = ((sym as u32) << 4) | l as u32;
                k += step;
            }
        }
        Ok(Table { entries, bits: max })
    }

    #[inline]
    fn decode(&self, br: &mut BitReader) -> Result<u16> {
        let e = self.entries[br.peek(self.bits) as usize];
        let len = e & 15;
        if len == 0 {
            return Err(malformed("invalid Huffman code"));
        }
        br.consume(len)?;
        Ok((e >> 4) as u16)
    }
}

/// Decompresses a raw DEFLATE stream.
///
/// Returns the data and the number of input bytes consumed. Fails if the
/// output would exceed `max_out` bytes.
pub fn inflate(data: &[u8], max_out: usize) -> Result<(Vec<u8>, usize)> {
    let mut br = BitReader::new(data);
    let mut out: Vec<u8> = Vec::with_capacity(max_out.min(64 << 20));
    let fixed = (
        Table::new(&fixed_lit_lengths())?,
        Table::new(&fixed_dist_lengths())?,
    );
    loop {
        let last = br.bits(1)? == 1;
        match br.bits(2)? {
            0 => {
                let start = br.align();
                br.seek(start);
                let hdr = data
                    .get(start..start + 4)
                    .ok_or_else(|| malformed("truncated stored block"))?;
                let len = u16::from_le_bytes([hdr[0], hdr[1]]) as usize;
                let nlen = u16::from_le_bytes([hdr[2], hdr[3]]) as usize;
                if len != !nlen & 0xFFFF {
                    return Err(malformed("stored block length mismatch"));
                }
                let body = data
                    .get(start + 4..start + 4 + len)
                    .ok_or_else(|| malformed("truncated stored block"))?;
                if out.len() + len > max_out {
                    return Err(malformed("decompressed data larger than expected"));
                }
                out.extend_from_slice(body);
                br.seek(start + 4 + len);
            }
            1 => decode_block(&mut br, &mut out, &fixed.0, &fixed.1, max_out)?,
            2 => {
                let (lit, dist) = read_dynamic_tables(&mut br)?;
                decode_block(&mut br, &mut out, &lit, &dist, max_out)?;
            }
            _ => return Err(malformed("invalid deflate block type")),
        }
        if last {
            break;
        }
    }
    let consumed = br.align();
    Ok((out, consumed))
}

/// Decompresses a zlib stream and checks its Adler-32.
pub fn zlib_decompress(data: &[u8], max_out: usize) -> Result<Vec<u8>> {
    if data.len() < 6 {
        return Err(malformed("zlib stream too short"));
    }
    let (cmf, flg) = (data[0], data[1]);
    if cmf & 0x0F != 8 || cmf >> 4 > 7 {
        return Err(malformed("zlib stream is not deflate"));
    }
    if !((cmf as u16) << 8 | flg as u16).is_multiple_of(31) {
        return Err(malformed("bad zlib header checksum"));
    }
    if flg & 0x20 != 0 {
        return Err(malformed("zlib preset dictionary not allowed"));
    }
    let (out, used) = inflate(&data[2..], max_out)?;
    let tail = data
        .get(2 + used..2 + used + 4)
        .ok_or_else(|| malformed("missing zlib checksum"))?;
    if u32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]) != adler32(&out) {
        return Err(malformed("zlib checksum mismatch"));
    }
    Ok(out)
}

fn read_dynamic_tables(br: &mut BitReader) -> Result<(Table, Table)> {
    let hlit = br.bits(5)? as usize + 257;
    let hdist = br.bits(5)? as usize + 1;
    let hclen = br.bits(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err(malformed("too many length or distance codes"));
    }
    let mut cl_lens = [0u8; 19];
    for &i in &CL_ORDER[..hclen] {
        cl_lens[i] = br.bits(3)? as u8;
    }
    let cl = Table::new(&cl_lens)?;
    let mut lens = vec![0u8; hlit + hdist];
    let mut i = 0;
    while i < lens.len() {
        let sym = cl.decode(br)?;
        let (value, repeat) = match sym {
            0..=15 => (sym as u8, 1),
            16 => {
                if i == 0 {
                    return Err(malformed("repeat with no previous length"));
                }
                (lens[i - 1], 3 + br.bits(2)? as usize)
            }
            17 => (0, 3 + br.bits(3)? as usize),
            _ => (0, 11 + br.bits(7)? as usize),
        };
        if i + repeat > lens.len() {
            return Err(malformed("code lengths overflow"));
        }
        lens[i..i + repeat].fill(value);
        i += repeat;
    }
    if lens[256] == 0 {
        return Err(malformed("missing end-of-block code"));
    }
    Ok((Table::new(&lens[..hlit])?, Table::new(&lens[hlit..])?))
}

fn decode_block(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    lit: &Table,
    dist: &Table,
    max_out: usize,
) -> Result<()> {
    loop {
        let sym = lit.decode(br)?;
        if sym < 256 {
            if out.len() >= max_out {
                return Err(malformed("decompressed data larger than expected"));
            }
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Ok(());
        }
        let li = (sym - 257) as usize;
        if li >= 29 {
            return Err(malformed("invalid length code"));
        }
        let len = LEN_BASE[li] as usize + br.bits(LEN_EXTRA[li] as u32)? as usize;
        let di = dist.decode(br)? as usize;
        if di >= 30 {
            return Err(malformed("invalid distance code"));
        }
        let d = DIST_BASE[di] as usize + br.bits(DIST_EXTRA[di] as u32)? as usize;
        if d > out.len() {
            return Err(malformed("distance too far back"));
        }
        if out.len() + len > max_out {
            return Err(malformed("decompressed data larger than expected"));
        }
        let start = out.len() - d;
        if d >= len {
            out.extend_from_within(start..start + len);
        } else {
            for k in 0..len {
                let b = out[start + k];
                out.push(b);
            }
        }
    }
}
