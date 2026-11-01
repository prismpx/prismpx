//! JPEG parsing and entropy decoding to quantized DCT coefficients.
//!
//! Supports baseline and extended sequential (SOF0/SOF1) and progressive
//! (SOF2) Huffman-coded 8-bit JPEGs, including successive approximation and
//! restart intervals. Coefficients are stored in zigzag order.

use super::huffman::{DecodeTable, Spec};
use super::{Component, Frame, Jpeg};
use crate::error::{malformed, Error, Result};

/// Largest image (in pixels) accepted, to bound memory use.
const MAX_PIXELS: u64 = 1 << 28;

/// MSB-first bit reader over entropy-coded data (handles 0xFF00 stuffing).
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    cnt: u32,
    /// Set when a marker is reached; further reads return zero bits.
    at_marker: bool,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        BitReader {
            data,
            pos,
            buf: 0,
            cnt: 0,
            at_marker: false,
        }
    }

    #[inline]
    fn fill(&mut self) {
        while self.cnt <= 56 {
            let mut byte = 0u8;
            if !self.at_marker && self.pos < self.data.len() {
                let b = self.data[self.pos];
                if b == 0xFF {
                    match self.data.get(self.pos + 1) {
                        Some(0x00) => {
                            byte = 0xFF;
                            self.pos += 2;
                        }
                        Some(0xFF) => {
                            // Fill byte before a marker.
                            self.pos += 1;
                            continue;
                        }
                        _ => self.at_marker = true,
                    }
                } else {
                    byte = b;
                    self.pos += 1;
                }
            }
            self.buf |= (byte as u64) << (56 - self.cnt);
            self.cnt += 8;
        }
    }

    #[inline]
    fn peek16(&mut self) -> u32 {
        if self.cnt < 16 {
            self.fill();
        }
        (self.buf >> 48) as u32
    }

    #[inline]
    fn consume(&mut self, n: u32) {
        self.buf <<= n;
        self.cnt -= n;
    }

    #[inline]
    fn bits(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if self.cnt < n {
            self.fill();
        }
        let v = (self.buf >> (64 - n)) as u32;
        self.consume(n);
        v
    }

    #[inline]
    fn huff(&mut self, t: &DecodeTable) -> Result<u8> {
        let peek = self.peek16();
        let (sym, len) = t
            .decode(peek)
            .ok_or_else(|| malformed("invalid Huffman code"))?;
        self.consume(len);
        Ok(sym)
    }

    #[inline]
    fn receive_extend(&mut self, s: u32) -> i32 {
        if s == 0 {
            return 0;
        }
        let v = self.bits(s) as i32;
        if v < 1 << (s - 1) {
            v - (1 << s) + 1
        } else {
            v
        }
    }

    /// Byte offset just past the entropy data that has been consumed.
    fn byte_pos(&self) -> usize {
        self.pos
    }

    /// Skips to just after the next RSTn marker and resets the bit buffer.
    fn restart(&mut self) {
        self.buf = 0;
        self.cnt = 0;
        self.at_marker = false;
        let d = self.data;
        while self.pos + 1 < d.len() {
            if d[self.pos] == 0xFF && (0xD0..=0xD7).contains(&d[self.pos + 1]) {
                self.pos += 2;
                return;
            }
            if d[self.pos] == 0xFF && d[self.pos + 1] != 0x00 && d[self.pos + 1] != 0xFF {
                return; // some other marker: let the caller find it
            }
            self.pos += 1;
        }
    }
}

struct ScanComp {
    index: usize,
    dc: usize,
    ac: usize,
}

struct Scan {
    comps: Vec<ScanComp>,
    ss: u8,
    se: u8,
    ah: u8,
    al: u8,
}

fn be16(d: &[u8], at: usize) -> Result<usize> {
    d.get(at..at + 2)
        .map(|b| (b[0] as usize) << 8 | b[1] as usize)
        .ok_or_else(|| malformed("truncated JPEG"))
}

