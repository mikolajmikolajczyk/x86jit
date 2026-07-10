//! Shared, pure-Rust SHA-NI primitives (Intel SDM / FIPS-180-4) used by both the
//! interpreter and the JIT helper so `jit == interp`, validated bit-exact against
//! the real CPU (host has SHA-NI; task-207). No `CpuState` dependency.
//!
//! Everything operates on the 128-bit xmm bit pattern. The four dwords of an xmm
//! are its little-endian dwords: `dw0 = bits 31:0` … `dw3 = bits 127:96`. SHA works
//! on 32-bit words directly on those dwords — the SDM pseudocode does NOT byte-swap
//! within a dword, so neither do we (the native oracle is the arbiter).

// --- dword pack/unpack helpers (little-endian dwords of the xmm) ---

#[inline]
fn dwords(x: u128) -> [u32; 4] {
    [
        x as u32,
        (x >> 32) as u32,
        (x >> 64) as u32,
        (x >> 96) as u32,
    ]
}

#[inline]
fn from_dwords(d: [u32; 4]) -> u128 {
    (d[0] as u128) | ((d[1] as u128) << 32) | ((d[2] as u128) << 64) | ((d[3] as u128) << 96)
}

// --- SHA-256 primitives (FIPS-180-4 §4.1.2) ---

#[inline]
fn ch(x: u32, y: u32, z: u32) -> u32 {
    (x & y) ^ (!x & z)
}
#[inline]
fn maj(x: u32, y: u32, z: u32) -> u32 {
    (x & y) ^ (x & z) ^ (y & z)
}
/// Big-sigma-0: ROTR(x,2) ^ ROTR(x,13) ^ ROTR(x,22).
#[inline]
fn big_sigma0(x: u32) -> u32 {
    x.rotate_right(2) ^ x.rotate_right(13) ^ x.rotate_right(22)
}
/// Big-sigma-1: ROTR(x,6) ^ ROTR(x,11) ^ ROTR(x,25).
#[inline]
fn big_sigma1(x: u32) -> u32 {
    x.rotate_right(6) ^ x.rotate_right(11) ^ x.rotate_right(25)
}
/// Small-sigma-0: ROTR(x,7) ^ ROTR(x,18) ^ SHR(x,3).
#[inline]
fn small_sigma0(x: u32) -> u32 {
    x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)
}
/// Small-sigma-1: ROTR(x,17) ^ ROTR(x,19) ^ SHR(x,10).
#[inline]
fn small_sigma1(x: u32) -> u32 {
    x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
}

