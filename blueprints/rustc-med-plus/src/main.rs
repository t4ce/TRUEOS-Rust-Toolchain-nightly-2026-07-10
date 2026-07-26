#![feature(rustc_private)]
#![no_main]

use std::fs;
use std::path::Path;
use std::process::ExitCode;

const WORKERS: usize = 4;
const TIER: &str = "med+";
const TARGET: &str = "rustc-sysroot/x86_64-unknown-trueos.json";
const SYSROOT: &str = "rustc-sysroot";
const SMOKE_SOURCE: &str = r#"fn main() {
    println!("Hello from rustc + Cranelift inside a TRUEOS Blueprint!");
}
"#;

struct CompilerCallbacks;

impl rustc_driver_impl::Callbacks for CompilerCallbacks {
    fn config(&mut self, config: &mut rustc_interface::interface::Config) {
        config.make_codegen_backend = Some(Box::new(|_| {
            rustc_codegen_cranelift::__rustc_codegen_backend()
        }));
    }
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

fn compiler_args() -> Result<Vec<String>, String> {
    let mut args: Vec<String> = trueos::env::args().collect();
    if args.len() == 1 {
        let work = Path::new("rustc-work");
        fs::create_dir_all(work).map_err(|err| format!("create rustc-work: {err}"))?;
        let source = work.join("hello.rs");
        fs::write(&source, SMOKE_SOURCE).map_err(|err| format!("write hello smoke: {err}"))?;
        args.extend([
            source.to_string_lossy().into_owned(),
            "--crate-name".into(),
            "trueos_hello".into(),
            "--crate-type".into(),
            "bin".into(),
            "--emit=obj".into(),
            "-o".into(),
            work.join("hello.o").to_string_lossy().into_owned(),
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
            println!("TRUEOS rustc {TIER}: Hello World object compile passed");
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
