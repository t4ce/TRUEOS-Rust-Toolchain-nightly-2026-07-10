# TRUEOS in-process compiler source

This archive is the exact `nightly-2026-07-10` toolchain:

- rustc `1.99.0-nightly`
- commit `af3d95584dbddcae597890340995509a7fb47a50`
- LLVM `22.1.8`

`lib/rustlib/rustc-src` comes from the matching `rustc-dev` component. TRUEOS
Blueprints build `rustc_driver_impl` from these sources so the compiler frontend
is a native relocatable Blueprint module rather than an embedded Linux process.

The bundled Cranelift source is built as an `rlib` and linked into the med+
Blueprint. Its unused `rustc_driver` sysroot anchor is removed because the
Blueprint links the same `rustc_driver_impl` graph directly. This keeps LLVM and
the C++ runtime out of the initial native code-generation path.

The Blueprint build supplies the bootstrap identity environment and verifies the
full rustc commit before compiling these sources.
