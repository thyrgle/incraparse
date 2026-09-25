# Publishing checklist

The increparse workspace publishes five crates to crates.io in
dependency order. This document is the runbook. (declint, the sibling
workspace, depends on `increparse` and `increparse-lsp` — publish this
workspace first.)

## One-time setup

- [ ] Create a crates.io account and `cargo login`.
- [ ] Verify `repository` in every `crates/*/Cargo.toml` points at
      `https://github.com/thyrgle/increparse`.
- [ ] Optional: `rustup install stable` is enough; no nightly needed.

## Versioning model

Unlike declint, versions here are **per-crate**, not shared:

| Crate | Depends on |
|-------|------------|
| `increparse` | — |
| `increparse-lsp` | `increparse` |
| `increparse-nom` | `increparse` |
| `increparse-chumsky` | `increparse` |
| `increparse-lua` | `increparse`, `increparse-lsp` |

Rules of thumb:

- Bump a crate's own version when *its* content changes.
- Bump `increparse`'s version requirement in the four dependents
  whenever `increparse` itself is re-published with changes they should
  pick up (0.x requirements tie the minor version: `0.1.0` means
  `>=0.1.0, <0.2.0`).
- A breaking `increparse` change therefore bumps all five crates at
  once.

## Per release

1. `cargo test --workspace && cargo clippy --workspace --all-targets`
   must be clean.
2. Dry-run each crate and eyeball the payload:

   ```sh
   cargo package -p increparse --list
   cargo package -p increparse-lsp --list
   cargo package -p increparse-nom --list
   cargo package -p increparse-chumsky --list
   cargo package -p increparse-lua --list
   ```

3. Publish in dependency order (lsp before lua; nom/chumsky anywhere
   after core):

   ```sh
   cargo publish -p increparse
   cargo publish -p increparse-lsp
   cargo publish -p increparse-nom
   cargo publish -p increparse-chumsky
   cargo publish -p increparse-lua
   ```

4. Tag and push:

   ```sh
   git tag v0.1.0-core && git push origin v0.1.0-core
   ```

   (adjust per release; the README links to docs.rs, which goes live
   automatically on first publish and fixes the doc links.)

## Notes

- The license **texts** live at the workspace root and are not copied
  into member packages (cargo only auto-includes license files from a
  package's own directory). That is fine: the `license` field in each
  manifest is what crates.io records and displays.
- `increparse-lua` vendors Lua C sources via mlua's `vendored` feature,
  so publishing needs no system Lua, but *building* it requires a C
  compiler (CI runners have one).
- New crates.io accounts must verify their email before `cargo publish`
  works.
- If a publish fails mid-way, re-running `cargo publish` for the failed
  crate is safe; crates.io rejects exact duplicates.
- declint (sibling workspace) requires `increparse` and
  `increparse-lsp` from crates.io — after bumping versions here, update
  declint's requirements before its next release.
