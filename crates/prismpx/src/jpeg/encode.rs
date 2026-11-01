//! Lossless JPEG re-encoding with per-image optimal Huffman tables.
//!
//! Two layouts are produced and the smaller is kept:
//! - baseline: one interleaved sequential scan (or one scan per component);
//! - progressive with spectral selection only (no successive approximation):
//!   a DC scan, then AC scans per component, where each component's AC band
//!   split is chosen independently by encoded size.
//!
//! Every scan gets its own optimal tables, so splitting AC bands lets each
//! band's statistics be coded separately.

use super::huffman::Spec;
use super::Jpeg;
use crate::options::Effort;

/// Huffman table slots: DC 0/1 and AC 0/1.
const DC0: usize = 0;
const AC0: usize = 2;

trait Sink {
    fn sym(&mut self, table: usize, s: u8);
    fn bits(&mut self, v: u32, n: u32);
}

struct Counter {
    freq: [[u32; 256]; 4],
}

impl Sink for Counter {
    #[inline]
    fn sym(&mut self, table: usize, s: u8) {
        self.freq[table][s as usize] += 1;
    }
    #[inline]
    fn bits(&mut self, _v: u32, _n: u32) {}
}

struct Writer {
    out: Vec<u8>,
    buf: u64,
    cnt: u32,
    codes: [[(u16, u8); 256]; 4],
}

impl Writer {
    #[inline]
    fn put(&mut self, v: u32, n: u32) {
        if n == 0 {
            return;
        }
        self.buf = (self.buf << n) | (v as u64 & ((1u64 << n) - 1));
        self.cnt += n;
        while self.cnt >= 8 {
            let b = (self.buf >> (self.cnt - 8)) as u8;
            self.out.push(b);
            if b == 0xFF {
                self.out.push(0);
            }
            self.cnt -= 8;
        }
        self.buf &= (1u64 << self.cnt) - 1;
    }

    fn finish(mut self) -> Vec<u8> {
        if self.cnt > 0 {
            let pad = 8 - self.cnt;
            self.put((1 << pad) - 1, pad);
        }
        self.out
    }
}

impl Sink for Writer {
    #[inline]
    fn sym(&mut self, table: usize, s: u8) {
        let (code, len) = self.codes[table][s as usize];
        debug_assert!(len > 0, "symbol without a code");
        self.put(code as u32, len as u32);
    }
    #[inline]
    fn bits(&mut self, v: u32, n: u32) {
        self.put(v, n);
    }
}

#[inline]
fn category(v: i32) -> u32 {
    32 - v.unsigned_abs().leading_zeros()
}

#[inline]
fn value_bits(v: i32, s: u32) -> u32 {
    (if v < 0 { v - 1 } else { v }) as u32 & ((1u32 << s) - 1)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Full blocks, DC and AC (baseline).
    Sequential,
    /// DC coefficients only (progressive, Al = 0).
    DcFirst,
    /// AC band `ss..=se` of one component (progressive, Al = 0).
    AcFirst(u8, u8),
}

struct Plan {
    comps: Vec<usize>,
    kind: Kind,
}

impl Plan {
    /// Table slot (0 or 1) for the i-th component of the scan.
    fn slot(&self, i: usize) -> usize {
        if self.comps.len() > 1 && self.comps[i] != 0 {
            1
        } else {
            0
        }
    }
}

fn encode_block_dc<S: Sink>(sink: &mut S, slot: usize, block: &[i16; 64], pred: &mut i32) {
    let dc = block[0] as i32;
    let diff = dc - *pred;
    *pred = dc;
    let s = category(diff);
    sink.sym(DC0 + slot, s as u8);
    sink.bits(value_bits(diff, s), s);
}

fn encode_block_ac<S: Sink>(sink: &mut S, slot: usize, block: &[i16; 64]) {
    let mut run = 0u32;
    for &c in &block[1..] {
        let v = c as i32;
        if v == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            sink.sym(AC0 + slot, 0xF0);
            run -= 16;
        }
        let s = category(v);
        sink.sym(AC0 + slot, (run << 4 | s) as u8);
        sink.bits(value_bits(v, s), s);
        run = 0;
    }
    if run > 0 {
        sink.sym(AC0 + slot, 0x00);
    }
}

