# TrueOS native rustc Blueprints

These packages turn the exact archived compiler source into three independently
packable TrueOS applications. They are pinned to:

- `rustc 1.99.0-nightly (af3d95584 2026-07-09)`
- commit `af3d95584dbddcae597890340995509a7fb47a50`
- Tokio `=1.52.3`

`rustc-min` is the frontend/no-core proof. `rustc-med` embeds the target metadata
sysroot and typechecks an ordinary `std` Hello program. `rustc-med-plus` adds a
statically selected Cranelift backend and emits an x86-64 relocatable object.
Their default rustc worker counts are 1, 2, and 4 respectively.

From `/home/t4ce/REPOS/TRUEOS-Blueprints`, pack a tier with:

```text
TRUEOS_RUST_TOOLCHAIN_ROOT=/home/t4ce/REPOS/TRUEOS-Rust-Toolchain-nightly-2026-07-10 \
TRUEOS_BLUEPRINT_SKIP_APPS_PUBLISH=1 \
cargo bp /home/t4ce/REPOS/TRUEOS-Rust-Toolchain-nightly-2026-07-10/blueprints/rustc-med-plus
```

The three output files are placed in
`/home/t4ce/REPOS/TRUEOS-Blueprints/dist/`. The repository app catalog also
lists all three as optional applications.

Med and med+ do not depend on files left in their Cargo target directories.
Their target JSON, exact-toolchain manifest, and current-invocation build-std
metadata closure are embedded in `.trueos.assets`, authenticated, and
materialized into `rustc-sysroot` before entry. Min intentionally carries no
asset bundle.

This is not a general Unix process environment. Filesystem operations map to the
TrueOS filesystem contract; unsupported process, signal, dynamic-loader, and
virtual-memory operations fail explicitly. Med+ uses Cranelift and contains no
LLVM codegen or C++ runtime dependency.

## Validated artifacts

The final 2026-07-26 packages are:

| artifact | bytes | SHA-256 |
| --- | ---: | --- |
| `rustc-min.bp` | 17,528,865 | `17967d13f2ee2c65ac326b0c7fc3a71f58ef179a0346f48c3798cffa9a091d2f` |
| `rustc-med.bp` | 44,744,171 | `e2d63d338171ea2a4d800f12e6d2090cb0d1ee59f16f5d623a7fdb730b2ffc51` |
| `rustc-med-plus.bp` | 50,538,591 | `a452c8fd3e7a2db1f394e6b0cc14907744b6ff91127440c75e2e7bf2bdc543d5` |

The no-boot gate decompressed every package, matched both header lengths,
verified `ET_REL`/x86-64, validated every asset path and SHA-256, and reconstructed
the two packaged sysroots without using their Cargo target directories. The
exact compiler driver then:

- typechecked the ordinary Hello program from med's packaged sysroot;
- emitted a 5,216-byte ELF64 x86-64 relocatable Hello object from med+'s
  packaged sysroot through static Cranelift;
- found only loader-supported allocatable relocation kinds in every tier;
- found 118 imports per tier and zero imports missing from the typed/generated
  kernel resolvers.

The packer's 41 tests and the final `cargo check --bin TRUEOS` also pass. Med's
default `-Zno-codegen` output file is intentionally empty; successful frontend
and typechecking completion is its contract.
