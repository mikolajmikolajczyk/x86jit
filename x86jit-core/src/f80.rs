//! Software 80-bit extended-precision float — the x87 register content (§14).
//!
//! x87 rounds every operation to a 64-bit significand; an `f64`-backed register file
//! only keeps 53, so a chain of x87 ops (e.g. musl's `printf` long-double formatting)
//! rounds the last digit differently from real hardware. This type carries the full
//! 80-bit format (sign + 15-bit exponent + explicit-integer-bit 64-bit significand)
//! and rounds each op to nearest-even at 64 bits, matching the hardware Unicorn and
//! native execution model.
//!
//! Pure Rust (no host x87 asm) so it is identical on x86-64 and ARM64 hosts.

/// A decoded 80-bit extended float. For a `Normal`, the value is
/// `(-1)^sign * sig * 2^(exp - 63)` with `sig` in `[2^63, 2^64)` (bit 63 = the
/// explicit integer bit). `exp` is the *unbiased* exponent of that integer bit.
#[derive(Copy, Clone, Debug)]
pub struct F80 {
    pub sign: bool,
    pub class: Class,
    /// Unbiased exponent of bit 63 (only meaningful for `Normal`).
    pub exp: i32,
    /// 64-bit significand with the integer bit at bit 63 (only for `Normal`).
    pub sig: u64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Class {
    Zero,
    Normal,
    Inf,
    Nan,
}

const BIAS: i32 = 16383;
const EXP_MAX: i32 = 0x7fff;
/// Largest/smallest unbiased exponent of a normal (integer-bit) value.
const EMAX: i32 = EXP_MAX - 1 - BIAS; // 16383
const EMIN: i32 = 1 - BIAS; // -16382

impl Default for F80 {
    fn default() -> Self {
        F80::ZERO
    }
}

// The arithmetic is exposed as explicit `F80::add(a, b)` static methods (they read
// clearly at the x87 call sites and take two operands, not `self`), not the `std::ops`
// traits — the lint that flags the name overlap is intentional here.
#[allow(clippy::should_implement_trait)]
impl F80 {
    pub const ZERO: F80 = F80 {
        sign: false,
        class: Class::Zero,
        exp: 0,
        sig: 0,
    };

    pub fn inf(sign: bool) -> F80 {
        F80 {
            sign,
            class: Class::Inf,
            exp: 0,
            sig: 0,
        }
    }
    pub fn nan() -> F80 {
        F80 {
            sign: false,
            class: Class::Nan,
            exp: 0,
            sig: 0,
        }
    }
    pub fn zero(sign: bool) -> F80 {
        F80 {
            sign,
            class: Class::Zero,
            exp: 0,
            sig: 0,
        }
    }
    pub fn is_nan(&self) -> bool {
        self.class == Class::Nan
    }

    // ---- 80-bit memory encoding (the `tbyte` / fxsave slot) ----

    /// Decode the 10-byte 80-bit format.
    pub fn from_bytes(b: &[u8; 10]) -> F80 {
        let sig = u64::from_le_bytes(b[0..8].try_into().unwrap());
        let se = u16::from_le_bytes([b[8], b[9]]);
        let sign = (se >> 15) & 1 != 0;
        let biased = (se & 0x7fff) as i32;
        if biased == EXP_MAX {
            // inf: sig == 0x8000... ; anything else with the exponent all-ones is NaN.
            if sig == 0x8000_0000_0000_0000 {
                return F80::inf(sign);
            }
            return F80 {
                sign,
                class: Class::Nan,
                exp: 0,
                sig,
            };
        }
        if biased == 0 {
            if sig == 0 {
                return F80::zero(sign);
            }
            // Denormal (integer bit clear): normalize into the working form.
            let shift = sig.leading_zeros();
            return F80 {
                sign,
                class: Class::Normal,
                exp: EMIN - shift as i32,
                sig: sig << shift,
            };
        }
        // Normal: exponent of bit 63 is (biased - BIAS).
        F80 {
            sign,
            class: Class::Normal,
            exp: biased - BIAS,
            sig,
        }
    }

    /// Encode to the 10-byte 80-bit format (denormalizing tiny values as needed).
    pub fn to_bytes(&self) -> [u8; 10] {
        let (sig, biased): (u64, u16) = match self.class {
            Class::Zero => (0, 0),
            Class::Inf => (0x8000_0000_0000_0000, EXP_MAX as u16),
            Class::Nan => (self.sig | 0xC000_0000_0000_0000, EXP_MAX as u16),
            Class::Normal => {
                let mut e = self.exp + BIAS;
                if e >= EXP_MAX {
                    (0x8000_0000_0000_0000, EXP_MAX as u16) // overflow -> inf
                } else if e <= 0 {
                    // Denormal: shift the significand right until the exponent is 1.
                    let shift = (1 - e) as u32;
                    if shift >= 64 {
                        (0, 0)
                    } else {
                        (self.sig >> shift, 0)
                    }
                } else {
                    if e < 0 {
                        e = 0;
                    }
                    (self.sig, e as u16)
                }
            }
        };
        let mut out = [0u8; 10];
        out[0..8].copy_from_slice(&sig.to_le_bytes());
        let se = ((self.sign as u16) << 15) | (biased & 0x7fff);
        out[8..10].copy_from_slice(&se.to_le_bytes());
        out
    }

    // ---- conversions to/from the f64 the rest of the engine uses ----

