//! SHA-256 (FIPS 180-4), the digest `checksums.txt` is written in.
//!
//! Hand-written, and the standard's own vectors are the test. The workspace
//! has no SHA-256 in its normal dependency tree — `sha2` is a dev-only edge
//! of the pty harness and `aws-lc-rs` builds C, which would cost this crate
//! the Windows cross-check that its rename dance needs (ADR-0043 §3). Sixty
//! lines of arithmetic buy that back and no dependency.

/// The round constants: the first thirty-two bits of the fractional parts of
/// the cube roots of the first sixty-four primes.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// The initial state: the fractional parts of the square roots of the first
/// eight primes.
const INITIAL: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// One message's digest.
pub fn digest(bytes: &[u8]) -> [u8; 32] {
    let mut state = INITIAL;
    let mut blocks = bytes.chunks_exact(64);
    for block in &mut blocks {
        compress(&mut state, block);
    }
    let (last, more) = tail(blocks.remainder(), bytes.len());
    compress(&mut state, &last);
    if let Some(block) = more {
        compress(&mut state, &block);
    }
    let mut out = [0u8; 32];
    for (word, slot) in state.iter().zip(out.chunks_exact_mut(4)) {
        slot.copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// The padding: what is left of the message, a set bit, zeros, and the length
/// in bits at the end — in one block, or in two when the length would not fit.
fn tail(rest: &[u8], len: usize) -> ([u8; 64], Option<[u8; 64]>) {
    let bits = (len as u64).wrapping_mul(8).to_be_bytes();
    let mut last = [0u8; 64];
    last[..rest.len()].copy_from_slice(rest);
    last[rest.len()] = 0x80;
    if rest.len() + 9 <= 64 {
        last[56..].copy_from_slice(&bits);
        return (last, None);
    }
    let mut more = [0u8; 64];
    more[56..].copy_from_slice(&bits);
    (last, Some(more))
}

/// One 64-byte block into the state.
fn compress(state: &mut [u32; 8], block: &[u8]) {
    let mut w = [0u32; 64];
    for (word, slot) in block.chunks_exact(4).zip(w.iter_mut()) {
        *slot = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
    }
    for i in 16..64 {
        let a = w[i - 15];
        let b = w[i - 2];
        let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
        let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let mut h = *state;
    for (k, word) in K.iter().zip(w.iter()) {
        h = round(h, *k, *word);
    }
    for (slot, add) in state.iter_mut().zip(h) {
        *slot = slot.wrapping_add(add);
    }
}

/// One of the sixty-four rounds.
fn round(h: [u32; 8], k: u32, word: u32) -> [u32; 8] {
    let s1 = h[4].rotate_right(6) ^ h[4].rotate_right(11) ^ h[4].rotate_right(25);
    let choice = (h[4] & h[5]) ^ (!h[4] & h[6]);
    let t1 = h[7]
        .wrapping_add(s1)
        .wrapping_add(choice)
        .wrapping_add(k)
        .wrapping_add(word);
    let s0 = h[0].rotate_right(2) ^ h[0].rotate_right(13) ^ h[0].rotate_right(22);
    let majority = (h[0] & h[1]) ^ (h[0] & h[2]) ^ (h[1] & h[2]);
    let t2 = s0.wrapping_add(majority);
    [
        t1.wrapping_add(t2),
        h[0],
        h[1],
        h[2],
        h[3].wrapping_add(t1),
        h[4],
        h[5],
        h[6],
    ]
}

/// A digest as `sha256sum` writes it.
pub fn hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(64);
    for byte in digest {
        // Writing to a `String` cannot fail.
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest_of(text: &str) -> String {
        hex(&digest(text.as_bytes()))
    }

    #[test]
    fn the_standards_own_vectors() {
        assert_eq!(
            digest_of(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            digest_of("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            digest_of("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            digest_of(
                "abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmn\
                 hijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
            ),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    #[test]
    fn a_million_letters_hash_as_the_standard_says() {
        let bytes = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(&digest(&bytes)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// Every length across the two block boundaries, against `shasum -a 256`:
    /// an off-by-one in the padding hides everywhere but here. 55 is the last
    /// length whose padding fits one block, 56 the first that needs two, and
    /// 64 and 120 are whole blocks with nothing left over.
    #[test]
    fn the_padding_is_right_on_both_sides_of_a_block() {
        let expected = [
            (
                55,
                "d5e285683cd4efc02d021a5c62014694958901005d6f71e89e0989fac77e4072",
            ),
            (
                56,
                "04c26261370ee7541549d16dee320c723e3fd14671e66a099afe0a377c16888e",
            ),
            (
                63,
                "75220b47218278e656f2013bb8f0c455a25eaf01e86c64924e9d48d89776d6f2",
            ),
            (
                64,
                "7ce100971f64e7001e8fe5a51973ecdfe1ced42befe7ee8d5fd6219506b5393c",
            ),
            (
                119,
                "000b48d4edf0fa7bee3c6236ecd2785baa5db4eeb8bb54341b029e0d9fa5fb0c",
            ),
            (
                120,
                "13f05a0b594787f5ecd315edc96141bd3243203d1b7d4f0836f37308b276ba98",
            ),
        ];
        for (len, digest_hex) in expected {
            assert_eq!(hex(&digest(&vec![b'x'; len])), digest_hex, "{len} bytes");
        }
    }
}
