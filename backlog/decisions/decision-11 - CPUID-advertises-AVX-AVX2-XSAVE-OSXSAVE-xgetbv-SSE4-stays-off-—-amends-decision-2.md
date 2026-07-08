---
id: decision-11
title: >-
  CPUID advertises AVX/AVX2 (+XSAVE/OSXSAVE, xgetbv); SSE4 stays off — amends
  decision-2
date: '2026-07-08 17:42'
status: accepted
---

**Deciders:** Mikołaj Mikołajczyk

## Context

M8-SIMD landed a broad VEX/AVX + AVX2 lifter (tasks 168.1–168.3): 128- and
256-bit data movement, logic, packed integer arithmetic, `vpshufb`, packed
shifts, broadcasts, `vinserti128`/`vextracti128`, `vpmovmskb`, and the
cross-lane permutes (`vpermq`/`vpermd`/`vperm2i128`/`vpalignr`). Until now
`cpuid_run` still advertised **no AVX** (leaf 1 ECX bit 28 clear, leaf 7 EBX
AVX2 clear), so glibc/Go IFUNC resolvers kept selecting their SSE2/SSSE3 paths
and the AVX code was exercised only by hand-written differential tests — never
by a real guest.

decision-2 masked SSE4.1/4.2 to keep glibc on the fully-lifted SSSE3 string
routines. AVX advertisement is the mirror-image move: flip the AVX/AVX2 bits so
glibc and Go select the AVX2 string/memory routines that the new lifter covers.

## Decision

Advertise the AVX enable triad and AVX2:

- **Leaf 1 ECX**: set XSAVE (26), OSXSAVE (27), AVX (28) on top of the existing
  SSE3/SSSE3/POPCNT. OSXSAVE signals the OS enabled XCR0; guests confirm it by
  executing `xgetbv`.
- **Leaf 7 EBX**: set AVX2 (bit 5). BMI1/BMI2 stay off (unlifted).
- **`xgetbv`** is now lifted (previously an unknown-instruction trap): with
  ECX=0 it returns XCR0 = `0x7` (x87|SSE|AVX state enabled) in EDX:EAX, matching
  the advertised AVX bits.

**SSE4.1/SSE4.2 stay off** (decision-2 unchanged). AVX2 routines use VEX-encoded
ops we lift; they do not need the legacy `pcmpistri`/`blendv`/`pmovzx` bits,
which remain live traps. Advertising AVX without SSE4 is architecturally unusual
but glibc/Go test each feature bit independently, so it is safe.

`vptest` (VEX `0F38.17`, 128 and 256) was the one gap advertisement exposed:
Go's AVX2 `memmove`/`memclr`/compare routines use it to test a YMM for all-zero.
It is now lifted (`ZF = (b & a == 0)`, `CF = (b & !a == 0)`, other flags cleared)
in interp + Cranelift. This is the `ptest`-gated trigger decision-2 anticipated —
resolved for the VEX form.

## Consequences

The full local real-binary corpus stays green three ways (native == interp ==
JIT) with glibc/Go now on their AVX2 paths:

- glibc-static: busybox (sha256sum/wc/sort/awk), gzip, djpeg.
- glibc-dynamic: python3, sqlite3, lua, `dynamic`/`musl` hello, pthreads.
- Go (own CPUID IFUNC dispatch, AVX2 memmove/memclr): hello, net, http, caddy.

CI caveat: the OCI/registry-pull corpus (ubuntu/alpine glibc) still SKIPs in CI
(no network egress); AVX2 coverage of those images is verified locally only.

## Alternatives considered

- **Keep AVX unadvertised** — the status quo; leaves the entire AVX2 lifter
  dead for real guests and forfeits the x86-64-v3 host-binary goal (task-168).
- **Advertise SSE4 too** — reintroduces the `pcmpistri`/`blendv` traps
  decision-2 removed, for no benefit (AVX2 routines are VEX).
- **Advertise BMI1/BMI2** — glibc AVX2 string routines gate on the AVX2 bit, not
  BMI; advertising unlifted `bextr`/`blsr`/etc. would only add traps.

## Trigger to revisit

A corpus/target that needs AVX-512/EVEX (CachyOS `/usr/bin` are v4), a genuine
BMI instruction, or an SSE4-only op with no fallback. Implement the specific
instruction, add its bit, and re-verify the whole differential corpus first —
glibc/Go silently change code paths on a feature bit.
