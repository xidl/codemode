# Code-mode vs direct-MCP benchmark

This benchmark measures the trade-off between:

- **codemode**: one `execute` tool, APIs injected as `mcp.<server>.<tool>()`;
- **raw**: every upstream MCP tool is exposed directly to the agent.

Both modes use the same upstream server (`codegraph`) and the same tasks. The
model is configurable; the default is WorkBuddy DeepSeek
(`workbuddy/deepseek-v4.1-flash`).

## Metrics

- prompt tokens, completion tokens, total tokens (sum over turns);
- assistant turns;
- tool calls;
- correctness (task-specific required substrings);
- wall-clock duration.

## Tasks

`bench/tasks.json` contains code-navigation tasks against an indexed checkout
of `/home/loongtao/xidl`. Each task has `must_contain` (case-insensitive) and
optional `must_not_contain` checks.

## Run

The runner needs an OpenAI-compatible chat-completions endpoint:

```bash
export BENCH_BASE_URL="https://<workbuddy-endpoint>/v1"
export BENCH_API_KEY="<key>"
export BENCH_MODEL="workbuddy/deepseek-v4.1-flash"

node bench/runner.mjs --mode both --runs 1
```

By default the runner spawns `target/release/xidl-codemode`; override with
`CODEMODE_BIN=/path/to/xidl-codemode`.

## Expected shape

Code mode should reduce prompt tokens and tool-count growth on multi-step
tasks, but adds one execution turn and a sandbox hop. Raw mode is simpler for
single-tool questions but its schema prefix grows linearly with the number of
upstream tools.

The runner prints a markdown table and writes `bench/results/<timestamp>.json`.
