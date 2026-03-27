# Cooldex

Cooldex is my upstream-aligned Codex fork with a thin layer of local addons.

The upstream Codex repository currently accepts external code contributions by invitation only, so this fork is where I keep the changes I actually want to run while still rebasing regularly on the official tree. The goal is not to replace upstream Codex or drift into a separate product line; it is to keep the official base intact and layer local behavior on top as clearly-scoped mods/addons.

## Install This Fork Locally

The reliable local install path for this fork is a direct Rust build from this checkout:

```sh
cd ~/work/codex/codex-rs
CARGO_TARGET_DIR="$HOME/.cache/cargo-target/codex-rs" cargo build -p codex-cli --bin codex
install -D -m 0755 "$HOME/.cache/cargo-target/codex-rs/debug/codex" "$HOME/.cargo/bin/codex"
hash -r
```

This assumes a working Rust toolchain, pins `CARGO_TARGET_DIR` so the build output and install path match, and installs the built binary into `$HOME/.cargo/bin/codex`.

The npm install path in the upstream README targets published `@openai/codex` artifacts. A plain Git-URL install of this fork is not documented here because the checked-out `codex-cli/` package does not ship the staged `vendor/` binaries that its Node launcher expects at runtime. If this fork ever gets its own staged or published npm artifacts, that install path can be documented separately.

## Local Addons

- `manage_context`: stricter retrieve/apply cleanup flow, `/sanitize` integration, and replacement-history handling wired to the live runtime docs.
- `recall`: args-less recall, compact/debug behavior, tighter rollout coupling, and fail-loud malformed-rollout handling.
- `prompt_gc` / `PromptGcSidecar`: automatic prompt compaction for regular lead turns with hidden sidecar execution and rollout-backed hydration.
- Resume transcript rendering: reconstructed resume history from persisted rollout turns, with optional truncation starting at the last surviving visible `Context compacted` marker.
- `/accounts`: multi-account ChatGPT auth management across core storage, TUI flows, and app-server surfaces.
- Sub-agent/runtime orchestration: local spawn/profile plumbing, background-agent handling, parallel tool execution, and collaboration/thread APIs beyond upstream.
- TUI debugging/operator surfaces: `/debug`, raw-response inspection, context-window visibility, and other operator-facing diagnostics.
- `mcp-standalone`: local bridge defaults for session cwd/config resolution, operator metadata plumbing, and runtime/auth expectations.
- Legacy Landlock override: preserve the local `use_legacy_landlock = true` path until upstream ships a safe writable-`gitdir` alternative.
- Workspace sync policy: keep the fork aligned with upstream while preserving local merge-safety invariants and removing `.github/**` from this workspace.

---

## Upstream README

<p align="center"><code>npm i -g @openai/codex</code><br />or <code>brew install --cask codex</code></p>
<p align="center"><strong>Codex CLI</strong> is a coding agent from OpenAI that runs locally on your computer.
<p align="center">
  <img src="https://github.com/openai/codex/blob/main/.github/codex-cli-splash.png" alt="Codex CLI splash" width="80%" />
</p>
</br>
If you want Codex in your code editor (VS Code, Cursor, Windsurf), <a href="https://developers.openai.com/codex/ide">install in your IDE.</a>
</br>If you want the desktop app experience, run <code>codex app</code> or visit <a href="https://chatgpt.com/codex?app-landing-page=true">the Codex App page</a>.
</br>If you are looking for the <em>cloud-based agent</em> from OpenAI, <strong>Codex Web</strong>, go to <a href="https://chatgpt.com/codex">chatgpt.com/codex</a>.</p>

---

## Quickstart

### Installing and running Codex CLI

Install globally with your preferred package manager:

```shell
# Install using npm
npm install -g @openai/codex
```

```shell
# Install using Homebrew
brew install --cask codex
```

Then simply run `codex` to get started.

<details>
<summary>You can also go to the <a href="https://github.com/openai/codex/releases/latest">latest GitHub Release</a> and download the appropriate binary for your platform.</summary>

Each GitHub Release contains many executables, but in practice, you likely want one of these:

- macOS
  - Apple Silicon/arm64: `codex-aarch64-apple-darwin.tar.gz`
  - x86_64 (older Mac hardware): `codex-x86_64-apple-darwin.tar.gz`
- Linux
  - x86_64: `codex-x86_64-unknown-linux-musl.tar.gz`
  - arm64: `codex-aarch64-unknown-linux-musl.tar.gz`

Each archive contains a single entry with the platform baked into the name (e.g., `codex-x86_64-unknown-linux-musl`), so you likely want to rename it to `codex` after extracting it.

</details>

### Using Codex with your ChatGPT plan

Run `codex` and select **Sign in with ChatGPT**. We recommend signing into your ChatGPT account to use Codex as part of your Plus, Pro, Team, Edu, or Enterprise plan. [Learn more about what's included in your ChatGPT plan](https://help.openai.com/en/articles/11369540-codex-in-chatgpt).

You can also use Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)
- [**Open source fund**](./docs/open-source-fund.md)

This repository is licensed under the [Apache-2.0 License](LICENSE).
