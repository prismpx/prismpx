//! JPEG Huffman tables: decoding tables and optimal table construction.

use crate::deflate::huffman::length_limited;
use crate::error::{malformed, Result};

/// A table as stored in a DHT segment: code counts per length 1..=16 and symbols.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spec {
    pub counts: [u8; 16],
    pub symbols: Vec<u8>,
}

impl Spec {
    /// Optimal table for the given symbol frequencies, with no code longer
    /// than 16 bits and the all-ones code left unused (as JPEG requires).
    pub fn optimal(freq: &[u32; 256]) -> Spec {
        // A reserved symbol with strictly the lowest weight gets the longest
        // code and, being the highest symbol value, the last (all-ones) code
        // of that length; dropping it reserves that code point.
        let mut weights: Vec<u32> = freq.iter().map(|&f| f.saturating_mul(2)).collect();
        if weights.iter().all(|&w| w == 0) {
            weights[0] = 2;
        }
        weights.push(1);
        let lens = length_limited(&weights, 16);
        let mut counts = [0u8; 16];
        let mut by_len: Vec<(u8, u8)> = Vec::new();
        for (sym, &l) in lens[..256].iter().enumerate() {
            if l > 0 {
                counts[l as usize - 1] += 1;
                by_len.push((l, sym as u8));
            }
        }
        by_len.sort_unstable();
        Spec {
            counts,
            symbols: by_len.into_iter().map(|(_, s)| s).collect(),
        }
    }

    /// Canonical (code, length) per symbol; length 0 for absent symbols.
    pub fn codes(&self) -> [(u16, u8); 256] {
        let mut out = [(0u16, 0u8); 256];
        let mut code = 0u32;
        let mut k = 0;
        for len in 1..=16u8 {
            for _ in 0..self.counts[len as usize - 1] {
                out[self.symbols[k] as usize] = (code as u16, len);
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        out
    }

    /// DHT payload bytes for this table with class (0 = DC, 1 = AC) and id.
    pub fn dht(&self, class: u8, id: u8) -> Vec<u8> {
        let mut v = vec![class << 4 | id];
        v.extend_from_slice(&self.counts);
        v.extend_from_slice(&self.symbols);
        v
    }
}

/// Decoding table: 9-bit lookahead plus canonical fallback for longer codes.
#[derive(Clone)]
pub(crate) struct DecodeTable {
    lookup: Vec<u16>, // (length << 8) | symbol; 0 = not resolved by lookahead
    maxcode: [i32; 17],
    mincode: [i32; 17],
    valptr: [i32; 17],
    symbols: Vec<u8>,
}

pub(crate) const LOOKAHEAD: u32 = 9;

impl DecodeTable {
    pub fn new(spec: &Spec) -> Result<DecodeTable> {
        let total: usize = spec.counts.iter().map(|&c| c as usize).sum();
        if total != spec.symbols.len() || total > 256 {
            return Err(malformed("invalid Huffman table"));
        }
        let mut maxcode = [-1i32; 17];
        let mut mincode = [0i32; 17];
        let mut valptr = [0i32; 17];
        let mut lookup = vec![0u16; 1 << LOOKAHEAD];
        let mut code = 0i32;
        let mut k = 0usize;
        for len in 1..=16usize {
            let n = spec.counts[len - 1] as i32;
            valptr[len] = k as i32;
            mincode[len] = code;
            if n > 0 {
                maxcode[len] = code + n - 1;
            }
            for i in 0..n {
                let c = code + i;
                if len as u32 <= LOOKAHEAD {
                    let shift = LOOKAHEAD - len as u32;
                    let base = (c as usize) << shift;
                    for j in 0..(1usize << shift) {
                        lookup[base + j] = (len as u16) << 8 | spec.symbols[k + i as usize] as u16;
                    }
                }
            }
            k += n as usize;
            code += n;
            if code > (1 << len) {
                return Err(malformed("over-subscribed Huffman table"));
            }
            code <<= 1;
        }
        Ok(DecodeTable {
            lookup,
            maxcode,
            mincode,
            valptr,
            symbols: spec.symbols.clone(),
        })
    }

    /// Decodes one symbol given the next 16 bits (MSB-first); returns (symbol, length).
    #[inline]
    pub fn decode(&self, peek16: u32) -> Option<(u8, u32)> {
        let e = self.lookup[(peek16 >> (16 - LOOKAHEAD)) as usize];
        if e != 0 {
            return Some((e as u8, (e >> 8) as u32));
        }
        for len in (LOOKAHEAD as usize + 1)..=16 {
            let c = (peek16 >> (16 - len)) as i32;
            if c <= self.maxcode[len] {
                let idx = self.valptr[len] + c - self.mincode[len];
                return self.symbols.get(idx as usize).map(|&s| (s, len as u32));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimal_tables_are_valid_and_reserve_all_ones() {
        let mut freq = [0u32; 256];
        // Skewed distribution with a long tail.
        for (i, f) in freq.iter_mut().enumerate().take(200) {
            *f = 1 + (1u32 << (i % 20)) / (i as u32 + 1);
        }
        let spec = Spec::optimal(&freq);
        let codes = spec.codes();
        assert_eq!(spec.symbols.len(), 200);
        for &(code, len) in codes.iter().filter(|c| c.1 > 0) {
            assert!(len <= 16);
            assert_ne!(code as u32, (1u32 << len) - 1, "all-ones code used");
        }
        let table = DecodeTable::new(&spec).unwrap();
        for (sym, &(code, len)) in codes.iter().enumerate() {
            if len > 0 {
                let peek = (code as u32) << (16 - len);
                assert_eq!(table.decode(peek), Some((sym as u8, len as u32)));
            }
        }
    }

    #[test]
    fn single_symbol_table() {
        let mut freq = [0u32; 256];
        freq[0] = 10;
        let spec = Spec::optimal(&freq);
        assert_eq!(spec.symbols, vec![0]);
        assert_eq!(spec.codes()[0], (0, 1));
    }
}
