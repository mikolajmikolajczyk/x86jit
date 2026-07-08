//! `x86jit-cli` — run a host x86-64 Linux binary under the recompiler, **no
//! recompilation**. Point it at an ELF on your system; its shared libraries are
//! served straight from the host rootfs (`/` by default), so a normal dynamic
//! binary (`/usr/bin/echo`, coreutils, …) runs as-is under the interpreter or JIT.
//!
//! It's glue over [`x86jit_run::run_config_argv_stdin`]: the OCI runner already
//! loads dynamic ELFs and resolves their interpreter + `DT_NEEDED` libs inside a
//! rootfs — a host binary is just that with `rootfs = /`.
//!
//! A guest that hits an unimplemented syscall or instruction surfaces as a clear
//! `guest trapped: …` — that message *is* the "what's missing to run this" answer.

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use x86jit_oci::ImageConfig;
use x86jit_run::{run_config_argv_stdin_features, CpuFeatures, EngineKind, RunResult};

const HELP: &str = "\
x86jit-cli — run a host x86-64 binary under the x86jit recompiler (no recompilation)

USAGE:
    x86jit-cli [OPTIONS] <BINARY> [GUEST_ARGS]...

ARGS:
    <BINARY>         An x86-64 ELF: an absolute/relative path, or a bare name found
                     on $PATH. Its shared libraries are served from the rootfs.
    [GUEST_ARGS]...  Arguments passed to the guest program (its argv[1..]).
                     Everything after <BINARY> goes to the guest verbatim.

OPTIONS:
    -b, --backend <interp|jit>   Engine (default: jit).
        --cpu <LEVEL>            Guest CPU feature level: baseline|v2|v3|v4|default
                                 (default: the built-in set — SSSE3+AVX2, no SSE4/
                                 AVX-512). `v4` advertises AVX-512; a guest then
                                 traps on any AVX-512 op the lifter can't execute.
    -r, --rootfs <DIR>           Filesystem the guest sees (default: /).
    -L, --lib <DIR>              Extra library search dir, prepended to
                                 LD_LIBRARY_PATH. Repeatable.
    -e, --env <KEY=VAL>          Set/override a guest env var. Repeatable.
        --no-inherit-env         Start from an empty environment.
    -q, --quiet                  Suppress the one-line run summary on stderr.
    -h, --help                   Show this help.

EXAMPLES:
    x86jit-cli /usr/bin/echo hello world
    x86jit-cli -b interp ls -la /tmp
    echo hi | x86jit-cli /usr/bin/cat

NOTE: file syscalls hit REAL host files under the rootfs — a writing guest writes
      to your disk. Point --rootfs at a throwaway tree if that matters.
";

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("x86jit-cli: {msg}");
    eprintln!("try `x86jit-cli --help`");
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

struct Args {
    backend: EngineKind,
    rootfs: String,
    libs: Vec<String>,
    envs: Vec<String>,
    inherit_env: bool,
    quiet: bool,
    features: CpuFeatures,
    binary: String,
    guest_args: Vec<String>,
}

/// Parse argv. `Ok(None)` means help was printed (exit success).
fn parse() -> Result<Option<Args>, String> {
    let mut backend = EngineKind::Jit;
    let mut rootfs = "/".to_string();
    let (mut libs, mut envs) = (Vec::new(), Vec::new());
    let (mut inherit_env, mut quiet) = (true, false);
    let mut features = CpuFeatures::default();
    let mut binary: Option<String> = None;
    let mut guest_args = Vec::new();

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        // Once the program is chosen, everything else is the guest's argv verbatim.
        if binary.is_some() {
            guest_args.push(a);
            continue;
        }
        let mut want = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "-b" | "--backend" => {
                backend = match want("--backend")?.as_str() {
                    "interp" | "interpreter" => EngineKind::Interpreter,
                    "jit" => EngineKind::Jit,
                    other => return Err(format!("unknown backend `{other}` (interp|jit)")),
                }
            }
            "--cpu" => {
                features = match want("--cpu")?.as_str() {
                    "baseline" | "v1" => CpuFeatures::baseline(),
                    "v2" => CpuFeatures::v2(),
                    "v3" => CpuFeatures::v3(),
                    "v4" => CpuFeatures::v4(),
                    "default" => CpuFeatures::default(),
                    other => {
                        return Err(format!(
                            "unknown --cpu `{other}` (baseline|v2|v3|v4|default)"
                        ))
                    }
                }
            }
            "-r" | "--rootfs" => rootfs = want("--rootfs")?,
            "-L" | "--lib" => libs.push(want("--lib")?),
            "-e" | "--env" => envs.push(want("--env")?),
            "--no-inherit-env" => inherit_env = false,
            "-q" | "--quiet" => quiet = true,
            s if s.starts_with('-') && s != "-" => return Err(format!("unknown option `{s}`")),
            _ => binary = Some(a),
        }
    }
    let binary = binary.ok_or("missing <BINARY>")?;
    Ok(Some(Args {
        backend,
        rootfs,
        libs,
        envs,
        inherit_env,
        quiet,
        features,
        binary,
        guest_args,
    }))
}

fn run(args: Args) -> ExitCode {
    let prog = match resolve_binary(&args.binary) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };

    let mut env: Vec<String> = if args.inherit_env {
        std::env::vars().map(|(k, v)| format!("{k}={v}")).collect()
    } else {
        Vec::new()
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

    let label = match args.backend {
        EngineKind::Interpreter => "interp",
        EngineKind::Jit => "jit",
    };
    match run_config_argv_stdin_features(
        &cfg,
        Path::new(&args.rootfs),
        args.backend,
        &argv,
        &stdin,
        args.features,
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

fn main() -> ExitCode {
    match parse() {
        Ok(Some(args)) => run(args),
        Ok(None) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}