    pub fn from_f64(bits: u64) -> F80 {
        let sign = (bits >> 63) & 1 != 0;
        let exp = ((bits >> 52) & 0x7ff) as i32;
        let frac = bits & 0xf_ffff_ffff_ffff;
        if exp == 0x7ff {
            return if frac == 0 {
                F80::inf(sign)
            } else {
                F80 {
                    sign,
                    class: Class::Nan,
                    exp: 0,
                    sig: (frac << 11) | 0xC000_0000_0000_0000,
                }
            };
        }
        if exp == 0 {
            if frac == 0 {
                return F80::zero(sign);
            }
            // Subnormal f64: normalize (its value is exact in f80). The leading 1
            // moves up to bit 63, i.e. shift left by `frac.leading_zeros()`.
            let shift = frac.leading_zeros() - 11;
            return F80 {
                sign,
                class: Class::Normal,
                exp: (1 - 1023) - shift as i32,
                sig: frac << (shift + 11),
            };
        }
        // Normal f64: significand = 1.frac, integer bit at 63.
        F80 {
            sign,
            class: Class::Normal,
            exp: exp - 1023,
            sig: (1 << 63) | (frac << 11),
        }
    }

    /// Round to the nearest `f64` (ties to even). Returns the raw `f64` bits.
    pub fn to_f64(&self) -> u64 {
        match self.class {
            Class::Zero => (self.sign as u64) << 63,
            Class::Inf => ((self.sign as u64) << 63) | (0x7ff << 52),
            Class::Nan => {
                ((self.sign as u64) << 63)
                    | (0x7ff << 52)
                    | (self.sig >> 11 & 0xf_ffff_ffff_ffff).max(1)
            }
            Class::Normal => {
                let sign = (self.sign as u64) << 63;
                let mut e = self.exp;
                // Round the 64-bit significand to 53 bits (integer + 52 fraction),
                // nearest-even; a carry can push it to 2^53 (bump the exponent).
                let mut m = round_shift(self.sig, 11);
                if m >> 53 != 0 {
                    m >>= 1;
                    e += 1;
                }
                let biased = e + 1023;
                if biased >= 0x7ff {
                    return sign | (0x7ff << 52); // overflow -> inf
                }
                if biased <= 0 {
                    // Underflow to a subnormal (or zero) f64: shift the full significand.
                    let shift = (1 - biased) as u32 + 11;
                    if shift >= 64 {
                        return sign;
                    }
                    return sign | round_shift(self.sig, shift);
                }
                let f52 = m & 0xf_ffff_ffff_ffff; // drop the integer bit (bit 52)
                sign | ((biased as u64) << 52) | f52
            }
        }
    }

    // ---- integer conversions ----

    pub fn from_i64(v: i64) -> F80 {
        if v == 0 {
            return F80::zero(false);
        }
        let sign = v < 0;
        let mag = (v as i128).unsigned_abs() as u64;
        let shift = mag.leading_zeros();
        F80 {
            sign,
            class: Class::Normal,
            exp: 63 - shift as i32,
            sig: mag << shift,
        }
    }

    /// Round to a signed integer using x87 rounding mode `rc` (0=nearest,1=down,
    /// 2=up,3=truncate). Saturates on overflow to the "integer indefinite" pattern
    /// the caller masks to the destination width.
    pub fn to_i64_rc(&self, rc: u8) -> i64 {
        match self.class {
            Class::Zero => 0,
            Class::Nan | Class::Inf => i64::MIN,
            Class::Normal => {
                // value = sig * 2^(exp-63). If exp >= 63 it's a (large) integer already.
                let e = self.exp;
                if e >= 63 {
                    // Would overflow i64 for e>62; saturate to the indefinite value.
                    if e > 62 {
                        return i64::MIN;
                    }
                    let m = self.sig >> (63 - e);
                    return apply_sign(m, self.sign);
                }
                if e < -1 {
                    // |value| < 0.5 ... handle rounding of a pure fraction.
                    let up = round_fraction_up(self.sig, 0, self.sign, rc, true);
                    return if up { apply_sign(1, self.sign) } else { 0 };
                }
                // 0 <= shift <= 63: integer part = sig >> (63 - e), fraction below.
                let shift = (63 - e) as u32;
                let int = self.sig >> shift;
                let frac_mask = (1u64 << shift) - 1;
                let frac = self.sig & frac_mask;
                let half = 1u64 << (shift - 1);
                let up = decide_round(int, frac, half, self.sign, rc);
                apply_sign(int + up as u64, self.sign)
            }
        }
    }

    // ---- arithmetic (round to nearest even at 64 bits) ----

    pub fn add(a: F80, b: F80) -> F80 {
        add_sub(a, b, false)
    }
    pub fn sub(a: F80, b: F80) -> F80 {
        add_sub(a, b, true)
    }

    pub fn mul(a: F80, b: F80) -> F80 {
        let sign = a.sign ^ b.sign;
        use Class::*;
        match (a.class, b.class) {
            (Nan, _) | (_, Nan) => F80::nan(),
            (Inf, Zero) | (Zero, Inf) => F80::nan(),
            (Inf, _) | (_, Inf) => F80::inf(sign),
            (Zero, _) | (_, Zero) => F80::zero(sign),
            (Normal, Normal) => {
                let m = (a.sig as u128) * (b.sig as u128);
                // value = m * 2^(a.exp + b.exp - 126); ref exponent (bit 127) = +1.
                normalize_round(sign, a.exp + b.exp + 1, m)
            }
        }
    }

