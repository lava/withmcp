# withmcp

Launch a coding-agent harness with a configurable set of extra MCP servers.

```sh
withmcp claude --resume                  # servers from the `default` profile
withmcp -p work +playwright -d github codex
WITHMCP_PROFILE=work withmcp claude
withmcp which claude                     # show what would be enabled and why
withmcp edit                             # open the config in $VISUAL/$EDITOR
withmcp -- edit                          # launch a harness called `edit`
```

withmcp only adds servers. Servers the harness defines itself stay untouched.
If one of those has the same name as an enabled withmcp server, withmcp prints
a warning and leaves its own server out.

Supported harnesses: `claude` (via `--mcp-config`) and `codex` (via `-c
mcp_servers.*` overrides).

## Configuration

The config lives at `$WITHMCP_CONFIG`, else
`$XDG_CONFIG_HOME/withmcp/config.toml`, else `~/.config/withmcp/config.toml`.
See [`examples/config.toml`](examples/config.toml); `withmcp edit` creates the
file from it.

The profile is `-p/--profile`, else `$WITHMCP_PROFILE`, else `default`.

A server is off unless something enables it. Later steps win:

1. `enable`/`disable` of each profile in the `extends` chain, root first.
2. Path rules (`[profiles.<name>.paths."<dir>"]`) from the whole chain that
   match the current directory, least specific first. At equal depth the child
   profile's rule applies last.
3. `-e`/`+<server>` and `-d` on the command line, in order.

String values of servers may reference the environment with `${VAR}`.
Authentication is left to the harness (e.g. `/mcp` in Claude Code or `codex
mcp login`).

## Building

```sh
cargo build --release
# Static binary:
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

## Known limitations

- Codex receives servers as command-line arguments, so expanded `${VAR}`
  values are visible in the process list.
- Detection of servers the harness already defines is best-effort. For
  example, Claude Code plugins and managed configs are not checked.
- `pick` and `-i` are not implemented yet.
