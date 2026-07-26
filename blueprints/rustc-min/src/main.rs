#![feature(rustc_private)]
#![no_main]

use std::fs;
use std::path::Path;
use std::process::ExitCode;

const WORKERS: usize = 1;
const TIER: &str = "min";
const SMOKE_SOURCE: &str = r#"#![feature(no_core)]
#![no_core]

#[unsafe(no_mangle)]
pub extern "C" fn trueos_rustc_smoke_answer() -> usize {
    42
}
"#;

struct CompilerCallbacks;

impl rustc_driver_impl::Callbacks for CompilerCallbacks {}

fn has_threads_option(args: &[String]) -> bool {
    args.iter().any(|arg| arg.starts_with("-Zthreads="))
        || args.windows(2).any(|pair| {
            pair[0] == "-Z" && (pair[1] == "threads" || pair[1].starts_with("threads="))
        })
}

fn compiler_args() -> Result<Vec<String>, String> {
    let mut args: Vec<String> = trueos::env::args().collect();
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
    if !has_threads_option(&args) {
        args.push(format!("-Zthreads={WORKERS}"));
    }
    Ok(args)
}

fn compile() -> Result<(), String> {
    let args = compiler_args()?;
    let exit = rustc_driver_impl::catch_with_exit_code(|| {
        rustc_driver_impl::run_compiler(&args, &mut CompilerCallbacks)
    });
    if exit == ExitCode::SUCCESS {
        Ok(())
    } else {
        Err(format!("rustc exited with {exit:?}"))
    }
}

fn run() -> bool {
    println!("TRUEOS rustc {TIER}: nightly-2026-07-10 af3d95584 workers={WORKERS}");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build TRUEOS Tokio runtime");
    let result = runtime.block_on(async {
        tokio::task::spawn_blocking(compile)
            .await
            .map_err(|err| format!("compiler lane failed: {err}"))?
    });
    match result {
        Ok(()) => {
            println!("TRUEOS rustc {TIER}: smoke compile passed");
            true
        }
        Err(err) => {
            eprintln!("TRUEOS rustc {TIER}: {err}");
            false
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const core::ffi::c_char) -> i32 {
    if run() { 0 } else { 1 }
}
