# Computer Use Extension Atlas

## Purpose

`codex-rs/ext/computer-use/` owns the fork-specific Computer Use extension crate,
its MCP sidecar implementation, and the bounded proprietary payload
metadata under `vendor/openai/`.

## Durable rules

- Keep new runtime behavior out of `codex-core`.
- Reuse the existing extension-api MCP contribution seam and app-server
  extension installation seam instead of creating a parallel owner.
- `vendor/openai/PROVENANCE.json` is the canonical vendored-payload manifest.
  Change vendored bytes only after exact hash/size verification against the
  approved source frontier, and update the manifest in the same change.
- Keep unit tests in dedicated `*_tests.rs` files. Keep host/Xvfb smoke tests in
  integration tests or ignored host-capability targets, not inline unit tests.
