#![feature(rustc_private)]
#![no_main]

mod blueprint;

use core::ffi::c_char;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

const ARGV0: &str = "rustc-min";
const MAX_COMPILER_ARGS: usize = 4096;
const MAX_COMPILER_ARG_BYTES: usize = 1024 * 1024;
const WORKERS: usize = 1;
const TIER: &str = "min";
const TARGET: &str = "rustc-sysroot/x86_64-unknown-trueos.json";
const TARGET_TRIPLE: &str = "x86_64-unknown-trueos";
const SYSROOT: &str = "rustc-sysroot";
const SYSROOT_MANIFEST: &str = "rustc-sysroot/TRUEOS_SYSROOT_MANIFEST";
const PAYLOAD_LIB: &str = "rustc-payload/lib";
const TRUEOS_HELLO_SOURCE: &str = r#"#![no_std]
#![no_main]

use trueos::logl::level;

unsafe extern "C" {
    fn trueos_cabi_write(stream: u32, bytes: *const u8, len: usize);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    let message = b"hello_world: rustc\n";
    let stream = if level::INFO >= 3 { 1 } else { 2 };
    unsafe {
        trueos_cabi_write(stream, message.as_ptr(), message.len());
    }
}
"#;

struct CompilerCallbacks {
    in_memory_smoke: bool,
}

impl rustc_driver_impl::Callbacks for CompilerCallbacks {
    fn config(&mut self, config: &mut rustc_interface::Config) {
        println!("TRUEOS rustc min: compiler config entered");
        config.make_codegen_backend = Some(Box::new(|_| {
            rustc_codegen_cranelift::__rustc_codegen_backend()
        }));
        if self.in_memory_smoke {
            config.input = rustc_session::config::Input::Str {
                name: rustc_span::FileName::Custom("rustc-work/trueos-hello.rs".into()),
                input: TRUEOS_HELLO_SOURCE.to_owned(),
            };
        }
        config.psess_created = Some(Box::new(|_| {
            println!("TRUEOS rustc min: compiler session created");
        }));
    }

    fn after_crate_root_parsing(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        _krate: &mut rustc_ast::ast::Crate,
    ) -> rustc_driver_impl::Compilation {
        println!("TRUEOS rustc min: crate root parsed");
        rustc_driver_impl::Compilation::Continue
    }

    fn after_expansion<'tcx>(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        _tcx: rustc_middle::ty::TyCtxt<'tcx>,
    ) -> rustc_driver_impl::Compilation {
        println!("TRUEOS rustc min: expansion complete");
        rustc_driver_impl::Compilation::Continue
    }

    fn after_analysis<'tcx>(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        _tcx: rustc_middle::ty::TyCtxt<'tcx>,
    ) -> rustc_driver_impl::Compilation {
        println!("TRUEOS rustc min: analysis complete");
        rustc_driver_impl::Compilation::Continue
    }
}

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

fn extern_crate_name(value: &str) -> &str {
    value
        .split_once('=')
        .map_or(value, |(name, _)| name)
        .rsplit(':')
        .next()
        .unwrap_or(value)
}

fn has_extern_crate(args: &[String], crate_name: &str) -> bool {
    args.iter().enumerate().any(|(index, arg)| {
        if let Some(value) = arg.strip_prefix("--extern=") {
            extern_crate_name(value) == crate_name
        } else if arg == "--extern" {
            args.get(index + 1)
                .is_some_and(|value| extern_crate_name(value) == crate_name)
        } else {
            false
        }
    })
}

#[derive(Clone)]
struct EmbeddedMetadata {
    crate_name: String,
    file_name: String,
}

struct EmbeddedDirectExtern {
    alias: String,
    crate_name: String,
    file_name: String,
}

struct EmbeddedManifest {
    sysroot: Vec<EmbeddedMetadata>,
    payload: Vec<EmbeddedMetadata>,
    direct_externs: Vec<EmbeddedDirectExtern>,
}

fn valid_crate_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn validate_metadata_file(crate_name: &str, file_name: &str) -> Result<(), String> {
    if !valid_crate_name(crate_name)
        || file_name.contains('/')
        || file_name.contains('\\')
        || !file_name.starts_with(&format!("lib{crate_name}-"))
        || !file_name.ends_with(".rmeta")
    {
        return Err(format!(
            "unsafe metadata entry in {SYSROOT_MANIFEST}: {crate_name} {file_name}"
        ));
    }
    Ok(())
}

