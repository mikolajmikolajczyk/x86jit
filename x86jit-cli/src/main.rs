//! `x86jit-cli` — run x86-64 Linux programs under the recompiler, **no recompilation**.
//!
//! Two modes:
//! - default (no subcommand): run a **host binary** — point it at an ELF on your
//!   system; its shared libraries are served from the host rootfs (`/` by default),
//!   so a normal dynamic binary (`/usr/bin/echo`, coreutils, …) runs as-is.
//! - `oci`: run a **`docker save` image** tarball; the rootfs is extracted to a temp dir.
//!
//! Both are thin glue over the [`x86jit_cli`] library. A guest that hits an
//! unimplemented syscall or instruction surfaces as a clear `guest trapped: …` — that
//! message *is* the "what's missing to run this" answer.

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use x86jit_cli::{
    run_config_argv_stdin_features, run_image, run_registry, EngineKind, GuestCpuFeatures,
    ImageConfig, RunOptions, RunResult,
};

#[derive(Parser)]
#[command(
    name = "x86jit-cli",
    version,
    about = "Run x86-64 Linux programs under the x86jit recompiler.",
    // cargo-style default subcommand: bare `x86jit-cli <BINARY>` runs a host ELF,
    // while `x86jit-cli oci <IMAGE>` runs an image. The top-level (host) args are
    // required only when no subcommand is given.
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
    #[command(flatten)]
    run: RunArgs,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run an OCI/Docker image — pull from a registry (`run`) or load a tar (`load`).
    Oci(OciArgs),
}

/// Run a host x86-64 ELF (the default mode). File syscalls hit REAL host files under
/// `--rootfs` — a writing guest writes to your disk; point it at a throwaway tree if
/// that matters.
#[derive(Args)]
struct RunArgs {
    /// An x86-64 ELF: an absolute/relative path, or a bare name found on $PATH.
    /// `Option` (not a clap-required arg) so a subcommand invocation doesn't trip the
    /// requirement; the default (host) mode validates it is present.
    binary: Option<String>,
    /// Arguments passed to the guest program (its argv[1..]), verbatim.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    guest_args: Vec<String>,
    /// Engine to run under.
    #[arg(short, long, value_enum, default_value_t = Backend::Jit)]
    backend: Backend,
    /// Guest CPU feature level: baseline|v2|v3|v4|default. `v4` advertises AVX-512; a
    /// guest then traps on any AVX-512 op the lifter can't execute.
    #[arg(long, default_value = "default")]
    cpu: String,
    /// Filesystem the guest sees.
    #[arg(short, long, default_value = "/")]
    rootfs: String,
    /// Extra library search dir, prepended to LD_LIBRARY_PATH. Repeatable.
    #[arg(short = 'L', long = "lib")]
    libs: Vec<String>,
    /// Set/override a guest env var (KEY=VAL). Repeatable.
    #[arg(short, long = "env")]
    envs: Vec<String>,
    /// Start from an empty environment instead of inheriting the host's.
    #[arg(long)]
    no_inherit_env: bool,
    /// Suppress the one-line run summary on stderr.
    #[arg(short, long)]
    quiet: bool,
}

#[derive(Args)]
struct OciArgs {
    #[command(subcommand)]
    what: OciCmd,
}

#[derive(Subcommand)]
enum OciCmd {
    /// Pull an image from a registry into a temp rootfs and run it (docker-run-like).
    Run(OciRunArgs),
    /// Run a local `docker save` image tarball.
    Load(OciLoadArgs),
}

/// `oci run [registry[:port]/]name[:tag|@digest] [-- CMD...]` — pull + run.
#[derive(Args)]
struct OciRunArgs {
    /// Image reference: `[registry[:port]/]name[:tag|@digest]` (defaults to Docker Hub).
    reference: String,
    /// Command to run instead of the image's entrypoint (everything after `--`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
    /// Engine(s) to run under; `both` runs each and flags any divergence.
    #[arg(short, long, value_enum, default_value_t = OciBackend::Jit)]
    backend: OciBackend,
    /// Pull over insecure `http://` (for a local `registry:port`) instead of HTTPS.
    #[arg(long)]
    plain_http: bool,
}

