# xidl-codemode

`xidl-codemode` turns a set of MCP servers into a single **code-mode MCP
gateway**. It exposes exactly one MCP tool, `execute`, and injects the upstream
tools into a QuickJS sandbox as:

```js
tools.github.get_issue({
  owner: "xidl",
  repo: "codemode",
  issue_number: 361,
})
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
   |                                   +--> agents.md (upstream instructions only)
   |                                   +--> external xidlc plugin
   |                                             |
   |                                             v
   |                                   glue.js + glue.d.ts
   |                                             |
   +---------------------------------------------+
                                                 v
MCP client --execute(code)--> QuickJS --tools.<server>.<tool>--> upstream MCP
```

The current behavior and the reasoning behind it are documented in
[`docs/design.md`](docs/design.md).

## Workspace

By default the gateway writes generated docs into a temporary run directory
that is removed when the process exits:

```text
.cache/
  codemode-<random>/
    github/
      agents.md
      issues.idl
    codegraph/
      agents.md
      default.idl
    .internal/
      github.bundle.idl
  .cache/
    <run-id>/
      github/
        agents.md
        issues.idl
      codegraph/
        agents.md
        default.idl
      .internal/
        github.bundle.idl
```

The random directory is created under `<cwd>/.cache/` on every `serve` start and
removed on normal exit; startup cleanup only removes `.cache/codemode-*` runs.
Clients whose file tools cannot read the run directory can pass
`--docs-dir <path>` to keep generated docs in a persistent directory; persistent
mode refreshes every start and removes namespace files that were not generated
this run.

`*.idl` is generated and must not be edited; there is no hand-written override
path. If a generated IDL is invalid, treat it as a bug: run
`xidl-codemode doctor --config <config>` and report the failing schema
manually. `agents.md` exists only when the upstream MCP server provides
`initialize.instructions`; it is not a generated tool/signature index.

The model discovers APIs from the filesystem. The codemode `instructions` only
state the workspace convention and call shape:

```bash
rg --files .cache/codemode-*
rg "@rename|string owner|int64 issue_number" .cache/codemode-*/github/*.idl
```

Parameter names, order, types, and optionality live only in the IDL files.

The QuickJS glue is produced by the external xidl plugin `xidl-codemode-glue`
from the bundle IDL. `xidlc` discovers it through `PATH` using the
`xidl-<lang>` convention (`codemode-glue`). The glue is installed into each
execute sandbox through a single injected `host.callTool` capability; the host
object itself is never exposed to user code. After generation, the glue is
installed in a probe sandbox and audited against the tool catalog before the
server is registered.

## Install

Download both binaries from a GitHub release and put them in the same directory:

```text
xidl-codemode
xidl-codemode-glue
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
      "args": ["serve", "--config", "/path/to/mcp.json"]
    }
  }
}
```

The gateway skips a server named `xidl-codemode` (and its own executable) so it
does not recurse.

## Use

The agent calls `execute` with JavaScript:

```js
const result = tools.codegraph.codegraph_explore({
  query: "xidl generator",
  project_path: "/path/to/xidl",
});
return result;
```

Tool calls return normal JSON values: structured content when present, a single
text block that is itself JSON unwrapped into that value, other text blocks as
strings, multiple blocks as an array, and empty content as null. Tool failures
throw JavaScript errors. Each tool takes a single object argument; server,
tool, and parameter names are snake_case, optional fields may be omitted,
explicit null values pass through, and unknown fields are rejected.

When an upstream tool declares an `outputSchema`, the generated IDL and
`glue.d.ts` include a named output struct and the operation returns that type
instead of a generic value; tools without an `outputSchema` stay untyped.

`await` works for syntax compatibility and pure JavaScript promises. Upstream
tool calls remain synchronous and serial; `Promise.all` does not provide real
upstream concurrency.

Inside `execute`, `catalog.servers()`, `catalog.search(query, server,
namespace)`, `catalog.tools(server, namespace)`, and `catalog.describe(server,
tool)` provide the same in-memory tool index used by the read-only
`search_tools` MCP tool.

`execute` also accepts `type: "file"` with a `path` to run an existing script
from disk instead of inline source. Relative paths resolve against the
workspace root, paths must stay inside the configured execute roots, and file
mode adds no filesystem capability to the sandbox: it only loads the source.

## CLI

```bash
xidl-codemode serve     --config ~/.config/mcp/mcp.json
xidl-codemode generate  --config ~/.config/mcp/mcp.json --docs-dir /path/to/docs
xidl-codemode doctor    --config ~/.config/mcp/mcp.json
```

`doctor` is read-only: it connects to the configured servers, generates and
audits the glue in memory, and prints a health report without writing any
workspace files.

## Limits and security

- The JavaScript sandbox has no filesystem, network, subprocess, or environment access.
- `execute.code` is limited to 256 KiB, 10s default timeout (soft limit; the
  configured `executeTimeoutHardMs` hard limit defaults to 60s), 128 MiB
  memory, 100 tool calls, 1 MiB result, and bounded console logs. Each
  upstream tool call has its own 10s timeout (`tool_timeout`) that cannot
  exceed the remaining execute deadline.
- Upstream child processes get a cleared environment plus `PATH`, `HOME`, and
  explicitly configured variables.
- `initialize.instructions` from upstream servers is written to `agents.md`
  under the docs root as untrusted data. The gateway instructions reference it
  instead of injecting its text.
- Config env/header values are not written to generated files. Logs and error
  strings are not systematically redacted yet, so avoid putting secrets in
  commands or URLs that may appear in error output.

## Development

```bash
mise install
pre-commit install
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

The design document describes the behavior that is actually implemented; early
ideas that are not implemented are listed at the end of it.

The external glue plugin is a second binary in this package:

```bash
cargo build --bins
xidlc gen --out-dir /tmp/out codemode-glue .cache/codemode-*/.internal/codegraph.bundle.idl
```

## License

Apache-2.0
