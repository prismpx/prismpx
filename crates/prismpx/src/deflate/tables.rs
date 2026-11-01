pub(crate) const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
pub(crate) const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
pub(crate) const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
pub(crate) const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Order in which code-length code lengths are stored in a dynamic block header.
pub(crate) const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Maps a match length (3..=258) to (code index 0..29, extra bit count, extra value).
#[inline]
pub(crate) fn len_code(len: u16) -> (usize, u8, u16) {
    let i = LEN_BASE.partition_point(|&b| b <= len) - 1;
    (i, LEN_EXTRA[i], len - LEN_BASE[i])
}

/// Maps a distance (1..=32768) to (code 0..30, extra bit count, extra value).
#[inline]
pub(crate) fn dist_code(dist: u16) -> (usize, u8, u16) {
    let i = DIST_BASE.partition_point(|&b| b <= dist) - 1;
    (i, DIST_EXTRA[i], dist - DIST_BASE[i])
}

pub(crate) fn fixed_lit_lengths() -> [u8; 288] {
    let mut l = [0u8; 288];
    l[..144].fill(8);
    l[144..256].fill(9);
    l[256..280].fill(7);
    l[280..].fill(8);
    l
}

pub(crate) fn fixed_dist_lengths() -> [u8; 30] {
    [5u8; 30]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_codes() {
        assert_eq!(len_code(3), (0, 0, 0));
        assert_eq!(len_code(11), (8, 1, 0));
        assert_eq!(len_code(12), (8, 1, 1));
        assert_eq!(len_code(257), (27, 5, 30));
        assert_eq!(len_code(258), (28, 0, 0));
        assert_eq!(dist_code(1), (0, 0, 0));
        assert_eq!(dist_code(5), (4, 1, 0));
        assert_eq!(dist_code(32768), (29, 13, 8191));
    }
}