#[derive(Args)]
struct OciLoadArgs {
    /// A `docker save` image tarball.
    image: String,
    /// Engine(s) to run under; `both` runs each and flags any divergence.
    #[arg(short, long, value_enum, default_value_t = OciBackend::Jit)]
    backend: OciBackend,
}

#[derive(Clone, Copy, ValueEnum)]
enum Backend {
    Interp,
    Jit,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OciBackend {
    Interp,
    Jit,
    Both,
}

impl From<Backend> for EngineKind {
    fn from(b: Backend) -> Self {
        match b {
            Backend::Interp => EngineKind::Interpreter,
            Backend::Jit => EngineKind::Jit,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Cmd::Oci(a)) => run_oci(a),
        None => run_host(cli.run),
    }
}

// --- host binary (`run`) ---------------------------------------------------------

fn run_host(args: RunArgs) -> ExitCode {
    let features = match parse_cpu(&args.cpu) {
        Ok(f) => f,
        Err(e) => return fail(e),
    };
    let binary = match &args.binary {
        Some(b) => b,
        None => return fail("missing <BINARY> (an ELF path or a name on $PATH)"),
    };
    let prog = match resolve_binary(binary) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };

    let mut env: Vec<String> = if args.no_inherit_env {
        Vec::new()
    } else {
        std::env::vars().map(|(k, v)| format!("{k}={v}")).collect()
    };
    if !args.libs.is_empty() {
        let prior = env
            .iter()
            .find_map(|e| e.strip_prefix("LD_LIBRARY_PATH="))
            .map(|s| format!(":{s}"))
            .unwrap_or_default();
        set_env(
            &mut env,
            &format!("LD_LIBRARY_PATH={}{prior}", args.libs.join(":")),
        );
    }
    for kv in &args.envs {
        set_env(&mut env, kv);
    }

    let cfg = ImageConfig {
        env,
        entrypoint: vec![prog.clone()],
        cmd: Vec::new(),
        working_dir: "/".into(),
        architecture: "amd64".into(),
        os: "linux".into(),
    };
    let argv: Vec<String> = std::iter::once(prog.clone())
        .chain(args.guest_args)
        .collect();

    // Forward piped host stdin to the guest; leave it empty on an interactive tty.
    let mut stdin = Vec::new();
    if !std::io::stdin().is_terminal() {
        let _ = std::io::stdin().read_to_end(&mut stdin);
    }

    let engine: EngineKind = args.backend.into();
    let label = match engine {
        EngineKind::Interpreter => "interp",
        EngineKind::Jit => "jit",
    };
    match run_config_argv_stdin_features(
        &cfg,
        Path::new(&args.rootfs),
        engine,
        &argv,
        &stdin,
        features,
    ) {
        Ok(RunResult {
            stdout,
            stderr,
            exit_code,
        }) => {
            let _ = std::io::stdout().write_all(&stdout);
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().write_all(&stderr);
            let code = exit_code.unwrap_or(0);
            if !args.quiet {
                eprintln!("x86jit-cli: {prog} [{label}] → exit {code}");
            }
            ExitCode::from(code.clamp(0, 255) as u8)
        }
        Err(e) => {
            // The trap message names the unimplemented syscall / instruction — the
            // concrete "what's missing to run this binary" answer.
            eprintln!("x86jit-cli: {prog} [{label}] ✗ {e}");
            ExitCode::FAILURE
        }
    }
}

// --- OCI image (`oci`) -----------------------------------------------------------

fn run_oci(args: OciArgs) -> ExitCode {
    match args.what {
        OciCmd::Run(a) => oci_run(a),
        OciCmd::Load(a) => oci_load(a),
    }
}

