# xidl-codemode

`xidl-codemode` turns a set of MCP servers into a single **code-mode MCP
gateway**. It exposes exactly one MCP tool, `execute`, and injects the upstream
tools into a QuickJS sandbox as:

```js
mcp.github.get_issue({ owner: "xidl", repo: "xidl", issue_number: 361 })
```

The generated xidl contracts are written to a workspace on disk so that coding
agents can index them with their normal file tools. The JavaScript passed to
`execute` travels over stdio and is never written to disk.

## Why

Passing every upstream MCP tool to an agent scales badly: tool schemas dominate
the context window, and multi-step work becomes a chain of round trips.
Code mode replaces that with one tool and a generated API facade.

```
MCP config
   |
   v
upstream MCP servers --tools/list--> catalog
   |                                   |
   |                                   +--> generated namespace IDL
   |                                   +--> agents.md index/signatures
   |                                   +--> external xidlc plugin
   |                                             |
   |                                             v
   |                                        manifest.json
   |                                             |
   +---------------------------------------------+
                                                 v
MCP client --execute(code)--> QuickJS --mcp.<server>.<tool>--> upstream MCP
```

## Workspace

By default the gateway writes:

```text
.codemode/
  github/
    agents.md
    issues.idl
  codegraph/
    agents.md
    default.idl
  .internal/
    github.bundle.idl
```

`agents.md` and `*.idl` are generated. Do not edit them: IDL is generated only,
there is no hand-written override path. If a generated IDL is invalid, treat it
as a bug and use `xidl-codemode bug-report`.

The runtime manifest is produced by the external xidl plugin
`xidl-codemode-manifest` from the bundle IDL. `xidlc` discovers it through
`PATH` using the `xidl-<lang>` convention (`codemode-manifest`).

## Install

Download both binaries from a GitHub release and put them in the same directory:

```text
xidl-codemode
xidl-codemode-manifest
```

Or build locally:

```bash
cargo install --path . --bins
```

## Configure

Add the gateway to your MCP client:

```json
{
  "mcpServers": {
    "xidl-codemode": {
      "type": "stdio",
      "command": "xidl-codemode",
      "args": ["serve", "--config", "/home/me/.config/mcp/mcp.json"]
    }
  }
}
```

The gateway skips a server named `xidl-codemode` (and its own executable) so it
does not recurse.

## Use

The agent calls `execute` with JavaScript:

```js
const result = mcp.codegraph.codegraph_explore({
  query: "xidl generator",
  projectPath: "/home/me/xidl",
});
return unwrap(result);
```

`unwrap` throws on `isError`, returns `structuredContent` when present, and
otherwise returns the content blocks.

## CLI

```bash
xidl-codemode serve     --config ~/.config/mcp/mcp.json
xidl-codemode generate  --config ~/.config/mcp/mcp.json
xidl-codemode doctor    --config ~/.config/mcp/mcp.json
xidl-codemode bug-report --config ~/.config/mcp/mcp.json
```

## Limits and security

- QuickJS has no filesystem, network, subprocess, or environment access.
- `execute.code` is limited to 256 KiB, 10s by default (60s max), 128 MiB
  memory, 100 tool calls, 1 MiB result, and bounded console logs.
- Upstream child processes get a cleared environment plus `PATH`, `HOME`, and
  explicitly configured variables.
- `initialize.instructions` from upstream servers is not injected by default.
- The gateway never prints secrets from MCP config; only health/status text is
  logged.

## Development

```bash
mise install
pre-commit install
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

The external manifest plugin is a second binary in this package:

```bash
cargo build --bins
xidlc gen --out-dir /tmp/out codemode-manifest .codemode/.internal/codegraph.bundle.idl
```

## Benchmarks

See [`bench/DESIGN.md`](bench/DESIGN.md) for the code-mode vs direct-MCP
experiment (token usage, turns, correctness) using WorkBuddy DeepSeek.

## License

Apache-2.0
