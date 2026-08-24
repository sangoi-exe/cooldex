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

### Installing and running Cooldex

Cooldex standalone releases currently support x86_64 Linux, including WSL on x86_64.

```shell
curl -fsSL https://github.com/sangoi-exe/cooldex/releases/latest/download/install.sh | sh
```

Then simply run `codex` to get started.

<details>
<summary>You can also go to the <a href="https://github.com/sangoi-exe/cooldex/releases/latest">latest Cooldex GitHub Release</a> to inspect the published assets.</summary>

The WSL/Linux x86_64 release contains exactly:

- `codex-package-x86_64-unknown-linux-musl.tar.gz`
- `codex-package_SHA256SUMS`
- `install.sh`

</details>

### Cooldex fork inventory

Cooldex tracks its upstream baseline and keeps a small local surface for bounded
current-thread `recall`, post-compaction continuity, MultiAgentV2 controls, a
local app-server child, a source/development Computer Use island, and the
full-Responses override for the GPT-5.6 model family. Local operator support
includes `cargo-guard` and separate `codex`, `cdx`, and `cdx-dev` command lanes.

`codex` is the regular stable lane. `cdx` selects the standalone package lane
for QA and promotion checks. `cdx-dev` is the development-session lane. These
roles support local workflow; they do not define separate deployment
environments. Release and cross-machine installation remain in the source tree
as a future retirement candidate. This inventory does not remove or change them.

The detailed current inventory is maintained in the companion `.sangoi`
repository at `reference/areas/cooldex-fork-feature-inventory.md`. It records
source owners and evidence limits. It is not a release or shipped-status ledger.

### Using Codex with your ChatGPT plan

Run `codex` and select **Sign in with ChatGPT**. We recommend signing into your ChatGPT account to use Codex as part of your Plus, Pro, Business, Edu, or Enterprise plan. [Learn more about what's included in your ChatGPT plan](https://help.openai.com/en/articles/11369540-codex-in-chatgpt).

You can also use Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)
- [**Open source fund**](./docs/open-source-fund.md)

This repository is licensed under the [Apache-2.0 License](LICENSE).