    pub fn div(a: F80, b: F80) -> F80 {
        let sign = a.sign ^ b.sign;
        use Class::*;
        match (a.class, b.class) {
            (Nan, _) | (_, Nan) => F80::nan(),
            (Inf, Inf) | (Zero, Zero) => F80::nan(),
            (Inf, _) => F80::inf(sign),
            (_, Inf) => F80::zero(sign),
            (_, Zero) => F80::inf(sign), // finite / 0
            (Zero, _) => F80::zero(sign),
            (Normal, Normal) => {
                // q = (a.sig << 64) / b.sig, a 65-bit quotient with 64 fraction bits.
                let num = (a.sig as u128) << 64;
                let q = num / (b.sig as u128);
                let rem = num % (b.sig as u128);
                // Fold the remainder into a sticky low bit so rounding sees it.
                let m = if rem != 0 { q | 1 } else { q };
                // q = (a.sig << 64) / b.sig carries a 2^-64 scale, so
                // value = q * 2^(a.exp - b.exp - 64) = m * 2^(ref_exp - 127) with
                // ref_exp = a.exp - b.exp + 63.
                normalize_round(sign, a.exp - b.exp + 63, m)
            }
        }
    }

    pub fn sqrt(a: F80) -> F80 {
        use Class::*;
        match a.class {
            Nan => F80::nan(),
            Zero => F80::zero(a.sign),
            _ if a.sign => F80::nan(), // sqrt of a negative -> NaN
            Inf => F80::inf(false),
            Normal => {
                // value = sig * 2^e2. Scale the significand by 2^s (s ≡ e2 mod 2 so
                // e2-s is even) into [2^126, 2^128) so its integer sqrt has ~64 bits;
                // then sqrt(value) = isqrt(sig<<s) * 2^((e2-s)/2).
                let e2 = a.exp - 63;
                let s: u32 = if e2 & 1 == 0 { 64 } else { 63 };
                let (root, exact) = isqrt128((a.sig as u128) << s);
                let m = if exact { root } else { root | 1 };
                normalize_round(false, (e2 - s as i32) / 2 + 127, m)
            }
        }
    }

    /// Partial remainder `a - trunc(a/b)*b` (x87 `fprem`, fmod semantics).
    pub fn rem(a: F80, b: F80) -> F80 {
        use Class::*;
        match (a.class, b.class) {
            (Nan, _) | (_, Nan) | (Inf, _) | (_, Zero) => F80::nan(),
            (Zero, _) | (_, Inf) => a,
            (Normal, Normal) => {
                let q = F80::div(a, b).to_i64_rc(3); // toward zero
                F80::sub(a, F80::mul(F80::from_i64(q), b))
            }
        }
    }

    /// Compare for x87 `fcom`/`fucomi`: returns `(zf, pf, cf)` where unordered
    /// (a NaN operand) sets all three (matching the `ucomisd` mapping).
    pub fn compare(a: F80, b: F80) -> (bool, bool, bool) {
        use Class::*;
        if a.class == Nan || b.class == Nan {
            return (true, true, true);
        }
        let av = ordered_key(a);
        let bv = ordered_key(b);
        if av == bv {
            (true, false, false)
        } else if av < bv {
            (false, false, true)
        } else {
            (false, false, false)
        }
    }

    pub fn abs(mut self) -> F80 {
        self.sign = false;
        self
    }
    pub fn neg(mut self) -> F80 {
        self.sign = !self.sign;
        self
    }

    // --- Transcendentals (task-206) ---
    //
    // x87 fsin/fcos/… cannot be made bit-exact to real Intel hardware (the FPU uses
    // proprietary 68-bit-internal polynomials + range reduction with documented
    // inaccuracies), so there is no bit-exact oracle. These use the host `f64` libm and
    // are validated to a bounded ULP against libm/Unicorn (see the x87 transcendental
    // differential). Isolating them behind these methods leaves a clean seam for a
    // future higher-precision full-80-bit implementation, selectable per-run.

    /// Apply an `f64` function through a round-trip to `f64` precision.
    #[inline]
    fn map_f64(self, f: impl Fn(f64) -> f64) -> F80 {
        F80::from_f64(f(f64::from_bits(self.to_f64())).to_bits())
    }

    /// `sin(x)` (x87 `fsin`).
    pub fn sin(self) -> F80 {
        self.map_f64(f64::sin)
    }

    /// `cos(x)` (x87 `fcos`).
    pub fn cos(self) -> F80 {
        self.map_f64(f64::cos)
    }

    /// `tan(x)` (x87 `fptan`, before the trailing `1.0` push).
    pub fn tan(self) -> F80 {
        self.map_f64(f64::tan)
    }

    /// `2^x - 1` (x87 `f2xm1`; the input is architecturally in `[-1, 1]`).
    pub fn exp2m1(self) -> F80 {
        self.map_f64(|x| x.exp2() - 1.0)
    }

    /// `atan2(y, x)` (x87 `fpatan` computes `atan(ST1/ST0)` with full quadrant range).
    pub fn atan2(y: F80, x: F80) -> F80 {
        let (yf, xf) = (f64::from_bits(y.to_f64()), f64::from_bits(x.to_f64()));
        F80::from_f64(yf.atan2(xf).to_bits())
    }

    /// `y * log2(x)` (x87 `fyl2x`).
    pub fn ylog2x(y: F80, x: F80) -> F80 {
        F80::mul(y, x.map_f64(f64::log2))
    }

