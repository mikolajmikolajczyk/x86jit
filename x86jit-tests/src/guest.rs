//! Shared guest-program harness for the real-program tests (testing.md §12).
//!
//! Every "run busybox/lua/python/… three ways" test used to hand-roll the same
//! spine: build a `Vm`, load the ELF, map RAM, `setup_stack`, wire the shim, then
//! run the `Exit::Syscall => shim.handle` loop. They drifted (e.g. some forgot to
//! set `mmap_base`/`mmap_limit`). This is that spine, once: a [`Guest`] builder that
//! loads a static or dynamic ELF, lays out heap/mmap/stack, and drives the syscall
//! loop to completion, returning captured stdout.
//!
//! Per-test shim extras (`allow_read`, `allow_dir`, `serve_lib`, scripted syscalls)
//! go through [`Guest::shim`], a `FnOnce(&mut LinuxShim)` escape hatch, so the
//! harness stays generic without enumerating every knob.

use x86jit_core::{
    Backend, Exit, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm, VmConfig,
};
use x86jit_elf::{load_dynamic_elf, load_static_elf, setup_stack, setup_stack_dyn};

use crate::syscall::LinuxShim;

/// Which ELF shape to load.
pub enum Image<'a> {
    /// A statically-linked ET_EXEC (loaded at its own vaddrs).
    Static(&'a [u8]),
    /// A dynamic PIE plus its interpreter (ld-linux/ld-musl), each with a load bias.
    Dynamic {
        exe: &'a [u8],
        interp: &'a [u8],
        exe_base: u64,
        interp_base: u64,
    },
}

type ShimSetup<'a> = Box<dyn FnOnce(&mut LinuxShim) + 'a>;

/// The observable result of a guest run.
pub struct Ran {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
}

/// Builder for a one-shot guest process. Defaults cover a small static program; the
/// setters adjust the layout, args, and shim configuration.
pub struct Guest<'a> {
    image: Image<'a>,
    flat: u64,
    heap_base: u64,
    /// `Some` when the program uses the mmap arena (glibc/musl allocators). The heap
    /// grows up to it; mmap serves from it to `stack_top - 1 MiB`.
    mmap_base: Option<u64>,
    /// Top of the mmap arena; defaults to `stack_top - 1 MiB` when a `mmap_base` is
    /// set. A few tight layouts override it.
    mmap_limit: Option<u64>,
    /// Cap for the brk allocator; defaults to `mmap_base` (if set) else `stack_top`.
    brk_limit: Option<u64>,
    stack_top: u64,
    argv: &'a [&'a [u8]],
    env: &'a [&'a [u8]],
    stdin: Vec<u8>,
    tier_up: Option<u32>,
    setup: Option<ShimSetup<'a>>,
}

impl<'a> Guest<'a> {
    /// A static ELF with a default 64 MiB flat layout (heap 6 MiB, no mmap arena).
    /// Override with the setters before [`run`](Guest::run).
    pub fn new_static(image: &'a [u8]) -> Self {
        Guest {
            image: Image::Static(image),
            flat: 0x400_0000,
            heap_base: 0x60_0000,
            mmap_base: None,
            mmap_limit: None,
            brk_limit: None,
            stack_top: 0x3f0_0000,
            argv: &[],
            env: &[],
            stdin: Vec::new(),
            tier_up: None,
            setup: None,
        }
    }

    /// A dynamic PIE + interpreter with the given load biases.
    pub fn new_dynamic(exe: &'a [u8], exe_base: u64, interp: &'a [u8], interp_base: u64) -> Self {
        Guest {
            image: Image::Dynamic {
                exe,
                interp,
                exe_base,
                interp_base,
            },
            ..Guest::new_static(&[])
        }
    }

