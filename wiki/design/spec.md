# Specification: `x86jit` — an x86-64 → host recompiler as a Rust library

Working draft 0.4 · design document for implementation with Claude Code

> **Changes 0.3 → 0.4 (implementation audit — holes that would break at build time):** IR gains flag-as-data (`GetCond`) and carry-consuming ops (`Adc`/`Sbb`) — without them `adc`/`sbb`/`setcc`/`cmovcc` cannot be lifted; `set_flags: bool` replaced by a `FlagMask` (`inc`/`dec` preserve CF; shift-by-0 leaves all flags untouched — a runtime-conditional update); added pitfall #0 **instruction atomicity** (every potential trap of an instruction must precede its first state commit, or the RIP-retry convention corrupts state — e.g. `push` moving RSP before a faulting store); `complete_mmio_read` redesigned as a **pending value consumed by the retried load** (the old "write into the temp" contradicted temps dying when a block returns, and could never work in the JIT); guest-RAM access contract switched to **interior mutability** (`&Memory` + `UnsafeCell`, manual `Sync`) — shared RAM written by multiple vcpus cannot flow through `&mut` (hits already in M1 as a borrow conflict); backend selection changed from a config enum to an **injected `Backend` trait object** (the core cannot name the downstream JIT crate — dependency direction); §8.2 gains a mandatory **memory-safety strategy for inlined access** (bounds/permission check vs guard pages) — raw `host_base+addr` without checks is host UB on any out-of-range guest address; notes added on budget-vs-block-chaining, the benign double-lift race, and same-block SMC; the native-oracle `hlt` caveat documented in testing.md (hlt is privileged); §14 gains open decisions (#DE representation, TSO tagging, breakpoint mechanism); `enforce_tso: bool` replaced by the three-tier **`MemConsistency`** knob (`Fast` bare STR/LDR · `AcqRel` STLR/LDAPR · `FullTso` STR+DMB) — a per-workload escalation ladder on weak hosts, identical code on x86 hosts, locked ops/`mfence` tier-independent, tier switch = cache flush (§4.1, §8.2.3).
>
> <details><summary>Changes 0.2 → 0.3 (structural review — minimizing surprises)</summary>
>
> **Changes 0.2 → 0.3 (structural review — minimizing surprises):** added section 7.1 **operand lowering** (effective address, memory operand, read-modify-write) — the biggest gap so far; added 7.2 temporaries generator + the `IrBlock.temp_count` field; separated the backend's responsibilities (backend-dependent materialization + uniform `execute` over `CachedBlock`) — removes the false `execute(&IrBlock)`; added 8.2.1–8.2.3 **compiled block ABI** (access to `CpuState` via pointer + offsets, result encoding, M4 build order); fixed `CachedBlock`/`CompiledPtr` with respect to `Send`/`Sync` (pitfall M7) and ownership of code memory; added a dependency map, table of contents, and a consolidated "where the surprises live" section; added section 17 **extensibility points (seams)** with `CpuMode`/`BlockKey` sketches and three `SEAM` markers in the code (decoder bitness, cache key, effective-address helper).
>
> <details><summary>Changes 0.1 → 0.2 (technical review)</summary>
>
> Fixed the contradiction in the backend's return type (`StepResult` instead of a direct `Exit`); split `Exit::Mmio` into `MmioRead`/`MmioWrite` with the correct data direction + `complete_mmio_read`; unified the types in `IrOp::Branch` (both branches static) and clarified `Cond` (signed vs unsigned); rewrote the dispatcher loop into something type-consistent and compilable (lift error → `Exit`, not `?`); added `LiftError`, the `Memory`/`MemTrap` contract, clarified `Flat` vs `map()`; documented two x86-64 semantic pitfalls (zeroing of the upper 32 bits, RIP-relative).
> </details>
> </details>

---

## How to read this document

Sections 1–5 are the **public contract** (what a user of the library sees) — read them first, because they define the boundaries. Sections 6–11 are the **internals** (how it works). Section 12 is the **plan** — implement in milestone order, not section order. Section 16 ("where the surprises live") gathers all the pitfalls in one place — check it before every milestone.

**Dependency map (what must exist before what):**

```
CpuState + Memory (3,4)
      │
      ▼
IR: IrOp + Val + TempGen (6, 7.2)
      │
      ▼
operand lowering (7.1) ──► mnemonic lift (7.3)
      │
      ▼
interpreter (8.1) ◄─── shared StepResult contract (8)
      │                        │
      │                        ▼
      │                 cache + dispatcher (9)
      ▼                        │
differential test (13)         ▼
                        JIT/Cranelift (8.2) ── needs block ABI (8.2.1)
                               │
                               ▼
                        SMC (10) ─► multithreading + TSO (11)
```

Key observation: **operand lowering (7.1) is the foundation of the lift** — without it you can't lift even `mov`, because you can't reduce a memory operand to a `Val`. Build it *before* lifting concrete instructions.

**Table of contents:**
1. Goal and scope · 2. Architecture · 3. Guest state · 4. Input API · 5. Output API · 6. IR · 7. Lift (7.1 operand lowering, 7.2 temps, 7.3 loop) · 8. Backends (8.1 interpreter, 8.2 JIT+ABI) · 9. Cache + dispatcher · 10. SMC · 11. Multithreading · 12. Milestones · 13. Testing · 14. Open decisions · 15. Dependencies · 16. Where the surprises live · 17. Extensibility points (seams)

---

## 1. Goal and scope

`x86jit` is a pure-Rust library that **executes x86-64 code on any host** (x86-64 or ARM64), via JIT recompilation. The core is **guest-agnostic** — it knows nothing about the PS4, ELF, the syscalls of any particular OS, or the GPU. It is a "CPU engine" that receives memory + a starting point and executes instructions, handing back control every time it encounters something it can't handle on its own.

### What is in scope (library core)

- x86-64 decoding (via `iced-x86`).
- Lifting instructions to a custom IR.
- Two backends: **interpreter** (reference, slow, debuggable) and **JIT** (via Cranelift).
- A translation cache keyed by guest address.
- A dispatcher loop.
- Guest memory model (flat buffer + softmmu later).
- Guest CPU state (registers, flags, RIP).
- Return-based output API (`run()` → `Exit`).

### What is OUT of scope (belongs to the user / separate crates)

- Parsing file formats (ELF, SELF, PE) — the user provides an already unpacked memory map.
- Handling the syscalls of a specific OS (HLE) — the user reacts to `Exit::Syscall`.
- MMIO / devices / GPU — the user reacts to `Exit::MmioRead` / `Exit::MmioWrite`.
- Decryption, loaders, higher-level guest thread scheduling.

> **Boundary rule:** the core calls the outside world only through **exit reasons** (`Exit`) and through **Trap-type memory regions**. Everything else (especially access to ordinary guest RAM) is inlined into the generated code and never goes through a callback.

---

## 2. High-level architecture

```
                         ┌─────────────────────────────────────────┐
   x86 bytes (guest RAM) │  iced-x86 ──► lift ──► IR ──► backend    │  ──► host code / execution
                         │                              ├ interpreter│
                         │                              └ Cranelift  │
                         │        translation cache (guest→host)     │
                         │        dispatcher loop                    │
                         └─────────────────────────────────────────┘
                                        ▲            │
                                        │ Exit       │ inline mem access (hot)
                                   user loop     guest RAM buffer
```

### Split into two entities (modeled on KVM: VM vs VCPU)

- **`Vm`** — shared: guest memory + translation cache. One instance per emulated machine.
- **`Vcpu`** — per guest thread: CPU state (registers, flags, RIP) + its own `run()` loop. Shares the `Vm`.

This split opens the road to multithreading from the start (many `Vcpu` over a single `Vm`), even if the first version is single-threaded.

### Workspace layout (proposal)

```
x86jit/
├── x86jit-core/        # Vm, Vcpu, IR, lift, cache, dispatcher, interpreter
├── x86jit-cranelift/   # JIT backend (feature-gated, optional)
├── x86jit-elf/          # OPTIONAL helper: ELF parser → memory map (convenience, not core)
└── x86jit-tests/        # differential testing, instruction corpus, fuzzing
```

---

## 3. Guest state model

### 3.1 Registers

```rust
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Reg {
    Rax, Rbx, Rcx, Rdx, Rsi, Rdi, Rbp, Rsp,
    R8, R9, R10, R11, R12, R13, R14, R15,
    Rip,
    // segment bases — FS/GS are used for TLS (thread-local storage),
    // real programs require this, so they must be present from the start.
    FsBase, GsBase,
}
```

Internally we keep state as a flat struct (not a `HashMap` — this is a hot path):

```rust
pub struct CpuState {
    pub gpr: [u64; 16],     // indexed by x86 order (RAX=0, RCX=1, ...)
    pub rip: u64,
    pub fs_base: u64,
    pub gs_base: u64,
    pub flags: Flags,       // see 3.2
    // XMM/YMM (SIMD) — added in a later milestone, not at the start:
    // pub xmm: [u128; 16],
}
```

> **Note on GPR numbering:** the register order in x86 encoding (RAX, RCX, RDX, RBX, RSP, RBP, RSI, RDI, R8..R15) differs from the "natural" one. `iced-x86` returns a `Register` enum — map it once, in one place, to the `gpr[]` index.

### 3.2 Flags

x86 has arithmetic flags: **CF** (carry), **PF** (parity), **AF** (adjust), **ZF** (zero), **SF** (sign), **OF** (overflow), and **DF** (direction, for string instructions).

Two strategies — the spec assumes the **simple version at the start, lazy as an optimization**:

**Variant A (start): materialized flags.** After every instruction that sets flags, you compute them right away and store them as bits.

```rust
#[repr(C)]   // JIT stores/loads flag fields at stable offsets in CpuState (§8.2.1)
pub struct Flags {
    pub cf: bool, pub pf: bool, pub af: bool,
    pub zf: bool, pub sf: bool, pub of: bool,
    pub df: bool,
}
```

