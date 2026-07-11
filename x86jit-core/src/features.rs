//! Guest CPU feature set (task-169). The embedder chooses which ISA extensions the
//! guest sees; `cpuid_run` and `xgetbv` project this into CPUID leaves / XCR0 instead
//! of hardcoding a single global set. This turns "advertise AVX-512" from a risky
//! all-or-nothing decision into a per-run parameter, and is the correct library shape:
//! the embedder declares the guest CPU (like `qemu -cpu`), not us.
//!
//! **Advertise ⊆ lift.** Advertising a feature the lifter can't execute is a live trap
//! (a CPUID-dispatched guest jumps straight into the instruction). The [`GuestCpuFeatures::default`]
//! set is exactly what we advertise today and is guarded by the compat tests
//! (`cpuid_advertises_only_what_lifts`). An embedder selecting a richer preset than the
//! lifter covers is a documented caller risk — a guest trap is a legal `Exit`, not a bug.
//! Supersedes the global model of `backlog/decisions/decision-2` and `decision-11`.

/// A single guest CPU feature bit. The discriminant is the internal bit index within
/// [`GuestCpuFeatures`]; the CPUID leaf position is assigned by the projection methods.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Feature {
    // x86-64-v1 baseline SIMD.
    Sse,
    Sse2,
    Mmx,
    // x86-64-v2.
    Sse3,
    Ssse3,
    Sse41,
    Sse42,
    Popcnt,
    Cx16,
    // x86-64-v3.
    Movbe,
    Avx,
    Avx2,
    Bmi1,
    Bmi2,
    Fma,
    F16c,
    Lzcnt,
    // AVX enable plumbing (XSAVE/OSXSAVE gate XCR0 + the AVX/AVX-512 state).
    Xsave,
    Osxsave,
    // x86-64-v4 (AVX-512).
    Avx512f,
    Avx512bw,
    Avx512dq,
    Avx512vl,
    Avx512cd,
    // Crypto ISA extensions (task-211). Orthogonal to the v-levels but ubiquitous on
    // real v2+ (AES/PCLMUL) and v4-era (SHA/GFNI) hardware. Only the 128-bit forms are
    // lifted — the wide VAES/VPCLMULQDQ (leaf7 ECX bits 9/10) stay unadvertised so guests
    // pick the AES-NI/PCLMULQDQ path, keeping "advertise ⊆ lift".
    Aes,
    Pclmul,
    Sha,
    Gfni,
}

impl Feature {
    #[inline]
    const fn bit(self) -> u64 {
        1u64 << (self as u8)
    }
}

/// A guest CPU feature set: a bitset over [`Feature`]. Build from a preset and refine
/// with [`GuestCpuFeatures::with`] / [`GuestCpuFeatures::without`].
///
/// ```ignore
/// let f = GuestCpuFeatures::v3().with(Feature::Avx512f); // v3 + a single v4 bit
/// vm.set_guest_cpu_features(GuestCpuFeatures::v4());
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GuestCpuFeatures(u64);

impl GuestCpuFeatures {
    /// Empty set (only the always-on scalar baseline that CPUID reports unconditionally).
    pub const fn empty() -> Self {
        GuestCpuFeatures(0)
    }

    const fn from_slice(fs: &[Feature]) -> Self {
        Self::empty().with_all(fs)
    }

    /// Union a slice of features into the set (const — used to layer each tier's delta
    /// onto the tier below it).
    const fn with_all(self, fs: &[Feature]) -> Self {
        let mut bits = self.0;
        let mut i = 0;
        while i < fs.len() {
            bits |= fs[i].bit();
            i += 1;
        }
        GuestCpuFeatures(bits)
    }

    /// x86-64-v1 baseline: MMX + SSE + SSE2 (+ scalar, always on). MMX is present on
    /// every x86-64 CPU and is load-bearing for glibc's cpu-features init (the level
    /// derivation mis-fires without it — see the decision-2 waiver), so every preset
    /// carries it even though no MMX instruction is lifted.
    pub const fn baseline() -> Self {
        Self::from_slice(&[Feature::Mmx, Feature::Sse, Feature::Sse2])
    }

    /// x86-64-v2: baseline + SSE3/SSSE3/SSE4.1/SSE4.2/POPCNT/CMPXCHG16B/MOVBE, plus the
    /// near-universal AES-NI + PCLMULQDQ crypto (task-211; present on essentially every
    /// v2-era CPU and load-bearing for openssl/ssh taking the hardware crypto path).
    pub const fn v2() -> Self {
        Self::baseline().with_all(&[
            Feature::Sse3,
            Feature::Ssse3,
            Feature::Sse41,
            Feature::Sse42,
            Feature::Popcnt,
            Feature::Cx16,
            Feature::Movbe,
            Feature::Aes,
            Feature::Pclmul,
        ])
    }