/// Run each selected engine over a fresh per-engine rootfs that `prepare` fills (pull
/// or extract) and executes, streaming stdout/stderr and flagging any divergence
/// under `both`. Returns the guest exit code.
fn oci_dispatch(
    backend: OciBackend,
    mut prepare: impl FnMut(EngineKind, &Path) -> Result<RunResult, String>,
) -> ExitCode {
    let engines: &[EngineKind] = match backend {
        OciBackend::Interp => &[EngineKind::Interpreter],
        OciBackend::Jit => &[EngineKind::Jit],
        OciBackend::Both => &[EngineKind::Interpreter, EngineKind::Jit],
    };
    let mut last: Option<RunResult> = None;
    for &engine in engines {
        let rootfs = scratch_dir(engine);
        match prepare(engine, &rootfs) {
            Ok(res) => {
                if engines.len() > 1 {
                    eprintln!("--- {engine:?} ---");
                }
                let _ = std::io::stdout().write_all(&res.stdout);
                let _ = std::io::stdout().flush();
                let _ = std::io::stderr().write_all(&res.stderr);
                if let Some(prev) = &last {
                    if prev != &res {
                        eprintln!("MISMATCH between backends");
                        return ExitCode::FAILURE;
                    }
                }
                last = Some(res);
            }
            Err(e) => {
                eprintln!("{engine:?}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    match last.and_then(|r| r.exit_code) {
        Some(code) => ExitCode::from(code.clamp(0, 255) as u8),
        None => ExitCode::SUCCESS,
    }
}

fn oci_load(a: OciLoadArgs) -> ExitCode {
    oci_dispatch(a.backend, |engine, rootfs| {
        run_image(Path::new(&a.image), rootfs, engine).map_err(|e| e.to_string())
    })
}

fn oci_run(a: OciRunArgs) -> ExitCode {
    // Pipe host stdin to the guest (like `docker run -i`); the `-t` interactive tty is
    // a later phase. `both` pulls once per engine — fine for a dev/differential run.
    let mut stdin = Vec::new();
    if !std::io::stdin().is_terminal() {
        let _ = std::io::stdin().read_to_end(&mut stdin);
    }
    oci_dispatch(a.backend, |engine, rootfs| {
        let opts = RunOptions {
            stdin: stdin.clone(),
            ..Default::default()
        };
        run_registry(&a.reference, rootfs, engine, &a.command, opts, a.plain_http)
            .map_err(|e| e.to_string())
    })
}

fn scratch_dir(engine: EngineKind) -> PathBuf {
    let d = std::env::temp_dir().join(format!("x86jit-cli-oci-{engine:?}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// --- helpers ---------------------------------------------------------------------

fn parse_cpu(level: &str) -> Result<GuestCpuFeatures, String> {
    Ok(match level {
        "baseline" | "v1" => GuestCpuFeatures::baseline(),
        "v2" => GuestCpuFeatures::v2(),
        "v3" => GuestCpuFeatures::v3(),
        "v4" => GuestCpuFeatures::v4(),
        "default" => GuestCpuFeatures::default(),
        other => {
            return Err(format!(
                "unknown --cpu `{other}` (baseline|v2|v3|v4|default)"
            ))
        }
    })
}

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("x86jit-cli: {msg}");
    ExitCode::FAILURE
}

/// Find a bare command name on `$PATH`; return the first executable match.
fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
}

/// Resolve the user's `<BINARY>` argument to a path the rootfs can serve.
fn resolve_binary(arg: &str) -> Result<String, String> {
    let path = if arg.contains('/') {
        let p = PathBuf::from(arg);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir().map_err(|e| e.to_string())?.join(p)
        }
    } else {
        which(arg).ok_or_else(|| format!("`{arg}` not found on $PATH (give a path instead?)"))?
    };
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    Ok(path.to_string_lossy().into_owned())
}

/// Upsert `KEY=VAL` into an env list, replacing an existing `KEY=`.
fn set_env(env: &mut Vec<String>, kv: &str) {
    let key = kv.split('=').next().unwrap_or(kv);
    let prefix = format!("{key}=");
    env.retain(|e| !e.starts_with(&prefix));
    env.push(kv.to_string());
}