> **`#[repr(C)]` on `Flags` is part of the ABI, not cosmetic.** `CpuState` is `#[repr(C)]` so codegen can address fields by offset (§8.2.1) — but a nested non-`repr(C)` struct has unspecified field layout, so the flag offsets the JIT bakes in would be undefined behavior to rely on. Mark `Flags` `#[repr(C)]` too. One-byte bools are fine to start; a packed RFLAGS-style `u64` (fewer stores per flag update in codegen) is an M4/M5 optimization.

**Variant B (optimization later): lazy flags.** Instead of always computing flags, you store "the last operation and its operands", and you compute a flag only when someone reads it (because most set flags are never read). This is a significant performance gain, but it complicates the IR — **don't do it at the start.**

```rust
// Variant B — sketch, to be implemented only after a working JIT:
pub struct LazyFlags {
    pub last_op: FlagOp,   // Add, Sub, Logic, ...
    pub a: u64, pub b: u64, pub result: u64, pub size: u8,
}
```

---

## 4. Input API (input)

### 4.1 Construction and configuration

```rust
pub struct VmConfig {
    /// Memory model. Start: Flat. Later: SoftMmu.
    pub memory_model: MemoryModel,
    /// Memory-CONSISTENCY tier for generated code on weak hosts (ARM) — three
    /// speed/strictness levels, per-Vm (a "per game" knob). On an x86 host all
    /// tiers emit identical code (native TSO). See §8.2.3.
    pub consistency: MemConsistency,
}

/// How faithfully generated code reproduces x86-TSO ordering on a weak host.
/// Escalation ladder: run Fast; if a multithreaded workload misbehaves, bump one
/// tier. Distinct from `MemoryModel` (address-space layout) — this is ORDERING.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum MemConsistency {
    /// Bare STR/LDR, no barriers. Fastest. Correct ONLY for code that doesn't
    /// synchronize through memory: single-threaded guests, or multithreaded ones
    /// whose threads don't communicate via shared structures.
    Fast,
    /// Stores → STLR, loads → LDAPR (RCpc, ARMv8.3; LDAR fallback pre-8.3).
    /// The standard x86-TSO mapping — covers ~99% of correct multithreaded code.
    /// Residual gap around store-load ordering in practice (see §8.2.3).
    AcqRel,
    /// Full fences: STR+DMB ISH / LDR+DMB ISHLD. Slowest; restores the store-load
    /// ordering AcqRel can miss. The hammer for workloads that still misbehave.
    FullTso,
}

pub enum MemoryModel {
    /// One contiguous host buffer of size `size`, representing the guest
    /// address space [0, size). Address translation = host_base + guest_addr
    /// (one addition — fastest). `map()` in this model only assigns
    /// permissions/type (Ram/Trap) to regions and checks bounds; it does not
    /// allocate separate blocks. Addresses >= size are UnmappedMemory.
    /// Good at the start and when the guest space is dense and fits in host RAM.
    Flat { size: u64 },

    /// Sparse address space via a page table / region map.
    /// `map()` allocates separate pages. Slower address translation (lookup),
    /// but handles scattered, high addresses without allocating the whole thing.
    /// You implement this when Flat stops being enough (e.g. the guest uses
    /// addresses near the top of the 64-bit space).
    SoftMmu,
}
impl Vm {
    /// Default: the interpreter backend (lives in the core).
    pub fn new(config: VmConfig) -> Self;
    /// Inject a backend — this is how the JIT gets in (see the note below).
    pub fn with_backend(config: VmConfig, backend: Box<dyn Backend>) -> Self;
}
```

> **Backend selection is an injected trait object, NOT a config enum.** An earlier draft had `pub enum Backend { Interpreter, Jit }` in `VmConfig` — that cannot work: `x86jit-cranelift` depends on the core, so the core **cannot name or construct** the JIT backend (the dependency points the other way). The `Backend` *trait* (§8) is defined in the core; the interpreter implements it in the core; `x86jit-cranelift` exports a `JitBackend` implementing the same trait, and the user passes it into `Vm::with_backend`. This keeps the JIT crate optional without a feature-flag knot in the core.

> **Flat vs map():** in the `Flat` model, `size` is the total size of the allocated host buffer. `map()` does NOT add memory — it divides the already existing buffer into regions with different permissions and type (Ram/Trap). In `SoftMmu`, `map()` really allocates pages. Keep this distinction clear, because otherwise `map(guest_addr = 0x7fff_0000_0000, ...)` in Flat mode would mean an attempt to allocate 128 TB.

### 4.2 Memory mapping

This is the **main input**: the user (or the ELF/SELF loader) maps segments to guest addresses. The core does not parse any format — it receives ready bytes and addresses.

```rust
pub enum Prot { R, RW, RX, RWX }

pub enum RegionKind {
    /// Ordinary RAM. Access is INLINED into the generated code — no trap-out.
    Ram,
    /// Trapped region. Every access causes Exit::MmioRead / Exit::MmioWrite
    /// (for devices, MMIO).
    Trap,
}

impl Vm {
    /// Reserve a region in the guest address space.
    pub fn map(&mut self, guest_addr: u64, size: usize, prot: Prot, kind: RegionKind)
        -> Result<(), MapError>;

    /// Write content (e.g. an ELF .text/.data segment) into an already mapped region.
    pub fn write_bytes(&mut self, guest_addr: u64, bytes: &[u8]) -> Result<(), MemError>;

    /// Read (for inspection / debugging / HLE reading guest structures).
    pub fn read_bytes(&self, guest_addr: u64, buf: &mut [u8]) -> Result<(), MemError>;

    pub fn unmap(&mut self, guest_addr: u64, size: usize) -> Result<(), MapError>;
}
```

A typical loading flow on the user's side (outside the core):

```rust
// pseudo ELF loader (lives in x86jit-elf or at the user's)
for seg in elf.load_segments() {
    vm.map(seg.vaddr, seg.memsz, seg.prot(), RegionKind::Ram)?;
    vm.write_bytes(seg.vaddr, &seg.data)?;
}
```

### 4.3 Creating a vcpu and setting the initial state

```rust
impl Vm {
    /// Create a new execution context (one per guest thread).
    pub fn new_vcpu(&self) -> Vcpu;
}

impl Vcpu {
    pub fn set_reg(&mut self, reg: Reg, val: u64);
    pub fn reg(&self, reg: Reg) -> u64;
    pub fn set_flags(&mut self, flags: Flags);
    pub fn flags(&self) -> Flags;
}
```

Setting the entry point = simply `set_reg(Reg::Rip, entry)`. Stack: the user allocates a RAM region and sets `Rsp`.

---

## 5. Execution and output API (output)

### 5.1 Return-based model

The core **does not call the user through a trait** in the hot path. Instead, `run()` executes until it encounters an event it can't handle, and **returns a reason**. The user handles the reason in a loop and calls `run()` again.

```rust
impl Vcpu {
    /// Execute guest code until an exit event or exhaustion of the budget.
    /// `budget` = maximum number of executed instructions (or blocks —
    /// see the note below). None = no limit (until the first Exit).
    pub fn run(&mut self, budget: Option<u64>) -> Exit;
}
```

> **The budget** serves cooperative scheduling of many guest threads: you execute N instructions of one vcpu, then switch to another. Without it, one thread in an infinite loop would starve the rest. Decide whether you count instructions or blocks — blocks are cheaper to count (increment once per block, not once per instruction), instructions give finer granularity. Recommendation: count **blocks**, budget in blocks.

### 5.2 Exit-reason enum

```rust
pub enum Exit {
    /// The guest executed a syscall instruction (syscall/sysenter/int 0x80).
    /// The arguments are in guest registers — the user reads them via vcpu.reg().
    /// After handling, the user sets the result register and calls run() again.
    Syscall,

    /// The guest executed a halt instruction (hlt).
    Hlt,

    /// Access to an address that is not mapped.
    UnmappedMemory { addr: u64, access: AccessKind },

    /// READ from a Trap region (MMIO). The guest is WAITING for a value.
    /// The user must supply the result via vcpu.complete_mmio_read(value)
    /// BEFORE the next run(), otherwise the guest state is inconsistent.
    MmioRead { addr: u64, size: u8 },

    /// WRITE to a Trap region (MMIO). The guest SUPPLIES a value.
    /// The user handles the side effect and simply calls run() again.
    MmioWrite { addr: u64, size: u8, value: u64 },

    /// Encountered an instruction that the lift does not yet handle.
    /// Invaluable during development: it tells you exactly what to add to the lift next.
    UnknownInstruction { addr: u64, bytes: [u8; 15], len: u8 },

    /// Hit a breakpoint set by the user (debug).
    Breakpoint { addr: u64 },

    /// Executed `budget` blocks — cooperative yield of control.
    BudgetExhausted,

    /// Internal error (e.g. corrupted state, inconsistent cache).
    Fault(FaultKind),
}

pub enum AccessKind { Read, Write, Execute }
```

> **Why MmioRead/MmioWrite are separated:** on an MMIO read, data flows FROM the user TO the guest (the guest wrote the address into the destination register and is waiting for a value); on a write — from the guest to the user. A single `value` field does not model both directions. A read additionally requires a mechanism for injecting the result:

```rust
impl Vcpu {
    /// Called after Exit::MmioRead: supplies the value the guest "read".
    /// Stored as a PENDING VALUE for (addr, size); the retried load consumes it.
    /// See the resume mechanism below.
    pub fn complete_mmio_read(&mut self, value: u64);
}
```

