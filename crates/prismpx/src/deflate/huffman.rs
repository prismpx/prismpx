//! Huffman code construction shared by the DEFLATE encoder and decoder.

#[derive(Clone, Copy)]
enum Node {
    Leaf(u16),
    /// Indices of the two items in the previous level that form this package.
    Package(u32, u32),
}

#[derive(Clone, Copy)]
struct Item {
    weight: u64,
    node: Node,
}

/// Optimal length-limited Huffman code lengths (package-merge algorithm).
///
/// Symbols with zero frequency get length 0. If only one symbol is used, a
/// second symbol is given length 1 as well so the code stays complete, which
/// every decoder accepts.
pub(crate) fn length_limited(freqs: &[u32], limit: u8) -> Vec<u8> {
    let n = freqs.len();
    let mut lens = vec![0u8; n];
    let mut leaves: Vec<Item> = freqs
        .iter()
        .enumerate()
        .filter(|(_, &f)| f > 0)
        .map(|(i, &f)| Item {
            weight: f as u64,
            node: Node::Leaf(i as u16),
        })
        .collect();

    match leaves.len() {
        0 => return lens,
        1 => {
            let sym = match leaves[0].node {
                Node::Leaf(s) => s as usize,
                Node::Package(..) => unreachable!(),
            };
            lens[sym] = 1;
            let other = if sym == 0 { 1 } else { 0 };
            if other < n {
                lens[other] = 1;
            }
            return lens;
        }
        _ => {}
    }
    assert!(
        leaves.len() <= 1usize << limit,
        "too many symbols for code length limit"
    );

    let leaf_sym = |it: &Item| match it.node {
        Node::Leaf(s) => s,
        Node::Package(..) => u16::MAX,
    };
    leaves.sort_by(|a, b| a.weight.cmp(&b.weight).then(leaf_sym(a).cmp(&leaf_sym(b))));

    let mut levels: Vec<Vec<Item>> = Vec::with_capacity(limit as usize);
    levels.push(leaves.clone());
    for _ in 1..limit {
        let prev = levels.last().unwrap();
        let packages = prev.len() / 2;
        let mut merged = Vec::with_capacity(leaves.len() + packages);
        let (mut i, mut j) = (0, 0);
        while i < leaves.len() || j < packages {
            let package = (j < packages).then(|| Item {
                weight: prev[2 * j].weight + prev[2 * j + 1].weight,
                node: Node::Package((2 * j) as u32, (2 * j + 1) as u32),
            });
            let take_leaf = match package {
                None => true,
                Some(p) => i < leaves.len() && leaves[i].weight <= p.weight,
            };
            if take_leaf {
                merged.push(leaves[i]);
                i += 1;
            } else {
                merged.push(package.unwrap());
                j += 1;
            }
        }
        levels.push(merged);
    }

    // The first 2n-2 items of the last level define the code: each time a leaf
    // appears (directly or inside a package) its code gets one bit longer.
    let top = levels.len() - 1;
    let take = 2 * leaves.len() - 2;
    let mut stack: Vec<(usize, u32)> = (0..take as u32).map(|k| (top, k)).collect();
    while let Some((level, idx)) = stack.pop() {
        match levels[level][idx as usize].node {
            Node::Leaf(s) => lens[s as usize] += 1,
            Node::Package(a, b) => {
                stack.push((level - 1, a));
                stack.push((level - 1, b));
            }
        }
    }
    lens
}

/// Canonical Huffman codes for the given lengths, bit-reversed so they can be
/// written LSB-first as DEFLATE requires.
pub(crate) fn canonical_codes(lens: &[u8]) -> Vec<u16> {
    let max = lens.iter().copied().max().unwrap_or(0) as usize;
    let mut bl_count = vec![0u32; max + 1];
    for &l in lens {
        if l > 0 {
            bl_count[l as usize] += 1;
        }
    }
    let mut next = vec![0u32; max + 1];
    let mut code = 0u32;
    for bits in 1..=max {
        code = (code + bl_count[bits - 1]) << 1;
        next[bits] = code;
    }
    lens.iter()
        .map(|&l| {
            if l == 0 {
                0
            } else {
                let c = next[l as usize];
                next[l as usize] += 1;
                (c.reverse_bits() >> (32 - l as u32)) as u16
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kraft_sum(lens: &[u8], limit: u8) -> u64 {
        lens.iter()
            .filter(|&&l| l > 0)
            .map(|&l| 1u64 << (limit - l))
            .sum()
    }

    #[test]
    fn complete_and_limited() {
        // Fibonacci frequencies force very deep unrestricted trees.
        let mut f = vec![1u32, 1];
        while f.len() < 30 {
            let k = f.len();
            f.push(f[k - 1] + f[k - 2]);
        }
        let lens = length_limited(&f, 15);
        assert!(lens.iter().all(|&l| (1..=15).contains(&l)));
        assert_eq!(kraft_sum(&lens, 15), 1 << 15);
    }

    #[test]
    fn matches_unrestricted_when_limit_is_loose() {
        let f = [5u32, 9, 12, 13, 16, 45];
        let lens = length_limited(&f, 15);
        let cost: u32 = f.iter().zip(&lens).map(|(&a, &b)| a * b as u32).sum();
        assert_eq!(cost, 224); // optimal Huffman cost for this classic example
    }

    #[test]
    fn single_symbol() {
        let lens = length_limited(&[0, 0, 7, 0], 15);
        assert_eq!(lens, vec![1, 0, 1, 0]);
    }
}
