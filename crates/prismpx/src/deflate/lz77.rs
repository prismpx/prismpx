use super::encode::Level;

/// A literal byte (`dist == 0`, value in `len`) or a back-reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Token {
    pub len: u16,
    pub dist: u16,
}

impl Token {
    fn literal(b: u8) -> Token {
        Token {
            len: b as u16,
            dist: 0,
        }
    }

    pub fn is_literal(self) -> bool {
        self.dist == 0
    }

    /// Number of input bytes this token covers.
    pub fn span(self) -> usize {
        if self.is_literal() {
            1
        } else {
            self.len as usize
        }
    }
}

const WINDOW: usize = 32768;
const HASH_BITS: u32 = 15;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
/// Length-3 matches this far back usually cost more than three literals.
const TOO_FAR: usize = 4096;

struct Matcher<'a> {
    data: &'a [u8],
    head: Vec<i32>,
    prev: Vec<i32>,
    max_chain: u32,
    nice: usize,
}

impl<'a> Matcher<'a> {
    #[inline]
    fn hash(&self, p: usize) -> usize {
        let d = self.data;
        let v = d[p] as u32 | (d[p + 1] as u32) << 8 | (d[p + 2] as u32) << 16;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    }

    #[inline]
    fn insert(&mut self, p: usize) {
        if p + MIN_MATCH <= self.data.len() {
            let h = self.hash(p);
            self.prev[p & (WINDOW - 1)] = self.head[h];
            self.head[h] = p as i32;
        }
    }

    /// Longest match for position `p` among already-inserted positions.
    fn find(&self, p: usize) -> (usize, usize) {
        let d = self.data;
        if p + MIN_MATCH > d.len() {
            return (0, 0);
        }
        let max_len = MAX_MATCH.min(d.len() - p);
        let (mut best, mut best_dist) = (0usize, 0usize);
        let mut cand = self.head[self.hash(p)];
        let mut chain = self.max_chain;
        while cand >= 0 && chain > 0 {
            let c = cand as usize;
            if c >= p || p - c > WINDOW {
                break;
            }
            if d[c + best] == d[p + best] {
                let l = d[c..c + max_len]
                    .iter()
                    .zip(&d[p..p + max_len])
                    .take_while(|(a, b)| a == b)
                    .count();
                if l > best {
                    best = l;
                    best_dist = p - c;
                    if l >= self.nice || l == max_len {
                        break;
                    }
                }
            }
            let next = self.prev[c & (WINDOW - 1)];
            if next >= cand {
                break; // stale entry from a newer position
            }
            cand = next;
            chain -= 1;
        }
        if best < MIN_MATCH || (best == MIN_MATCH && best_dist > TOO_FAR) {
            (0, 0)
        } else {
            (best, best_dist)
        }
    }
}

pub(crate) fn parse(data: &[u8], level: &Level) -> Vec<Token> {
    let n = data.len();
    let mut tokens = Vec::with_capacity(n / 3 + 16);
    let mut m = Matcher {
        data,
        head: vec![-1; 1 << HASH_BITS],
        prev: vec![-1; WINDOW],
        max_chain: level.max_chain.max(1),
        nice: level.nice_len.clamp(MIN_MATCH, MAX_MATCH),
    };
    let emit_match = |tokens: &mut Vec<Token>, len: usize, dist: usize| {
        tokens.push(Token {
            len: len as u16,
            dist: dist as u16,
        })
    };

    if !level.lazy {
        let mut p = 0;
        while p < n {
            let (len, dist) = m.find(p);
            m.insert(p);
            if len >= MIN_MATCH {
                emit_match(&mut tokens, len, dist);
                for q in p + 1..p + len {
                    m.insert(q);
                }
                p += len;
            } else {
                tokens.push(Token::literal(data[p]));
                p += 1;
            }
        }
        return tokens;
    }

    // Lazy evaluation: defer each match by one byte in case the next
    // position starts a longer one.
    let (mut prev_len, mut prev_dist) = (0usize, 0usize);
    let mut pending = false;
    let mut p = 0;
    while p < n {
        let (cur_len, cur_dist) = if prev_len >= m.nice {
            (0, 0)
        } else {
            m.find(p)
        };
        m.insert(p);
        if prev_len >= MIN_MATCH && cur_len <= prev_len {
            emit_match(&mut tokens, prev_len, prev_dist);
            let end = p - 1 + prev_len;
            for q in p + 1..end {
                m.insert(q);
            }
            p = end;
            prev_len = 0;
            pending = false;
        } else {
            if pending {
                tokens.push(Token::literal(data[p - 1]));
            }
            pending = true;
            prev_len = cur_len;
            prev_dist = cur_dist;
            p += 1;
        }
    }
    if pending {
        tokens.push(Token::literal(data[n - 1]));
    }
    tokens
}