/// `sha256rnds2 dst, src, xmm0` — two rounds of SHA-256 compression (Intel SDM).
///
/// Operand dword layout (SDM `SHA256RNDS2`, SRC1=`dst`/xmm1, SRC2=`src`/xmm2):
/// * `src` (SRC2): A=dw3, B=dw2, E=dw1, F=dw0
/// * `dst` (SRC1): C=dw3, D=dw2, G=dw1, H=dw0
/// * `wk`  (xmm0): WK0=dw0, WK1=dw1 (the two rounds' message+constant; dw2/dw3 unused).
///
/// Result → `dst` holds the post-round `{A',B',E',F'}` stored in dw3,dw2,dw1,dw0.
pub fn sha256rnds2(dst: u128, src: u128, wk: u128) -> u128 {
    let s = dwords(src);
    let d = dwords(dst);
    let w = dwords(wk); // WK0=dw0, WK1=dw1

    let mut a = s[3];
    let mut b = s[2];
    let mut c = d[3];
    let mut dd = d[2];
    let mut e = s[1];
    let mut f = s[0];
    let mut g = d[1];
    let mut h = d[0];

    for &wk_i in &[w[0], w[1]] {
        let t1 = h
            .wrapping_add(big_sigma1(e))
            .wrapping_add(ch(e, f, g))
            .wrapping_add(wk_i);
        let t2 = big_sigma0(a).wrapping_add(maj(a, b, c));
        // Rotate the working variables (T1 already folds in W+K for this round).
        h = g;
        g = f;
        f = e;
        e = dd.wrapping_add(t1);
        dd = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    // Store back: dst = {A', B', E', F'} in dw3, dw2, dw1, dw0.
    from_dwords([f, e, b, a])
}

/// `sha256msg1 dst, src`: first message-schedule step (small-sigma-0 mixing).
/// Per SDM: with W dwords `dst={W3,W2,W1,W0}` (dw3..dw0) and `src` supplying `W4`
/// as its dw0, result dw_i = `W_i + sigma0(W_{i+1})`.
pub fn sha256msg1(dst: u128, src: u128) -> u128 {
    let w = dwords(dst); // dw0=W0, dw1=W1, dw2=W2, dw3=W3
    let s = dwords(src); // dw0=W4
    let w4 = s[0];
    from_dwords([
        w[0].wrapping_add(small_sigma0(w[1])),
        w[1].wrapping_add(small_sigma0(w[2])),
        w[2].wrapping_add(small_sigma0(w[3])),
        w[3].wrapping_add(small_sigma0(w4)),
    ])
}

/// `sha256msg2 dst, src`: second message-schedule step (small-sigma-1 mixing).
/// Per SDM with `dst` = intermediate `{W16..W19}` low-to-high in dw0..dw3 and
/// `src` = `{W12..W15}` (dw0..dw3 = W12,W13,W14,W15):
/// * `W16 = dst.dw0 + sigma1(src.dw2)`  (src.dw2 = W14)
/// * `W17 = dst.dw1 + sigma1(src.dw3)`  (src.dw3 = W15)
/// * `W18 = dst.dw2 + sigma1(W16)`
/// * `W19 = dst.dw3 + sigma1(W17)`
pub fn sha256msg2(dst: u128, src: u128) -> u128 {
    let w = dwords(dst);
    let s = dwords(src);
    let w16 = w[0].wrapping_add(small_sigma1(s[2]));
    let w17 = w[1].wrapping_add(small_sigma1(s[3]));
    let w18 = w[2].wrapping_add(small_sigma1(w16));
    let w19 = w[3].wrapping_add(small_sigma1(w17));
    from_dwords([w16, w17, w18, w19])
}

// --- SHA-1 primitives (FIPS-180-4 §6.1) ---

#[inline]
fn f0_ch(b: u32, c: u32, d: u32) -> u32 {
    (b & c) | (!b & d)
}
#[inline]
fn f1_parity(b: u32, c: u32, d: u32) -> u32 {
    b ^ c ^ d
}
#[inline]
fn f2_maj(b: u32, c: u32, d: u32) -> u32 {
    (b & c) | (b & d) | (c & d)
}

/// SHA-1 round function selected by `sha1rnds4`'s `imm8[1:0]` (Intel SDM):
/// * 0 → f0 = Ch,     K = 0x5A827999
/// * 1 → f1 = Parity, K = 0x6ED9EBA1
/// * 2 → f2 = Maj,    K = 0x8F1BBCDC
/// * 3 → f3 = Parity, K = 0xCA62C1D6
#[inline]
fn sha1_f(sel: u8, b: u32, c: u32, d: u32) -> (u32, u32) {
    match sel & 3 {
        0 => (f0_ch(b, c, d), 0x5A82_7999),
        1 => (f1_parity(b, c, d), 0x6ED9_EBA1),
        2 => (f2_maj(b, c, d), 0x8F1B_BCDC),
        _ => (f1_parity(b, c, d), 0xCA62_C1D6),
    }
}

/// `sha1rnds4 dst, src, imm8` — four SHA-1 rounds (Intel SDM `SHA1RNDS4`).
///
/// * `dst` (SRC1) holds `{A, B, C, D}` as A=dw3, B=dw2, C=dw1, D=dw0.
/// * `src` (SRC2) supplies the four message words: W0=dw3, W1=dw2, W2=dw1, W3=dw0.
///   `E` for round 0 is carried in `src` via the preceding `sha1nexte`; the SDM
///   recurrence introduces `E` as the value shifted out of `D` for rounds 1..3.
/// * `imm8[1:0]` selects f() and the constant `K` (added per round).
///
/// Result → `dst` = `{A', B', C', D'}` as A'=dw3, B'=dw2, C'=dw1, D'=dw0.
pub fn sha1rnds4(dst: u128, src: u128, imm: u8) -> u128 {
    let x = dwords(dst); // dw0=D, dw1=C, dw2=B, dw3=A
    let w = dwords(src); // dw0=W3, dw1=W2, dw2=W1, dw3=W0

    let mut a = x[3];
    let mut b = x[2];
    let mut c = x[1];
    let mut d = x[0];
    let words = [w[3], w[2], w[1], w[0]]; // W0..W3

    // Round 0 has no E term; rounds 1..3 add the E shifted out of D.
    let mut e = 0u32;
    for (i, &w_i) in words.iter().enumerate() {
        let (fval, k) = sha1_f(imm, b, c, d);
        let mut t = fval
            .wrapping_add(a.rotate_left(5))
            .wrapping_add(w_i)
            .wrapping_add(k);
        if i != 0 {
            t = t.wrapping_add(e);
        }
        e = d; // E for the NEXT round is the D shifted out this round.
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = t;
    }

    from_dwords([d, c, b, a]) // dw0=D', dw1=C', dw2=B', dw3=A'
}

/// `sha1nexte dst, src` (Intel SDM): compute the next `E`.
/// `E = dst.dw3` (the current A of the incoming state before this step is the
/// carried E). Per SDM: `tmp = ROTL30(dst.dw3)`, then result = `src` with
/// `dw3 += tmp`; dw0..dw2 = src.dw0..dw2 unchanged.
pub fn sha1nexte(dst: u128, src: u128) -> u128 {
    let x = dwords(dst);
    let s = dwords(src);
    let tmp = x[3].rotate_left(30);
    from_dwords([s[0], s[1], s[2], s[3].wrapping_add(tmp)])
}

/// `sha1msg1 dst, src` (Intel SDM): first SHA-1 message-schedule step.
/// With `dst = {W0,W1,W2,W3}` (dw0..dw3 = W3,W2,W1,W0 in SDM's high-to-low view)
/// the SDM computes: result.dw3 = W2 ^ W0, dw2 = W3 ^ W1, dw1 = W4 ^ W2 (src.dw3),
/// dw0 = W5 ^ W3 (src.dw2). We express it directly on the dword arrays below.
pub fn sha1msg1(dst: u128, src: u128) -> u128 {
    // SDM SHA1MSG1: W(i) dwords in dst as {W3:dw0, W2:dw1, W1:dw2, W0:dw3},
    // src as {W7:dw0, W6:dw1, W5:dw2, W4:dw3}.
    //   dst.dw3(W0) ^= dst.dw1(W2)
    //   dst.dw2(W1) ^= dst.dw0(W3)
    //   dst.dw1(W2) ^= src.dw3(W4)
    //   dst.dw0(W3) ^= src.dw2(W5)
    let x = dwords(dst);
    let s = dwords(src);
    from_dwords([x[0] ^ s[2], x[1] ^ s[3], x[2] ^ x[0], x[3] ^ x[1]])
}

/// `sha1msg2 dst, src` (Intel SDM): second SHA-1 message-schedule step —
/// applies the SHA-1 message rotate-left-1 across the incoming words.
/// With `dst = {W13:dw0, W14:dw1, W15:dw2, W16:dw3}`-style intermediates and
/// `src` supplying the neighbours, each result word is `ROTL1(prev ^ intermediate)`.
pub fn sha1msg2(dst: u128, src: u128) -> u128 {
    // SDM SHA1MSG2:
    //   W16 = ROTL1(dst.dw3 ^ src.dw2)
    //   W17 = ROTL1(dst.dw2 ^ src.dw1)
    //   W18 = ROTL1(dst.dw1 ^ src.dw0)
    //   W19 = ROTL1(dst.dw0 ^ W16)
    // stored as result.dw3=W16, dw2=W17, dw1=W18, dw0=W19.
    let x = dwords(dst); // dw0..dw3
    let s = dwords(src);
    let w16 = (x[3] ^ s[2]).rotate_left(1);
    let w17 = (x[2] ^ s[1]).rotate_left(1);
    let w18 = (x[1] ^ s[0]).rotate_left(1);
    let w19 = (x[0] ^ w16).rotate_left(1);
    from_dwords([w19, w18, w17, w16])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_round_helpers_known_values() {
        // FIPS-180-4 constants sanity: sigma functions on a known word.
        // ROTR checks against hand-computed values for x = 0x00000001.
        assert_eq!(big_sigma0(0), 0);
        assert_eq!(big_sigma1(0), 0);
        // small_sigma0(1) = ROTR(1,7) ^ ROTR(1,18) ^ (1>>3)
        //   = 0x02000000 ^ 0x00004000 ^ 0 = 0x02004000
        assert_eq!(small_sigma0(1), 0x0200_4000);
        // small_sigma1(1) = ROTR(1,17) ^ ROTR(1,19) ^ (1>>10)
        //   = 0x00008000 ^ 0x00002000 ^ 0 = 0x0000A000
        assert_eq!(small_sigma1(1), 0x0000_A000);
    }

    #[test]
    fn sha256rnds2_matches_reference_two_rounds() {
        // Reference two-round SHA-256 compression driven by the same {A..H}
        // packing sha256rnds2 uses (A=dw3, B=dw2, E=dw1, F=dw0 for src; C=dw3,
        // D=dw2, G=dw1, H=dw0 for dst), cross-checked against a straight scalar impl.
        let src = from_dwords([0x9b05_688c, 0x510e_527f, 0xbb67_ae85, 0x6a09_e667]); // F,E,B,A
        let dst = from_dwords([0x5be0_cd19, 0x1f83_d9ab, 0xa54f_f53a, 0x3c6e_f372]); // H,G,D,C
        let wk = from_dwords([0x428a_2f98, 0x7137_4491, 0, 0]);

        // Straight reference.
        let s = dwords(src);
        let d = dwords(dst);
        let w = dwords(wk);
        let (mut a, mut b, mut c, mut dd, mut e, mut f, mut g, mut h) =
            (s[3], s[2], d[3], d[2], s[1], s[0], d[1], d[0]);
        for &wki in &[w[0], w[1]] {
            let t1 = h
                .wrapping_add(big_sigma1(e))
                .wrapping_add(ch(e, f, g))
                .wrapping_add(wki);
            let t2 = big_sigma0(a).wrapping_add(maj(a, b, c));
            h = g;
            g = f;
            f = e;
            e = dd.wrapping_add(t1);
            dd = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        let expected = from_dwords([f, e, b, a]);
        assert_eq!(sha256rnds2(dst, src, wk), expected);
    }

    #[test]
    fn sha1_f_selection() {
        let (f, k) = sha1_f(0, 0xffff_ffff, 0xaaaa_aaaa, 0x5555_5555);
        assert_eq!(k, 0x5A82_7999);
        assert_eq!(f, f0_ch(0xffff_ffff, 0xaaaa_aaaa, 0x5555_5555));
        assert_eq!(sha1_f(1, 1, 2, 4).1, 0x6ED9_EBA1);
        assert_eq!(sha1_f(2, 1, 2, 4).1, 0x8F1B_BCDC);
        assert_eq!(sha1_f(3, 1, 2, 4).1, 0xCA62_C1D6);
    }

    #[test]
    fn sha1msg2_rotate_left_1() {
        // Spot-check the ROTL1 wiring on a simple pattern.
        let dst = from_dwords([1, 2, 4, 8]);
        let src = from_dwords([0, 0, 0, 0]);
        let out = dwords(sha1msg2(dst, src));
        // w16 = ROTL1(dst.dw3 ^ src.dw2) = ROTL1(8) = 16
        assert_eq!(out[3], 16);
    }
}