    /// x86-64-v3: v2 + AVX/AVX2/BMI1/BMI2/FMA/F16C/LZCNT (+ XSAVE/OSXSAVE).
    pub const fn v3() -> Self {
        Self::v2().with_all(&[
            Feature::Avx,
            Feature::Avx2,
            Feature::Bmi1,
            Feature::Bmi2,
            Feature::Fma,
            Feature::F16c,
            Feature::Lzcnt,
            Feature::Xsave,
            Feature::Osxsave,
        ])
    }

    /// x86-64-v4: v3 + AVX-512 F/BW/DQ/VL/CD, plus SHA-NI + GFNI (task-211; standard on
    /// the v4-era cores this preset models — Ice Lake / Zen 4 — and lets openssl/ssh
    /// dgst-sha256 and GFNI-accelerated codecs exercise our lifts).
    pub const fn v4() -> Self {
        Self::v3().with_all(&[
            Feature::Avx512f,
            Feature::Avx512bw,
            Feature::Avx512dq,
            Feature::Avx512vl,
            Feature::Avx512cd,
            Feature::Sha,
            Feature::Gfni,
        ])
    }

    /// The set x86jit advertises by default — exactly what `cpuid_run` reported before
    /// task-169 (SSE, SSE2, SSE3, SSSE3, POPCNT, MMX, XSAVE, OSXSAVE, AVX, AVX2). Chosen
    /// so the lifter fully executes every IFUNC-selected path (SSE4/BMI/AVX-512 stay off:
    /// their `pcmpistri`/`bextr`/masked ops aren't lifted yet — decision-2/11). MMX is a
    /// detection-only bit glibc's cpu-features init needs (waived in the compat map).
    pub const fn stable() -> Self {
        Self::from_slice(&[
            Feature::Sse,
            Feature::Sse2,
            Feature::Mmx,
            Feature::Sse3,
            Feature::Ssse3,
            Feature::Popcnt,
            Feature::Xsave,
            Feature::Osxsave,
            Feature::Avx,
            Feature::Avx2,
        ])
    }

    /// Add a feature.
    pub const fn with(self, f: Feature) -> Self {
        GuestCpuFeatures(self.0 | f.bit())
    }

    /// Remove a feature.
    pub const fn without(self, f: Feature) -> Self {
        GuestCpuFeatures(self.0 & !f.bit())
    }

    /// Is a feature present?
    #[inline]
    pub const fn has(self, f: Feature) -> bool {
        self.0 & f.bit() != 0
    }

    #[inline]
    fn if_has(self, f: Feature, bit: u32) -> u32 {
        if self.has(f) {
            1 << bit
        } else {
            0
        }
    }

    // --- CPUID projections. The single place feature → leaf-bit mapping lives. ---

    /// CPUID leaf 1 ECX. Every bit is a feature (no always-on scalar bits here).
    pub fn leaf1_ecx(self) -> u32 {
        self.if_has(Feature::Sse3, 0)
            | self.if_has(Feature::Pclmul, 1)
            | self.if_has(Feature::Ssse3, 9)
            | self.if_has(Feature::Fma, 12)
            | self.if_has(Feature::Cx16, 13)
            | self.if_has(Feature::Sse41, 19)
            | self.if_has(Feature::Sse42, 20)
            | self.if_has(Feature::Movbe, 22)
            | self.if_has(Feature::Popcnt, 23)
            | self.if_has(Feature::Aes, 25)
            | self.if_has(Feature::Xsave, 26)
            | self.if_has(Feature::Osxsave, 27)
            | self.if_has(Feature::Avx, 28)
            | self.if_has(Feature::F16c, 29)
    }

    /// CPUID leaf 1 EDX. The always-on scalar baseline (FPU/TSC/CX8/CMOV/FXSR) plus the
    /// SSE/SSE2/MMX feature bits.
    pub fn leaf1_edx(self) -> u32 {
        const BASELINE: u32 = (1 << 0)   // FPU
            | (1 << 4)   // TSC
            | (1 << 8)   // CX8 (cmpxchg8b)
            | (1 << 15)  // CMOV
            | (1 << 24); // FXSR
        BASELINE
            | self.if_has(Feature::Mmx, 23)
            | self.if_has(Feature::Sse, 25)
            | self.if_has(Feature::Sse2, 26)
    }

