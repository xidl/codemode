# WorkBuddy DeepSeek results

Model: `remote/workbuddy/deepseek-v4.1-flash`
Agent: `codex exec` (same system prompt, same `$BENCH_REPO` checkout)
Upstream: `codegraph` only
Tasks: 3 code-navigation tasks × 3 runs per mode = 9 runs per mode

| mode | runs | correct | avg total tokens | avg duration |
| --- | ---: | ---: | ---: | ---: |
| codemode | 9 | 9/9 | 48,699 | 9,587 ms |
| raw MCP | 9 | 9/9 | 51,689 | 9,718 ms |

Code mode used **5.8% fewer total tokens** on this sample while preserving
9/9 correctness. The effect is task-dependent:

| task | codemode tokens | raw tokens | result |
| --- | ---: | ---: | --- |
| generate-from-idl | 40,090 | 52,176 | codemode lower |
| typescript-codegen | 42,012 | 39,394 | raw slightly lower |
| external-plugin | 63,993 | 63,498 | effectively equal |

Single-run results were noisy (one run even inverted the aggregate), so use
the 3-run aggregate and treat code mode as a context/tool-count optimization,
not a guaranteed token reduction on every task. Per-run results are in
[`results/codex-results.jsonl`](results/codex-results.jsonl).

Reproduce:

```bash
BENCH_RUNS=3 BENCH_MODEL=remote/workbuddy/deepseek-v4.1-flash ./bench/codex_runner.sh
```