fn parse_frame(p: &[u8], progressive: bool) -> Result<Frame> {
    if p.len() < 6 {
        return Err(malformed("truncated SOF"));
    }
    if p[0] != 8 {
        return Err(Error::Unsupported(format!("{}-bit JPEG", p[0])));
    }
    let height = u16::from_be_bytes([p[1], p[2]]);
    let width = u16::from_be_bytes([p[3], p[4]]);
    let n = p[5] as usize;
    if height == 0 {
        return Err(Error::Unsupported("JPEG with DNL-defined height".into()));
    }
    if width == 0 || !(1..=4).contains(&n) || p.len() < 6 + 3 * n {
        return Err(malformed("invalid SOF"));
    }
    if width as u64 * height as u64 > MAX_PIXELS {
        return Err(Error::Unsupported("image too large".into()));
    }
    let comps: Vec<Component> = (0..n)
        .map(|i| {
            let c = &p[6 + 3 * i..9 + 3 * i];
            Component {
                id: c[0],
                h: c[1] >> 4,
                v: c[1] & 15,
                tq: c[2],
            }
        })
        .collect();
    if comps
        .iter()
        .any(|c| !(1..=4).contains(&c.h) || !(1..=4).contains(&c.v) || c.tq > 3)
    {
        return Err(malformed("invalid sampling factors"));
    }
    Ok(Frame {
        progressive,
        width,
        height,
        comps,
    })
}

fn parse_dht(
    p: &[u8],
    dc: &mut [Option<DecodeTable>; 4],
    ac: &mut [Option<DecodeTable>; 4],
) -> Result<()> {
    let mut i = 0;
    while i < p.len() {
        if i + 17 > p.len() {
            return Err(malformed("truncated DHT"));
        }
        let (class, id) = (p[i] >> 4, (p[i] & 15) as usize);
        if class > 1 || id > 3 {
            return Err(malformed("invalid DHT table id"));
        }
        let mut counts = [0u8; 16];
        counts.copy_from_slice(&p[i + 1..i + 17]);
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        let symbols = p
            .get(i + 17..i + 17 + total)
            .ok_or_else(|| malformed("truncated DHT"))?
            .to_vec();
        let table = DecodeTable::new(&Spec { counts, symbols })?;
        if class == 0 {
            dc[id] = Some(table);
        } else {
            ac[id] = Some(table);
        }
        i += 17 + total;
    }
    Ok(())
}

/// Parses a DQT payload into `tables` (zigzag order). Returns true if any
/// table is 16-bit and sets `redefined` if a table id is defined again with
/// different values.
fn parse_dqt(p: &[u8], tables: &mut [Option<[u16; 64]>; 4], redefined: &mut bool) -> Result<bool> {
    let mut i = 0;
    let mut wide = false;
    while i < p.len() {
        let (pq, id) = (p[i] >> 4, (p[i] & 15) as usize);
        if pq > 1 || id > 3 {
            return Err(malformed("invalid DQT"));
        }
        let size = 64 * (1 + pq as usize);
        let body = p
            .get(i + 1..i + 1 + size)
            .ok_or_else(|| malformed("truncated DQT"))?;
        let mut t = [0u16; 64];
        for (k, v) in t.iter_mut().enumerate() {
            *v = if pq == 1 {
                u16::from_be_bytes([body[2 * k], body[2 * k + 1]])
            } else {
                body[k] as u16
            };
        }
        if t.contains(&0) {
            return Err(malformed("zero quantization step"));
        }
        if tables[id].is_some_and(|old| old != t) {
            *redefined = true;
        }
        tables[id] = Some(t);
        wide |= pq == 1;
        i += 1 + size;
    }
    Ok(wide)
}