> **MMIO-read resume mechanism — pending value, NOT "write into the temp".** A naive contract ("the value lands in the destination temp, then run() resumes") cannot work: temps live in the executing block's local storage and **die the moment the block returns** with `Exit::MmioRead` — and a compiled JIT block cannot be re-entered mid-way at all. The working design, consistent with the RIP convention (§8):
>
> 1. On a load from a Trap region, the backend sets **RIP = the faulting instruction** and returns `Exit::MmioRead { addr, size }`. Per the instruction-atomicity rule (§7 pitfall 3), nothing of that instruction has committed yet.
> 2. `complete_mmio_read(value)` stores `(addr, size, value)` as a **pending MMIO value** on the `Vcpu`.
> 3. The next `run()` resumes at the faulting instruction — the dispatcher lifts a block starting there (any address can start a block, so partial-block progress is fine). The re-executed `Load` hits the Trap region again, the memory layer sees a matching pending value, **consumes it and returns it as the load result** instead of trapping. Execution flows on.
>
> This works identically in the interpreter and the JIT (the retried load is a fresh execution in both), costs one `Option` check on the Trap-slow-path only, and never requires resuming a half-executed block.

> **Note on MMIO vs JIT:** in generated code, ordinary RAM is inlined, but access to a Trap region must leave the block. This complicates codegen (the JIT must generate a check "is this address a Trap" or know it statically). At the start (interpreter) this is trivial. With the JIT, consider: either a runtime check on accesses that may hit a Trap, or keeping MMIO out of the hot path and accepting that accesses to unknown addresses go the slow route. Resolve this in M4 together with the inline-access safety check (§8.2.3) — one range/permission check can serve both.

### 5.3 Inspecting and modifying state between calls

After `run()` (and before the next one) the user reads/writes the guest state — this is how syscall HLE reads arguments and writes the result:

```rust
// example of handling the "write" syscall (Linux x86-64: nr in RAX, args in RDI/RSI/RDX)
match cpu.run(Some(100_000)) {
    Exit::Syscall => {
        let nr  = cpu.reg(Reg::Rax);
        let fd  = cpu.reg(Reg::Rdi);
        let buf = cpu.reg(Reg::Rsi);
        let len = cpu.reg(Reg::Rdx);
        let ret = host_handle_syscall(nr, fd, buf, len, &vm);
        cpu.set_reg(Reg::Rax, ret);      // result back to the guest
        // the loop returns to run()
    }
    Exit::UnknownInstruction { addr, bytes, len } => {
        panic!("no lift for instruction @ {addr:#x}: {:02x?}", &bytes[..len as usize]);
    }
    other => { /* ... */ }
}
```

---

## 6. IR (intermediate representation)

The IR is **custom** — it is the layer "smart about x86 semantics" that you either interpret or translate to Cranelift. It is what lets you have an interpreter *and* a JIT behind the same frontend.

### 6.1 Value model

The IR is a list of operations on **temporary values** (temporaries) — three-address code, not a tree. Each operation produces a value, subsequent ones consume it.

```rust
pub type Temp = u32;

pub enum Val {
    Temp(Temp),   // result of an earlier operation
    Imm(u64),     // constant
}
```

### 6.2 IR operations (sketch — you extend it as you lift)

```rust
/// Which flags an operation updates. x86 is NOT "all or nothing":
/// inc/dec update everything EXCEPT CF; and/or/xor force CF=OF=0 and leave AF
/// undefined; shifts touch flags ONLY when count != 0 (runtime-conditional!).
/// A plain bool cannot express any of that.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct FlagMask(pub u8);   // bit per flag: CF|PF|AF|ZF|SF|OF
impl FlagMask {
    pub const NONE: FlagMask;        // e.g. mov, lea
    pub const ALL: FlagMask;         // add, sub, cmp
    pub const ALL_BUT_CF: FlagMask;  // inc, dec
    // logic ops: ALL, but the backend forces CF=OF=0 per the op's rules
}

pub enum IrOp {
    // --- instruction boundary marker ---
    // Emitted by the lift at the START of each guest instruction. Carries the
    // guest address so a backend can set cpu.rip to the FAULTING instruction on a
    // memory trap or exception (§8). Without it, RIP-on-trap has no address —
    // guest_len gives only the block END (right for syscall, wrong for a mid-block
    // fault). Interp: hold it in a `cur_addr` local. JIT: bake it as a const for
    // the following trapping accesses. Also delimits instructions for SMC.
    InsnStart { guest_addr: u64 },

    // --- data movement ---
    ReadReg  { dst: Temp, reg: Reg },   // Reg::Rip is FORBIDDEN here — see the note below
    WriteReg { reg: Reg, src: Val },

    // --- arithmetic / logic (size: 1,2,4,8 bytes) ---
    Add { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    Sub { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    // Adc/Sbb CONSUME CF as an input (a + b + CF): flags can't be add-then-add,
    // the carry chain and OF/CF must be computed over the full three-operand sum.
    Adc { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    Sbb { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    And { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    Or  { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    Xor { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    // Shifts: flag update happens ONLY if the (masked) count != 0 — the backend
    // implements the runtime condition (interp: an if; JIT: a conditional).
    Shl { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    Shr { dst: Temp, a: Val, b: Val, size: u8, set_flags: FlagMask },
    // ... Mul, Div, Neg, Not, Sar, Rol, Ror, etc.

    // --- flags as DATA (not only as branch conditions) ---
    // setcc, cmovcc, adc/sbb lowering, rcl/rcr all need to READ flags into a value.
    // GetCond materializes a condition as 0/1. (CF alone = GetCond(Below).)
    GetCond { dst: Temp, cond: Cond },

    // --- memory ---
    Load  { dst: Temp, addr: Val, size: u8 },
    Store { addr: Val, src: Val, size: u8, order: MemOrder },

    // --- control: EACH of these operations ENDS the block ---
    // Note: for jmp/jcc to a constant target, addresses are KNOWN statically at
    // lift time (Imm). For indirect jmp (jmp rax, ret) the target is dynamic (Temp).
    Jump   { target: Val },                                 // jmp (direct: Imm, indirect: Temp)
    Branch { cond: Cond, taken: u64, fallthrough: u64 },    // jcc — BOTH branches are known addresses
    Call   { target: Val, return_addr: u64 },               // return_addr = address after call (to push on stack)
    Ret,                                                    // dynamic target — from the stack
    Syscall,                                                // trap-out to the user
    Hlt,
}

pub enum MemOrder { None, Release, Acquire }  // for TSO barriers

// NOTE — ReadReg(Rip) is forbidden in the IR: cpu.rip is updated only at block
// end, so a mid-block read would see a stale value. Everything that "reads RIP"
// (RIP-relative addressing, call's return address) is known statically at lift
// time and lowered to an Imm. Assert this in the lift.

// jcc conditions. They correspond to flag combinations, NOT to "sign" abstractly —
// x86 distinguishes signed comparisons (l/g: SF,OF,ZF) and unsigned ones (b/a: CF,ZF).
pub enum Cond {
    Eq, Ne,               // ZF        (je/jne)
    Below, BelowEq,       // CF, CF|ZF (jb/jbe — unsigned)
    Above, AboveEq,       // !CF&!ZF   (ja/jae — unsigned)
    Less, LessEq,         // SF!=OF    (jl/jle — signed)
    Greater, GreaterEq,   // SF==OF    (jg/jge — signed)
    Sign, NoSign,         // SF        (js/jns)
    Overflow, NoOverflow, // OF        (jo/jno)
    Parity, NoParity,     // PF        (jp/jnp)
}
```

### 6.3 IR block

```rust
pub struct IrBlock {
    pub guest_start: u64,   // guest address (key in the cache)
    pub ops: Vec<IrOp>,     // operations, the last always ends the block (Jump/Branch/Ret/...)
    pub temp_count: u32,    // how many temporaries were allocated (the backend reserves that many slots)
    pub guest_len: u32,     // how many guest bytes the block spanned (for SMC invalidation)
    pub icount: u32,        // how many x86 instructions (for budget/statistics)
}
```

---

## 7. Lift: x86 → IR

The lift is a `match` on the mnemonic from `iced-x86`, producing IR operations. Use `InstructionInfo` from iced (registers read/written, flags touched, control-flow classification) — it shortens the lift and tells you directly when to close the block.

### 7.1 Operand lowering — READ THIS BEFORE YOU START THE LIFT

> **This is the biggest source of surprises during implementation.** The IR examples in section 6 show operands as ready `Val`s — that's a simplification. In reality an x86 operand has three forms, and one of them (memory) itself requires several IR operations. The lift MUST have an "operand lowering" layer *beneath* the mnemonic lift, otherwise `Add`/`Mov`/the rest won't come together.

An x86 operand is one of three things:

1. **Register** — `rax`. Lowering: `ReadReg` → `Temp`.
2. **Immediate** — `0x42`. Lowering: `Val::Imm`.
3. **Memory** — `[rbx + rcx*4 + 0x10]`. Lowering: compute the **effective address** (this is arithmetic: `base + index*scale + disp`), then `Load`. This is several IR operations per one operand.

That's why the lift of each instruction is really **two levels**: (a) reduce the operands to `Val` (lowering), (b) generate the operation on those `Val`s. Design two helper functions and use them everywhere:

```rust
/// Reduce a SOURCE operand to a Val (reads a register / immediate / loads from memory).
/// For a memory operand it emits the effective-address computation + Load.
fn lower_read(insn: &Instruction, op_idx: u32, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Val;

/// Reduce a DESTINATION operand to a "write location" (register or memory address).
/// Returns a handle you later use to write the result (WriteReg or Store).
fn lower_write_target(insn: &Instruction, op_idx: u32, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> WriteTarget;

enum WriteTarget {
    Reg(Reg),
    Mem { addr: Val, size: u8 },   // address already computed as a Val
}
```

**The effective address** is a separate helper — called by `lower_read`/`lower_write_target` when the operand is memory:

```rust
/// Emits IR operations computing base + index*scale + disp, returns a Temp with the address.
/// iced gives the components: memory_base(), memory_index(), memory_index_scale(),
/// memory_displacement(). Watch out for RIP-relative (see pitfall no. 2) and for the
/// FS/GS segment base (TLS) — if the instruction has a segment prefix, add fs_base/gs_base.
///
/// SEAM (§17.5): this is the ONLY place a memory address is computed. No lift computes
/// an address itself. Today in Long64 we skip segment bases except FS/GS; if a 32-bit
/// mode were added, segmentation would be added HERE (one function), not in every lift.
fn effective_address(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) -> Val;
```

