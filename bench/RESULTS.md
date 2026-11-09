# WorkBuddy DeepSeek results

Model: `remote/workbuddy/deepseek-v4.1-flash`
Agent: `codex exec` (same system prompt, same repo `~/xidl`)
Upstream: `codegraph` only
Tasks: 3 code-navigation tasks, 3/3 correctness required substrings

| mode | runs | correct | avg total tokens | avg duration |
| --- | ---: | ---: | ---: | ---: |
| codemode | 3 | 3/3 | 43,565 | 10,296 ms |
| raw MCP | 3 | 3/3 | 59,572 | 11,059 ms |

Code mode used **26.9% fewer total tokens** on this sample while preserving
3/3 correctness. Per-task results are in
[`results/codex-results.jsonl`](results/codex-results.jsonl).

Reproduce:

```bash
BENCH_MODEL=remote/workbuddy/deepseek-v4.1-flash ./bench/codex_runner.sh
```
