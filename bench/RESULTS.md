# WorkBuddy DeepSeek results

Model: `remote/workbuddy/deepseek-v4.1-flash`
Agent: `codex exec` (same system prompt, same `$BENCH_REPO` checkout)
Upstream: `codegraph` only
Current build: `instructions` only states the workspace convention and injects
upstream MCP instructions. Tool signatures are discovered by grepping/reading
the generated IDL; no signature index is injected.

Tasks: 3 code-navigation tasks × 10 runs per mode = 30 runs per mode

| mode | runs | correct | mean tokens | median tokens | mean duration |
| --- | ---: | ---: | ---: | ---: | ---: |
| codemode | 30 | 30/30 | 63,325 | 53,382 | 11,298 ms |
| raw MCP | 30 | 30/30 | 57,556 | 51,752 | 9,659 ms |

On this small single-tool task set, code mode did **not** reduce tokens:
mean +10.0%, median +3.1% versus raw MCP, both 30/30 correct. The likely reason
is that the agent must discover/read IDL before writing the script, while raw
MCP exposes the single tool schema directly.

| task | codemode median | raw median | result |
| --- | ---: | ---: | --- |
| generate-from-idl | 46,461 | 36,360 | codemode higher |
| typescript-codegen | 34,326 | 33,996 | effectively equal |
| external-plugin | 79,044 | 78,121 | effectively equal |

Run-to-run variance is high (individual runs ranged roughly 34k–209k), so the
median is the primary comparison. Treat code mode as a context/tool-count
optimization for many-tool/multi-step workloads, not a guaranteed token
reduction on small tasks. Per-run results are in
[`results/codex-results.jsonl`](results/codex-results.jsonl).

Reproduce:

```bash
BENCH_RUNS=10 BENCH_MODEL=remote/workbuddy/deepseek-v4.1-flash ./bench/codex_runner.sh
```