fn manifest_count(value: &str, key: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| format!("bad {key} in {SYSROOT_MANIFEST}: {value}"))
}

impl EmbeddedManifest {
    fn load() -> Result<Self, String> {
        let raw = fs::read_to_string(SYSROOT_MANIFEST)
            .map_err(|err| format!("read embedded sysroot manifest {SYSROOT_MANIFEST}: {err}"))?;
        let mut format = None;
        let mut target = None;
        let mut metadata_count = None;
        let mut payload_metadata_count = None;
        let mut payload_extern_count = None;
        let mut sysroot = Vec::new();
        let mut payload = Vec::new();
        let mut direct_externs = Vec::new();
        let mut rows = BTreeSet::new();

        for line in raw.lines() {
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("malformed line in {SYSROOT_MANIFEST}: {line}"));
            };
            match key {
                "format" => format = Some(value),
                "target" => target = Some(value),
                "metadata-count" => metadata_count = Some(manifest_count(value, key)?),
                "payload-metadata-count" => {
                    payload_metadata_count = Some(manifest_count(value, key)?)
                }
                "payload-extern-count" => payload_extern_count = Some(manifest_count(value, key)?),
                "metadata" | "payload" => {
                    let fields = value.split_ascii_whitespace().collect::<Vec<_>>();
                    if fields.len() != 2 {
                        return Err(format!("malformed {key} entry in {SYSROOT_MANIFEST}"));
                    }
                    validate_metadata_file(fields[0], fields[1])?;
                    if !rows.insert((key, fields[0], fields[1])) {
                        return Err(format!("duplicate {key} entry in {SYSROOT_MANIFEST}"));
                    }
                    let entry = EmbeddedMetadata {
                        crate_name: fields[0].to_owned(),
                        file_name: fields[1].to_owned(),
                    };
                    if key == "metadata" {
                        sysroot.push(entry);
                    } else {
                        payload.push(entry);
                    }
                }
                "extern" => {
                    let fields = value.split_ascii_whitespace().collect::<Vec<_>>();
                    if fields.len() != 3
                        || !valid_crate_name(fields[0])
                        || !valid_crate_name(fields[1])
                    {
                        return Err(format!("malformed extern entry in {SYSROOT_MANIFEST}"));
                    }
                    validate_metadata_file(fields[1], fields[2])?;
                    if !rows.insert((key, fields[0], fields[2])) {
                        return Err(format!("duplicate extern entry in {SYSROOT_MANIFEST}"));
                    }
                    direct_externs.push(EmbeddedDirectExtern {
                        alias: fields[0].to_owned(),
                        crate_name: fields[1].to_owned(),
                        file_name: fields[2].to_owned(),
                    });
                }
                _ => {}
            }
        }

        if format != Some("trueos-rustc-sysroot-v1") {
            return Err(format!("unsupported format in {SYSROOT_MANIFEST}"));
        }
        if target != Some(TARGET_TRIPLE) {
            return Err(format!("wrong target in {SYSROOT_MANIFEST}"));
        }
        if metadata_count != Some(sysroot.len())
            || payload_metadata_count != Some(payload.len())
            || payload_extern_count != Some(direct_externs.len())
        {
            return Err(format!("metadata count mismatch in {SYSROOT_MANIFEST}"));
        }
        for direct in &direct_externs {
            if !payload.iter().any(|artifact| {
                artifact.crate_name == direct.crate_name && artifact.file_name == direct.file_name
            }) {
                return Err(format!(
                    "direct extern {} has no payload artifact in {SYSROOT_MANIFEST}",
                    direct.alias
                ));
            }
        }

        Ok(Self {
            sysroot,
            payload,
            direct_externs,
        })
    }

    fn sysroot_path(&self, crate_name: &str) -> Result<String, String> {
        let mut matches = self
            .sysroot
            .iter()
            .filter(|metadata| metadata.crate_name == crate_name);
        let metadata = matches.next().ok_or_else(|| {
            format!("embedded sysroot manifest contains no metadata for {crate_name}")
        })?;
        if matches.next().is_some() {
            return Err(format!(
                "embedded sysroot manifest contains ambiguous metadata for {crate_name}"
            ));
        }
        Ok(format!(
            "{SYSROOT}/lib/rustlib/{TARGET_TRIPLE}/lib/{}",
            metadata.file_name
        ))
    }
}

