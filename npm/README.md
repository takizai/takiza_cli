# Takiza Code

An AI coding agent for your terminal: inspect projects, edit files, run commands,
use skills, attach images, and restore files and conversation to earlier checkpoints.

```sh
npm install -g takiza
takiza
```

Or run without a global installation:

```sh
npx takiza
```

Update with `takiza update`. The launcher uses npm and preserves the original
global prefix or local project installation. Automatic updates are enabled by
default: the launcher checks at most once a day and installs a newer release
after a successful app exit, ready for the next launch. To disable them, add
`"auto_update": false` to your existing `~/.config/takiza/config.json` or
`.takiza/config.json`. A global opt-out also applies inside workspaces. Manual
updates still work. Source builds, CI and temporary npx runs do not auto-update;
for npx, use `npx --yes takiza@latest`.

Requires Node.js 18 or newer. Prebuilt binaries are included for Linux x64
(glibc and musl), Linux ARM64 (glibc), macOS Intel and Apple Silicon, and Windows x64.
Rust is not required. Installation needs no build scripts or extra downloads.

Configure saved provider profiles, API keys and models using `/provider`.
Supports Chat Completions, OpenAI Responses and Anthropic Messages APIs.
With a third-party provider, `/mode` is unavailable and `/usage` directs you
to the provider dashboard for current usage and limits.

New configuration files include Exa MCP for web search and page fetching without
a separate API key, subject to anonymous usage limits. Existing configurations
keep their MCP settings; see the full documentation to add or disable Exa.

Version 0.1.12 contains binaries from the corresponding
[GitHub release](https://github.com/takizai/takiza_cli/releases/tag/v0.1.12).

[Source and full documentation](https://github.com/takizai/takiza_cli)
