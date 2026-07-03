# M0 — Skeleton

**Goal:** workspace + core types compile; a flat guest memory you can map/write/read; an iced-x86 decode loop that prints instructions (zero execution).

**Spec:** spec.md §2, §3, §4, §12 (M0). **Prereq:** none.

## Tasks

- [x] **M0-T1** — Cargo workspace with four crates (`x86jit-core`, `x86jit-cranelift`, `x86jit-elf`, `x86jit-tests`); builds clean. (§2)
- [x] **M0-T2** — Nix flake devShell (rust toolchain + nextest); `nix develop` verified.
- [x] **M0-T3** — `state`: `Reg`, `CpuState` (`#[repr(C)]`, flat `gpr[16]`), `Flags` (Variant A). (§3)
- [x] **M0-T4** — `memory`: `MemoryModel`/`Prot`/`RegionKind`/`MemTrap` types; `Memory` owning a flat backing buffer + `host_base()`. (§4)
- [x] **M0-T5** — `ir`, `exit`, `cache`, `vm`, `lift` module type-stubs; dispatcher `run()` loop wired. (§5, §6, §8, §9)
- [x] **M0-T6** — `Reg` ↔ `gpr[]` index map, in ONE place. iced `Register` → `gpr` index; RAX=0, RCX=1, … x86 encoding order (**not** enum order). (§3.1 note)
- [x] **M0-T7** — `Vcpu::set_reg` / `reg` implemented over the map (rip / fs_base / gs_base handled). (§4.3)
- [x] **M0-T8** — `Memory::map` (Flat: tag region prot/kind + bounds-check, no allocation), `write_bytes`, `read_bytes`, `unmap`. Keep the Flat-vs-`map()` distinction (map tags, doesn't allocate). (§4.1, §4.2, §16)
- [x] **M0-T9** — iced `Decoder` set up over guest bytes at a given address; loop that decodes and pretty-prints instructions. No lift, no execution. (§12 M0)

## Acceptance

- [x] **M0-T10** — Load hand-assembled bytes, decode, print → output matches `objdump -d` on the same bytes. (§12 M0)

## Exit criteria

`cargo build` + `cargo test` green; you can map memory, write code bytes, and dump a correct disassembly. `CpuState` layout is `#[repr(C)]` and the GPR index map is centralized (both are contracts for later milestones).
