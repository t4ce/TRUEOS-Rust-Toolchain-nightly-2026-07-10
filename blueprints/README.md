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
Those counts name rustc's `-Zthreads` query pool; Tokio's blocking carrier is a
separate parked supervisor.

From `/home/t4ce/REPOS/TRUEOS-Blueprints`, pack a tier with:

```text
TRUEOS_RUST_TOOLCHAIN_ROOT=/home/t4ce/REPOS/TRUEOS-Rust-Toolchain-nightly-2026-07-10 \
TRUEOS_BLUEPRINT_SKIP_APPS_PUBLISH=1 \
cargo bp /home/t4ce/REPOS/TRUEOS-Rust-Toolchain-nightly-2026-07-10/blueprints/rustc-med-plus
```

The three output files are placed in
`/home/t4ce/REPOS/TRUEOS-Blueprints/dist/`. The repository app catalog also
lists all three as optional applications.

None of the tiers depend on files left in their Cargo target directories. All
three authenticate and embed the target JSON at both the explicit `--target`
path and rustc's host-target sysroot fallback, then materialize it into
`rustc-sysroot` before entry. Min's bundle contains only that target contract
and its exact-toolchain manifest. Med and med+ additionally carry the current
invocation's build-std metadata closure.

All three packages set the Blueprint `argv-entry-v1` capability. This makes the
kernel pass the loader's `argc/argv` to the compiler wrapper and consume its
normal `i32` status, while older unflagged applications retain the legacy entry
ABI. The packer also takes native linker inputs only from the root `.rlink`
closure and rejects `panic_unwind` in these `panic=abort` packages.

This is not a general Unix process environment. Filesystem operations map to the
TrueOS filesystem contract; unsupported process, signal, dynamic-loader, and
virtual-memory operations fail explicitly. Med+ uses Cranelift and contains no
LLVM codegen or C++ runtime dependency.

The valid default smoke path returns normally. A compiler-fatal diagnostic
still reaches the current `panic=abort` boundary rather than unwinding back to
the wrapper; recoverable compiler-error isolation is outside this first proof.

## Validated artifacts

The final 2026-07-26 packages are:

| artifact | bytes | SHA-256 |
| --- | ---: | --- |
| `rustc-min.bp` | 17,521,563 | `9f87285d97cc67336cf876f73fdf0425f6837b7ede2da770ca54632be7345db0` |
| `rustc-med.bp` | 44,762,431 | `7025e63014bc7c9126d32e6cb887855aad124ac4c50ff2736e07e33da454290a` |
| `rustc-med-plus.bp` | 50,549,800 | `4f8768d00e53445890e469643878278928e0c49b491823b1510a4c83140baed2` |

The no-boot gate required the `0x0202` argv-entry-v1 header, decompressed every
package, matched both header lengths, verified `ET_REL`/x86-64, validated every
asset path and SHA-256, and reconstructed all three packaged sysroots without
using their Cargo target directories. The exact archived compiler then:

- emitted a 2,144-byte metadata file for min's `no_core` smoke;
- typechecked the ordinary Hello program from med's packaged sysroot;
- emitted a 4,520-byte ELF64 x86-64 relocatable Hello object from med+'s
  packaged sysroot;
- found only loader-supported allocatable relocation kinds in every tier;
- found an identical 115-import closure in every tier and no
  `_Unwind_RaiseException`.

The packaged med+ driver is independently built with its statically selected
Cranelift backend. The pack-time CABI guard accepted its two typed TrueOS
imports. The packer's 45 tests and the final `cargo check --bin TRUEOS` also
pass. Med's default `-Zno-codegen` output file is intentionally empty;
successful frontend and typechecking completion is its contract.
