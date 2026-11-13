use sha2::{Digest, Sha256};

/// In-memory bytes or a reproducible xorshift64* stream, in little-endian words.
/// Seed zero is mapped to one, since zero is the absorbing xorshift state.
#[derive(Clone, Debug)]
pub enum Content {
    Generated { seed: u64, len: u64 },
    Bytes(Vec<u8>),
}

fn step(mut x: u64) -> u64 {
    x ^= x >> 12;
    x ^= x << 25;
    x ^ (x >> 27)
}

fn apply(matrix: &[u64; 64], mut x: u64) -> u64 {
    let mut result = 0;
    while x != 0 {
        let bit = x.trailing_zeros() as usize;
        result ^= matrix[bit];
        x &= x - 1;
    }
    result
}

// Jump ahead in the linear xorshift state without materializing the prefix.
fn jump(mut state: u64, mut words: u64) -> u64 {
    let mut matrix = std::array::from_fn(|i| step(1 << i));
    while words != 0 {
        if words & 1 != 0 {
            state = apply(&matrix, state);
        }
        words >>= 1;
        if words != 0 {
            matrix = std::array::from_fn(|i| apply(&matrix, matrix[i]));
        }
    }
    state
}

impl Content {
    pub fn len(&self) -> u64 {
        match self {
            Self::Generated { len, .. } => *len,
            Self::Bytes(bytes) => bytes.len() as u64,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn read_range(&self, start: u64, len: usize) -> Vec<u8> {
        let count = self.len().saturating_sub(start).min(len as u64) as usize;
        if count == 0 {
            return Vec::new();
        }
        match self {
            Self::Bytes(bytes) => bytes[start as usize..start as usize + count].to_vec(),
            Self::Generated { seed, .. } => {
                let mut state = jump((*seed).max(1), start / 8);
                let mut skip = (start % 8) as usize;
                let mut bytes = Vec::with_capacity(count);
                while bytes.len() < count {
                    state = step(state);
                    let word = state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes();
                    let take = (count - bytes.len()).min(8 - skip);
                    bytes.extend_from_slice(&word[skip..skip + take]);
                    skip = 0;
                }
                bytes
            }
        }
    }

    pub fn sha256_hex(&self) -> String {
        let mut hash = Sha256::new();
        let mut offset = 0;
        while offset < self.len() {
            let bytes = self.read_range(offset, 64 * 1024);
            offset += bytes.len() as u64;
            hash.update(bytes);
        }
        format!("{:x}", hash.finalize())
    }

    pub(crate) fn auto_etag(&self) -> String {
        match self {
            Self::Generated { seed, len } => format!("\"generated-{}-{len}\"", (*seed).max(1)),
            Self::Bytes(_) => format!("\"{}\"", self.sha256_hex()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_sha256_three_mb_matches_independent_stream() {
        let content = Content::Generated {
            seed: 42,
            len: 3 * 1024 * 1024,
        };
        let mut state = 42;
        let mut hash = Sha256::new();
        for _ in 0..content.len() / 8 {
            state = step(state);
            hash.update(state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes());
        }
        assert_eq!(content.sha256_hex(), format!("{:x}", hash.finalize()));
    }

    #[test]
    fn generated_huge_offsets_are_bounded_and_consistent() {
        let content = Content::Generated {
            seed: 17,
            len: u64::MAX,
        };
        let start = u64::MAX - 1024;
        let full = content.read_range(start, 1024);
        assert_eq!(full.len(), 1024);
        assert_eq!(content.read_range(start + 7, 99), full[7..106]);
        assert_eq!(content.read_range(u64::MAX - 3, 100), full[1021..]);
        assert!(content.read_range(u64::MAX, 100).is_empty());
    }
}