fn checked_metadata_file(path: &str, label: &str) -> Result<u64, String> {
    let metadata = fs::metadata(path)
        .map_err(|err| format!("inspect embedded {label} metadata {path}: {err}"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!(
            "embedded {label} metadata is not a non-empty file: {path}"
        ));
    }
    Ok(metadata.len())
}

fn compiler_args(mut args: Vec<String>) -> Result<Vec<String>, String> {
    let default_smoke = args.len() == 1;
    if default_smoke {
        let work = Path::new("rustc-work");
        fs::create_dir_all(work).map_err(|err| format!("create rustc-work: {err}"))?;
        let source = work.join("trueos-hello.rs");
        args.extend([
            source.to_string_lossy().into_owned(),
            "--crate-name".into(),
            "trueos_rustc_hello_smoke".into(),
            "--edition=2024".into(),
            "--crate-type".into(),
            "lib".into(),
            "--emit=obj".into(),
            "-Cpanic=abort".into(),
            "-Cdebuginfo=0".into(),
            "-Ccodegen-units=1".into(),
            "-Zno-parallel-backend".into(),
            "-o".into(),
            work.join("trueos-rustc-hello.o")
                .to_string_lossy()
                .into_owned(),
        ]);
    }
    if default_smoke {
        let manifest = EmbeddedManifest::load()?;
        for crate_name in ["core", "compiler_builtins", "alloc"] {
            if has_extern_crate(&args, crate_name) {
                continue;
            }
            let metadata = manifest.sysroot_path(crate_name)?;
            checked_metadata_file(&metadata, crate_name)?;
            println!("TRUEOS rustc min: {crate_name} metadata {metadata}");
            args.extend(["--extern".into(), format!("{crate_name}={metadata}")]);
        }
        for artifact in &manifest.sysroot {
            if matches!(
                artifact.crate_name.as_str(),
                "core" | "compiler_builtins" | "alloc"
            ) {
                continue;
            }
            let path = format!(
                "{SYSROOT}/lib/rustlib/{TARGET_TRIPLE}/lib/{}",
                artifact.file_name
            );
            args.extend([
                "--extern".into(),
                format!("noprelude,nounused:{}={path}", artifact.crate_name),
            ]);
        }
        for artifact in &manifest.payload {
            let path = format!("{PAYLOAD_LIB}/{}", artifact.file_name);
            args.extend([
                "--extern".into(),
                format!("noprelude,nounused:{}={path}", artifact.crate_name),
            ]);
        }
        for direct in &manifest.direct_externs {
            let path = format!("{PAYLOAD_LIB}/{}", direct.file_name);
            let bytes = checked_metadata_file(&path, direct.alias.as_str())?;
            println!(
                "TRUEOS rustc min: direct extern {}={} bytes={bytes}",
                direct.alias, path
            );
            args.extend(["--extern".into(), format!("{}={path}", direct.alias)]);
        }
        if !manifest.sysroot.is_empty() || !manifest.payload.is_empty() {
            println!(
                "TRUEOS rustc min: sysroot metadata={} payload metadata={} direct={}",
                manifest.sysroot.len(),
                manifest.payload.len(),
                manifest.direct_externs.len()
            );
        }
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

fn compile(
    args: Vec<String>,
    in_memory_smoke: bool,
) -> Result<Option<blueprint::PublishedBlueprint>, String> {
    println!("TRUEOS rustc min: compiler entered");
    let exit = rustc_driver_impl::catch_with_exit_code(|| {
        rustc_driver_impl::run_compiler(&args, &mut CompilerCallbacks { in_memory_smoke })
    });
    if exit != ExitCode::SUCCESS {
        return Err(format!("rustc exited with {exit:?}"));
    }
    if in_memory_smoke {
        blueprint::publish(Path::new("rustc-work/trueos-rustc-hello.o")).map(Some)
    } else {
        Ok(None)
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
        tokio::task::spawn_blocking(move || compile(args, default_smoke))
            .await
            .map_err(|err| format!("compiler lane failed: {err}"))?
    });
    match result {
        Ok(Some(published)) => {
            println!("TRUEOS rustc {TIER}: trueos hello Blueprint compile passed");
            println!(
                "TRUEOS rustc {TIER}: Blueprint stored path={} bytes={} object={} entry=sec:{}+0x{:x}",
                blueprint::OUTPUT_PATH,
                published.blueprint_bytes,
                published.object_bytes,
                published.entry_hint >> 32,
                published.entry_hint as u32,
            );
            true
        }
        Ok(None) => {
            println!("TRUEOS rustc {TIER}: compiler invocation passed");
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
