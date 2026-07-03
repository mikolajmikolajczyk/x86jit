# M1 — IR + interpreter + test harness

**Goal:** lift a minimal instruction set to IR, interpret it correctly, and stand up the test harness *alongside* (not after) the interpreter. This is the foundation — the harness is what proves every later milestone.

**Spec:** spec.md §6, §7, §8.1, §12 (M1); testing.md §2–§6, §11 (M1). **Prereq:** M0.

## Build tasks

### Operand lowering — do this BEFORE any per-mnemonic lift (§7.1)

- [ ] **M1-T1** — Central GPR write with **size-dependent semantics**: 32-bit write zeroes upper 32 bits; 16/8-bit writes preserve them. One place, used by `WriteReg` interpretation and (later) codegen. (§7.1, §16 — the #1 silent bug)
- [ ] **M1-T2** — `effective_address(insn, ops, tg)`: emit `base + index*scale + disp`; use iced's RIP-relative value (next-insn base); add FS/GS base when a segment prefix is present. (§7.1, §16)
- [ ] **M1-T3** — `lower_read(insn, op_idx, …) -> Val`: register → `ReadReg`; immediate → `Imm`; memory → `effective_address` + `Load`. (§7.1)
- [ ] **M1-T4** — `lower_write_target(insn, op_idx, …) -> WriteTarget` (`Reg` | `Mem{addr,size}`); for RMW compute the effective address **once** and reuse for Load + Store. (§7.1, §16)

### Lift + block loop (§7.3)

- [ ] **M1-T5** — `lift_block`: iced decode loop; end block at first control-flow op (use iced flow-control info, not a hand list); `LiftError::Unsupported` / `DecodeFault`; fill `IrBlock` (temp_count, guest_len, icount). (§7.3)
- [ ] **M1-T5c** — Decode from `Memory::code_slice(addr, ..)` (iced needs a byte slice, not scalar `read`); emit `IrOp::InsnStart { guest_addr }` at each instruction boundary — required so a mem-trap can set RIP to the faulting instruction (`guest_len` is only the block end). (§6.2, §7.3, §8, §16)
- [ ] **M1-T5b** — Seam discipline (§17): decoder bitness comes from a `CpuMode` value (today only `Long64`), NOT the literal `64`; keep `effective_address` (M1-T2) the *single* place any address is computed. Leave the seams, build no mode machinery (no `trait ExecutionMode`, no `Protected32` API). (§17.3, §17.5, §17.6)
- [ ] **M1-T6** — Per-mnemonic lifts using the lowering helpers: `mov`, `add`, `sub`, `cmp`, `and`/`or`/`xor`, `push`/`pop`, `lea`, `jmp`, `jcc`, `call`, `ret`, plus explicit `load`/`store` forms. (§12 M1)
- [ ] **M1-T7** — Flag computation (Variant A, materialized) using **`FlagMask`, not `bool`** (§6.2): `inc`/`dec` keep CF; logic ops force CF=OF=0; shifts update flags **only when count ≠ 0** (runtime-conditional). iced says *which* flags; you encode *how*. (§3.2, §7, §16)
- [ ] **M1-T7b** — Flags-as-input / flags-as-data ops: `Adc`/`Sbb` (consume CF into the sum) and `GetCond { dst, cond }` (materialize a condition as 0/1 for `setcc`/`cmovcc`/`rcl`/`rcr`). Without these you can't lift `adc`/`sbb`, which appear in every 128-bit add chain glibc/compilers emit. (§6.2, §16)
- [ ] **M1-T7c** — Add the missing single-operand / test ops to the lift + IR: `inc`, `dec`, `neg`, `not`, `test`, `movzx`, `movsx`/`movsxd`, `cdqe`/`cqo`, `setcc`, `cmovcc`. (§16)

### Interpreter (§8.1)

- [ ] **M1-T8** — `Memory::read`/`write` scalar with **`write(&self)`** (interior mutability, `UnsafeCell` + `unsafe impl Sync`) — NOT `&mut self`. Guest RAM is shared across vcpus and written concurrently; `&mut` can't model M7 and forces a signature rewrite. Bounds-check every access → RAM value or `MemTrap` (never panic/UB). (§8 pitfall, §8.1)
- [ ] **M1-T9** — `interpret_block`: execute every `IrOp` over a `temps: Vec<u64>`; take `mem: &Memory` (not `&mut`); track `cur_addr` from `InsnStart` and set `cpu.rip = cur_addr` on any memory trap/exception; return `StepResult`. Wire `execute()` interpreter arm. (§8.1, §16)
- [ ] **M1-T10** — Trap-out + RIP convention: after `syscall` RIP = past the instruction; on memory trap RIP = the faulting instruction. Same rule the JIT will follow. (§8)
- [ ] **M1-T10b** — **Instruction atomicity** (pitfall #0, §16): within one guest instruction, emit all trapping ops (load/store) **before** all committing ops (WriteReg, flags), or prove idempotence — else a fault-retry corrupts state (`push` moving RSP before a faulting store, RMW writing flags before a faulting store). Bake the ordering into the lowering helpers. (§7 pitfall 3)
- [ ] **M1-T11** — `Exit` surface live for M1: `UnknownInstruction`, `Syscall`, `Hlt`, `BudgetExhausted`. (§12 M1)

## Test-harness tasks (build WITH the interpreter — T§11)

- [ ] **M1-T12** — `TestVector` + `CpuSnapshot`/`MemChunk`/`RunSpec`/`Expectation`/`ExpectedExit`; RON (de)serialize, bytes as hex. (T§2, T§3)
- [ ] **M1-T13** — `Oracle` trait + `VectorInput`/`RunOutcome`. (T§4)
- [ ] **M1-T14** — `UnicornOracle` (primary, cross-platform): map snapshot→regs incl. FS/GS base, map memory, run, read back, stop on `hlt` hook or fixed insn count. (T§4)
- [ ] **M1-T14b** — `NativeOracle` (optional, `#[cfg(target_arch = "x86_64")]`): substitute a non-privileged terminator (`int3`+`SIGTRAP` handler, or `ret` trampoline) for the vector's `hlt` — `hlt` faults in user mode. (T§2, T§4)
- [ ] **M1-T15** — `compare(expected, got) -> Option<Divergence>`: precise per-reg / per-flag / per-byte / exit diffs. (T§5)
- [ ] **M1-T16** — Undefined-flag masking (`dont_care_flags` per vector) so differential runs don't chase architecturally-undefined bits. (T§5)
- [ ] **M1-T17** — `capture` CLI: `--asm … --init … --name … --tags … --out …` → assemble (iced encoder) → run through Unicorn → write `.ron` vector. (T§6.1)
- [ ] **M1-T18** — Inline `Vector::asm(...).init(...).expect_via_oracle().run_on::<Interpreter>()` builder. (T§6.2)
- [ ] **M1-T19** — First corpus under `vectors/`: `flags/`, `zero_extend/`, `addressing/`, `shifts/` — cover the M1 instruction set and the §10 checklist classes reachable now. (T§3, T§10)

## Acceptance

- **M1-T20** — Differential: for each vector, `InterpreterOracle` state == `UnicornOracle` state (via `compare`, with masking). Run manually this milestone. (T§11 M1, §12 M1)
- **M1-T21** — Per-instruction unit vectors pass, including edge cases: overflow, zero, sign, upper-32 zeroing, RIP-relative, RMW. (§13, T§10)

## Exit criteria

The interpreter runs the M1 instruction set and matches Unicorn on the whole starter corpus. Harness (`TestVector`, `compare`, `UnicornOracle`, `capture`) exists and is the reusable spine for M2–M5.
