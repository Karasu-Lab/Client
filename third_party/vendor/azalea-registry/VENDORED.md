# Vendored azalea-registry

Source: `azalea-registry` 0.16.0+mc26.2 from <https://github.com/azalea-rs/azalea>
at rev `ffedf17c9b6ff9fbafbf7e0d14c8d2366b2d0a32`, MIT (see `LICENSE.md`).

## Changes from upstream

- `Cargo.toml`, `azalea-registry-macros/Cargo.toml`: workspace inheritance replaced
  with the values of the upstream workspace, so the crate builds outside it.
- `src/identifier.rs`: the layout assertion `size_of::<Identifier>() == 24` is
  `3 * size_of::<usize>()`. `Identifier` is an `Option<NonZeroUsize>` and a
  `Box<str>`, which is 12 bytes on `wasm32-unknown-unknown`; nothing depends on the
  64-bit size.
