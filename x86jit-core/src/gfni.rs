//! Shared, pure-Rust GFNI (Galois Field New Instructions) primitives used by both
//! the interpreter and the JIT helper so `jit == interp`, validated bit-exact against
//! the real CPU (host has GFNI; task-210).
//!
//! Everything operates on the 128-bit xmm bit pattern as a little-endian value:
//! byte `i` occupies bits `[8*i + 7 : 8*i]`. `gf2p8mulb` is a per-byte GF(2^8)
//! multiply mod the AES polynomial 0x11B (reusing `aes::gmul`). The affine ops apply
//! an 8x8 GF(2) bit-matrix (one qword of the second operand per byte lane) followed
//! by an XOR with `imm8`, per the Intel SDM `affine_byte` pseudocode.

/// GF(2^8) multiplicative-inverse LUT (mod 0x11B), `inv(0) = 0` per GFNI/SDM.
/// Built once at first use by the same brute-force search that used to run per byte.
/// GFNI now drives the inverse over a full ZMM (64 bytes) in openssl's vectorized-AES
/// hot loop (task-215/220), so a 256-entry table turns each inverse into one array index
/// instead of an O(255) `gmul` search. The field inverse is unique, so the table is
/// bit-identical to the old per-call search.
fn gf_inv_lut() -> &'static [u8; 256] {
    static LUT: std::sync::OnceLock<[u8; 256]> = std::sync::OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0u8; 256];
        for (a, slot) in t.iter_mut().enumerate().skip(1) {
            for b in 1u8..=255 {
                if crate::aes::gmul(a as u8, b) == 1 {
                    *slot = b;
                    break;
                }
            }
        }
        t
    })
}

/// GF(2^8) multiplicative inverse (mod 0x11B), with `inv(0) = 0` per GFNI/SDM.
#[inline]
fn gf_inv(a: u8) -> u8 {
    gf_inv_lut()[a as usize]
}

/// `gf2p8mulb dst, src`: per byte, GF(2^8) multiply mod 0x11B.
pub fn gf2p8mulb(a: u128, b: u128) -> u128 {
    let ab = a.to_le_bytes();
    let bb = b.to_le_bytes();
    let mut o = [0u8; 16];
    for i in 0..16 {
        o[i] = crate::aes::gmul(ab[i], bb[i]);
    }
    u128::from_le_bytes(o)
}

/// The affine transform on a single byte per the Intel SDM `affine_byte`:
///
/// `FOR i := 0 to 7: retbyte.bit[i] := parity(qw.byte[7-i] AND x) XOR imm8.bit[i]`
///
/// where `qw` is the 8 matrix rows (as 8 bytes), `x` the input byte, `imm8` the constant.
#[inline]
fn affine_byte(qw: [u8; 8], x: u8, imm: u8) -> u8 {
    let mut ret = 0u8;
    for i in 0..8 {
        let row = qw[7 - i];
        let parity = (row & x).count_ones() & 1;
        ret |= ((parity as u8) ^ ((imm >> i) & 1)) << i;
    }
    ret
}

/// Split a 128-bit value into two qwords (each an 8-byte matrix for a byte-lane group).
#[inline]
fn qwords(v: u128) -> [[u8; 8]; 2] {
    let b = v.to_le_bytes();
    let mut q = [[0u8; 8]; 2];
    q[0].copy_from_slice(&b[0..8]);
    q[1].copy_from_slice(&b[8..16]);
    q
}

/// `gf2p8affineqb dst, src2, imm8`: for each of the 16 bytes of `x` (=op1/dst), the
/// corresponding qword of `mat` (=src2, one qword per group of 8 byte-lanes) is the
/// 8x8 bit-matrix `A`; `dst[byte] = affine(A, x[byte]) XOR imm8`.
pub fn gf2p8affineqb(x: u128, mat: u128, imm: u8) -> u128 {
    let xb = x.to_le_bytes();
    let q = qwords(mat);
    let mut o = [0u8; 16];
    for i in 0..16 {
        o[i] = affine_byte(q[i / 8], xb[i], imm);
    }
    u128::from_le_bytes(o)
}

/// `gf2p8affineinvqb dst, src2, imm8`: as `gf2p8affineqb` but the input byte is first
/// mapped through the GF(2^8) multiplicative inverse (mod 0x11B) before the affine step.
pub fn gf2p8affineinvqb(x: u128, mat: u128, imm: u8) -> u128 {
    let xb = x.to_le_bytes();
    let q = qwords(mat);
    let mut o = [0u8; 16];
    for i in 0..16 {
        o[i] = affine_byte(q[i / 8], gf_inv(xb[i]), imm);
    }
    u128::from_le_bytes(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mulb_known() {
        // GF(2^8) mod 0x11B: 0x53 * 0xCA == 0x01 (classic AES example).
        let a = 0x53u128;
        let b = 0xCAu128;
        assert_eq!(gf2p8mulb(a, b) & 0xff, 0x01);
    }

    #[test]
    fn inv_is_involutive_pair() {
        // gf_inv(gf_inv(a)) == a for all a.
        for a in 0u8..=255 {
            assert_eq!(gf_inv(gf_inv(a)), a);
        }
    }

    #[test]
    fn affine_identity_matrix() {
        // The identity matrix (rows 0x80,0x40,0x20,...,0x01 read MSB-first) with imm=0
        // must map each byte to itself. Row j selects bit (7-j) of x via the SDM ordering:
        // byte[7-i] is row i, and parity(row & x) picks that single bit.
        // Identity: byte[k] = 1<<(7-k) so that qw.byte[7-i] = 1<<i selects x.bit[i].
        let mut rows = [0u8; 8];
        for (k, r) in rows.iter_mut().enumerate() {
            *r = 1 << (7 - k);
        }
        let mat = u128::from_le_bytes([
            rows[0], rows[1], rows[2], rows[3], rows[4], rows[5], rows[6], rows[7], rows[0],
            rows[1], rows[2], rows[3], rows[4], rows[5], rows[6], rows[7],
        ]);
        let x = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210u128;
        assert_eq!(gf2p8affineqb(x, mat, 0), x);
        // With imm=0xff, each byte is complemented.
        assert_eq!(
            gf2p8affineqb(x, mat, 0xff),
            x ^ u128::from_le_bytes([0xff; 16])
        );
    }
}
