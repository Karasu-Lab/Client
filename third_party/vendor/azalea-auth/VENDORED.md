# Vendored azalea-auth

Source: `azalea-auth` 0.16.0+mc26.2 from <https://github.com/azalea-rs/azalea>
at rev `ffedf17c9b6ff9fbafbf7e0d14c8d2366b2d0a32`, MIT (see `LICENSE.md`).

## Changes from upstream

- `Cargo.toml`: workspace inheritance replaced with the values of the upstream
  workspace, so the crate builds outside it. `tokio` (with its `fs` feature, which
  tokio rejects on `wasm32`) is only a dependency of the `online-mode` feature,
  the only code that uses it. Without that feature the crate is `game_profile`
  alone, which `azalea-inventory` needs and which builds for the browser.
- The lint table of the upstream workspace is not applied.