fn flush_eobrun<S: Sink>(sink: &mut S, eobrun: &mut u32) {
    if *eobrun > 0 {
        let nbits = 31 - eobrun.leading_zeros();
        sink.sym(AC0, (nbits << 4) as u8);
        sink.bits(*eobrun - (1 << nbits), nbits);
        *eobrun = 0;
    }
}

fn encode_scan<S: Sink>(sink: &mut S, jpeg: &Jpeg, plan: &Plan) {
    let frame = &jpeg.frame;
    match plan.kind {
        Kind::Sequential | Kind::DcFirst => {
            let mut pred = vec![0i32; plan.comps.len()];
            let ac = plan.kind == Kind::Sequential;
            let mut block_fn = |sink: &mut S, i: usize, block: &[i16; 64]| {
                encode_block_dc(sink, plan.slot(i), block, &mut pred[i]);
                if ac {
                    encode_block_ac(sink, plan.slot(i), block);
                }
            };
            if plan.comps.len() == 1 {
                let c = plan.comps[0];
                let (bw, bh) = frame.blocks(c);
                let (sw, _) = frame.storage(c);
                for y in 0..bh {
                    for x in 0..bw {
                        block_fn(sink, 0, &jpeg.coeffs[c][y * sw + x]);
                    }
                }
            } else {
                let (mx, my) = frame.mcus();
                for y in 0..my {
                    for x in 0..mx {
                        for (i, &c) in plan.comps.iter().enumerate() {
                            let f = frame.comps[c];
                            let (sw, _) = frame.storage(c);
                            for v in 0..f.v as usize {
                                for h in 0..f.h as usize {
                                    let (bx, by) = (x * f.h as usize + h, y * f.v as usize + v);
                                    block_fn(sink, i, &jpeg.coeffs[c][by * sw + bx]);
                                }
                            }
                        }
                    }
                }
            }
        }
        Kind::AcFirst(ss, se) => {
            let c = plan.comps[0];
            let (bw, bh) = frame.blocks(c);
            let (sw, _) = frame.storage(c);
            let mut eobrun = 0u32;
            for y in 0..bh {
                for x in 0..bw {
                    let block = &jpeg.coeffs[c][y * sw + x];
                    let mut run = 0u32;
                    for &coef in &block[ss as usize..=se as usize] {
                        let v = coef as i32;
                        if v == 0 {
                            run += 1;
                            continue;
                        }
                        flush_eobrun(sink, &mut eobrun);
                        while run > 15 {
                            sink.sym(AC0, 0xF0);
                            run -= 16;
                        }
                        let s = category(v);
                        sink.sym(AC0, (run << 4 | s) as u8);
                        sink.bits(value_bits(v, s), s);
                        run = 0;
                    }
                    if run > 0 {
                        eobrun += 1;
                        if eobrun == 0x7FFF {
                            flush_eobrun(sink, &mut eobrun);
                        }
                    }
                }
            }
            flush_eobrun(sink, &mut eobrun);
        }
    }
}

fn segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(payload);
}

/// DHT + SOS + entropy-coded data for one scan.
fn scan_bytes(jpeg: &Jpeg, plan: &Plan) -> Vec<u8> {
    let mut counter = Counter {
        freq: [[0; 256]; 4],
    };
    encode_scan(&mut counter, jpeg, plan);
    let mut dht = Vec::new();
    let mut codes = [[(0u16, 0u8); 256]; 4];
    for (t, freq) in counter.freq.iter().enumerate() {
        if freq.iter().any(|&f| f > 0) {
            let spec = Spec::optimal(freq);
            codes[t] = spec.codes();
            dht.extend(spec.dht((t / 2) as u8, (t % 2) as u8));
        }
    }
    let mut out = Vec::new();
    segment(&mut out, 0xC4, &dht);

    let (ss, se) = match plan.kind {
        Kind::Sequential => (0, 63),
        Kind::DcFirst => (0, 0),
        Kind::AcFirst(ss, se) => (ss, se),
    };
    let mut sos = vec![plan.comps.len() as u8];
    for (i, &c) in plan.comps.iter().enumerate() {
        let slot = plan.slot(i) as u8;
        let tables = match plan.kind {
            Kind::Sequential => slot << 4 | slot,
            Kind::DcFirst => slot << 4,
            Kind::AcFirst(..) => 0,
        };
        sos.extend_from_slice(&[jpeg.frame.comps[c].id, tables]);
    }
    sos.extend_from_slice(&[ss, se, 0]);
    segment(&mut out, 0xDA, &sos);

    let mut w = Writer {
        out,
        buf: 0,
        cnt: 0,
        codes,
    };
    encode_scan(&mut w, jpeg, plan);
    w.finish()
}

