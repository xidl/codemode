#!/usr/bin/env bash
set -euo pipefail

MODEL="${BENCH_MODEL:-remote/workbuddy/deepseek-v4.1-flash}"
CODEMODE_BIN="${CODEMODE_BIN:-$(pwd)/target/release/xidl-codemode}"
REPO="${BENCH_REPO:-/home/loongtao/xidl}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${BENCH_OUT:-$ROOT/bench/results}"
mkdir -p "$OUT"

TASK_IDS=(
  "generate-from-idl"
  "typescript-codegen"
  "external-plugin"
)
TASK_PROMPTS=(
  "In /home/loongtao/xidl, find where generate_from_idl is defined. Answer with the file path and line number."
  "In /home/loongtao/xidl, find the file that defines TypescriptCodegen. Answer with the file path."
  "In /home/loongtao/xidl, where is PluginClient::spawn defined, and what binary naming convention does it use for external plugins?"
)
TASK_NEEDLES=(
  "xidlc/src/driver/generate.rs"
  "typescript/mod.rs"
  "jsonrpc/ipc.rs"
)

make_home() {
  local mode="$1"
  local home
  home="$(mktemp -d "${TMPDIR:-/tmp}/codex-bench-home.XXXXXX")"
  cp "$HOME/.codex/auth.json" "$home/auth.json"
  cp "$HOME/.codex/magpie-models.json" "$home/magpie-models.json"
  cat > "$home/config.toml" <<EOF
model = "$MODEL"
model_provider = "magpie"
model_catalog_json = "$home/magpie-models.json"
model_reasoning_effort = "low"

[model_providers.magpie]
name = "magpie"
base_url = "http://127.0.0.1:3425/v1"
wire_api = "responses"
experimental_bearer_token = "magpie"
http_headers = { "x-openai-actor-authorization" = "magpie" }
EOF
  if [[ "$mode" == "raw" ]]; then
    cat >> "$home/config.toml" <<EOF

[mcp_servers.codegraph]
type = "stdio"
command = "codegraph"
args = ["serve", "--mcp"]
EOF
  else
    local cfg ws
    cfg="$(mktemp "${TMPDIR:-/tmp}/codemode-config.XXXXXX.json")"
    ws="$(mktemp -d "${TMPDIR:-/tmp}/codemode-ws.XXXXXX")"
    cat > "$cfg" <<'EOF'
{"mcpServers":{"codegraph":{"type":"stdio","command":"codegraph","args":["serve","--mcp"]}}}
EOF
    cat >> "$home/config.toml" <<EOF

[mcp_servers.xidl_codemode]
type = "stdio"
command = "$CODEMODE_BIN"
args = ["serve", "--config", "$cfg", "--workspace", "$ws"]
EOF
  fi
  echo "$home"
}

run_one() {
  local mode="$1" id="$2" prompt="$3" needle="$4"
  local home final events start end
  home="$(make_home "$mode")"
  final="$home/final.txt"
  events="$home/events.jsonl"
  start="$(date +%s%3N)"
  CODEX_HOME="$home" codex exec --json --skip-git-repo-check \
    -C "$REPO" -s danger-full-access -c approval_policy="never" \
    -o "$final" "$prompt" >"$events" 2>"$home/stderr.log" || true
  end="$(date +%s%3N)"
  local usage correct
  usage="$(node - "$events" <<'NODE'
const fs = require("fs");
const file = process.argv[2];
const lines = fs.readFileSync(file, "utf8").split("\n").filter(Boolean);
let usage = { input_tokens: 0, output_tokens: 0, total_tokens: 0 };
for (const line of lines) {
  try {
    const event = JSON.parse(line);
    if (event.type === "turn.completed" && event.usage) {
      usage = {
        input_tokens: event.usage.input_tokens || 0,
        output_tokens: event.usage.output_tokens || 0,
        total_tokens: (event.usage.input_tokens || 0) + (event.usage.output_tokens || 0),
      };
    }
  } catch {}
}
process.stdout.write(JSON.stringify(usage));
NODE
)"
  if [[ -f "$final" ]] && grep -qi -- "$needle" "$final"; then
    correct="true"
  else
    correct="false"
  fi
  printf '{"mode":"%s","task":"%s","correct":%s,"usage":%s,"duration_ms":%d}\n' \
    "$mode" "$id" "$correct" "$usage" "$((end - start))" | tee -a "$OUT/codex-results.jsonl"
}

: > "$OUT/codex-results.jsonl"
for mode in codemode raw; do
  for i in "${!TASK_IDS[@]}"; do
    echo "[$mode] ${TASK_IDS[$i]}" >&2
    run_one "$mode" "${TASK_IDS[$i]}" "${TASK_PROMPTS[$i]}" "${TASK_NEEDLES[$i]}"
  done
done

node - "$OUT/codex-results.jsonl" <<'NODE'
const fs = require("fs");
const rows = fs.readFileSync(process.argv[2], "utf8").trim().split("\n").filter(Boolean).map(JSON.parse);
const by = {};
for (const row of rows) {
  by[row.mode] ||= { n: 0, correct: 0, tokens: 0, ms: 0 };
  by[row.mode].n += 1;
  by[row.mode].correct += row.correct ? 1 : 0;
  by[row.mode].tokens += row.usage.total_tokens || 0;
  by[row.mode].ms += row.duration_ms || 0;
}
console.log("\n| mode | runs | correct | avg total tokens | avg ms |");
console.log("| --- | ---: | ---: | ---: | ---: |");
for (const [mode, v] of Object.entries(by)) {
  console.log(`| ${mode} | ${v.n} | ${v.correct}/${v.n} | ${Math.round(v.tokens / v.n)} | ${Math.round(v.ms / v.n)} |`);
}
NODE
