# CPUID advertises SSSE3 but not SSE4.1/SSE4.2

**Date:** 2026-07-06
**Decider:** Mikołaj Mikołajczyk (autonomous session)
**Tags:** guest-compat | x86-semantics

## Context

`cpuid_run` (leaf 1, ECX) previously advertised SSE3, SSSE3, SSE4.1, SSE4.2 and
POPCNT, but only *partially* lifted SSE4 (crc32/pextrb/pcmpeqq existed;
`pcmpistri`/`pcmpestri`, `pmovzx`, `pmulld`, `ptest`, `round*`, `blendv*` did
not). glibc's IFUNC resolvers jump straight into an advertised instruction, so a
modern ubuntu (25.10) glibc `dash` executed the SSE4.2 string workhorse
`pcmpistri` during startup and hit `UnknownInstruction`. The `palignr` gap was
filled this session, so the SSSE3 string routines are now fully executable.

## Decision

Advertise **SSE3 + SSSE3 + POPCNT only**; drop the SSE4.1 and SSE4.2 bits. glibc
then selects its SSSE3/SSE2 string variants (which use `pshufb`/`palignr`/
`pcmpeqb` — all lifted) instead of the SSE4.2 `pcmpistri` family. Result: ubuntu
`dash -c 'echo …'` runs three ways (interp == JIT). The full differential corpus
(busybox/alpine/glibc/sqlite/lua/cpython + native oracle) stays green — nothing
in it selected an SSE4-only path we implement; `crc32`/`pcmpeqq` degrade to
correct software/SSE2 equivalents.

## Alternatives considered

- **Implement `pcmpistri`/`pcmpestri`** — the complex SSE4.2 string-compare
  aggregation ops. Correct but large and error-prone; masking gets the same
  guest-compat win with no new instruction and dodges the other SSE4.1 gaps too.
- **Keep advertising SSE4, leave the trap** — the status quo; blocks every
  modern glibc dynamic binary at startup.

## Trigger to revisit

A corpus/target program that needs a genuine SSE4-only instruction (e.g. hardware
`crc32c` for performance parity, or `ptest`-gated code with no fallback). Then
implement the specific instruction and re-add its feature bit — and re-verify the
whole differential corpus before doing so (glibc silently changes code paths).