fn assemble(jpeg: &Jpeg, segments: &[(u8, Vec<u8>)], sof: u8, scans: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    for (m, p) in segments {
        segment(&mut out, *m, p);
    }
    for q in &jpeg.dqt {
        segment(&mut out, 0xDB, q);
    }
    let f = &jpeg.frame;
    let mut p = vec![8];
    p.extend_from_slice(&f.height.to_be_bytes());
    p.extend_from_slice(&f.width.to_be_bytes());
    p.push(f.comps.len() as u8);
    for c in &f.comps {
        p.extend_from_slice(&[c.id, c.h << 4 | c.v, c.tq]);
    }
    segment(&mut out, sof, &p);
    for s in scans {
        out.extend_from_slice(s);
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

/// AC band splits tried per component in progressive mode.
const SPLITS: [&[(u8, u8)]; 5] = [
    &[(1, 63)],
    &[(1, 5), (6, 63)],
    &[(1, 2), (3, 63)],
    &[(1, 9), (10, 63)],
    &[(1, 2), (3, 9), (10, 63)],
];

fn baseline(jpeg: &Jpeg, segments: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let n = jpeg.frame.comps.len();
    let plans: Vec<Plan> = if jpeg.frame.can_interleave() {
        vec![Plan {
            comps: (0..n).collect(),
            kind: Kind::Sequential,
        }]
    } else {
        (0..n)
            .map(|c| Plan {
                comps: vec![c],
                kind: Kind::Sequential,
            })
            .collect()
    };
    let scans: Vec<Vec<u8>> = plans.iter().map(|p| scan_bytes(jpeg, p)).collect();
    let sof = if jpeg.dqt_16bit { 0xC1 } else { 0xC0 };
    assemble(jpeg, segments, sof, &scans)
}

fn progressive(jpeg: &Jpeg, segments: &[(u8, Vec<u8>)], effort: Effort) -> Vec<u8> {
    let n = jpeg.frame.comps.len();
    let mut scans: Vec<Vec<u8>> = if jpeg.frame.can_interleave() {
        vec![scan_bytes(
            jpeg,
            &Plan {
                comps: (0..n).collect(),
                kind: Kind::DcFirst,
            },
        )]
    } else {
        (0..n)
            .map(|c| {
                scan_bytes(
                    jpeg,
                    &Plan {
                        comps: vec![c],
                        kind: Kind::DcFirst,
                    },
                )
            })
            .collect()
    };
    let splits: &[&[(u8, u8)]] = if effort == Effort::Fast {
        &SPLITS[..2]
    } else {
        &SPLITS
    };
    for c in 0..n {
        let best = splits
            .iter()
            .map(|split| {
                split
                    .iter()
                    .map(|&(ss, se)| {
                        scan_bytes(
                            jpeg,
                            &Plan {
                                comps: vec![c],
                                kind: Kind::AcFirst(ss, se),
                            },
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .min_by_key(|bands| bands.iter().map(Vec::len).sum::<usize>())
            .expect("at least one split");
        scans.extend(best);
    }
    assemble(jpeg, segments, 0xC2, &scans)
}

/// Encodes baseline and progressive versions and returns the smaller with its layout name.
pub(super) fn encode_best(
    jpeg: &Jpeg,
    segments: &[(u8, Vec<u8>)],
    effort: Effort,
) -> (Vec<u8>, &'static str) {
    let b = baseline(jpeg, segments);
    let p = progressive(jpeg, segments, effort);
    if p.len() < b.len() {
        (p, "progressive")
    } else {
        (b, "baseline")
    }
}
