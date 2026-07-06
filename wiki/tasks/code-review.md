# Code review — 2026-07-06 (whole-tree, our crates)

High-effort multi-agent review (8 finder angles → verify). Scope: our crates
only (`x86jit-core`, `-cranelift`, `-elf`, `-linux`, `-oci`, `-run`); 3rd-party
excluded. `CONFIRMED` = constructible from code; `PLAUSIBLE` = real reachable
state; one refuted candidate dropped (`plain_read` size>8 — no lifted atomic
carries size>8, CMPXCHG16B never lifted).

Ranked by severity. Check off as fixed.

## Correctness

- [x] **1. `lift.rs:2277` — BT/BTS/BTR/BTC mem+reg bit index masks instead of bit-string addressing** `CONFIRMED` — FIXED (reg-index mem form now byte-addresses `base + sar(sext(idx),3)`, bit `idx&7`; immediate form unchanged. Test `bt_mem_reg_bit_string_matches_unicorn`, green vs Unicorn.)
  `bts qword [rdi], rax`, rax≥64 must address byte `rdi + rax/8`. Lifter loads
  only `size` bytes at EA, passes bit into `IrOp::Bt` which masks `bit &
  (size*8-1)` (interp.rs:439, codegen.rs:538). rax=64 → bit 0 of `[rdi]` not
  `[rdi+8]`. Wrong byte + wrong CF, **both backends** (silent; only
  native/unicorn catches). Compilers emit `bts/btr/bts [mem],reg` for bitmaps.
  Fix: bit-string base adjust (`ea += (signed_bit >> 3)` at operand width) or
  return Unsupported for the mem+reg case.

- [x] **2. `shim.rs:1860` — socket `read` panics host on bad guest buffer (after draining bytes)** `CONFIRMED` — FIXED (do_read file/socket/pipe/stdin write-backs return -EFAULT via is_err; try_resize_scratch/try_reserve guard huge len. Test read_into_unmapped_buffer_efaults_not_panics.)
  `libc::read` drains n bytes, then
  `vm.write_bytes(buf,…).expect("socket read buffer mapped")` aborts host when
  guest `buf` (Rdi) unmapped/short. Double harm: host panic from guest input +
  lost network bytes. `scratch.resize(guest_len)` also alloc-aborts on huge len.
  File path :1842 same shape. Violates "no host panic from guest input"
  (504e97d). Fix → `-EFAULT`.

- [x] **3. `shim.rs:1496` — bind/connect/setsockopt panic host on bad guest sockaddr** `CONFIRMED` — FIXED (bind/connect/setsockopt use try_fill_scratch → -EFAULT; try_reserve guards huge len.)
  `fill_scratch(vm, addr, guest_len)` → `read_bytes(…).expect("syscall buffer is
  mapped")`. Unmapped/edge-straddling guest sockaddr/optval → host abort; huge
  len → resize alloc abort. Read-side arms: bind:1496, connect:1502,
  setsockopt:1586. Write-back paths already use `let _ = write_bytes`. Fix:
  fallible read → `-EFAULT`.

- [x] **4. `codegen.rs:1972` — JIT vs interp divergence on unmapped-in-span access** `CONFIRMED` (live, even Flat) — RECORDED as a decision (not fixed): [`wiki/decisions/2026-07-06-jit-interp-unmapped-in-span.md`](../decisions/2026-07-06-jit-interp-unmapped-in-span.md); behavior pinned by `unmapped_in_span_access_diverges_interp_vs_jit_known_gap`. Resolution = guard pages under Phase-3 signals.
  `checked_addr` bounds only `addr+size <= memsize`, no region membership. Guest
  nil/wild pointer in-span-but-unmapped: JIT reads backing (demand-zero) / writes
  silently; interp `region_at` → `MemTrap::Unmapped` → Exit. Breaks the core
  interp==JIT invariant; wild write silently grows RSS. Deeper fix (region check
  in JIT, or a guard page) — larger, may defer. **DEFERRED — needs a design call:**
  a per-access region check rewrites the flat hot path ADR-0001 deliberately keeps
  cheap; a guard-page scheme is a separate mechanism. Not bandaided; left for the
  maintainer to decide alongside the SoftMmu direction.

- [x] **5. `shim.rs:882` — `writev` to socket discards return, reports full success** `CONFIRMED` — FIXED (socket writev honors libc::write return: short/EPIPE surface host_errno, no false full-success; iov base EFAULT-guarded.)
  `libc::write(...)` return dropped, `total += len` unconditional → EPIPE/short
  write reported as full success, silent data loss. Sibling `SYS_WRITE` socket
  arm (:753) does it right. Fix: honor the return like SYS_WRITE.

- [x] **6. `memory.rs:242` — `from_host_ram` never checks `span <= ram.len`** `CONFIRMED` — FIXED (from_host_ram asserts span <= ram.len.)
  `size()` returns span, map()/JIT bound against span, backing has only `ram.len`
  bytes. span>len → access in `[ram.len, span)` passes the bound, derefs past the
  mmap → host OOB/UB (JIT) or slice panic (interp). Latent (only a test calls it)
  but a Phase-1b footgun. Fix: `assert!(span <= ram.len)` in `from_host_ram`.