    pub fn flat(mut self, flat: u64) -> Self {
        self.flat = flat;
        self
    }
    pub fn heap_base(mut self, heap_base: u64) -> Self {
        self.heap_base = heap_base;
        self
    }
    pub fn mmap_base(mut self, mmap_base: u64) -> Self {
        self.mmap_base = Some(mmap_base);
        self
    }
    /// Override the mmap arena top (default `stack_top - 1 MiB`) for tight layouts.
    pub fn mmap_limit(mut self, mmap_limit: u64) -> Self {
        self.mmap_limit = Some(mmap_limit);
        self
    }
    /// Override the brk cap (default `mmap_base` if set, else `stack_top`).
    pub fn brk_limit(mut self, brk_limit: u64) -> Self {
        self.brk_limit = Some(brk_limit);
        self
    }
    pub fn stack_top(mut self, stack_top: u64) -> Self {
        self.stack_top = stack_top;
        self
    }
    pub fn argv(mut self, argv: &'a [&'a [u8]]) -> Self {
        self.argv = argv;
        self
    }
    pub fn env(mut self, env: &'a [&'a [u8]]) -> Self {
        self.env = env;
        self
    }
    pub fn stdin(mut self, stdin: &[u8]) -> Self {
        self.stdin = stdin.to_vec();
        self
    }
    pub fn tier_up(mut self, after: Option<u32>) -> Self {
        self.tier_up = after;
        self
    }
    /// Escape hatch for per-test shim configuration (`allow_read`, `serve_lib`, …),
    /// run just before the guest starts.
    pub fn shim(mut self, f: impl FnOnce(&mut LinuxShim) + 'a) -> Self {
        self.setup = Some(Box::new(f));
        self
    }

    /// Load the program, drive it to exit under `backend`, and return its stdout.
    /// Panics on any non-syscall exit (an engine gap), like the hand-rolled loops.
    pub fn run(self, backend: Box<dyn Backend>) -> Vec<u8> {
        self.run_full(backend).stdout
    }

    /// As [`run`](Guest::run), but returns stdout, stderr, and the exit code (for
    /// tests that assert more than stdout).
    pub fn run_full(self, backend: Box<dyn Backend>) -> Ran {
        let mut vm = Vm::with_backend(
            VmConfig {
                memory_model: MemoryModel::Flat { size: self.flat },
                consistency: MemConsistency::Fast,
            },
            backend,
        );
        vm.set_tier_up_after(self.tier_up);

        // Load first (the loader maps its own segments), then one RW region from the
        // heap base to `flat` covers heap + mmap arena + stack.
        let (entry, sp) = match self.image {
            Image::Static(img) => {
                let entry = load_static_elf(&mut vm, img).expect("load static elf");
                self.map_ram(&mut vm);
                let sp =
                    setup_stack(&mut vm, self.stack_top, self.argv, self.env).expect("setup stack");
                (entry, sp)
            }
            Image::Dynamic {
                exe,
                interp,
                exe_base,
                interp_base,
            } => {
                let img = load_dynamic_elf(&mut vm, exe, exe_base, interp, interp_base)
                    .expect("load dynamic elf");
                self.map_ram(&mut vm);
                let sp = setup_stack_dyn(&mut vm, self.stack_top, self.argv, self.env, &img)
                    .expect("setup dynamic stack");
                (img.entry, sp)
            }
        };

        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, entry);
        cpu.set_reg(Reg::Rsp, sp);

        let mut shim = LinuxShim::new();
        shim.brk = self.heap_base;
        match self.mmap_base {
            Some(mb) => {
                shim.brk_limit = self.brk_limit.unwrap_or(mb);
                shim.mmap_base = mb;
                shim.mmap_limit = self.mmap_limit.unwrap_or(self.stack_top - 0x10_0000);
            }
            None => shim.brk_limit = self.brk_limit.unwrap_or(self.stack_top),
        }
        shim.stdin = self.stdin;
        if let Some(setup) = self.setup {
            setup(&mut shim);
        }

        loop {
            match cpu.run(&vm, None) {
                Exit::Syscall => {
                    if shim.handle(&mut cpu, &vm) {
                        break;
                    }
                }
                other => panic!("gap at rip={:#x}: {other:?}", cpu.reg(Reg::Rip)),
            }
        }
        Ran {
            stdout: shim.stdout,
            stderr: shim.stderr,
            exit_code: shim.exit_code,
        }
    }

    fn map_ram(&self, vm: &mut Vm) {
        vm.map(
            self.heap_base,
            (self.flat - self.heap_base) as usize,
            Prot::RW,
            RegionKind::Ram,
        )
        .expect("map guest ram");
    }
}
