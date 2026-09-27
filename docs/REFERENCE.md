# withmcp reference

Full reference for `withmcp`, a launcher that adds MCP servers to a coding
agent harness. Printed by `withmcp docs`. For an introduction and quick
start, see the [README](../README.md).

## Commands

```
withmcp [options] [--] <harness> [args...]   launch a harness with the resolved servers
withmcp [options] list                       show servers for the current directory
withmcp [options] which [[--] <harness>...]  same, plus why, and what a launch would do
withmcp [options] enable [--scope global|project] <server>...
withmcp [options] disable [--scope global|project] <server>...
withmcp [options] clientsecret <server>      store an OAuth client secret for a server
withmcp [options] export [<harness>]         sync into the harness user config
withmcp [options] docs                       print this reference
withmcp [options] edit                       open the selected profile file
```

Options: `-p/--profile <name>`, `+<server>`/`-<server>` (or `--enable`/`--disable`) to
toggle a server for this run, `--config <file>` to use a profile file directly.
A server name may point to a group, or to `<profile>/<server>` to pull one in from
another profile.

`clientsecret` stores the OAuth client secret of a server with `oauth`
settings in Claude Code, which only accepts secrets when a server is added.
It adds a placeholder entry with local scope in
`~/.local/share/withmcp/claude-secrets`; keep that entry. Only Claude Code is
supported.

## Profile file

Profiles are TOML files at `~/.config/withmcp/profiles/<name>.toml` (or the
file given by `--config`, whose base name becomes the profile's name).
Everything in them is optional.

Top level:

- `prefix` — prepended to server names before they are passed to the
  harness; defaults to `<profile>_`. Required if the profile name itself
  isn't a valid bare key (letters, digits, `-`, `_`).
- `env_passthrough` — an array of variable names merged into every command
  server's own `env_passthrough` in this profile, so shared variables need
  not be repeated per server. Must appear before the first `[...]` table
  header, since TOML keys after one belong to that table.

`[servers.<name>]` is a command server:

- `command`, `args` — the program and its arguments.
- `env` — a table of environment variables to set for it.
- `env_passthrough` — variable names from the launching shell to pass
  through unchanged. Only Codex sandboxes a server's environment; Claude
  Code and Pi already inherit the full environment, so this has no effect
  there.
- `enabled` — whether the server is on by default (default: `false`).

or an HTTP server:

- `url` — the server's endpoint.
- `headers` — a table of HTTP headers.
- `oauth = { client_id = "...", callback_port = <port> }` — a
  pre-registered OAuth client, for servers without dynamic client
  registration. For a server that only supports a confidential client
  (e.g. Slack), use `withmcp clientsecret` once instead of storing a
  secret here. Claude Code only.
- `enabled` — as above.

A server needs exactly one of `command` or `url`; `command`/`args`/`env`/
`env_passthrough` and `url`/`headers`/`oauth` cannot be mixed on the same
entry. Values may reference the launching shell's environment with
`${VAR}`, or the output of a command with `$(cmd)`, e.g.
`headers = { Authorization = "Bearer $(gh auth token)" }`. Server and
group names, and `env`/`headers` keys, must use only letters, digits, `-`
and `_`.

`[groups.<name>]` switches related servers together; a name cannot be both
a server and a group:

- `servers` — the member server names.
- `enabled` — whether the group, and so its members, is on by default.

`[paths."<absolute-path-or-~/...>"]` overrides applied when the current
directory is under that path (see "How the server set is built" below):

- `enable`, `disable` — server or group names to switch on or off for
  that path.

## How the server set is built

For each configured server, in order (later steps win):

1. **Flags and groups** — a server starts on if its own `enabled = true`, or it
   belongs to a group with `enabled = true`.
2. **Path rules** — `[paths."..."]` rules matching `cwd`, applied least-specific
   first; within a rule, group `enable`/`disable` apply before individual servers.
3. **CLI overrides** — `+server`/`-server` (or `enable`/`disable` names), applied
   in the order given.

Servers already configured natively for the selected harness are left out of
the generated config. Server names passed to the harness get the profile's
`prefix` (default: `<profile>_`).

## Export to a harness

`withmcp export claude`, `withmcp export codex`, and `withmcp export pi` write
top-level enabled servers into that harness's user-wide MCP config. `withmcp
export` without a harness does this for every harness whose program is found
on `$PATH`. Each
`[paths."..."]` rule is exported to that directory's project config, with
nested rules overriding their parent. Disabled servers in the selected profile
are removed from the user config. Project disables use each harness's native
project setting. Native servers absent from the selected withmcp profile are
left in place and reported as info. The usual profile selection and one-run
`+server`/`-server` options apply. Missing path directories are skipped with
an info message.

The destination is `~/.claude.json` (or `CLAUDE_CONFIG_DIR/.claude.json`),
`$CODEX_HOME/config.toml` (default `~/.codex/config.toml`), or Pi's agent
`mcp.json` (default `~/.pi/agent/mcp.json`). Project entries go to `.mcp.json`
for Claude, `.codex/config.toml` for Codex, and `.pi/mcp.json` for Pi. Claude's
project disable list is stored in `~/.claude.json`. Server values are expanded
before they are saved, so environment variables and command output are stored
in the destination file.

Codex sandboxes the environment it gives MCP servers; a command server's
`env_passthrough` (settable per server, or at the profile's top level to
cover every command server at once) lists variables from the launching shell
to let through unchanged, exported as Codex's `env_vars`. Claude and Pi
already inherit the full environment, so the option has no effect there.

## Environment variables

- `WITHMCP_PROFILE` — profile to use when `--profile` is not given
- `WITHMCP_CONFIG_DIR` — config directory instead of `~/.config/withmcp`
- `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `HOME` — used to locate config/data dirs
  when the withmcp-specific ones above are not set
- `VISUAL`, `EDITOR` — editor for `edit` (default: `vi`)
- `NO_COLOR` — disable colored output
- `XDG_RUNTIME_DIR` — used to place adapter runtime files (falls back to a
  per-user temp dir)

Server definitions can also reference environment variables via `${VAR}` and
`$(command)` expansion in config values.

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
- Claude Code only accepts an OAuth client secret when a server is added, so
  `withmcp clientsecret <server>` stores it up front via `claude mcp add` in a
  local-scope entry that withmcp's own config then reuses at launch. Other
  harnesses do not support MCP servers with a client secret at all, so only
  Claude Code is supported by this command today.