fn parse_scan(p: &[u8], frame: &Frame) -> Result<Scan> {
    let n = *p.first().ok_or_else(|| malformed("truncated SOS"))? as usize;
    if !(1..=4).contains(&n) || p.len() != 4 + 2 * n {
        return Err(malformed("invalid SOS"));
    }
    let mut comps = Vec::with_capacity(n);
    for i in 0..n {
        let id = p[1 + 2 * i];
        let t = p[2 + 2 * i];
        let index = frame
            .comps
            .iter()
            .position(|c| c.id == id)
            .ok_or_else(|| malformed("scan references unknown component"))?;
        comps.push(ScanComp {
            index,
            dc: (t >> 4) as usize & 3,
            ac: (t & 15) as usize & 3,
        });
    }
    let k = 1 + 2 * n;
    let scan = Scan {
        comps,
        ss: p[k],
        se: p[k + 1],
        ah: p[k + 2] >> 4,
        al: p[k + 2] & 15,
    };
    let valid = if frame.progressive {
        scan.se <= 63
            && scan.ss <= scan.se
            && (scan.ss == 0) == (scan.se == 0)
            && (scan.ss == 0 || n == 1)
            && scan.al <= 13
    } else {
        scan.ss == 0 && scan.se == 63 && scan.ah == 0 && scan.al == 0
    };
    if !valid {
        return Err(malformed("invalid scan parameters"));
    }
    Ok(scan)
}

struct ScanState<'t> {
    dc_tables: Vec<&'t DecodeTable>,
    ac_tables: Vec<&'t DecodeTable>,
    pred: Vec<i32>,
    eobrun: u32,
}

