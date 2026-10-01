//! R110: embed the git commit and its date into the binary so `dbxt --version`
//! and the in-TUI About dialog can name the exact round a user is running.
//!
//! Written by hand — no `vergen` or any other new dependency. It is a
//! best-effort `git` invocation: when `git` or the repository is missing (a
//! crates.io / source-tarball build), nothing is emitted, `option_env!` in the
//! source yields `None`, and the extra fields are simply omitted. A release
//! build may pre-set `DBXT_GIT_SHA` / `DBXT_BUILD_DATE` in its environment;
//! those are never overridden here.

use std::process::Command;

fn main() {
    // Re-run when the checkout moves so the embedded SHA stays current.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    println!("cargo:rerun-if-env-changed=DBXT_GIT_SHA");
    println!("cargo:rerun-if-env-changed=DBXT_BUILD_DATE");

    if std::env::var_os("DBXT_GIT_SHA").is_none() {
        if let Some(sha) = git(&["rev-parse", "--short", "HEAD"]) {
            println!("cargo:rustc-env=DBXT_GIT_SHA={sha}");
        }
    }
    if std::env::var_os("DBXT_BUILD_DATE").is_none() {
        // The commit's own date (not the wall-clock build time) keeps the build
        // reproducible and works on every platform without a `date` command.
        if let Some(date) = git(&["log", "-1", "--format=%cd", "--date=short"]) {
            println!("cargo:rustc-env=DBXT_BUILD_DATE={date}");
        }
    }
}

/// Run `git` with `args` and return its trimmed, non-empty stdout, or `None`
/// when git is unavailable, the command fails, or the output is blank.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}
