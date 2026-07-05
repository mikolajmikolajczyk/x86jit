//! OCI-4: shell entrypoints — `busybox sh -c <cmd>`. A single command `execve`s
//! (process replacement, no fork); pipelines and `$(...)` fork children joined by
//! pipes, reaped via wait4. The native leg runs only for a shell builtin (no
//! rootfs-relative command to resolve); external commands and pipelines resolve
//! paths inside the rootfs a host subprocess can't see, so `interp == jit` against
//! the known output is the oracle there.

mod common;
use common::{oci, Native};

/// A shell builtin (`echo`) — single process, no exec. Native oracle available.
#[test]
fn sh_builtin_three_ways() {
    oci("busybox-musl.tar", "sh-builtin")
        .sh("echo hello from the shell")
        .native(Native::Host)
        .expect_stdout(b"hello from the shell\n")
        .run();
}

/// An external command — the shell `execve`s it (process replacement, no fork).
#[test]
fn sh_execve_command_interp_eq_jit() {
    oci("busybox-musl.tar", "sh-exec")
        .sh("/bin/busybox echo executed via execve")
        .expect_stdout(b"executed via execve\n")
        .expect_exit(0)
        .run();
}

/// A pipeline: `echo hello | cat` forks two children joined by a pipe (OCI-4).
/// Exercises pipe + fork + wait4 + fd inheritance across the whole runner.
#[test]
fn sh_pipeline_echo_cat_interp_eq_jit() {
    oci("busybox-musl.tar", "sh-pipe")
        .sh("echo hello | cat")
        .expect_stdout(b"hello\n")
        .expect_exit(0)
        .run();
}

/// Command substitution `$(...)`: the shell forks a child, captures its stdout via a
/// pipe, and splices it into the command line — fork + pipe + wait with the parent
/// as the reader (spec §6).
#[test]
fn sh_command_substitution_interp_eq_jit() {
    oci("busybox-musl.tar", "sh-cmdsub")
        .sh("echo out-$(echo inner)")
        .expect_stdout(b"out-inner\n")
        .run();
}

/// A two-stage pipeline through a real applet: `printf` (builtin) into `grep` (an
/// external applet the child execve's).
#[test]
fn sh_pipeline_grep_interp_eq_jit() {
    oci("busybox-musl.tar", "sh-grep")
        .sh("printf 'a\\nb\\nc\\n' | grep b")
        .expect_stdout(b"b\n")
        .run();
}