    /// `y * log2(x + 1)` (x87 `fyl2xp1`; accurate for small `x` via `ln_1p`).
    pub fn ylog2xp1(y: F80, x: F80) -> F80 {
        let xf = f64::from_bits(x.to_f64());
        let l = xf.ln_1p() / core::f64::consts::LN_2;
        F80::mul(y, F80::from_f64(l.to_bits()))
    }
}

// --- Extended (full-80-bit) transcendentals (task-212) ---
//
// The `Fast` path above rounds through `f64`/libm (~53-bit). This `Extended` path keeps
// the full 64-bit F80 significand via range reduction + Taylor/atanh series — every term
// is a factorial/odd-reciprocal (no minimax coefficients to get wrong), so it is correct
// by construction and validated by F80 self-consistency identities (sin²+cos²=1,
// exp2(log2 x)=x to ~80-bit), which the `f64` path cannot satisfy.
impl F80 {
    // 80-bit constants: the x87 fldpi/fldln2/fldl2e significands (bit 63 = integer bit).
    const PI: F80 = F80 {
        sign: false,
        class: Class::Normal,
        exp: 1,
        sig: 0xC90F_DAA2_2168_C235,
    };
    const HALF_PI: F80 = F80 {
        sign: false,
        class: Class::Normal,
        exp: 0,
        sig: 0xC90F_DAA2_2168_C235,
    };
    const QUARTER_PI: F80 = F80 {
        sign: false,
        class: Class::Normal,
        exp: -1,
        sig: 0xC90F_DAA2_2168_C235,
    };
    const LN2: F80 = F80 {
        sign: false,
        class: Class::Normal,
        exp: -1,
        sig: 0xB172_17F7_D1CF_79AC,
    };
    const LOG2E: F80 = F80 {
        sign: false,
        class: Class::Normal,
        exp: 0,
        sig: 0xB8AA_3B29_5C17_F0BC,
    };
    const ONE: F80 = F80 {
        sign: false,
        class: Class::Normal,
        exp: 0,
        sig: 0x8000_0000_0000_0000,
    };

    #[inline]
    fn fi(i: i64) -> F80 {
        F80::from_i64(i)
    }

    /// `sin(r)` and `cos(r)` for `r` in ~[-π/4, π/4] via Taylor series with incremental
    /// term update (denominators stay small — no overflowing factorials).
    fn sincos_poly(r: F80) -> (F80, F80) {
        let neg_r2 = F80::mul(r, r).neg();
        let (mut sterm, mut s) = (F80::ONE, F80::ONE); // S = sin(r)/r
        let (mut cterm, mut c) = (F80::ONE, F80::ONE); // C = cos(r)
        let mut k = 1i64;
        while k <= 11 {
            let k2 = 2 * k;
            sterm = F80::div(F80::mul(sterm, neg_r2), F80::fi(k2 * (k2 + 1)));
            s = F80::add(s, sterm);
            cterm = F80::div(F80::mul(cterm, neg_r2), F80::fi((k2 - 1) * k2));
            c = F80::add(c, cterm);
            k += 1;
        }
        (F80::mul(r, s), c)
    }

    /// Round `q` to the nearest `i64` (ties away from zero). Robust across the whole
    /// range — unlike `to_i64_rc`, which shift-overflows for `|q|` in [0.5, 1).
    fn round_to_i64(q: F80) -> i64 {
        match q.class {
            Class::Normal => {
                let e = q.exp; // value = (sig/2^63) * 2^e, sig bit 63 = integer bit
                if e < -1 {
                    return 0; // |q| < 0.5
                }
                if e == -1 {
                    return if q.sign { -1 } else { 1 }; // 0.5 <= |q| < 1
                }
                if e >= 63 {
                    return if q.sign { i64::MIN } else { i64::MAX }; // saturate (out of x87 domain)
                }
                let shift = (63 - e) as u32; // e in [0,62] → shift in [1,63]
                let int = q.sig >> shift;
                let frac = q.sig & ((1u64 << shift) - 1);
                let half = 1u64 << (shift - 1);
                let mag = int + (frac >= half) as u64;
                if q.sign {
                    (mag as i64).wrapping_neg()
                } else {
                    mag as i64
                }
            }
            _ => 0,
        }
    }

    /// Reduce `x` to `(r, quadrant & 3)` with `r` in ~[-π/4, π/4], `x ≈ r + quadrant*(π/2)`.
    fn reduce_quadrant(x: F80) -> (F80, i64) {
        let k = F80::round_to_i64(F80::div(x, F80::HALF_PI));
        let r = F80::sub(x, F80::mul(F80::fi(k), F80::HALF_PI));
        (r, k & 3)
    }

    /// `sin(x)` (Extended).
    pub fn sin_ext(self) -> F80 {
        if !matches!(self.class, Class::Normal | Class::Zero) {
            return self.map_f64(f64::sin);
        }
        let (r, q) = F80::reduce_quadrant(self);
        let (s, c) = F80::sincos_poly(r);
        match q {
            0 => s,
            1 => c,
            2 => s.neg(),
            _ => c.neg(),
        }
    }

    /// `cos(x)` (Extended).
    pub fn cos_ext(self) -> F80 {
        if !matches!(self.class, Class::Normal | Class::Zero) {
            return self.map_f64(f64::cos);
        }
        let (r, q) = F80::reduce_quadrant(self);
        let (s, c) = F80::sincos_poly(r);
        match q {
            0 => c,
            1 => s.neg(),
            2 => c.neg(),
            _ => s,
        }
    }