- [x] **7. `lift.rs:333` — LEA with FS/GS override wrongly adds segment base** `CONFIRMED` — FIXED (LEA lifts via effective_address_no_segment, ignores FS/GS base. Test lea_ignores_segment_base_matches_unicorn.)
  `lea rax, fs:[rbx]` must yield rbx. `effective_address` → `with_segment` adds
  fs_base/gs_base with no LEA guard → off by TLS base. Fix: LEA computes the raw
  address, skips the segment base.

- [x] **8. `memory.rs:300` — SMC tracking silently no-ops above CODE_WINDOW (4 GiB)** `PLAUSIBLE` — DOCUMENTED, not asserted. Second-pass review: a debug_assert here false-aborts a >4 GiB Flat or a block straddling the 4 GiB window, and contradicts the documented graceful no-op (memory.rs:189-192). Left as the intentional no-op; comment records the limitation.
  `code_page.get(page)` → None past 4 GiB. Code executing ≥4 GiB (Reserved span,
  Flat>4GiB, high exec mmap): self-modifying write never marks dirty → stale JIT
  block runs. Rests on unenforced "code lives low" assumption. Fix: at least a
  `debug_assert` on `mark_code` above the window, or widen the window on demand.

- [x] **9. `memory.rs:680` — atomic alignment tested on host ptr, not guest addr** `PLAUSIBLE` — DOCUMENTED, not asserted. Second-pass review: `[u8]` layout is align-1 by contract, so a 16-align debug_assert false-aborts under Miri (which the dtor tests target) or a swapped `#[global_allocator]`. Comment in from_backing records that host alignment tracks guest alignment in practice (system alloc / mmap ≥16-aligned) but is not type-guaranteed.
  `(host_base+addr) % size == 0`. Misaligned `host_base` (Vec is align-1 at type
  level) degrades a guest-aligned LOCK atomic to non-atomic RMW → torn/lost
  cross-thread updates. Rare (allocations happen aligned). Fix: test guest-addr
  alignment, or assert host_base alignment.

- [x] **10. `lift.rs:2648` — 0x67 address-size override not truncated to 32 bits** `CONFIRMED` — FIXED (effective_address_no_segment masks EA to 32 bits when base/index is a 32-bit reg. Test addr_size_override_truncates_to_32_bits_matches_unicorn. Second pass: also truncate the 32-bit RIP-relative (EIP) early-return.)
  `mov eax,[ebx]` under 0x67 reads full 64-bit RBX, no 32-bit mask → wrong
  address if upper RBX nonzero. Rare prefix in 64-bit code. Fix: mask EA to 32
  bits when the 0x67 prefix is present.

## Also noted (lower severity / latent / cleanup)

- [ ] `shim.rs:1597` — setsockopt always returns 0, masks SO_REUSEADDR/TCP_NODELAY failure.
- [ ] `memory.rs:362` — `highest_mapped_below` returns `end` for a region straddling `limit` (violates "strictly below").
- [ ] `proc.rs:257` + `memory.rs:274` — fork under host-backed Reserved panics host; latent (Reserved not wired to loader). "Go never forks" is wrong (os/exec forks via clone-without-CLONE_VM). Should be a typed Exit, not panic.
- [ ] Cleanup: socket-arm `EBADF`/`host_errno` skeleton duplicated ×7 (`with_socket` helper); fd-install alloc+insert ×5-6 (`install(fd)` helper); `code_page_range(addr,len)` span math duplicated in mark_code/note_write; `Vm::with_backend` vs `with_backend_host_ram` struct-literal copy; iovec decode ×2; scratch zero-fill ×4.
- [ ] Pre-existing (surfaced reviewing the fixes, NOT regressions): `lock bts/btr/btc [mem], reg` lifts to a non-atomic byte RMW (matches the immediate-form gap — no `has_lock_prefix`→AtomicRmw path for mem-BT); `writev` to a bad/closed fd or a `File`/`PipeWrite` whose host write fails still reports full success (`total += len`) instead of -EBADF / short; `do_read` pipe/stdin consume-then-EFAULT loses drained bytes on an unmapped destination (error path only).
- [ ] Efficiency: `interp.rs:51` zero-fills whole temps scratch per block dispatch (SSA define-before-use makes it unneeded); `fresh_code_pages` builds ~1M `AtomicBool` element-by-element at Reserved startup; `vm.rs:244` recomputes `trap_window` (full region scan) per block materialize.

## How this was produced

8 parallel finder agents (3 correctness + 3 cleanup + altitude + conventions),
dedup, then per-file verifier agents (1-vote, recall-biased). Test command:
`cargo nextest run -E 'not binary(fuzz_robustness)'` (see memory
`x86jit-run-tests`).
