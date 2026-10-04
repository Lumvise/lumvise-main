![Lumvise — semantic memory for your projects](assets/lumvise-banner.png)

# Lumvise

**A visual workspace for your code, conversations, and project knowledge.**

Explore your project, talk through ideas with an Assistant, and keep the useful
context attached to your code. Lumvise combines an interactive desktop experience
with a shared local knowledge core that AI tools can access through the
Model Context Protocol (MCP).

[Download for macOS](https://github.com/Lumvise/lumvise-main/releases/latest) ·
[Explore a graph online](https://lumvise.com/workspace?repo=https%3A%2F%2Fraw.githubusercontent.com%2FLumvise%2Flumvise-main%2Fmain%2Fdemos%2Fhuggingface__safetensors%2F.lv%2Fgraph.pz) ·
[Choose an edition](#choose-an-edition) · [Demo projects](demos/) ·
[Contributing](#contributing)

## Explore, discuss, and keep the context

- **Interactive workspaces and whiteboards.** Work with visual project context
  alongside your conversations, with workspace and whiteboard conversations
  kept separate.
- **Voice and text Assistant.** Talk through a question or type it. Each
  conversation owns its canvas; stopping voice keeps that canvas intact.
  Close a conversation to archive it, then reopen it when you want to continue.
- **Knowledge connected to your code.** Explore semantic relationships and
  attach decisions, specifications, reports, and notes to the elements they
  describe. Retrieve that context in later conversations.
- **Project context for your AI tools.** Connect an MCP-capable editor or AI
  client to inspect project structure and create, assign, and retrieve knowledge
  artifacts from the local database.
- **An extensible plugin system.** Add capabilities through separately installed
  plugins. Successful plugin updates keep their selected version after restart.

The [demo projects](demos/) provide sample material to explore.

## Try a graph online

[Inspect the Safetensors demo graph in your browser](https://lumvise.com/workspace?repo=https%3A%2F%2Fraw.githubusercontent.com%2FLumvise%2Flumvise-main%2Fmain%2Fdemos%2Fhuggingface__safetensors%2F.lv%2Fgraph.pz)
with the online graph viewer. The link points to the sample graph hosted in this
repository, so you can explore it without installing the desktop app.

## Choose an edition

Choose how you interact with Lumvise. Both editions share the local database,
Knowledge and Semantic plugins, and MCP access. They use the same foundation for
project and knowledge workflows; Full adds the graphical interfaces and desktop
Assistant experience.

| Edition | How you use it | What is included | Source license |
| --- | --- | --- | --- |
| **Community** | Through an MCP-capable editor or AI client | Headless runtime, local database, MCP access, Knowledge and Semantic plugins | MIT core; LGPL-3.0-only public plugins |
| **Full** | Through the desktop app or an MCP client | Community capabilities, plus the graphical workspace, whiteboard, voice and text Assistant, and conversation canvases | Proprietary desktop application; included open-source components retain their licenses |

Use Community when your MCP client is your interface. Use Full when you also
want to explore and discuss the same project knowledge visually. Community has
no desktop window; Full also supports MCP access.

This repository contains the Community source. The full application's
proprietary source and internal implementation documentation stay private.

### Signed macOS release

[Download Lumvise v0.1.2](https://github.com/Lumvise/lumvise-main/releases/tag/v0.1.2)
for **Apple Silicon Macs running macOS 14 or newer**. Full and Community DMGs
are available; this release provides macOS installers only.

Both editions are **Developer ID signed and Apple-notarized**, with notarization
tickets stapled to the apps and DMGs. Each download includes a matching SHA-256
checksum file. The release notes identify the source commit and completed
signature, Gatekeeper, integrity, and packaged command-line checks.

## Build from source

Install the current stable Rust toolchain, a C/C++ toolchain, CMake and
Protobuf's `protoc`.
The macOS installer targets Apple Silicon and macOS 14 or newer.

```sh
git clone https://github.com/Lumvise/lumvise-main.git
cd lumvise-main
cargo build --locked --release -p lumvise-mcp-app-adapter --bin lumvise
```

The bare runtime has an empty plugin catalog. Install the Community plugin
bundle to enable Knowledge and Semantic tools; the packaged Community installer
includes that bundle. See [building and installing plugins](#plugins).

Saved sign-ins persist only on macOS, where they are stored in the login
Keychain. Linux and Windows builds have no persistent credential store yet:
sign-ins work for the running session and are lost when Lumvise quits.

### Terminal commands

Running `lumvise` without arguments in a terminal shows brief help for your
edition. Use `lumvise start` to launch the Full desktop app or the Community
headless runtime, and `lumvise --help` to show help explicitly. Opening the app
from Finder still starts it normally.

For an installed macOS edition, use its executable path:

```sh
"/Applications/Lumvise.app/Contents/MacOS/lumvise" start
"/Applications/Lumvise Community.app/Contents/MacOS/lumvise" --help
```

## Connect an MCP client

Set your client's MCP server command to the absolute path of `lumvise`, with the
`mcp` role and your project's absolute path. For clients using `mcpServers`:

```json
{
  "mcpServers": {
    "lumvise": {
      "command": "/absolute/path/to/lumvise",
      "args": ["mcp", "--project-root", "/absolute/path/to/your-project"]
    }
  }
}
```

The MCP process starts the local runtime when needed. Client configuration
formats vary; preserve the command and arguments when adapting this example.

Once the project is imported and indexed, try:

> Find the module that owns authentication. Create a decision artifact explaining
> why tokens expire after one hour, attach it to that module, and read it back.

Knowledge and Semantic tools return the real artifact and element identifiers.
Use those identifiers when assigning knowledge, rather than guessing paths or
IDs. Reconnect after restarting the runtime to retrieve the saved record.

## Development

```sh
cargo fmt --all -- --check
cargo check --workspace --locked
RUST_TEST_THREADS=1 cargo test --workspace --locked
```

For a locally signed macOS installer and the official signing workflow, see
[macOS builds](#macos-builds). Development builds use your own plugin signing
key; company signing credentials are not required to build Community from source.

Bug reports, focused fixes, new language support and plugin contributions are
welcome. Read [contribution guide](#contributing) for the workflow and licensing
requirements. Please include a reproducible example with bug reports.

## Licensing

First-party Community core, SDK, protocol and tooling are licensed under
[MIT](LICENSE). The public Knowledge and Semantic plugin directories are licensed
under [LGPL-3.0-only](LICENSES/LGPL-3.0-only.txt), together with the incorporated
[GPL-3.0 terms](LICENSES/GPL-3.0-only.txt). Per-directory notices take precedence.
Both open-source licenses permit commercial use under their respective terms.

Third-party code, fonts, models and other assets retain their original licenses
and attribution. Release bundles include third-party notices and corresponding
source references. The proprietary full application has separate terms; its
restrictions do not limit rights in the included open-source components.

## Plugins

Community includes two plugins:

| Plugin | Purpose | License |
| --- | --- | --- |
| Knowledge | Create, retrieve and assign typed project knowledge | LGPL-3.0-only |
| Semantic | Index and explore code elements and relationships | LGPL-3.0-only |

Plugins run as separate executables. The MIT-licensed SDK owns the Protobuf
handshake, framing, host calls and shutdown. Packages contain a signed manifest,
executables for declared targets, and any declared runtime assets.

### Build a Community bundle

On Apple Silicon macOS, use a protected Ed25519 signing key containing 32 raw
bytes or 64 hexadecimal characters, with file permissions `600`. Keep the key
outside the repository. A development key is independent of official release
credentials.

```sh
cargo run --release --locked -p lumvise-builtin-plugin-release -- \
  --composition minimal /absolute/private/development.key \
  /absolute/output/community-plugins aarch64-apple-darwin
```

`minimal` is the package format's name for the Community composition. The output
contains `builtins-release.json`, the Knowledge and Semantic `.lvp` archives,
`publisher-trust.json`, and `host-capability-grants.json`. Preserve the directory
as a unit. The signing key is not part of the output.

### Install and run

For a fresh, isolated installation, point the runtime at the bundle and separate
state directories:

```sh
export LUMVISE_BUILTIN_RELEASE_DIR=/absolute/output/community-plugins
export LUMVISE_PLUGIN_ROOT=/absolute/development-state/plugins
export LUMVISE_RUNTIME_ROOT=/absolute/development-state/runtime
export LUMVISE_DB_PATH=/absolute/development-state/database
export LUMVISE_STATE_ROOT=/absolute/development-state/state
./target/release/lumvise
```

Use the same environment when launching the MCP client command. Existing
publisher trust and capability-grant policies are preserved. Installing a new
publisher requires deliberately adding its verified public key and grants;
an incoming bundle cannot replace your policy.

For an existing installation whose publisher is already trusted, stop the
runtime, install the release, then restart:

```sh
cargo run --locked -p lumvise-plugin-admin -- \
  install-release /absolute/output/community-plugins
```

Updates must use a higher semantic version. Restarting with an older bundled
plugin preserves the selected newer version. A version identifies immutable
signed content: rebuilds with changed content need a new version.

### Create a plugin

Implement `PluginApplication` from `lumvise-plugin-sdk`, declare the exported
capabilities and required host capabilities in the manifest, and call the SDK's
`run_stdio` entrypoint. The SDK's crate documentation contains a minimal example.
Use host capabilities for application services instead of opening the host's
database or reconstructing its storage backends.

Package a compiled payload with:

```sh
cargo run --locked -p lumvise-plugin-package --bin lumvise-plugin-packager -- \
  /absolute/plugin/manifest.json /absolute/plugin/payload \
  /absolute/private/development.key /absolute/output/example.lvp
```

Match the manifest's protocol range to the SDK version, and include each
executable at its declared target path. Add tests through your plugin's public
capabilities. Do not log credentials or place signing material in packages.

### Knowledge assignments through MCP

Search with `app_plugin.builtin.semantic.search_graph`, using `project_root`
and a name or path filter. Copy a returned `semantic_element_id`. Pass that ID,
the project root, an artifact ID, title, content and a supported knowledge type
to `app_plugin.builtin.knowledge.create_knowledge`. Retrieve it using
`get_knowledge`, or inspect the element's assignments using
`list_knowledge_for_element`.

Read the mounted tool schemas for the exact arguments. Element and artifact
identifiers are returned by the tools; do not invent an element ID from a path.

### Rebuilding the LGPL plugins

The Knowledge and Semantic source, build descriptors and package manifests are
in this repository. You can modify and rebuild them with the commands above and
your own signing key. Use an isolated state directory or explicitly configure
your installation to trust that key. The host does not require the company
signing key for user-built plugins.

When redistributing these plugins, retain their license notices and satisfy the
LGPL's applicable source and modification requirements. The SDK and runtime
remain MIT; each third-party component retains its own license.


## macOS builds

Lumvise Community is the headless edition. It installs the local Lumvise MCP
server and the Knowledge and Semantic built-in plugins. It does not open a
desktop window.

### Requirements

- An Apple Silicon Mac running macOS 14 or later.
- Xcode Command Line Tools and the Rust, CMake, and Protocol Buffers tools
  required by the project.
- The Rust target `aarch64-apple-darwin` (`rustup target add aarch64-apple-darwin`).
- A protected plugin release key accepted by the build script. Keep the key
  outside the checkout and set its permissions to `600`.

### Build a local installer

From the repository root, run:

```sh
chmod 600 "$HOME/.config/lumvise/plugin-release.key"
./scripts/build-macos-installer \
  "$HOME/.config/lumvise/plugin-release.key" \
  --edition community \
  --ad-hoc \
  --output-dir "$PWD/dist/community"
```

The Community edition is the default. `--ad-hoc` is for local testing: it skips
Apple notarization and does not produce an official signed release. For an official build, omit `--ad-hoc`, provide a complete reviewed
`--third-party-notices` inventory, and configure `APPLE_SIGNING_IDENTITY`
with a valid Developer ID Application identity and `APPLE_NOTARY_PROFILE`
with the notarization keychain profile.

Collect the dependency notices before packaging an official release:

```sh
python3 scripts/collect-third-party-notices \
  --cargo-manifest Cargo.toml \
  --license-overrides distribution/third-party-overrides \
  --output target/community-notices
```

Use a new output directory for each scan. The collector reports missing license
texts as errors and includes corresponding source for copyleft dependencies.
Review its inventory, then pass `--third-party-notices target/community-notices`
to the installer command. Private application sources are not required.

For packages distributed as native binaries, a reviewed license override can
include `source_archive` with `file` (a relative `.tar.gz` path), `sha256`, and
an HTTPS `source_url`. The collector verifies and copies that upstream source
archive, preserving its provenance, instead of archiving the installed binary.

With your Developer ID Application certificate and private key in Keychain, and
notarization credentials stored as `lumvise-notary`, build a signed installer:

```sh
export APPLE_SIGNING_IDENTITY='Developer ID Application: Your Company (TEAMID)'
export APPLE_NOTARY_PROFILE='lumvise-notary'
./scripts/build-macos-installer "$HOME/.config/lumvise/plugin-release.key" \
  --third-party-notices target/community-notices \
  --output-dir dist/community-signed
```

This signs the plugin executables before sealing their archives, signs and
notarizes the app and DMG, staples Apple's tickets, and checks Gatekeeper.
New signatures change plugin archive bytes: use a new plugin patch version for
a newly signed release. Reuse the same prepared signed archives when packaging
that release again; official builds reject ad-hoc plugin signatures.

### Resume an interrupted notarization

If a build fails after the app has been signed, the installer prints a retained
staging path. Keep that directory and repeat the same command with the same
edition, key, notices, output path, and other arguments, adding
`--resume-from '/path/printed/by/the/installer'`. Keep
`APPLE_SIGNING_IDENTITY` and `APPLE_NOTARY_PROFILE` set as for the original
command. For example, retry the Community command above as:

```sh
./scripts/build-macos-installer "$HOME/.config/lumvise/plugin-release.key" \
  --third-party-notices target/community-notices \
  --output-dir dist/community-signed \
  --resume-from '/path/printed/by/the/installer'
```

The staging directory retains the signed app, upload archives, DMG when
created, and notarization receipts. Receipts save Apple's submission ID and the
upload's SHA-256 before waiting; retries check the retained bytes and resume
that submission without uploading it again. The installer reuses the signed
plugin archives already in the app. To inspect a submission, use
`xcrun notarytool info ID --keychain-profile lumvise-notary` or retrieve its log
with `xcrun notarytool log ID --keychain-profile lumvise-notary`.

Resume applies only after a signed app exists. If the failure happened earlier,
diagnose it and make a fresh build. Resume also requires the original output
path, signing identity, and ad-hoc setting to match.

Rust build artifacts use the selected workspace's `target` directory by
default. To share a build cache across checkouts, add `--target-dir` followed by
the cache directory path. If you already have a signed Community plugin release
bundle, pass `--plugin-release` and its directory; the installer verifies it
with the release key before packaging it.

The output directory contains `Lumvise Community.app`, a versioned
`Lumvise-Community-<version>-macos-arm64-adhoc.dmg`, and its SHA-256 file.
Official builds omit the `-adhoc` suffix. Check the DMG checksum from that
directory with:

```sh
cd dist/community
shasum -a 256 -c Lumvise-Community-*.dmg.sha256
```

### Install and connect an MCP client

Open the DMG and drag `Lumvise Community.app` to Applications. The DMG includes
an `Install.txt` with the same connection details.

Add this server entry to the MCP client's JSON configuration, replacing the
project path with an absolute path:

```json
{
  "mcpServers": {
    "lumvise": {
      "command": "/Applications/Lumvise Community.app/Contents/MacOS/lumvise",
      "args": ["mcp", "--project-root", "/absolute/path/to/project"]
    }
  }
}
```

Restart the MCP client after saving its configuration. Lumvise runs as a
headless local service for that project.


## Contributing

Start with a small, focused change. For a substantial feature or public API
change, open an issue describing the user-visible behavior and proposed scope
before implementing it.

1. Fork this repository and create a branch.
2. Keep the change in the module that owns the behavior.
3. Add a regression test for a bug fix, or tests demonstrating new behavior.
4. Run formatting, checks and the relevant tests.
5. Open a pull request explaining the problem, the resulting behavior and what
   you verified. Include remaining limitations.

```sh
cargo fmt --all -- --check
cargo check --workspace --locked
RUST_TEST_THREADS=1 cargo test --workspace --locked
```

Use the existing plugin SDK and process protocol when adding a plugin. See
[plugin development](#plugins). Do not commit signing keys, credentials,
local databases, generated installers or machine-specific configuration.

Public core, SDK and tooling contributions use MIT. Contributions to the
Knowledge or Semantic plugins use LGPL-3.0-only. By submitting a contribution,
you agree to license it under the license applying to the files you change and
confirm that you have the right to do so. Preserve third-party notices, and
identify any third-party material you add.

This repository accepts changes to the Community edition. Keep confidential
material and proprietary application source out of issues and pull requests.
For sensitive security reports, use GitHub's private vulnerability reporting
when available instead of posting exploit details in a public issue.