**Read-modify-write** — instructions such as `add [mem], rax` read the memory operand, modify it, and write it back. The pattern: `effective_address` (once!) → `Load` → operation → `Store` to the same address. **Compute the effective address only once** and use it for both Load and Store — don't compute it twice (it would be a bug if the address depended on something that changes, and wasteful).

Example — the full lift of `add rax, [rbx+8]` (destination operand register, source memory):

```rust
fn lift_add(insn: &Instruction, ops: &mut Vec<IrOp>, tg: &mut TempGen) {
    let dst = lower_write_target(insn, 0, ops, tg);   // op0 = rax → WriteTarget::Reg(Rax)
    let a   = lower_read(insn, 0, ops, tg);           // read the current value of rax
    let b   = lower_read(insn, 1, ops, tg);           // op1 = [rbx+8] → effective_address + Load
    let res = tg.fresh();
    let size = operand_size(insn);                    // 8 for rax
    ops.push(IrOp::Add { dst: res, a, b, size, set_flags: true });
    // write the result to the destination:
    match dst {
        WriteTarget::Reg(r)          => ops.push(IrOp::WriteReg { reg: r, src: Val::Temp(res) }),
        WriteTarget::Mem { addr, size } => ops.push(IrOp::Store { addr, src: Val::Temp(res), size, order: MemOrder::None }),
    }
}
```

> You can see why without this layer the lift wouldn't work: `add` is not "add two Vals", but "reduce both operands (perhaps loading from memory via a computed address), add, write the result to the destination (perhaps writing to memory)". This structure repeats for ALL two-operand instructions — write it once, use it everywhere.

### 7.2 Allocating temporaries

`Temp` is simply an increasing counter within the block. A simple generator:

```rust
pub struct TempGen(u32);
impl TempGen {
    fn new() -> Self { TempGen(0) }
    fn fresh(&mut self) -> Temp { let t = self.0; self.0 += 1; t }
    fn count(&self) -> u32 { self.0 }   // how many temps were allocated (the backend reserves that many slots)
}
```

The interpreter keeps `temps: Vec<u64>` of size `count()`. The JIT maps `Temp → cranelift Value` (see 8.2). Temps are local to the block — you reset `TempGen` for each block.

### 7.3 Skeleton of the block lift loop

```rust
pub fn lift_block(vm: &Vm, start: u64) -> Result<IrBlock, LiftError> {
    let mut decoder = /* iced Decoder set to guest bytes from `start`. SEAM (§17.3):
                         bitness from CpuMode (mode.bits()), NOT the literal 64 — today always Long64. */;
    let mut ops = Vec::new();
    let mut tg = TempGen::new();
    let mut icount = 0u32;
    let mut guest_len = 0u32;
    loop {
        let insn = decoder.decode();
        if insn.is_invalid() { return Err(LiftError::DecodeFault { addr: /* current PC */ }); }
        icount += 1;
        guest_len += insn.len() as u32;
        // Mark the instruction boundary so a mem-trap / exception can set RIP to
        // THIS instruction (§8). Every per-mnemonic lift below emits its trapping
        // ops after this marker.
        ops.push(IrOp::InsnStart { guest_addr: insn.ip() });
        // tg is NOT reset between instructions in the block — temps grow through the whole block.
        match insn.mnemonic() {
            Mnemonic::Mov => lift_mov(&insn, &mut ops, &mut tg),
            Mnemonic::Add => lift_add(&insn, &mut ops, &mut tg),
            Mnemonic::Jmp => { lift_jmp(&insn, &mut ops, &mut tg); break; }   // end of block
            Mnemonic::Je | Mnemonic::Jne /* ... */ => { lift_jcc(&insn, &mut ops); break; }
            Mnemonic::Call => { lift_call(&insn, &mut ops, &mut tg); break; }
            Mnemonic::Ret  => { ops.push(IrOp::Ret); break; }
            Mnemonic::Syscall => { ops.push(IrOp::Syscall); break; }
            // NOTE: the catch-all arm must consult iced's flow-control info BEFORE
            // returning Unsupported — otherwise a rarer jump variant not listed above
            // falls through to Unsupported and a trailing `if is_flow_control` check
            // after the match would be dead code (an earlier draft had that bug).
            _ if is_flow_control(&insn) => { lift_flow(&insn, &mut ops, &mut tg)?; break; }
            _ => return Err(LiftError::Unsupported {
                addr: insn.ip(),
                bytes: copy_insn_bytes(&insn),   // 15-byte buffer + len
                len: insn.len() as u8,
            }),
        }
    }
    Ok(IrBlock { guest_start: start, ops, temp_count: tg.count(), guest_len, icount })
}

// Lift errors — mapped in the dispatcher to an Exit, NOT to a panic.
pub enum LiftError {
    /// Instruction recognized by iced, but the lift does not handle it yet.
    Unsupported { addr: u64, bytes: [u8; 15], len: u8 },
    /// Can't even be decoded (e.g. bytes outside mapped memory, garbage).
    DecodeFault { addr: u64 },
}
```

> **Block-closing rule:** a block ends at the first instruction changing control flow (jmp/jcc/call/ret/syscall/hlt). `iced-x86` provides info about control flow — use it instead of a manual list of opcodes.

> **Flag semantics are the essence of the lift.** `Add` with `set_flags: FlagMask::ALL` must generate the computation of the six flags per the ADD rules. This is the place where you actually encode "what each x86 instruction means". iced tells you *which* flags are touched; *how* to compute them — that's your lift. Remember the non-uniform cases baked into `FlagMask` (§6.2): `inc`/`dec` keep CF, logic ops force CF=OF=0, shifts update flags only when the masked count ≠ 0.

> **Pitfall no. 1 when porting x86-64 — a write to a 32-bit register ZEROES the upper 32 bits.** `mov eax, ...` (or any operation on `eax`) sets the whole `rax`, zeroing bits 32–63. But an operation on `ax` (16-bit) or `al` (8-bit) preserves the higher bits. This is NOT symmetric and is a source of subtle bugs. Your `WriteReg`/interpreter must respect this depending on the size of the destination operand. Encode this rule once, centrally, in the write to a GPR — don't scatter it across the lifts of individual instructions.

> **Pitfall no. 2 — RIP-relative addressing.** x86-64 has a RIP-relative addressing mode (`lea rax, [rip+0x1234]`). iced gives it to you computed (`insn.ip_rel_memory_address()` / `memory_displacement`), but you must use the address of the *next* instruction as the base, not the current one. iced computes this — just use its value, don't compute it by hand.

