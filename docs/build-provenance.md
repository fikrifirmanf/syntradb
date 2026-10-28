# Build fingerprints

The extension and benchmark runner now embed a SHA-256 source fingerprint at
build time. A benchmark reads the extension's fingerprint and compares it with
the runner's embedded value before loading data. A mismatch stops the run and
asks for both binaries to be rebuilt from unchanged sources.

Reports record `extension_source_fingerprint`, `runner_source_fingerprint`, and
`source_fingerprint_schema`. Earlier reports predate these fields. Do not assign
the current fingerprint to an older report.

Version 1 sorts relative paths by their components, then hashes names and contents.
For example, `replay/checkpoint.rs` sorts before `replay.rs`. This matches Rust
`PathBuf` ordering; an independent verifier must use the same order. Unsigned
64-bit little-endian lengths precede each name and each file's contents.
The domain prefix is `syntra-source-v1` plus a zero
byte. File names use forward slashes. Duplicate paths count once. The build rejects
symlinks and paths with traversal rather than silently following external sources.

The input set includes root Cargo manifests and lockfile, the Rust toolchain and
Cargo config, crate Rust/SQL/manifests/control files, binary fixtures, build support,
shell scripts, and SQL examples included by integration tests. Build output and
Markdown are excluded. Tests check an
independent Python SHA-256 fixture, changed bytes and names, path order, duplicate
paths, different checkout locations, traversal, symlinks, and mismatched binaries.

The fingerprint identifies the declared repository source inputs. It is not a
binary checksum, signature, or complete hermetic build record. Compiler profile,
PostgreSQL build flags, host architecture and runtime settings remain separate
report fields. Dependency contents rely on the pinned lockfile and Cargo checksums.
External compiler and system library changes can affect performance without
changing this source fingerprint.
