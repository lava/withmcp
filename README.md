# withmcp

withmcp is a tool for managing your system-wide collection of MCP servers,
allowing easy toggling of individual servers or groups of them, globally or per
project.

Why would you want to do that? These were my motivating examples:

* Found a cool server that has very specific use cases so you don't want it
  in the context by default? -> Save it in your config, enable when needed.

* Want to switch between different toolsets for different tasks? -> Save them
  in your config, enable toolsets when needed.

* Want to enable an MCP server globally like voice mode when working
  remotely? -> Save it in your config, enable when needed.

## Quick start

```sh
cargo install --git https://github.com/lava/withmcp
withmcp edit  # create ~/.config/withmcp/profiles/default.toml
```

Create a minimal profile:

```toml
[servers.linear]
url = "https://mcp.linear.app/mcp"
```

Run with that server enabled:

```sh
withmcp +linear claude
```

## Supported agents

Currently `claude`, `codex` and `pi` are supported.

## Usage

Run a one-off session with a given server enabled:

```sh
❯ withmcp
Usage:
  withmcp [options] [--] <harness> [args...]

Configured Servers:
  excalidraw  https://api.excalidraw.com/api/v1/mcp  (not enabled)
  github      https://api.githubcopilot.com/mcp/  (not enabled)
  linear      https://mcp.linear.app/mcp  (not enabled)
  playwright  npx @playwright/mcp@latest  (not enabled)
  slack       https://mcp.slack.com/mcp  (not enabled)


❯ withmcp +linear claude
```

Permanently enable it:
```sh
❯ withmcp enable linear  # all withmcp sessions now have this
```

The inverse also works:

```sh
❯ withmcp -linear claude   # run without linear mcp
```

You can keep separate profiles that define their own set of servers:

```sh
withmcp -p work codex
```

You can pull in servers from other profiles for a one-off run:

```sh
withmcp +work/slack claude
```

## How it works

`withmcp` is a thin launcher for the selected agent harness.
It generates a list of MCP servers that should be enabled for the
current directory by looking at its own config.
It then passes that configuration in the required harness-specific
format to the agent.

Note that it does not do anything after launching the requested harness,
so there is no way to update the config at runtime. For that, you would
need to use a MCP proxy server.

It also does not attempt to perform any communication with the MCP servers
themselves. In particular, authentication still happens completely inside
your harness of choice, after launching.

When it detects that a given MCP server is already configured natively for
the selected harness, it will be left out of the generated config.

## Building

```sh
cargo install --path .
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

## Known limitations

- Codex receives servers as command-line arguments, so expanded `${VAR}` and
  `$(command)` values are visible in the process list.
- Detection of servers the harness already defines is best-effort. For
  example, Claude Code plugins and managed configs are not checked.
- Pi uses a copy of its global `mcp.json` while withmcp runs, so changes
  the adapter writes to that file during the session (e.g. from `/mcp setup`)
  are lost. Servers pulled in through the file's `imports` are not checked
  for collisions.
- `list` does not check for collisions with a harness's own servers; use
  `which <harness>` for that.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