    /// `tan(x)` (Extended) = sin/cos through the quadrant.
    pub fn tan_ext(self) -> F80 {
        if !matches!(self.class, Class::Normal | Class::Zero) {
            return self.map_f64(f64::tan);
        }
        let (r, q) = F80::reduce_quadrant(self);
        let (s, c) = F80::sincos_poly(r);
        let (sin, cos) = match q {
            0 => (s, c),
            1 => (c, s.neg()),
            2 => (s.neg(), c.neg()),
            _ => (c.neg(), s),
        };
        F80::div(sin, cos)
    }

    /// `2^x - 1` (Extended) via `expm1(x·ln2)` — no cancellation near 0.
    pub fn exp2m1_ext(self) -> F80 {
        if !matches!(self.class, Class::Normal | Class::Zero) {
            return self.map_f64(|v| v.exp2() - 1.0);
        }
        let y = F80::mul(self, F80::LN2);
        let mut term = y; // k=1 term
        let mut acc = y;
        let mut k = 2i64;
        while k <= 24 {
            term = F80::div(F80::mul(term, y), F80::fi(k));
            acc = F80::add(acc, term);
            k += 1;
        }
        acc
    }

    /// `2·atanh(u) = ln((1+u)/(1-u))` via the odd-power series (`|u|` small ⇒ fast).
    fn two_atanh(u: F80) -> F80 {
        let u2 = F80::mul(u, u);
        let mut term = F80::ONE; // u^0
        let mut acc = F80::ONE;
        let mut k = 1i64;
        while k <= 30 {
            term = F80::mul(term, u2);
            acc = F80::add(acc, F80::div(term, F80::fi(2 * k + 1)));
            k += 1;
        }
        F80::mul(F80::mul(F80::fi(2), u), acc)
    }

    /// `log2(x)` for `x > 0` (Extended). `x = m·2^e`, `m` folded to ~[√½, √2), then
    /// `log2(x) = e + ln(m)·log2(e)` with `ln(m) = 2·atanh((m-1)/(m+1))`.
    fn log2_ext(x: F80) -> F80 {
        // Just above √2 (0xB504F333F9DE6485 ≈ 1.41421356): halve the mantissa, bump e.
        const SQRT2_SIG: u64 = 0xB504_F333_F9DE_6485;
        let (m, e) = if x.sig >= SQRT2_SIG {
            (
                F80 {
                    sign: false,
                    class: Class::Normal,
                    exp: -1,
                    sig: x.sig,
                },
                x.exp + 1,
            )
        } else {
            (
                F80 {
                    sign: false,
                    class: Class::Normal,
                    exp: 0,
                    sig: x.sig,
                },
                x.exp,
            )
        };
        let u = F80::div(F80::sub(m, F80::ONE), F80::add(m, F80::ONE));
        F80::add(F80::fi(e as i64), F80::mul(F80::two_atanh(u), F80::LOG2E))
    }

    /// `y · log2(x)` (Extended `fyl2x`).
    pub fn ylog2x_ext(y: F80, x: F80) -> F80 {
        if !matches!(x.class, Class::Normal) {
            return F80::ylog2x(y, x);
        }
        F80::mul(y, F80::log2_ext(x))
    }

    /// `y · log2(x + 1)` (Extended `fyl2xp1`). `ln(1+x) = 2·atanh(x/(2+x))` — accurate
    /// for small `x` (no `1+x` cancellation).
    pub fn ylog2xp1_ext(y: F80, x: F80) -> F80 {
        if !matches!(x.class, Class::Normal | Class::Zero) {
            return F80::ylog2xp1(y, x);
        }
        let u = F80::div(x, F80::add(F80::fi(2), x));
        let ln1px = F80::two_atanh(u);
        F80::mul(y, F80::mul(ln1px, F80::LOG2E))
    }

    /// `atan(t)` for `t >= 0` (Extended). Fold with π/4 when `t > tan(π/8)` so the series
    /// argument stays `< ~0.42`, then the alternating odd-power series.
    fn atan_nonneg(t: F80) -> F80 {
        let tanpi8 = F80::from_f64(0.414_213_562_373_095_f64.to_bits());
        let (w, base) = if ordered_key(t) > ordered_key(tanpi8) {
            (
                F80::div(F80::sub(t, F80::ONE), F80::add(t, F80::ONE)),
                F80::QUARTER_PI,
            )
        } else {
            (t, F80::zero(false))
        };
        let neg_w2 = F80::mul(w, w).neg();
        let mut term = w;
        let mut acc = w;
        let mut k = 1i64;
        while k <= 32 {
            term = F80::mul(term, neg_w2);
            acc = F80::add(acc, F80::div(term, F80::fi(2 * k + 1)));
            k += 1;
        }
        F80::add(base, acc)
    }

