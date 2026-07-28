# TrueOS native rustc Blueprints

This package turns the exact archived compiler source into a packable TrueOS
compiler appliance. It is pinned to:

- `rustc 1.99.0-nightly (af3d95584 2026-07-09)`
- commit `af3d95584dbddcae597890340995509a7fb47a50`
- Tokio `=1.52.3`

`rustc-min` is now the consolidated end-to-end proof. The former `rustc-med`
and `rustc-med-plus` bring-up packages were made redundant as min acquired their
authenticated sysroot and static Cranelift capabilities, then advanced beyond
them to validate and publish a compiler-produced Blueprint. Min remains at one
rustc `-Zthreads` query worker; Tokio's blocking carrier is a separate parked
supervisor.

From `/home/t4ce/REPOS/TRUEOS-Blueprints`, pack it with:

```text
TRUEOS_RUST_TOOLCHAIN_ROOT=/home/t4ce/REPOS/TRUEOS-Rust-Toolchain-nightly-2026-07-10 \
TRUEOS_BLUEPRINT_SKIP_APPS_PUBLISH=1 \
cargo bp /home/t4ce/REPOS/TRUEOS-Rust-Toolchain-nightly-2026-07-10/blueprints/rustc-min
```

The output is
`/home/t4ce/REPOS/TRUEOS-Blueprints/dist/rustc-min.bp`. The repository app
catalog lists it as an optional application.

The package does not depend on files left in its Cargo target directory. It
authenticates and embeds the target JSON at both the explicit `--target` path
and rustc's host-target sysroot fallback, the current invocation's complete
build-std metadata closure, and the selected TrueOS API payload. The loader
materializes them into `rustc-sysroot` and `rustc-payload` before entry.

The package sets the Blueprint `argv-entry-v1` capability. This makes the
kernel pass the loader's `argc/argv` to the compiler wrapper and consume its
normal `i32` status, while older unflagged applications retain the legacy entry
ABI. The packer also takes native linker inputs only from the root `.rlink`
closure and rejects `panic_unwind` in this `panic=abort` package.

This is not a general Unix process environment. Filesystem operations map to
the TrueOS filesystem contract; unsupported process, signal, dynamic-loader,
and virtual-memory operations fail explicitly. The compiler uses the statically
selected Cranelift backend and contains no LLVM codegen or C++ runtime
dependency.

With no compiler arguments, min maintains
`/common/localcompile/main.rs`, compiles it to a relocatable x86-64 object,
validates its imports, relocations, and `_start`, then publishes the raw
Blueprint `/common/localcompile/local.bp`. Explicit compiler arguments retain
the direct compiler-invocation path.

The current managed source is `apps/gridpaper/src/rustc_oneshot.rs`: a
self-contained 39×55 Gridpaper producer that submits one visible page and then
parks cooperatively. It intentionally calls the Gridpaper CABI directly.
Cranelift does not provide full-graph LTO, so compiling the complete Cargo
Gridpaper application from metadata alone would leave upstream Rust symbols for
a future in-process relocatable-link stage.

## Validated artifacts

The consolidated package used by the successful 2026-07-28 bare-metal run is:

| artifact | bytes | SHA-256 |
| --- | ---: | --- |
| `rustc-min.bp` | 57,067,454 | `4bf0a257de1739f11e1c96d9f8c5c827fafeae494f4c0b504d93d238c9e41267` |

That run authenticated 38 sysroot metadata artifacts and 22 TrueOS payload
artifacts, completed parsing, expansion, analysis, and Cranelift object
emission, published a 1,448-byte Blueprint containing a 1,424-byte object, and
launched the generated Blueprint successfully. Its observable result was
`hello_world: rustc`.

The valid default compiler path returns normally. A compiler-fatal diagnostic
still reaches the current `panic=abort` boundary rather than unwinding back to
the wrapper; recoverable compiler-error isolation remains outside this proof.
