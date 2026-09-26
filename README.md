# withmcp

Launch a coding-agent harness with a configurable set of extra MCP servers.

```sh
withmcp claude --resume                  # servers from the `default` profile
withmcp -p work +playwright -d linear codex
WITHMCP_PROFILE=work withmcp claude
withmcp list                             # servers enabled in the current directory
withmcp which claude                     # ...and why, plus what a launch would do
withmcp -p work edit                     # open the profile in $VISUAL/$EDITOR
withmcp -- edit                          # launch a harness called `edit`
```

withmcp only adds servers. Servers the harness defines itself stay untouched.
If one of those has the same name as an enabled withmcp server, withmcp prints
a warning and leaves its own server out.

Supported harnesses: `claude` (via `--mcp-config`) and `codex` (via `-c
mcp_servers.*` overrides).

## Profiles

Each profile is a file, `~/.config/withmcp/profiles/<name>.toml` (the
directory is `$WITHMCP_CONFIG_DIR`, else `$XDG_CONFIG_HOME/withmcp`). The
profile is `-p/--profile`, else `$WITHMCP_PROFILE`, else `default`. A missing
`default.toml` counts as an empty profile; any other missing profile is an
error. `--config <file>` uses a profile file from anywhere instead; its
file name serves as the profile name. See [`examples/profile.toml`](examples/profile.toml); `withmcp edit`
creates new profiles from it.

Servers are passed to the harness as `<prefix><name>`. The prefix defaults to
`<profile>_`, so `linear` in `work.toml` becomes `work_linear` and gets its own
login, separate from `linear` in other profiles. Set `prefix = ""` to pass the
plain names. On the command line and in path rules, servers are referred to
without the prefix.

A server is on unless it has `enabled = false`. Later steps win:

1. Each server's `enabled` flag.
2. Path rules (`[paths."<dir>"]`) matching the current directory, least
   specific first.
3. `-e`/`+<server>` and `-d` on the command line, in order.

String values of servers may reference the environment with `${VAR}`.
Authentication is left to the harness (e.g. `/mcp` in Claude Code or `codex
mcp login`).

## Building

```sh
cargo install --path .
# Static binary:
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

## Known limitations

- Codex receives servers as command-line arguments, so expanded `${VAR}`
  values are visible in the process list.
- Detection of servers the harness already defines is best-effort. For
  example, Claude Code plugins and managed configs are not checked.
- `list` does not check for collisions with a harness's own servers; use
  `which <harness>` for that.
- `pick` and `-i` are not implemented yet.