> **Pitfall no. 3 — instruction atomicity vs the RIP-retry convention (silent state corruption).** The trap-out convention (§8) says: on a memory fault, RIP points at the *faulting instruction* so the user can map/handle and retry. Retry means **re-executing the instruction from scratch** — so **no effect of that instruction may commit before its last potential trap**. The lift must order the emitted IR accordingly:
>
> - `push rax` must NOT be lowered as "WriteReg rsp-8, then Store" — if the Store faults, RSP has already moved and the retry corrupts the stack. Lower as: compute the new-RSP temp → `Store` (may trap) → only then `WriteReg rsp`.
> - RMW (`add [mem], rax`): the `Store` can trap — flags and any register write must be emitted *after* it, or recomputed on retry (they are, since the whole instruction re-executes — as long as nothing was committed).
> - General rule: within one guest instruction, emit **all ops that can trap (loads/stores) before all ops that commit state (WriteReg, flag updates)** — or prove the commit is idempotent under re-execution.
>
> Partial-block progress is fine (each *completed* instruction's effects are committed as the block runs, and a retried block simply starts at the faulting instruction — any address can start a block). The invariant is strictly *intra*-instruction. Violations are invisible until a fault-retry actually happens, then corrupt state at a distance — encode the discipline in the lowering helpers, and fuzz with faulting addresses (testing.md).

---

## 8. Backends

Both backends consume an `IrBlock`. A shared interface.

> **Important — the return type.** The backend does NOT return `Exit` directly. It must distinguish two cases: (a) the block ended normally and execution should flow on (it gives a new RIP), (b) the block traps out to the user (`Exit`). That's why we introduce `StepResult`. This resolves the contradiction where the dispatcher loop must know whether to continue or to return.

> **Important — two different responsibilities, not one `execute`.** The backend does not have a single "execute" method, because the interpreter and the JIT execute *different things*: the interpreter walks the `IrBlock`, the JIT jumps into a compiled `code_ptr`. We split this into:
> - **materialization** (backend-dependent): from an `IrBlock` make a `CachedBlock` — the interpreter wraps it in an `Arc`, the JIT compiles it to host code;
> - **execution** (uniform): the dispatcher `match`es on the `CachedBlock` variant and either interprets or jumps into the code.
>
> Thanks to this there is no false `execute(&IrBlock)`, which makes no sense for the JIT (after materialization the JIT no longer touches the `IrBlock`). The return type in both cases is `StepResult`.

```rust
pub enum StepResult {
    /// The block executed to the end (jump/ret/branch resolved internally).
    /// The new RIP is already stored in CpuState; execution should flow on.
    Continue,
    /// The block traps out to the user. Execution stopped; RIP per the convention below.
    Exit(Exit),
}

/// The backend materializes IR into an executable form. This is the only backend-dependent
/// operation. Injected into the Vm as a `Box<dyn Backend>` (§4.1): the interpreter impl lives
/// in the core, the JIT impl in x86jit-cranelift.
pub trait Backend {
    fn materialize(&mut self, ir: &IrBlock) -> CachedBlock;
}

/// Execution is uniform — the dispatcher calls it on a CachedBlock regardless of the backend.
/// NOTE: `mem` is `&Memory`, NOT `&mut Memory` — guest RAM uses interior mutability
/// (see the pitfall below). Stores go through `&self` methods.
fn execute(block: &CachedBlock, cpu: &mut CpuState, mem: &Memory) -> StepResult {
    match block {
        CachedBlock::Interpreted(ir) => interpret_block(ir, cpu, mem),
        CachedBlock::Compiled { entry, .. } => {
            // compiled block ABI — see 8.2.
            unsafe { run_compiled(*entry, cpu, mem) }
        }
    }
}
```

> **Pitfall — guest RAM cannot be `&mut Memory` (hits in M1, not M7).** A store needs to *write* guest RAM, so the obvious signature is `&mut Memory`. But: (a) the dispatcher reads the cache from the same `Vm` while a block executes, and (b) in M7 **multiple vcpus write the same RAM concurrently** — that's what real hardware does, and `&mut` (exclusive) cannot model it at all. Start with `&mut Memory` and you rewrite every backend signature at M7. Decide it now: the guest RAM buffer uses **interior mutability** — `UnsafeCell<[u8]>` (or a raw base pointer) behind `&Memory`, with `read(&self, …)`/`write(&self, …)` and a hand-written `unsafe impl Sync`. Document the contract: concurrent guest stores race exactly like real hardware, and the TSO barriers (§8.2.3, §11) — not Rust's `&mut` — provide the ordering the guest expects. This is the one place the core is deliberately `unsafe`; wrap it tightly and keep `CpuState` (`&mut`, per-vcpu) separate from `Memory` (`&`, shared).

> **RIP semantics on trap-out (to be resolved and held consistently):** when a block traps out on `syscall`, RIP should point to the instruction *after* `syscall` (because after handling you resume past it). When it traps out on `UnmappedMemory`/`Mmio`, RIP points to the instruction *causing* the access (because the user may want to retry it after mapping/handling). Write down this convention and stick to it in both backends — otherwise the JIT and the interpreter will diverge on trap-outs.

### 8.1 Interpreter (build FIRST)

Walks the list of `IrOp` in a loop and executes each operation with native Rust code. Slow, but simple, fully debuggable, and on an x86 host comparable with native execution (oracle).

Memory-access contract (used by both backends in the interpreter version and by the trap-out):

```rust
pub enum MemTrap { Unmapped, Mmio }

impl Memory {
    // Both take &self — guest RAM is interior-mutable (see §8 pitfall). `write`
    // does NOT need &mut, which is what lets one Memory be shared across vcpus.
    pub fn read(&self,  addr: u64, size: u8) -> Result<u64, MemTrap>;
    pub fn write(&self, addr: u64, val: u64, size: u8) -> Result<(), MemTrap>;

    // The lift (§7.3) decodes from a byte SLICE, not scalar reads — iced needs
    // contiguous bytes. Returns up to `max_len` bytes from `addr`. Flat: a
    // subslice; SoftMmu: capped at the page boundary (don't cross pages silently).
    pub fn code_slice(&self, addr: u64, max_len: usize) -> Result<&[u8], MemTrap>;
}
```

```rust
// `cur_addr` tracks the current guest instruction so a trap can set RIP to it.
IrOp::InsnStart { guest_addr } => { cur_addr = *guest_addr; }
IrOp::Add { dst, a, b, size, set_flags } => {
    let va = read_val(a, &temps);
    let vb = read_val(b, &temps);
    let res = va.wrapping_add(vb) & mask(size);
    temps[*dst] = res;
    // FlagMask, not bool: update only the flags in the mask (§6.2).
    compute_add_flags(va, vb, res, *size, *set_flags, &mut cpu.flags);
}
IrOp::Store { addr, src, size, .. } => {
    // On a trap, RIP = the faulting instruction (cur_addr, from InsnStart) — NOT
    // block.guest_start + guest_len (that's the block end). Retry re-executes it.
    match mem.write(read_val(addr, &temps), read_val(src, &temps), *size) {
        Ok(())            => {}
        Err(MemTrap::Unmapped) => { cpu.rip = cur_addr; return StepResult::Exit(Exit::UnmappedMemory { .. }); }
        Err(MemTrap::Mmio)     => { cpu.rip = cur_addr; return StepResult::Exit(Exit::MmioWrite { .. }); }
    }
}
IrOp::Jump { target } => {
    cpu.rip = read_val(target, &temps);   // store the new RIP in the state
    return StepResult::Continue;          // the dispatcher loop will pick up from the new RIP
}
IrOp::Syscall => {
    cpu.rip = block.guest_start + block.guest_len as u64;  // past the syscall instruction
    return StepResult::Exit(Exit::Syscall);
}
```

### 8.2 JIT via Cranelift (build SECOND)

#### 8.2.1 Compiled block ABI and access to guest state — READ BEFORE M4

> **This is the black box that has to be opened before you write the JIT backend.** The "baked-in base address" from section 1 is not enough — you must decide exactly *how* the generated host code reads and writes guest registers and memory. Without this you won't move.

The model (standard for JITs): **the guest register file is a struct in host memory, and the compiled block receives a pointer to it.** The block does not keep guest registers in host registers permanently — it reads/writes them as fields of the `CpuState` struct at known offsets. Cranelift allocates host registers only for *temporaries within the block*.

The compiled block has a fixed signature (ABI). Proposal:

```rust
/// The signature of every compiled block. All blocks have THE SAME signature,
/// so that the dispatcher can jump into them uniformly.
type CompiledFn = unsafe extern "C" fn(
    cpu: *mut CpuState,   // pointer to the guest register file
    mem: *mut MemCtx,     // memory context (host_base of the guest buffer + metadata)
) -> u64;                 // encoded StepResult/Exit — see below
```

Inside the block:
- **`ReadReg { reg }`** → Cranelift emits a `load` from `cpu + offset_of(reg)`.
- **`WriteReg { reg, .. }`** → `store` to `cpu + offset_of(reg)` (with the upper-bit zeroing rule for a 32-bit write!).
- **`Load { addr, size }`** (RAM) → compute `mem.host_base + guest_addr`, `load`. Inline, no callback.
- **`Store`** analogously.
- **`InsnStart { guest_addr }`** → no code emitted; remember `guest_addr` as a compile-time const so the trapping accesses that follow can store it to `cpu.rip` before returning an `Exit`.
- **End of block** (`Jump`/`Branch`/`Ret`) → store the new RIP to `cpu.rip`, return the "Continue" code.
- **`Syscall`/access to Trap/exception** → store RIP per the convention (past `syscall`; the remembered `InsnStart` address for a fault), return the code of the appropriate `Exit`.

> **The offsets of `CpuState` fields** must be stable and known in codegen. Use `#[repr(C)]` on `CpuState`, compute the offsets once (via `core::mem::offset_of!` or constants) and pass them to the backend. This ties the struct layout to the generated code — document it as a contract.

#### 8.2.2 Block result encoding

The compiled block returns a `u64`, because `extern "C"` won't conveniently return an enum. Define an encoding: e.g. the value `0` = `Continue`, and nonzero values encode an `Exit` variant (the type in the upper bits, data such as the syscall number/address in the lower bits — or write the details to a field in `CpuState`/`MemCtx` and return only the discriminator). `run_compiled` from section 8 decodes this `u64` back into a `StepResult`. Keep this encoding in one place, shared between codegen and `run_compiled`.

#### 8.2.3 Translating IR operations to Cranelift

The same `match` on `IrOp`, but instead of *executing*, it **describes operations to the Cranelift builder**, which generates host code. Cranelift does register allocation and instruction emission per-target (x86-64 on a desktop, ARM64 on a Mac — from the same IR). You build the `Temp → cranelift Value` map per-block (a vector of size `ir.temp_count`).

```rust
IrOp::Add { dst, a, b, .. } => {
    let va = clif_val(a, &mut builder, &temp_map);   // Temp→Value or iconst for Imm
    let vb = clif_val(b, &mut builder, &temp_map);
    let res = builder.ins().iadd(va, vb);            // does NOT compute — describes
    temp_map[*dst] = res;
    // flags: emit Cranelift operations computing CF/ZF/SF/OF and store to the flag fields in CpuState
}
IrOp::ReadReg { dst, reg } => {
    let off = reg_offset(*reg);
    let v = builder.ins().load(types::I64, MemFlags::trusted(), cpu_ptr, off);
    temp_map[*dst] = v;
}
```

**Guest memory access must be inlined** — the JIT emits direct `load`/`store` to the guest buffer (`mem.host_base + guest_addr`), NOT a callback. Only `Trap` regions and syscalls cause a trap-out. This is the "thick for the rare, thin for the hot" boundary from section 1.

> **Inline access needs a safety strategy — this is a zero-th-class M4 decision, not a detail.** `host_base + guest_addr` with `MemFlags::trusted()` and no check is **host-process UB** the moment the guest computes an address outside `[0, size)` (a guest bug, a wild pointer, an attack) — it reads/writes outside the host `Vec`, corrupting the emulator. `Exit::UnmappedMemory` exists in the API but *inlined code has no way to reach it* unless you emit something. Pick one, up front:
> - **Explicit bounds/permission check** (recommended start): before each inlined access, emit `cmp guest_addr, size` + a branch to a slow-path stub that returns `Exit::UnmappedMemory`/`MmioRead`/`MmioWrite`. Cheap (a predictable branch), portable, and the *same* check routes Trap/MMIO out — so it does double duty with §5.2. A per-page permission bitmap (R/W/X, Ram/Trap) makes it one lookup.
> - **Guard pages + SIGSEGV handler** (qemu-style): map the guest space with `PROT_NONE` guards, catch the fault, translate the host fault PC back to a guest exit. Fast (no per-access branch) but heavy, deeply `unsafe`, and platform-specific — defer.
> - **Reserve the full space** (only viable for a 32-bit guest, where 4 GB is mmap-able): no per-access check at all. Not an option for a 64-bit guest.
>
> Corollary: in `Flat`, guest address `0` is a valid buffer offset, so a guest null-deref does **not** fault unless a per-page permission bit says "unmapped". If you want faithful `#GP`/`#PF` on null, you need that bitmap — the "one addition" hot path silently assumed checks that must actually exist.

**Memory-consistency tiers (`MemConsistency`, §4.1):** the guest assumes x86-TSO; a weak host (ARM) doesn't give it. Three tiers trade speed for strictness — how ordinary guest loads/stores are emitted on an ARM host:

| Tier | Ordinary store | Ordinary load | Speed | Correct for |
|------|---------------|---------------|-------|-------------|
| `Fast` | `STR` | `LDR` | fastest | code that never synchronizes through memory: single-threaded guests, or threads that don't share mutable structures |
| `AcqRel` | `STLR` | `LDAPR` (RCpc, ARMv8.3 `FEAT_LRCPC`; fallback `LDAR` pre-8.3 — stronger, slightly slower) | fast | ~99% of correct multithreaded code — the standard x86-TSO mapping |
| `FullTso` | `STR` + `DMB ISH` | `LDR` + `DMB ISHLD` | slowest | workloads that still misbehave under `AcqRel` — brute-force fences restore the store-load ordering it can miss |

On an **x86 host all three tiers emit identical code** (native TSO — barriers are free) — the knob only exists for weak hosts. Usage model is an **escalation ladder, per game/workload**: start `Fast`; a multithreaded guest glitches → `AcqRel`; still glitches → `FullTso`. This mirrors what mature translators do in the field (Box64's `STRONGMEM` levels, FEX's RCpc use).

> **Theory vs practice on the `AcqRel` "gap".** With `LDAPR` (RCpc) the STLR/LDAPR mapping is *theoretically* the exact x86-TSO mapping — RCpc was added to ARMv8.3 precisely to permit the store→load reordering TSO allows while keeping everything else ordered. The `FullTso` tier exists because practice is messier than theory: pre-8.3 fallbacks, mixed-size accesses, unaligned/store-forwarding corner cases, and any future selective-tagging optimization all open real-world gaps. `FullTso` is over-strong (approaches sequential consistency) and slow — it's the diagnostic hammer and the last resort, not the default.

> **What the tier does NOT govern:** locked instructions (`lock xadd`, `xchg`, `lock cmpxchg`) and explicit fences (`mfence`) always translate to real atomics / full barriers (`DMB ISH`), in **every** tier including `Fast` — they are explicit synchronization, not ordinary accesses. The tier governs ordinary loads/stores only. (`MemOrder` on an IR op is the per-op channel for these explicit cases; the tier is the blanket policy for `MemOrder::None` accesses, applied at codegen time.)

> **Switching tiers = flushing the translation cache.** Barriers are baked into compiled blocks, so a block compiled under `Fast` is wrong under `FullTso`. The tier is per-`Vm` config; if you ever make it switchable at runtime, the switch must invalidate the whole cache (and it's the cheap, correct way to implement it — don't try to key the cache by tier). Decide the tagging mechanics (lift-tags vs codegen-applies-blanket) in M7 — the lift sketches emit `MemOrder::None` today on purpose, which makes codegen-applies-blanket the natural fit.

> **M4 build order:** first just the offsets + ABI + "a block that only returns Continue with a new RIP" (without real operations) — verify that the dispatcher jumps into it and returns. Then add the translation of `IrOp` one by one, validating each against the interpreter (oracle). Don't write the whole backend at once.

---

## 9. Translation cache and dispatcher

### 9.1 Cache

Key = guest address. Value = the translated block (for the JIT: a pointer to executable host code; for the interpreter: an `IrBlock` to walk in a loop).

```rust
pub struct TranslationCache {
    // Shared between vcpus (multithreading) — hence the synchronization.
    // SEAM (§17.4): the key is u64 (guest address). If processor modes ever get
    // added, switch to BlockKey { guest_addr, mode } — today mode is always Long64.
    map: RwLock<HashMap<u64, CachedBlock>>,
}

#[derive(Clone)]
pub enum CachedBlock {
    Interpreted(Arc<IrBlock>),
    Compiled {
        entry: CompiledPtr,   // pointer to host code — see the note on Send/Sync
        guest_len: u32,
    },
}
```

> **Send/Sync — pitfall M7 (multithreading).** A raw pointer `*const u8`/`fn` is **not `Send + Sync`**, so a `CachedBlock` with a bare pointer CANNOT be put into a cache shared between threads behind an `Arc<TranslationCache>` — the compiler will reject it only in M7, when you add threads, and this will be surprising, because single-threaded it worked. Solve it right away in M4: wrap the pointer in a type for which you manually attest safety (the code is immutable after compilation and executable from all threads):
> ```rust
> #[derive(Copy, Clone)]
> pub struct CompiledPtr(pub *const u8);
> // Safe: compiled code is read-only and executable from any thread;
> // its lifetime is guaranteed by the owner of the code memory (the JIT arena lives as long as the Vm).
> unsafe impl Send for CompiledPtr {}
> unsafe impl Sync for CompiledPtr {}
> ```
> The interpreter (`Arc<IrBlock>`) is `Send + Sync` automatically. Make this type in M4, even if multithreading comes in M7 — otherwise you'll be reworking signatures later.

> **Ownership of JIT code memory.** Compiled blocks live in an arena of executable memory (`memmap2`, W^X; on macOS `pthread_jit_write_protect`). The arena belongs to the `Vm` and lives as long as it. `CompiledPtr` is a borrow into this arena — don't free it as long as the cache may point to it. SMC invalidation (section 10) removes the entry from the map, but the code memory is freed only when the arena is destroyed or via a separate recycling mechanism (a late optimization).

### 9.2 Dispatcher loop (the heart)

```rust
fn run(&mut self, budget: Option<u64>) -> Exit {
    let mut blocks_run: u64 = 0;
    loop {
        if let Some(b) = budget {
            if blocks_run >= b { return Exit::BudgetExhausted; }
        }

        let pc = self.cpu.rip;

        // Fetch from the cache or lift (miss). CachedBlock is cheap to clone
        // (Arc<IrBlock> or pointer+len) — we don't hold a reference into the RwLock's
        // interior for the duration of execution, because the backend mutates memory/cache (SMC).
        let block: CachedBlock = match self.vm.cache_get(pc) {
            Some(b) => b,                        // HIT — clone of the Arc/pointer
            None => {
                // MISS. A lift error (unknown instruction) is NOT a run() error —
                // it's a legal exit that tells the user "add this instruction to the lift".
                match lift_block(&self.vm, pc) {
                    Ok(ir) => {
                        let materialized = self.backend.materialize(&ir);
                        self.vm.cache_insert(pc, materialized.clone());
                        materialized
                    }
                    Err(LiftError::Unsupported { addr, bytes, len }) => {
                        return Exit::UnknownInstruction { addr, bytes, len };
                    }
                    Err(LiftError::DecodeFault { addr }) => {
                        return Exit::UnmappedMemory { addr, access: AccessKind::Execute };
                    }
                }
            }
        };

        // &self.vm.mem — NOT &mut: guest RAM is interior-mutable (§8 pitfall).
        match execute(&block, &mut self.cpu, &self.vm.mem) {
            StepResult::Continue => { blocks_run += 1; }   // RIP already updated in the block
            StepResult::Exit(exit) => { return exit; }     // syscall, mmio, breakpoint, ...
        }
    }
}
```

> **Budget vs block chaining (a trap that lands in M5, kills M7).** The budget is counted here, once per block. **Block chaining** (§12 M5) makes blocks jump straight into each other *without returning to the dispatcher* — so `blocks_run` stops ticking and a tight guest loop never yields `BudgetExhausted`, starving every other vcpu. When you add chaining, keep a preemption path: either a periodic counter check compiled into chained edges, or an external "please exit" flag the chained code polls at back-edges. Decide it with chaining, not after.

> **Benign double-lift race (M7).** Two vcpus can miss the same PC simultaneously, both lift, both insert. Harmless — lifting is pure and the result is identical, one insert just overwrites the other with an equal value. Don't add locking to prevent it; at most dedupe opportunistically. Worth one sentence so nobody "fixes" it into a bottleneck.

> **Cache ownership model.** `cache_get` returns a `CachedBlock` by clone (Arc for the interpreter, a copy of pointer+len for the JIT), NOT a reference into the interior of the `RwLock`. The reason: during `execute` the backend mutates guest memory, which may trigger SMC invalidation (a write to the cache) — holding a live `&` reference into the interior of the same lock would be a deadlock/borrow conflict. The clone breaks that dependency. `cache_get`/`cache_insert` take and release the lock briefly inside.

---

## 10. Self-modifying code (SMC) — cache invalidation

Because guest code lies in a *data* buffer (the guest may modify it), a write to a region from which you have a translated block invalidates that entry. At the start you can ignore it; add it when you hit a game/program that modifies its own code.

- Track which guest pages have translated blocks (a "code" bit per page).
- On a write to such a page → remove the affected entries from the cache (with the JIT: also mark the host code as dead).
- On the next execution → miss → re-lift from the changed bytes.

> **Same-block SMC — the effect is deferred, and that's an accepted deviation.** If a block writes into *its own* page (or a page whose translation is currently executing), the running block keeps executing the *old* code to its end — the `Arc<IrBlock>`/`CompiledPtr` cloned out by the dispatcher (§9.2) keeps it alive even after the cache entry is dropped; the re-lift only takes effect on the *next* dispatch. Real x86 can observe a self-modifying store more eagerly. This deviation is standard (QEMU makes it too) and fine for the target workloads, but **write it down**: if you ever chase a bug where a program rewrites the instruction it is about to run, this is the reason. Making it faithful means bounding blocks at write-detected boundaries — a large, late refinement, not a day-one requirement.

---

## 11. Multithreading (late milestone)

- **Per-vcpu:** `CpuState` (registers, flags, RIP), its own `run()` loop, its own host thread.
- **Shared (behind an `Arc`):** guest memory and the translation cache.
- **Cache:** translation happens once per block (whoever hits it first); synchronization via `RwLock` or a lock-free structure. Consider a per-vcpu cache for reading + a shared one for writing.
- **Memory model:** the `MemConsistency` tier (§4.1) + barriers in codegen (§8.2.3). This is the proper solution to the problem that the guest assumes x86 TSO, while the ARM host has a weak model. Without it, multithreaded programs produce nondeterministic bugs. Escalation ladder per workload: `Fast` → `AcqRel` → `FullTso`.

---

## 12. Implementation plan (milestones)

The order is chosen so as to **have a working, testable core as fast as possible**, and to defer the hard things.

### M0 — Skeleton (days)
- Workspace, `Vm`/`Vcpu`, `CpuState`, `Memory` (flat buffer + map/write/read).
- Integrate `iced-x86`: a decoding loop that only prints instructions. Zero execution.
- **Test:** load hand-assembled bytes, decode, print — matches `objdump`.

### M1 — IR + interpreter, minimal instruction set (1–2 weeks)
- IR (the `IrOp`, `IrBlock` enums), lift for: `mov`, `add`, `sub`, `cmp`, `and`/`or`/`xor`, `push`/`pop`, `jmp`, `jcc`, `call`, `ret`, `lea`, `load`/`store`.
- Interpreter executing the IR. Flags variant A (materialized).
- Return-based `run()` + `Exit` (for now: `UnknownInstruction`, `Syscall`, `BudgetExhausted`, `Hlt`).
- **Test — differential (crucial):** on an x86 host execute the same block natively (a small asm stub) and compare the register/flag state with the interpreter. This is your free oracle.

### M2 — First real program (1–2 weeks)
- Extend instruction coverage until you build and run a **static x86-64 "hello world" ELF (Linux)** under the interpreter.
- A minimal syscall shim on the test side: `write`, `exit` (reacting to `Exit::Syscall`).
- (Optionally) `x86jit-elf` — a simple segment loader.
- **Psychological milestone:** you see "hello world" printed by emulated code. The whole loop works end-to-end.
- **⚠️ Pick a nolibc / freestanding binary, NOT a static glibc one.** A static-glibc "hello world" runs `__libc_start_main`, whose very first steps call SSE2 `memcpy`/`strlen` — so "static glibc hello world" secretly requires a chunk of M8 (SIMD) before it prints anything. For M2 use a freestanding binary that issues `write`/`exit` via raw `syscall` (hand asm or `-nostdlib`), or musl built with SIMD disabled. Note it in the M2 tasks; otherwise "1–2 weeks" hits a SIMD wall.

### M3 — Translation cache (days–a week)
- A cache keyed by guest address, a dispatcher with hit/miss.
- For now the value = `IrBlock` (interpreter with cache — you skip the re-lift, but not yet the JIT).
- **Test:** a loop in guest code does not re-lift blocks (count miss/hit).

### M4 — Cranelift / JIT backend (2–4 weeks)
- `x86jit-cranelift`: the same `match` on `IrOp`, but describing to Cranelift.
- RAM access inlined; syscall/trap trap-out.
- **Test:** the JIT gives identical state to the interpreter over the whole corpus (interpreter = oracle for the JIT). Measure the speedup.

### M5 — Performance (ongoing)
- Block chaining (stitching blocks without returning to the dispatcher).
- Lazy flags (variant B).
- Superblocks / traces, if worth it.

### M6 — SMC invalidation
- Tracking pages with code, invalidation on write.

### M7 — Multithreading + TSO
- Many `Vcpu` over a shared `Vm`, cache synchronization.
- `MemConsistency` tiers + barriers in codegen (`Fast`/`AcqRel`/`FullTso`, §8.2.3).
- **Test:** a multithreaded program communicating through memory produces a deterministic result.

### M8+ — SIMD (SSE/AVX)
- XMM/YMM in the state, lift of vector instructions. A big, separate chapter. Games require it, but not at the start.

---

## 13. Testing strategy

1. **Differential testing (the most important).** On an x86 host execute a block natively and compare with the interpreter/JIT. Automate: generate random instruction sequences, execute both, compare state. This catches 90% of semantic bugs.
2. **Interpreter as oracle for the JIT.** For every block: the interpreter and the JIT must produce identical state.
3. **Per-instruction unit tests.** For each lifted instruction: a set of inputs (including edges: overflows, zero, sign) → expected state + flags.
4. **Decoder fuzzing.** Random bytes → the lift must not panic (at most `UnknownInstruction`).
5. **Corpus of real binaries.** A growing set of static ELFs as end-to-end tests.

---

## 14. Open design decisions (to be resolved along the way)

- **Budget in instructions or blocks?** Recommendation: blocks (cheaper). Resolve in M1.
- **Lazy flags right away or later?** Recommendation: later (M5). Simplicity first.
- **Flat memory or softmmu?** Flat at the start (M0), softmmu when you hit a sparse/scattered address space.
- **Hooks (model A) alongside return-based?** Add optionally after M4, if they turn out to be convenient for debugging. The core stays return-based.
- **How to represent Cranelift values vs IR temps?** A `Temp → cranelift Value` map built per-block in the backend.
- **How to represent CPU exceptions (`#DE` divide-by-zero, `#UD` from `ud2`, `int3`)?** They're guest-visible faults, not lift failures. Options: a dedicated `Exit::Exception { vector, error_code }` (extensible, honest) vs. folding into `Exit::Fault`. Recommendation: a dedicated exit — real programs `SIGFPE`/`SIGILL` on these and HLE must see the vector. Resolve before lifting `div` (M1/M2).
- **Inline memory-access safety: bounds check vs guard pages?** (§8.2.3) Recommendation: explicit per-access bounds+permission check first (portable, doubles as the MMIO trap path), guard pages only as a measured M5 perf optimization. Resolve in M4.
- **Consistency-tier mechanics** (§8.2.3): does the lift tag every `Load`/`Store`, or does codegen apply the tier as a blanket policy to `MemOrder::None` accesses? Recommendation: codegen-applies-blanket (lift stays tier-agnostic; sketches emit `None` today). Selective tagging (only provably-shared pages) is a later optimization that *reopens the AcqRel gap* — measure before doing it. Resolve in M7.
- **Breakpoint mechanism** (`Exit::Breakpoint`): a set breakpoint must split/bound the block at that address and invalidate any cached block covering it, so execution actually stops there. Needs a `set_breakpoint`/`clear_breakpoint` API + block-boundary handling. Design when debug support is actually built (post-M4); today the enum variant is a placeholder.
- **`hlt` in unit-test snippets** (testing.md): `hlt` is privileged and faults in the native oracle's user-mode process — vectors that terminate with `hlt` need a different terminator for the native oracle (int3+handler, or a ret trampoline). Unicorn/interpreter handle `hlt` fine. Resolve when building the native oracle (M1).

---

## 15. Dependencies

- `iced-x86` (MIT) — x86 decoder. Core.
- `cranelift-*` (Apache-2.0) — JIT backend. Feature-gated (`x86jit-cranelift`).
- `memmap2` or similar — allocation of executable memory for the JIT cache (W^X; on macOS `pthread_jit_write_protect`).

> **License:** all core dependencies are permissive (MIT/Apache), so you have freedom to choose your own library's license — including copyleft, if you want.

---

## 16. Where the surprises live (consolidated list of pitfalls)

One place gathering all the mines scattered across the document. Review it before every milestone — each of these things will block progress or give a silent, hard-to-find bug if you overlook it.

**Instruction atomicity vs the retry convention (pitfall #0 — silent, cross-cutting, hits in M1):**
- **No state may commit before an instruction's last possible trap.** RIP points at the faulting instruction and the user retries it (§8), so a `push` that moves RSP before a faulting store, or an RMW that writes flags before a faulting store, corrupts state on retry. Within one guest instruction, emit all trapping ops (load/store) **before** all committing ops (WriteReg, flags), or prove idempotence. Encode in the lowering helpers (§7 pitfall 3). Invisible until a fault-retry happens.
- **RIP-on-trap needs the instruction's address, which the IR must carry.** A trapping `Load`/`Store` (or an exception) must set `cpu.rip` to the *faulting* instruction — but `IrOp`s don't otherwise know their address, and `guest_start + guest_len` is the block END (right only for the block-terminating `syscall`). The lift emits `IrOp::InsnStart { guest_addr }` at each instruction boundary; the interpreter tracks it in `cur_addr`, the JIT bakes it as a const for the following trapping accesses (§6.2, §8.1, §8.2.1). Miss this and every mid-block fault resumes at the wrong RIP.

**x86-64 semantics (hits in M1–M2, at lift time):**
- **Zeroing of the upper 32 bits.** A write to `eax` zeroes bits 32–63 of `rax`; a write to `ax`/`al` — does not. Asymmetric. Encode it centrally in the write to a GPR (§7.1, §8.2.1). The most common silent bug.
- **A memory operand is not a `Val`.** It requires computing the effective address (`base + index*scale + disp`) + `Load`. Without the operand lowering layer (§7.1) you can't lift even `mov`.
- **Read-modify-write** (`add [mem], rax`): compute the effective address **once**, use it for Load and Store (§7.1).
- **RIP-relative** is computed relative to the *next* instruction. Use the value from iced, don't compute it by hand (§7 pitfall 2). `ReadReg(Rip)` is forbidden in IR — RIP is stale mid-block; lower to `Imm` (§6.2).
- **FS/GS segment base** for TLS — if the instruction has a segment prefix, add `fs_base`/`gs_base` to the address (§7.1).
- **Flags: `FlagMask`, not `bool`, and not uniform.** `inc`/`dec` keep CF; logic ops force CF=OF=0; shifts update flags **only when the count ≠ 0** (runtime-conditional). A `set_flags: bool` cannot express these — use `FlagMask` (§6.2). iced tells you *which* flags, you encode *how* (§7).
- **Flags as input, not just output.** `adc`/`sbb` consume CF into the arithmetic; `setcc`/`cmovcc`/`rcl`/`rcr` read flags as data. IR needs `Adc`/`Sbb` and `GetCond` — without them you can't lift 128-bit add chains that glibc/compilers emit constantly (§6.2).
- **glibc static "hello world" needs SSE.** `__libc_start_main` uses SSE2 `memcpy`/`strlen` immediately — M2 must use a nolibc/freestanding binary, or M2 secretly requires M8 (§12 M2).
- **`#DE`/`#UD`/`int3` are guest exceptions, not lift errors** — represent them as an `Exit`, decide the shape before lifting `div` (§14).

**Rust aliasing / ownership (hits in M1, gets worse in M7):**
- **Guest RAM is `&Memory` with interior mutability, never `&mut Memory`.** A store writes through `&self`; multiple vcpus share one `Memory` and race like real hardware (ordered by TSO barriers, not `&mut`). Starting with `&mut` forces a full signature rewrite at M7. One deliberate `unsafe` (`UnsafeCell` + manual `Sync`), tightly wrapped (§8).
- **Backend is an injected `Box<dyn Backend>`, not a config enum.** The core can't name the downstream JIT crate — `enum Backend { Interpreter, Jit }` in config is unbuildable. Inject the trait object (§4.1, §8).

**Type contracts (hits at compile time, early):**
- **The backend has no `execute(&IrBlock)`.** Materialization is backend-dependent, execution is uniform over `CachedBlock` (§8). You won't glue the interpreter and the JIT together with a single `execute` on IR.
- **`StepResult`, not `Exit`, from the execution layer.** It distinguishes "flow on" from "trap-out" (§8).
- **The RIP convention on trap-out** must be identical in the interpreter and the JIT (past the instruction for syscall, on the instruction for a memory access), otherwise the backends will diverge (§8).

**JIT / Cranelift (hits in M4):**
- **The block ABI is a decision, not a detail.** `CpuState` via pointer + stable offsets (`#[repr(C)]`), memory via `host_base`, the result encoded in a `u64` (§8.2.1–8.2.2). Open this box before you write codegen.
- **Build the JIT incrementally** against the interpreter-oracle: first an empty block "return Continue with RIP", then `IrOp` one by one (§8.2.3).
- **Inlined memory access is host UB without a safety strategy.** Raw `host_base + guest_addr` reads/writes outside the host buffer on any out-of-range guest address. Emit a bounds+permission check (which also routes Trap/MMIO out) or use guard pages — decide up front (§8.2.3). In `Flat`, address 0 is valid, so a faithful null-`#PF` needs a per-page permission bitmap.
- **MMIO in the JIT** requires a runtime check or a conscious decision that unknown addresses go the slow route (§5.2). The MMIO-read resume is a **pending value consumed by the retried load**, not a write into a dead temp (§5.2).
- **Budget stops ticking under block chaining** — a chained loop never yields `BudgetExhausted`, starving other vcpus. Add a preemption path with chaining (§9.2, M5).

**Multithreading and memory (hits in M7, but prepare in M4):**
- **`CompiledPtr` must be `Send + Sync`** manually, otherwise the cache won't go behind an `Arc` between threads. Make this type in M4, not in M7 (§9.1).
- **The TSO memory model** — `MemConsistency` tiers in codegen (`Fast`/`AcqRel`/`FullTso`, §8.2.3). Without barriers, multithreaded programs produce nondeterministic bugs that aren't there single-threaded — the worst kind to chase. Escalate the tier before debugging "impossible" races. Locked ops/`mfence` get real atomics/fences in EVERY tier. Switching tiers flushes the translation cache (barriers are baked into blocks).
- **Ownership of code memory** — the arena lives with the `Vm`, `CompiledPtr` is a borrow; don't free it while the cache points to it (§9.1).

**Guest memory model (hits in M0, then again at large addresses):**
- **`Flat` vs `map()`** — in `Flat`, `map()` only assigns permissions, does not allocate. `map(high_address)` in `Flat` is not an attempt to allocate 128 TB (§4.1). High/sparse addresses → only with `SoftMmu`.

**SMC (hits when you meet a game modifying its own code — can be ignored for a long time):**
- **Consistency of cache↔guest buffer.** A write to a page with translated code invalidates the entry (§10). Ignore it until you actually hit such a case — but then remember it comes from here.

---

## 17. Extensibility points (seams, not a framework)

This library supports **x86-64 long mode only** (see §1). This section defines how to leave clean seams in case someone (or you in a few years) wants to extend it — **without building any framework now**. Goal: don't cement the "always 64" assumption where leaving a parameter is cheap. Nothing here mandates implementing other modes — it only keeps the door open.

### 17.1 Three kinds of extension — two are already supported, one needs seams

- **More instructions (same long mode).** E.g. SSE/AVX, rarer instructions. **Already supported by the architecture** — a new arm in the lift `match` + possibly a new IR operation + its handling in the backends. Zero structural change. The pattern "hit `UnknownInstruction` → add the lift". Needs no seams.
- **A different guest architecture (ARM, MIPS, 6502…).** **Already supported by the IR layer** — someone writes a new front-end (decoder + lift → the same IR), and the whole backend (interpreter, Cranelift, cache, dispatcher) comes for free. A lot of work (new decoder + lift), but clean: it adds a parallel front-end, doesn't touch the existing one. Needs no seams in that sense — just a second lift.
- **A different x86 processor MODE (protected/compatibility 32-bit, theoretically real mode).** **This is the only case that requires seams left in advance**, because it touches semantics *scattered* across the core (decoding, addressing, cache). The rest of this section is about those three seams.

### 17.2 Why 32-bit mode is invasive (so you know what the seam protects)

In long mode: the decoder is always in 64-bit mode, segments (except FS/GS) have base 0, an address is a flat offset. In 32-bit protected mode: **the same bytes decode differently**, and **CS/DS/ES/SS have meaningful bases and limits** — every memory access is `segment.base + offset` with a limit check. On top of that the mode can **change at runtime** (mode transitions), so the same guest address means different code in different modes. These are three concrete touch points — and exactly where we leave seams.

### 17.3 Seam 1 — mode as an explicit field, not the constant `64`

Don't scatter the literal `64` (at `Decoder::new`) or the "segments are always 0" assumption across the code. Introduce an explicit mode type — **today with a single value** — and read it where you decode and compute addresses.

```rust
/// Guest execution mode. TODAY always Long64. The enum exists solely so the
/// decoder and address computation ask for the mode instead of hardcoding "64".
/// Adding a variant later then does NOT require rewriting scattered assumptions.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum CpuMode {
    Long64,
    // Protected32,   // future: decoder in 32-bit mode, segmentation active
    // Real16,        // future: if someone ever wanted a full machine
}
```

Where it lives: logically the mode is a hidden "mode register" of the guest (because in real x86 it can change via a mode transition), so its home is next to the CPU state / lift context. Today it's effectively a constant — but pass it as a value, don't hardcode `64`.

Cost now: negligible (one field, one value, `Decoder::new(mode.bits(), ...)` instead of `Decoder::new(64, ...)`). Payoff later: adding a mode is adding an enum variant, not hunting for `64` in a hundred places.

### 17.4 Seam 2 — mode in the cache key

The cache is keyed by guest address (§9.1). The same address in 32- and 64-bit mode is **different** translated code (because the bytes decode differently). So the key is conceptually an `(address, mode)` pair, not the address alone — even if the second field has a single value today.

```rust
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct BlockKey {
    pub guest_addr: u64,
    pub mode: CpuMode,      // today always Long64; part of the key so modes don't get confused
}
// cache: RwLock<HashMap<BlockKey, CachedBlock>>
```

Cost now: practically zero (wrapping a `u64` in a struct with a single-value field). Payoff later: adding a mode doesn't require redesigning the cache or risking a block from one mode being used in another.

### 17.5 Seam 3 — all memory access through one helper

Segmentation in 32-bit mode touches **every** memory access. The only way its addition isn't surgery on a hundred lifts is for *every* access to go through one address-computation helper — `effective_address` from §7.1. You already have this seam for another reason (RMW, FS/GS); here it's about **keeping the discipline**: no lift computes an address itself.

Today `effective_address` in `Long64` skips segment bases (except FS/GS). In a future `Protected32` the same function would add `segment.base` and check the limit — **a change in one function**, not in every memory-touching instruction. As long as the "address only through the helper" discipline is ironclad, this seam is free.

### 17.6 Directive: leave seams, do NOT build machinery

This is the line you must not cross. The seams above cost pennies and are simply good code (a parameter instead of a constant, a key struct, a single choke-point for the address). **Machinery** is a different thing and is forbidden here until a second implementation exists:

- **DON'T** write `trait AddressingMode` / `trait ExecutionMode` with a single implementation.
- **DON'T** parametrize things that are identical in 32- and 64-bit.
- **DON'T** add "just in case" layers, mode configuration, plugins.
- **DON'T** design an API for `Protected32` that nobody has written.

The reason isn't only about time: **empty abstractions never validated by a second implementation usually turn out wrong when the second one finally arrives** — because you guessed its shape instead of knowing it. Better one solid mode with three clean seams than a multi-mode framework tested with a single mode. When a second mode really arrives, then — with the concrete case in front of you — you'll design the right abstraction; now you'd only make it up.

### 17.7 The other side of the seam: today reject other modes LOUDLY

The seam opens a path to the future; but today another mode must not "almost work", it must be clearly rejected — because silently mis-decoding 32-bit code in 64-bit mode yields garbage, not an error (the same bytes are a different instruction). The loader (outside the core) checks in the header that the file is 64-bit (ELF: `ELFCLASS64` + `EM_X86_64`; SELF analogously) and rejects others with a clear message "only x86-64 long mode supported". The library name communicates the contract, header validation enforces it, the seams leave a back door — three layers, each doing its job.

---

- **Input:** an unpacked memory map + entry point. Format parsing OUTSIDE the core.
- **Core:** iced → lift → custom IR → interpreter/Cranelift → cache(guest→host) → dispatcher.
- **Output:** `run()` returns an `Exit` (syscall/mmio/unknown/…); guest state readable/writable between calls.
- **Hot path (RAM):** inline in codegen, never a callback.
- **Rare events (syscall, MMIO):** trap-out via `Exit`.