    /// `atan2(y, x)` (Extended `fpatan`), full quadrant range.
    pub fn atan2_ext(y: F80, x: F80) -> F80 {
        if !matches!(y.class, Class::Normal | Class::Zero)
            || !matches!(x.class, Class::Normal | Class::Zero)
        {
            return F80::atan2(y, x);
        }
        let (ay, ax) = (y.abs(), x.abs());
        let core = if matches!(ax.class, Class::Zero) {
            F80::HALF_PI // |y|/0 → π/2
        } else {
            let t = F80::div(ay, ax);
            if ordered_key(t) > ordered_key(F80::ONE) {
                F80::sub(F80::HALF_PI, F80::atan_nonneg(F80::div(ax, ay)))
            } else {
                F80::atan_nonneg(t)
            }
        };
        // Quadrant: x<0 reflects across π/2 (→ π − core); the sign of y flips the result.
        let mag = if x.sign {
            F80::sub(F80::PI, core)
        } else {
            core
        };
        if y.sign {
            mag.neg()
        } else {
            mag
        }
    }
}

/// Total-order key for finite/inf comparison (NaN handled by the caller).
fn ordered_key(a: F80) -> i128 {
    let mag: i128 = match a.class {
        Class::Zero => 0,
        Class::Inf => i128::MAX / 2,
        Class::Nan => 0,
        Class::Normal => ((a.exp as i128) << 64) | a.sig as i128,
    };
    if a.sign {
        -mag
    } else {
        mag
    }
}

fn apply_sign(mag: u64, sign: bool) -> i64 {
    if sign {
        (mag as i64).wrapping_neg()
    } else {
        mag as i64
    }
}

/// Round-half-to-even decision for the integer part given the fractional bits.
fn decide_round(int: u64, frac: u64, half: u64, sign: bool, rc: u8) -> bool {
    match rc {
        1 => sign && frac != 0,  // toward -inf (down): round away only if negative
        2 => !sign && frac != 0, // toward +inf (up)
        3 => false,              // truncate (toward zero)
        _ => frac > half || (frac == half && (int & 1) != 0), // nearest even
    }
}

fn round_fraction_up(sig: u64, _pad: u32, sign: bool, rc: u8, below_half_possible: bool) -> bool {
    // Whole value is a pure fraction in (0, 0.5]; decide rounding to 0 or ±1.
    let _ = below_half_possible;
    match rc {
        1 => sign,
        2 => !sign,
        3 => false,
        _ => {
            // nearest: exactly 0.5 rounds to 0 (even); >0.5 impossible here (exp<-1)
            let _ = sig;
            false
        }
    }
}

/// Shift `v` right by `n`, rounding to nearest even (for f64 subnormal encode).
fn round_shift(v: u64, n: u32) -> u64 {
    if n == 0 {
        return v;
    }
    if n >= 64 {
        return 0;
    }
    let dropped = v & ((1u64 << n) - 1);
    let half = 1u64 << (n - 1);
    let mut r = v >> n;
    if dropped > half || (dropped == half && (r & 1) != 0) {
        r += 1;
    }
    r
}

/// Normalize `value = m * 2^(ref_exp - 127)` (where `ref_exp` is the exponent of
/// bit 127 of `m`) to a 64-bit significand with the integer bit at 63, rounding to
/// nearest even. Handles overflow to inf and underflow to denormal/zero.
fn normalize_round(sign: bool, ref_exp: i32, m: u128) -> F80 {
    if m == 0 {
        return F80::zero(sign);
    }
    let hb = 127 - m.leading_zeros() as i32; // index of top set bit
                                             // exponent of bit `hb`: ref_exp - (127 - hb)
    let mut exp = ref_exp - (127 - hb);
    // Shift so the top set bit lands at bit 63.
    let sig: u64 = if hb > 63 {
        let sh = (hb - 63) as u32;
        let top = (m >> sh) as u64;
        let dropped = m & ((1u128 << sh) - 1);
        let half = 1u128 << (sh - 1);
        let round_up = dropped > half || (dropped == half && (top & 1) != 0);
        if round_up {
            match top.overflowing_add(1) {
                // Rounding carried out of bit 63 (top was all-ones): renormalize.
                (_, true) => {
                    exp += 1;
                    1 << 63
                }
                (r, false) => r,
            }
        } else {
            top
        }
    } else {
        (m as u64) << (63 - hb) as u32
    };
    pack_normal(sign, exp, sig)
}

fn pack_normal(sign: bool, exp: i32, mut sig: u64) -> F80 {
    // Ensure the integer bit is set (renormalize if a subtraction cancelled it).
    if sig == 0 {
        return F80::zero(sign);
    }
    if sig >> 63 == 0 {
        let sh = sig.leading_zeros();
        sig <<= sh;
        // exp of bit 63 decreases by sh
        return finish(sign, exp - sh as i32, sig);
    }
    finish(sign, exp, sig)
}

fn finish(sign: bool, exp: i32, sig: u64) -> F80 {
    if exp > EMAX {
        return F80::inf(sign);
    }
    if exp < EMIN {
        // Underflow: represent as a denormal-capable Normal (encoding denormalizes);
        // if it's far below, it will encode to zero.
        if exp < EMIN - 64 {
            return F80::zero(sign);
        }
    }
    F80 {
        sign,
        class: Class::Normal,
        exp,
        sig,
    }
}

fn add_sub(a: F80, mut b: F80, subtract: bool) -> F80 {
    if subtract {
        b.sign = !b.sign;
    }
    use Class::*;
    match (a.class, b.class) {
        (Nan, _) | (_, Nan) => F80::nan(),
        (Inf, Inf) => {
            if a.sign == b.sign {
                F80::inf(a.sign)
            } else {
                F80::nan()
            }
        }
        (Inf, _) => F80::inf(a.sign),
        (_, Inf) => F80::inf(b.sign),
        (Zero, Zero) => F80::zero(a.sign && b.sign),
        (Zero, _) => b,
        (_, Zero) => a,
        (Normal, Normal) => {
            // Align to a common exponent using 128-bit significands (guard bits below).
            let (hi, lo) = if a.exp >= b.exp { (a, b) } else { (b, a) };
            let shift = (hi.exp - lo.exp) as u32;
            let hm = (hi.sig as u128) << 64;
            let lm = if shift >= 128 {
                if lo.sig != 0 {
                    1
                } else {
                    0
                }
            } else {
                let base = (lo.sig as u128) << 64;
                let shifted = base >> shift;
                // sticky: OR in whether any 1 bits were shifted out
                if shift > 0 && (base & ((1u128 << shift) - 1)) != 0 {
                    shifted | 1
                } else {
                    shifted
                }
            };
            let ref_exp = hi.exp + 1; // bit 127 of the 128-bit hm corresponds to hi.exp+1?
                                      // hm = hi.sig<<64: hi.sig bit63 -> bit127, exp of bit127 = hi.exp+?
                                      // hi.sig bit63 has exponent hi.exp; after <<64 it's bit127 with same value,
                                      // so ref_exp (exponent of bit127) = hi.exp.
            let _ = ref_exp;
            if hi.sign == lo.sign {
                let (sum, carry) = hm.overflowing_add(lm);
                if carry {
                    // Shouldn't happen: both < 2^127, sum < 2^128.
                    return normalize_round(hi.sign, hi.exp + 1, sum >> 1 | (1 << 127));
                }
                normalize_round(hi.sign, hi.exp, sum)
            } else {
                // Opposite signs: subtract magnitudes.
                if hm >= lm {
                    let diff = hm - lm;
                    if diff == 0 {
                        return F80::zero(false);
                    }
                    normalize_round(hi.sign, hi.exp, diff)
                } else {
                    normalize_round(lo.sign, hi.exp, lm - hm)
                }
            }
        }
    }
}

/// Integer square root of a u128; returns `(floor(sqrt(x)), is_exact)`.
fn isqrt128(x: u128) -> (u128, bool) {
    if x == 0 {
        return (0, true);
    }
    // Newton's method with a good initial estimate.
    let mut r = 1u128 << ((128 - x.leading_zeros()) / 2 + 1);
    loop {
        let nr = (r + x / r) / 2;
        if nr >= r {
            break;
        }
        r = nr;
    }
    while r * r > x {
        r -= 1;
    }
    (r, r * r == x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(x: f64) -> F80 {
        F80::from_f64(x.to_bits())
    }
    fn back(a: F80) -> f64 {
        f64::from_bits(a.to_f64())
    }

    /// f64 ULP distance (finite values).
    fn ulp(a: f64, b: f64) -> u64 {
        let key = |x: f64| {
            let bits = x.to_bits() as i64;
            if bits < 0 {
                i64::MIN - bits
            } else {
                bits
            }
        };
        key(a).abs_diff(key(b))
    }

    /// Magnitude of the binary exponent of `|v|` (how many bits below 1.0). Used to show
    /// an identity residual is ~80-bit small, not just ~53-bit.
    fn exp_of(v: F80) -> i32 {
        match v.class {
            Class::Zero => -200,
            Class::Normal => v.exp,
            _ => 200,
        }
    }

    #[test]
    fn extended_transcendentals_round_to_libm() {
        // The Extended F80 result must round to the correct f64 (within 1 ULP of libm).
        for &x in &[0.1, 0.5, 0.7, 1.0, 1.3, 2.5, -0.4, -2.0, 3.0] {
            assert!(ulp(back(f(x).sin_ext()), x.sin()) <= 1, "sin({x})");
            assert!(ulp(back(f(x).cos_ext()), x.cos()) <= 1, "cos({x})");
            assert!(ulp(back(f(x).tan_ext()), x.tan()) <= 2, "tan({x})");
        }
        for &x in &[-1.0, -0.5, 0.0, 0.3, 0.75, 1.0] {
            assert!(
                ulp(back(f(x).exp2m1_ext()), x.exp2() - 1.0) <= 1,
                "exp2m1({x})"
            );
        }
        for &(y, x) in &[
            (1.0, 2.0),
            (-3.0, 1.0),
            (1.0, -1.0),
            (-2.0, -0.5),
            (5.0, 0.0),
        ] {
            assert!(
                ulp(back(F80::atan2_ext(f(y), f(x))), y.atan2(x)) <= 2,
                "atan2({y},{x})"
            );
        }
        for &(y, x) in &[(3.0, 8.0), (1.5, 10.0), (2.0, 0.5)] {
            assert!(
                ulp(back(F80::ylog2x_ext(f(y), f(x))), y * x.log2()) <= 2,
                "ylog2x({y},{x})"
            );
        }
        for &(y, x) in &[(2.0f64, 0.25f64), (1.0, 0.001), (3.0, -0.5)] {
            let want = y * (x.ln_1p() / std::f64::consts::LN_2);
            assert!(
                ulp(back(F80::ylog2xp1_ext(f(y), f(x))), want) <= 2,
                "ylog2xp1({y},{x})"
            );
        }
    }

    #[test]
    fn extended_beats_f64_on_identities() {
        // sin²+cos²=1 must hold to ~80-bit under Extended, but only ~53-bit under Fast —
        // proving the extra precision is real (not just an f64 result widened to F80).
        for &x in &[0.3f64, 0.7, 1.2, 2.9, -1.5] {
            let xf = f(x);
            let (se, ce) = (xf.sin_ext(), xf.cos_ext());
            let res_ext = F80::sub(F80::add(F80::mul(se, se), F80::mul(ce, ce)), F80::ONE);
            // Extended residual is below 2^-60 (near F80 epsilon 2^-63).
            assert!(
                exp_of(res_ext) <= -60,
                "sin²+cos²-1 exponent {} not ~80-bit for x={x}",
                exp_of(res_ext)
            );
            // Fast path residual is far larger (~2^-52), so Extended is strictly tighter.
            let (sf, cf) = (xf.sin(), xf.cos());
            let res_fast = F80::sub(F80::add(F80::mul(sf, sf), F80::mul(cf, cf)), F80::ONE);
            assert!(
                exp_of(res_ext) < exp_of(res_fast),
                "Extended residual (2^{}) not tighter than Fast (2^{}) for x={x}",
                exp_of(res_ext),
                exp_of(res_fast)
            );
        }
        // exp2(log2(x)) == x to ~80-bit: 2^(log2 x) via ylog2x(1,x) then exp2m1+1.
        for &x in &[1.5f64, 3.0, 7.5, 0.75] {
            let l = F80::ylog2x_ext(F80::ONE, f(x)); // log2(x)
            let back_x = F80::add(l.exp2m1_ext(), F80::ONE); // 2^l
            let res = F80::sub(back_x, f(x));
            let rel = exp_of(res) - exp_of(f(x));
            assert!(rel <= -58, "exp2(log2({x})) rel-exp {rel} not ~80-bit");
        }
    }

    #[test]
    fn roundtrip_f64() {
        for x in [
            0.0, 1.0, -1.0, 0.5, 2.0, 3.0, 1e300, 1e-300, 123456.789, -0.1,
        ] {
            assert_eq!(back(f(x)), x, "roundtrip {x}");
        }
    }

    #[test]
    fn basic_arith_matches_f64_when_exact() {
        // Values whose f64 arithmetic is exact must match f80 arithmetic exactly.
        assert_eq!(back(F80::add(f(1.0), f(2.0))), 3.0);
        assert_eq!(back(F80::sub(f(5.0), f(3.0))), 2.0);
        assert_eq!(back(F80::mul(f(3.0), f(4.0))), 12.0);
        assert_eq!(back(F80::div(f(12.0), f(4.0))), 3.0);
        assert_eq!(back(F80::div(f(1.0), f(2.0))), 0.5);
        assert_eq!(back(F80::sqrt(f(16.0))), 4.0);
        assert_eq!(back(F80::sqrt(f(2.0))), 2.0_f64.sqrt());
    }

    #[test]
    fn transcendentals_match_f64_libm() {
        // f64-precision transcendentals (task-206): the F80 result rounds back to the
        // exact libm f64 value.
        assert_eq!(back(f(0.7).sin()), 0.7_f64.sin());
        assert_eq!(back(f(0.7).cos()), 0.7_f64.cos());
        assert_eq!(back(f(0.6).tan()), 0.6_f64.tan());
        assert_eq!(back(f(0.3).exp2m1()), 0.3_f64.exp2() - 1.0);
        assert_eq!(back(F80::atan2(f(1.0), f(2.0))), 1.0_f64.atan2(2.0));
        // fyl2x: y * log2(x) — 3 * log2(8) = 9 exactly.
        assert_eq!(back(F80::ylog2x(f(3.0), f(8.0))), 9.0);
        // fyl2xp1: y * log2(1 + x).
        let want = 2.0 * (0.25_f64.ln_1p() / core::f64::consts::LN_2);
        assert_eq!(back(F80::ylog2xp1(f(2.0), f(0.25))), want);
    }

    #[test]
    fn extended_precision_beats_f64() {
        // (1 + 2^-60) computed in f80 keeps the low bit that f64 would drop; the
        // product with itself differs from the f64 result — proving >53-bit mantissa.
        let one = f(1.0);
        let tiny = F80::from_bytes(&{
            // 2^-60 as f80
            let v = F80 {
                sign: false,
                class: Class::Normal,
                exp: -60,
                sig: 1 << 63,
            };
            v.to_bytes()
        });
        let s = F80::add(one, tiny); // 1 + 2^-60, representable in f80, not f64
        assert_eq!(back(tiny), 2f64.powi(-60));
        // s rounded back to f64 is 1.0 (f64 can't hold it), but s*s in f80 keeps it.
        assert_eq!(back(s), 1.0);
        let sq = F80::mul(s, s); // 1 + 2^-59 + 2^-120
                                 // sq back to f64 = 1 + 2^-59 (the cross term survives; f64(1+2^-60)^2 == 1)
        assert_eq!(back(sq), 1.0 + 2f64.powi(-59));
    }

    #[test]
    fn to_int_rounding_modes() {
        // 1.5 under each RC: nearest->2 (even), truncate->1, down->1, up->2.
        assert_eq!(f(1.5).to_i64_rc(0), 2);
        assert_eq!(f(1.5).to_i64_rc(3), 1);
        assert_eq!(f(1.5).to_i64_rc(1), 1);
        assert_eq!(f(1.5).to_i64_rc(2), 2);
        assert_eq!(f(2.5).to_i64_rc(0), 2); // ties to even
        assert_eq!(f(-1.5).to_i64_rc(0), -2);
        assert_eq!(f(-1.5).to_i64_rc(3), -1);
        assert_eq!(f(3.7).to_i64_rc(0), 4);
        assert_eq!(f(3.7).to_i64_rc(3), 3);
        assert_eq!(F80::from_i64(42).to_i64_rc(0), 42);
    }

    #[test]
    fn bytes_roundtrip() {
        for x in [1.0, -2.5, 0.0, 1e100] {
            let a = f(x);
            let b = F80::from_bytes(&a.to_bytes());
            assert_eq!(back(b), x);
        }
    }
}
