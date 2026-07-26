#![feature(rustc_private)]
#![no_main]

use core::ffi::c_char;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

const ARGV0: &str = "rustc-min";
const MAX_COMPILER_ARGS: usize = 4096;
const MAX_COMPILER_ARG_BYTES: usize = 1024 * 1024;
const WORKERS: usize = 1;
const TIER: &str = "min";
const TARGET: &str = "rustc-sysroot/x86_64-unknown-trueos.json";
const SYSROOT: &str = "rustc-sysroot";
const SMOKE_SOURCE: &str = r#"#![feature(no_core, lang_items)]
#![no_core]
#![allow(internal_features)]

#[lang = "pointee_sized"]
pub trait PointeeSized {}

#[lang = "meta_sized"]
pub trait MetaSized: PointeeSized {}

#[lang = "sized"]
pub trait Sized: MetaSized {}

#[unsafe(no_mangle)]
pub extern "C" fn trueos_rustc_smoke_answer() {}
"#;

struct CompilerCallbacks;

impl rustc_driver_impl::Callbacks for CompilerCallbacks {}

/// Copies the loader-owned C argv while execution is still on the Hull carrier.
///
/// The Blueprint loader guarantees that every non-null argv entry points to a
/// NUL-terminated byte string for the duration of `_start`. The explicit bounds
/// keep a malformed loader contract from causing an unbounded scan.
unsafe fn capture_compiler_argv(
    argc: usize,
    argv: *const *const c_char,
) -> Result<Vec<String>, String> {
    if argc == 0 || argv.is_null() {
        return Ok(vec![ARGV0.to_owned()]);
    }
    if argc > MAX_COMPILER_ARGS {
        return Err(format!(
            "loader supplied {argc} arguments; limit is {MAX_COMPILER_ARGS}"
        ));
    }

    let mut args = Vec::with_capacity(argc);
    for index in 0..argc {
        let arg = unsafe { *argv.add(index) };
        if arg.is_null() {
            return Err(format!("loader argv[{index}] is null"));
        }

        let mut len = 0usize;
        while len < MAX_COMPILER_ARG_BYTES && unsafe { *arg.add(len) } != 0 {
            len += 1;
        }
        if len == MAX_COMPILER_ARG_BYTES {
            return Err(format!(
                "loader argv[{index}] exceeds {MAX_COMPILER_ARG_BYTES} bytes"
            ));
        }

        let bytes = unsafe { core::slice::from_raw_parts(arg.cast::<u8>(), len) };
        args.push(String::from_utf8_lossy(bytes).into_owned());
    }

    if let Some(argv0) = args.first_mut() {
        if argv0.is_empty() {
            *argv0 = ARGV0.to_owned();
        }
    } else {
        args.push(ARGV0.to_owned());
    }
    Ok(args)
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter()
        .any(|arg| arg == flag || arg.starts_with(&format!("{flag}=")))
}

fn has_threads_option(args: &[String]) -> bool {
    args.iter().any(|arg| arg.starts_with("-Zthreads="))
        || args.windows(2).any(|pair| {
            pair[0] == "-Z" && (pair[1] == "threads" || pair[1].starts_with("threads="))
        })
}

fn compiler_args(mut args: Vec<String>) -> Result<Vec<String>, String> {
    if args.len() == 1 {
        let work = Path::new("rustc-work");
        fs::create_dir_all(work).map_err(|err| format!("create rustc-work: {err}"))?;
        let source = work.join("min-smoke.rs");
        fs::write(&source, SMOKE_SOURCE).map_err(|err| format!("write min smoke: {err}"))?;
        args.extend([
            source.to_string_lossy().into_owned(),
            "--crate-name".into(),
            "trueos_rustc_min_smoke".into(),
            "--crate-type".into(),
            "rlib".into(),
            "--emit=metadata".into(),
            "-Zno-codegen".into(),
            "-o".into(),
            work.join("min-smoke.rmeta").to_string_lossy().into_owned(),
        ]);
    }
    if !has_flag(&args, "--target") {
        args.extend(["--target".into(), TARGET.into()]);
    }
    if !has_flag(&args, "--sysroot") {
        args.extend(["--sysroot".into(), SYSROOT.into()]);
    }
    if !has_threads_option(&args) {
        args.push(format!("-Zthreads={WORKERS}"));
    }
    if !has_flag(&args, "-Zunstable-options") {
        args.push("-Zunstable-options".into());
    }
    Ok(args)
}

fn compile(args: Vec<String>) -> Result<(), String> {
    let exit = rustc_driver_impl::catch_with_exit_code(|| {
        rustc_driver_impl::run_compiler(&args, &mut CompilerCallbacks)
    });
    if exit == ExitCode::SUCCESS {
        Ok(())
    } else {
        Err(format!("rustc exited with {exit:?}"))
    }
}

fn run(process_args: Vec<String>) -> bool {
    println!("TRUEOS rustc {TIER}: nightly-2026-07-10 af3d95584 workers={WORKERS}");
    let default_smoke = process_args.len() == 1;
    let args = match compiler_args(process_args) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("TRUEOS rustc {TIER}: {err}");
            return false;
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build TRUEOS Tokio runtime");
    let result = runtime.block_on(async move {
        tokio::task::spawn_blocking(move || compile(args))
            .await
            .map_err(|err| format!("compiler lane failed: {err}"))?
    });
    match result {
        Ok(()) => {
            if default_smoke {
                println!("TRUEOS rustc {TIER}: smoke compile passed");
            } else {
                println!("TRUEOS rustc {TIER}: compiler invocation passed");
            }
            true
        }
        Err(err) => {
            eprintln!("TRUEOS rustc {TIER}: {err}");
            false
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const c_char) -> i32 {
    let process_args = match unsafe { capture_compiler_argv(argc, argv) } {
        Ok(args) => args,
        Err(err) => {
            eprintln!("TRUEOS rustc {TIER}: {err}");
            return 1;
        }
    };
    if run(process_args) { 0 } else { 1 }
}
