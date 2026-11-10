# WorkBuddy DeepSeek results

Model: `remote/workbuddy/deepseek-v4.1-flash`
Agent: `codex exec` (same system prompt, same `$BENCH_REPO` checkout)
Upstream: `codegraph` only
Tasks: 3 code-navigation tasks × 10 runs per mode = 30 runs per mode

| mode | runs | correct | mean tokens | median tokens | mean duration |
| --- | ---: | ---: | ---: | ---: | ---: |
| codemode | 30 | 30/30 | 56,196 | 52,007 | 10,380 ms |
| raw MCP | 30 | 30/30 | 57,671 | 57,866 | 9,215 ms |

Code mode used **2.6% fewer tokens by mean** and **10.1% fewer by median**
while preserving 30/30 correctness. The effect is task-dependent:

| task | codemode median | raw median | result |
| --- | ---: | ---: | --- |
| generate-from-idl | 52,086 | 57,872 | codemode lower |
| typescript-codegen | 31,636 | 42,176 | codemode lower |
| external-plugin | 75,413 | 76,440 | effectively equal |

Run-to-run variance is high (individual runs ranged roughly 31k–123k), so the
median is the primary comparison. Treat code mode as a context/tool-count
optimization, not a guaranteed token reduction on every task. Per-run results are in
[`results/codex-results.jsonl`](results/codex-results.jsonl).

Reproduce:

```bash
BENCH_RUNS=10 BENCH_MODEL=remote/workbuddy/deepseek-v4.1-flash ./bench/codex_runner.sh
```
