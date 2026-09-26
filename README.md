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

Enable a server by default:

```sh
withmcp enable linear
```

The inverse of course also works:

```sh
withmcp -linear claude   # run without linear mcp
```

You can keep separate profiles:

```sh
withmcp -p work codex
```

that can define their own servers. You can pull in servers from other
profiles:

```sh
withmcp +work/slack claude
```

## How it works

`withmcp` works as a launcher that generates a list of MCP servers for the
current directory by computing the union of all servers enabled for the
current directory in the configuration file. It then passes that configuration
in the required harness-specific format to the agent.

It does not attempt to perform any communication with the MCP servers
themselves, so you'll still have to authenticate manually inside your harness
of choice.

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