fn decode_block(
    br: &mut BitReader,
    scan: &Scan,
    st: &mut ScanState,
    sc: usize,
    block: &mut [i16; 64],
    progressive: bool,
) -> Result<()> {
    let (ss, se, ah, al) = (scan.ss as usize, scan.se as usize, scan.ah, scan.al as u32);
    if !progressive {
        let t = br.huff(st.dc_tables[sc])? as u32;
        if t > 11 {
            return Err(malformed("DC magnitude too large"));
        }
        st.pred[sc] += br.receive_extend(t);
        block[0] = st.pred[sc] as i16;
        let mut k = 1;
        while k < 64 {
            let rs = br.huff(st.ac_tables[sc])?;
            let (r, s) = ((rs >> 4) as usize, (rs & 15) as u32);
            if s == 0 {
                if r == 15 {
                    k += 16;
                    continue;
                }
                break;
            }
            k += r;
            if k > 63 {
                return Err(malformed("AC coefficient index out of range"));
            }
            block[k] = br.receive_extend(s) as i16;
            k += 1;
        }
        return Ok(());
    }

    if ss == 0 {
        // DC scans (possibly interleaved).
        if ah == 0 {
            let t = br.huff(st.dc_tables[sc])? as u32;
            if t > 11 {
                return Err(malformed("DC magnitude too large"));
            }
            st.pred[sc] += br.receive_extend(t);
            block[0] = (st.pred[sc] << al) as i16;
        } else if br.bits(1) == 1 {
            block[0] |= (1 << al) as i16;
        }
        return Ok(());
    }

    let table = st.ac_tables[sc];
    if ah == 0 {
        // AC first pass.
        if st.eobrun > 0 {
            st.eobrun -= 1;
            return Ok(());
        }
        let mut k = ss;
        while k <= se {
            let rs = br.huff(table)?;
            let (r, s) = ((rs >> 4) as u32, (rs & 15) as u32);
            if s == 0 {
                if r < 15 {
                    st.eobrun = (1 << r) - 1;
                    if r > 0 {
                        st.eobrun += br.bits(r);
                    }
                    break;
                }
                k += 16;
                continue;
            }
            k += r as usize;
            if k > 63 {
                return Err(malformed("AC coefficient index out of range"));
            }
            block[k] = (br.receive_extend(s) << al) as i16;
            k += 1;
        }
        return Ok(());
    }

    // AC refinement (follows libjpeg's decode_mcu_AC_refine).
    let p1 = 1i16 << al;
    let m1 = -1i16 << al;
    let mut k = ss;
    if st.eobrun == 0 {
        while k <= se {
            let rs = br.huff(table)?;
            let mut r = (rs >> 4) as i32;
            let mut s = 0i16;
            if rs & 15 != 0 {
                s = if br.bits(1) == 1 { p1 } else { m1 };
            } else if r != 15 {
                st.eobrun = 1 << r;
                if r > 0 {
                    st.eobrun += br.bits(r as u32);
                }
                break;
            }
            while k <= se {
                let c = &mut block[k];
                if *c != 0 {
                    if br.bits(1) == 1 && (*c & p1) == 0 {
                        *c += if *c >= 0 { p1 } else { m1 };
                    }
                } else {
                    r -= 1;
                    if r < 0 {
                        break;
                    }
                }
                k += 1;
            }
            if s != 0 {
                if k > 63 {
                    return Err(malformed("AC coefficient index out of range"));
                }
                block[k] = s;
            }
            k += 1;
        }
    }
    if st.eobrun > 0 {
        while k <= se {
            let c = &mut block[k];
            if *c != 0 && br.bits(1) == 1 && (*c & p1) == 0 {
                *c += if *c >= 0 { p1 } else { m1 };
            }
            k += 1;
        }
        st.eobrun -= 1;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_scan(
    data: &[u8],
    pos: usize,
    frame: &Frame,
    scan: &Scan,
    dc: &[Option<DecodeTable>; 4],
    ac: &[Option<DecodeTable>; 4],
    restart_interval: usize,
    coeffs: &mut [Vec<[i16; 64]>],
) -> Result<usize> {
    let need_dc = !frame.progressive || scan.ss == 0 && scan.ah == 0;
    let need_ac = !frame.progressive || scan.ss > 0;
    let mut st = ScanState {
        dc_tables: Vec::new(),
        ac_tables: Vec::new(),
        pred: vec![0; scan.comps.len()],
        eobrun: 0,
    };
    // Placeholder table for slots a scan doesn't use.
    let unused = DecodeTable::new(&Spec {
        counts: [0; 16],
        symbols: vec![],
    })?;
    for sc in &scan.comps {
        let d = if need_dc {
            dc[sc.dc]
                .as_ref()
                .ok_or_else(|| malformed("missing DC Huffman table"))?
        } else {
            &unused
        };
        let a = if need_ac {
            ac[sc.ac]
                .as_ref()
                .ok_or_else(|| malformed("missing AC Huffman table"))?
        } else {
            &unused
        };
        st.dc_tables.push(d);
        st.ac_tables.push(a);
    }

    let mut br = BitReader::new(data, pos);
    let mut mcu = 0usize;
    let handle_restart = |br: &mut BitReader, st: &mut ScanState, mcu: &mut usize, last: bool| {
        *mcu += 1;
        if restart_interval > 0 && (*mcu).is_multiple_of(restart_interval) && !last {
            br.restart();
            st.pred.iter_mut().for_each(|p| *p = 0);
            st.eobrun = 0;
        }
    };

    if scan.comps.len() == 1 {
        let c = scan.comps[0].index;
        let (bw, bh) = frame.blocks(c);
        let (sw, _) = frame.storage(c);
        let total = bw * bh;
        for by in 0..bh {
            for bx in 0..bw {
                decode_block(
                    &mut br,
                    scan,
                    &mut st,
                    0,
                    &mut coeffs[c][by * sw + bx],
                    frame.progressive,
                )?;
                let last = by * bw + bx + 1 == total;
                handle_restart(&mut br, &mut st, &mut mcu, last);
            }
        }
    } else {
        let (mx, my) = frame.mcus();
        for y in 0..my {
            for x in 0..mx {
                for (sc, comp) in scan.comps.iter().enumerate() {
                    let f = frame.comps[comp.index];
                    let (sw, _) = frame.storage(comp.index);
                    for v in 0..f.v as usize {
                        for h in 0..f.h as usize {
                            let bx = x * f.h as usize + h;
                            let by = y * f.v as usize + v;
                            decode_block(
                                &mut br,
                                scan,
                                &mut st,
                                sc,
                                &mut coeffs[comp.index][by * sw + bx],
                                frame.progressive,
                            )?;
                        }
                    }
                }
                handle_restart(&mut br, &mut st, &mut mcu, y * mx + x + 1 == mx * my);
            }
        }
    }

    // Find the next marker after the entropy-coded data.
    let mut p = br.byte_pos();
    while p + 1 < data.len() {
        if data[p] == 0xFF
            && data[p + 1] != 0x00
            && data[p + 1] != 0xFF
            && !(0xD0..=0xD7).contains(&data[p + 1])
        {
            return Ok(p);
        }
        p += 1;
    }
    Err(malformed("missing marker after scan"))
}

/// Parses a JPEG and decodes all scans into coefficients.
pub(crate) fn decode(data: &[u8]) -> Result<Jpeg> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err(Error::UnsupportedFormat);
    }
    let mut pos = 2;
    let mut segments = Vec::new();
    let mut dqt = Vec::new();
    let mut dqt_16bit = false;
    let mut qtables: [Option<[u16; 64]>; 4] = [None; 4];
    let mut qtables_redefined = false;
    let mut frame: Option<Frame> = None;
    let mut coeffs: Vec<Vec<[i16; 64]>> = Vec::new();
    let mut dc: [Option<DecodeTable>; 4] = Default::default();
    let mut ac: [Option<DecodeTable>; 4] = Default::default();
    let mut restart_interval = 0usize;
    let mut scans = 0;

    loop {
        // Skip fill bytes before the marker.
        while pos < data.len() && data[pos] == 0xFF && data.get(pos + 1) == Some(&0xFF) {
            pos += 1;
        }
        if pos + 1 >= data.len() || data[pos] != 0xFF {
            return Err(malformed("expected marker"));
        }
        let marker = data[pos + 1];
        pos += 2;
        if marker == 0xD9 {
            break;
        }
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            continue; // stray RSTn / TEM carry no payload
        }
        let len = be16(data, pos)?;
        if len < 2 || pos + len > data.len() {
            return Err(malformed("segment extends past end of file"));
        }
        let payload = &data[pos + 2..pos + len];
        let next = pos + len;
        match marker {
            0xC0..=0xC2 => {
                if frame.is_some() {
                    return Err(malformed("multiple frames"));
                }
                let f = parse_frame(payload, marker == 0xC2)?;
                coeffs = (0..f.comps.len())
                    .map(|c| {
                        let (w, h) = f.storage(c);
                        vec![[0i16; 64]; w * h]
                    })
                    .collect();
                frame = Some(f);
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Err(Error::Unsupported(
                    "lossless, hierarchical or arithmetic-coded JPEG".into(),
                ));
            }
            0xC4 => parse_dht(payload, &mut dc, &mut ac)?,
            0xDB => {
                dqt_16bit |= parse_dqt(payload, &mut qtables, &mut qtables_redefined)?;
                dqt.push(payload.to_vec());
            }
            0xDD => {
                restart_interval = be16(payload, 0)?;
            }
            0xDA => {
                let f = frame
                    .as_ref()
                    .ok_or_else(|| malformed("scan before frame header"))?;
                let scan = parse_scan(payload, f)?;
                pos = decode_scan(
                    data,
                    next,
                    f,
                    &scan,
                    &dc,
                    &ac,
                    restart_interval,
                    &mut coeffs,
                )?;
                scans += 1;
                continue;
            }
            0xDC => return Err(Error::Unsupported("DNL marker".into())),
            0xE0..=0xEF | 0xFE => segments.push((marker, payload.to_vec())),
            _ => return Err(Error::Unsupported(format!("JPEG marker 0x{marker:02X}"))),
        }
        pos = next;
    }
    let frame = frame.ok_or_else(|| malformed("missing frame header"))?;
    if scans == 0 {
        return Err(malformed("no scans"));
    }
    Ok(Jpeg {
        frame,
        segments,
        dqt,
        dqt_16bit,
        qtables,
        qtables_redefined,
        coeffs,
        trailing: data.len() - pos,
    })
}
