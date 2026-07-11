//! Carry-less multiply primitive for `pclmulqdq` / `vpclmulqdq` (task-211), shared by
//! the interpreter and the JIT helper so `jit == interp`, validated bit-exact against the
//! real CPU (host has PCLMULQDQ). GHASH/GCM and CRC use this; it is the polynomial
//! multiply in GF(2)[x] with no reduction (the 64×64 product is a full 128-bit value).
//!
//! Operates on the 128-bit xmm bit pattern as `u128`. No `CpuState` dependency.

/// `pclmulqdq a, b, imm8`: carry-less product of two `imm8`-selected 64-bit halves.
///
/// * `imm8[0]` selects the source dword-pair from `a` (`0` → `a[63:0]`, `1` → `a[127:64]`).
/// * `imm8[4]` selects the source dword-pair from `b` (`0` → `b[63:0]`, `1` → `b[127:64]`).
///
/// The result is the 128-bit GF(2)[x] product: `∑ (x << i)` over set bits `i` of `y`,
/// XOR-accumulated (carry-less). All other `imm8` bits are reserved and ignored, matching
/// the Intel SDM.
pub fn pclmul(a: u128, b: u128, imm: u8) -> u128 {
    let x = if imm & 0x01 != 0 {
        (a >> 64) as u64
    } else {
        a as u64
    };
    let y = if imm & 0x10 != 0 {
        (b >> 64) as u64
    } else {
        b as u64
    };
    let mut result = 0u128;
    for i in 0..64 {
        if (y >> i) & 1 != 0 {
            result ^= (x as u128) << i;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_and_identity() {
        assert_eq!(pclmul(0, 0xffff_ffff_ffff_ffff, 0), 0);
        // multiply by 1 (x^0) is identity.
        assert_eq!(pclmul(0xdead_beef_cafe_babe, 1, 0), 0xdead_beef_cafe_babe);
    }

    #[test]
    fn half_selection() {
        let a = 0x0000_0000_0000_0002_0000_0000_0000_0003u128; // hi=2, lo=3
        let b = 0x0000_0000_0000_0005_0000_0000_0000_0007u128; // hi=5, lo=7
        assert_eq!(pclmul(a, b, 0x00), pclmul(3u128, 7u128, 0)); // lo·lo = 3·7
        assert_eq!(pclmul(a, b, 0x01), pclmul(2u128, 7u128, 0)); // hi·lo = 2·7
        assert_eq!(pclmul(a, b, 0x10), pclmul(3u128, 5u128, 0)); // lo·hi = 3·5
        assert_eq!(pclmul(a, b, 0x11), pclmul(2u128, 5u128, 0)); // hi·hi = 2·5
    }

    #[test]
    fn carryless_no_carry() {
        // 3·3 = (x+1)(x+1) = x^2 + 1 in GF(2)[x] (the x^1 terms cancel), i.e. 0b101 = 5.
        assert_eq!(pclmul(3, 3, 0), 5);
        // x^63 · x^1 = x^64 → bit 64 set, no overflow past 128.
        assert_eq!(pclmul(1u128 << 63, 2, 0), 1u128 << 64);
    }

    #[test]
    fn commutative() {
        let a = 0x0123_4567_89ab_cdefu128;
        let b = 0xfedc_ba98_7654_3210u128;
        assert_eq!(pclmul(a, b, 0), pclmul(b, a, 0));
    }
}