    /// CPUID leaf 7 subleaf 0 EBX (AVX2, BMI1/2, AVX-512 family, SHA-NI).
    pub fn leaf7_ebx(self) -> u32 {
        self.if_has(Feature::Bmi1, 3)
            | self.if_has(Feature::Avx2, 5)
            | self.if_has(Feature::Bmi2, 8)
            | self.if_has(Feature::Avx512f, 16)
            | self.if_has(Feature::Avx512dq, 17)
            | self.if_has(Feature::Sha, 29)
            | self.if_has(Feature::Avx512cd, 28)
            | self.if_has(Feature::Avx512bw, 30)
            | self.if_has(Feature::Avx512vl, 31)
    }

    /// CPUID leaf 7 subleaf 0 ECX (GFNI at bit 8; the wide VAES/VPCLMULQDQ bits 9/10 stay
    /// off — we lift only the 128-bit forms, so a guest picks the AES-NI/PCLMULQDQ path).
    pub fn leaf7_ecx(self) -> u32 {
        self.if_has(Feature::Gfni, 8)
    }

    /// CPUID extended leaf 0x8000_0001 ECX (LZCNT/ABM). LAHF (bit 0) always on.
    pub fn ext_leaf1_ecx(self) -> u32 {
        (1 << 0) | self.if_has(Feature::Lzcnt, 5)
    }

    /// XCR0 value returned by `xgetbv` (ECX=0). x87|SSE always; +AVX (bit 2) when AVX is
    /// enabled; +opmask|ZMM_hi|hi16_ZMM (bits 5..7) when AVX-512 is enabled.
    pub fn xcr0(self) -> u64 {
        let mut x = 0b011; // x87 | SSE
        if self.has(Feature::Avx) {
            x |= 0b100; // AVX state
        }
        if self.has(Feature::Avx512f) {
            x |= 0b1110_0000; // opmask | ZMM_hi256 | hi16_ZMM
        }
        x
    }
}

impl Default for GuestCpuFeatures {
    /// Today's advertised set — see [`GuestCpuFeatures::stable`]. Preserves behavior for every
    /// embedder that doesn't call `set_guest_cpu_features`.
    fn default() -> Self {
        Self::stable()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_reproduces_the_historical_cpuid() {
        // Exactly what cpuid_run hardcoded before task-169.
        let f = GuestCpuFeatures::default();
        assert_eq!(
            f.leaf1_ecx(),
            (1 << 0) | (1 << 9) | (1 << 23) | (1 << 26) | (1 << 27) | (1 << 28)
        );
        assert_eq!(
            f.leaf1_edx(),
            (1 << 0)
                | (1 << 4)
                | (1 << 8)
                | (1 << 15)
                | (1 << 23)
                | (1 << 24)
                | (1 << 25)
                | (1 << 26)
        );
        assert_eq!(f.leaf7_ebx(), 1 << 5); // AVX2 only
        assert_eq!(f.ext_leaf1_ecx(), 1 << 0); // LAHF, no LZCNT
        assert_eq!(f.xcr0(), 0x7); // x87|SSE|AVX
    }

    #[test]
    fn v4_advertises_avx512_and_wide_xcr0() {
        let f = GuestCpuFeatures::v4();
        // leaf7 EBX: AVX2(5) + AVX512 F(16)/DQ(17)/CD(28)/BW(30)/VL(31) + BMI1(3)/BMI2(8).
        assert!(f.leaf7_ebx() & (1 << 16) != 0, "AVX512F");
        assert!(f.leaf7_ebx() & (1 << 30) != 0, "AVX512BW");
        assert!(f.leaf7_ebx() & (1 << 31) != 0, "AVX512VL");
        assert_eq!(f.xcr0(), 0xE7); // + opmask|ZMM_hi|hi16_ZMM
                                    // leaf1 ECX gains SSE4.1/4.2/FMA/F16C/MOVBE.
        assert!(f.leaf1_ecx() & (1 << 20) != 0, "SSE4.2");
    }

    #[test]
    fn baseline_has_no_avx() {
        let f = GuestCpuFeatures::baseline();
        assert_eq!(f.leaf1_ecx() & (1 << 28), 0); // no AVX
        assert_eq!(f.leaf7_ebx(), 0); // no AVX2/AVX-512
        assert_eq!(f.xcr0(), 0x3); // x87|SSE only
    }

    #[test]
    fn with_without_toggle() {
        let f = GuestCpuFeatures::v3().with(Feature::Avx512f);
        assert!(f.has(Feature::Avx512f));
        assert!(f.has(Feature::Avx2));
        assert!(!f.without(Feature::Avx2).has(Feature::Avx2));
    }
}
