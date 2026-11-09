#!/usr/bin/env node
import { spawn } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const BASE_URL = process.env.BENCH_BASE_URL;
const API_KEY = process.env.BENCH_API_KEY;
const MODEL = process.env.BENCH_MODEL || "workbuddy/deepseek-v4.1-flash";
const CODEMODE_BIN = process.env.CODEMODE_BIN || "target/release/xidl-codemode";

const args = process.argv.slice(2);
function arg(name, fallback) {
  const index = args.indexOf(name);
  return index >= 0 ? args[index + 1] : fallback;
}
const modeArg = arg("--mode", "both");
const runs = Number(arg("--runs", "1"));
const tasksPath = resolve(arg("--tasks", "bench/tasks.json"));
const tasks = JSON.parse(readFileSync(tasksPath, "utf8"));

if (!BASE_URL || !API_KEY) {
  console.error("BENCH_BASE_URL and BENCH_API_KEY are required.");
  console.error("See bench/DESIGN.md. Default model:", MODEL);
  process.exit(2);
}

class McpClient {
  constructor(command, argv, cwd) {
    this.nextId = 1;
    this.pending = new Map();
    this.buffer = "";
    this.child = spawn(command, argv, { cwd, stdio: ["pipe", "pipe", "pipe"] });
    this.child.stderr.on("data", (data) => {
      const text = data.toString();
      if (process.env.BENCH_VERBOSE) process.stderr.write(text);
    });
    this.child.stdout.on("data", (data) => {
      this.buffer += data.toString();
      let index;
      while ((index = this.buffer.indexOf("\n")) >= 0) {
        const line = this.buffer.slice(0, index);
        this.buffer = this.buffer.slice(index + 1);
        if (!line.trim()) continue;
        let message;
        try { message = JSON.parse(line); } catch { continue; }
        if (message.id && this.pending.has(message.id)) {
          const { resolve: done, reject } = this.pending.get(message.id);
          this.pending.delete(message.id);
          if (message.error) reject(new Error(JSON.stringify(message.error)));
          else done(message.result);
        }
      }
    });
  }

  request(method, params) {
    const id = this.nextId++;
    const promise = new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
    this.child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    return promise;
  }

  notify(method, params) {
    this.child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method, params }) + "\n");
  }

  async initialize() {
    const result = await this.request("initialize", {
      protocolVersion: "2025-11-25",
      capabilities: {},
      clientInfo: { name: "xidl-codemode-bench", version: "0.1.0" },
    });
    this.notify("notifications/initialized", {});
    return result;
  }

  async listTools() {
    const result = await this.request("tools/list", {});
    return result.tools || [];
  }

  async callTool(name, arguments_) {
    return this.request("tools/call", { name, arguments: arguments_ });
  }

  close() {
    this.child.kill();
  }
}

function toolSchema(tools) {
  return tools.map((tool) => ({
    type: "function",
    function: {
      name: tool.name,
      description: tool.description || "",
      parameters: tool.inputSchema || { type: "object", properties: {} },
    },
  }));
}

async function chat(messages, tools) {
  const response = await fetch(`${BASE_URL.replace(/\/$/, "")}/chat/completions`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${API_KEY}`,
    },
    body: JSON.stringify({
      model: MODEL,
      messages,
      tools,
      tool_choice: "auto",
      temperature: 0,
      stream: false,
    }),
  });
  if (!response.ok) {
    throw new Error(`chat completion failed: ${response.status} ${await response.text()}`);
  }
  return response.json();
}

function toolResultText(result) {
  const blocks = result?.content || [];
  return blocks.map((block) => block.text ?? JSON.stringify(block)).join("\n");
}

async function runTask(mode, task) {
  const started = Date.now();
  const workspace = mkdtempSync(join(tmpdir(), "codemode-bench-"));
  let client;
  let tools;
  let instructions = "";
  if (mode === "codemode") {
    const config = join(workspace, "mcp.json");
    writeFileSync(
      config,
      JSON.stringify({
        mcpServers: {
          codegraph: { type: "stdio", command: "codegraph", args: ["serve", "--mcp"] },
        },
      }),
    );
    client = new McpClient(CODEMODE_BIN, ["serve", "--config", config, "--workspace", join(workspace, "ws")], process.cwd());
    const init = await client.initialize();
    instructions = init.instructions || "";
    tools = await client.listTools();
  } else {
    client = new McpClient("codegraph", ["serve", "--mcp"], process.cwd());
    await client.initialize();
    tools = await client.listTools();
  }

  const system = [
    "You are a coding agent. Use the available tools to answer accurately.",
    "When you have the answer, reply with a concise final answer.",
    mode === "codemode"
      ? "The `execute` tool runs JavaScript. MCP APIs are `mcp.<server>.<tool>(args)`. Use `unwrap(result)`. The workspace contains generated agents.md and IDL files."
      : "Upstream MCP tools are exposed directly.",
    instructions,
  ].join("\n\n");
  const messages = [
    { role: "system", content: system },
    { role: "user", content: task.prompt },
  ];
  const usage = { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 };
  let turns = 0;
  let toolCalls = 0;
  let answer = "";
  const maxTurns = 8;

  for (; turns < maxTurns; turns += 1) {
    const response = await chat(messages, toolSchema(tools));
    if (response.usage) {
      usage.prompt_tokens += response.usage.prompt_tokens || 0;
      usage.completion_tokens += response.usage.completion_tokens || 0;
      usage.total_tokens += response.usage.total_tokens || 0;
    }
    const message = response.choices?.[0]?.message;
    if (!message) throw new Error("no assistant message");
    messages.push(message);
    if (!message.tool_calls?.length) {
      answer = message.content || "";
      break;
    }
    for (const call of message.tool_calls) {
      toolCalls += 1;
      let result;
      try {
        const parsed = JSON.parse(call.function.arguments || "{}");
        result = await client.callTool(call.function.name, parsed);
      } catch (error) {
        result = { isError: true, content: [{ type: "text", text: String(error) }] };
      }
      messages.push({
        role: "tool",
        tool_call_id: call.id,
        content: toolResultText(result),
      });
    }
  }

  client.close();
  const lower = answer.toLowerCase();
  const mustContain = task.must_contain || [];
  const mustNotContain = task.must_not_contain || [];
  const correct =
    mustContain.every((needle) => lower.includes(needle.toLowerCase())) &&
    mustNotContain.every((needle) => !lower.includes(needle.toLowerCase()));
  return {
    mode,
    task: task.id,
    correct,
    turns,
    tool_calls: toolCalls,
    usage,
    duration_ms: Date.now() - started,
    answer_preview: answer.slice(0, 240),
  };
}

const modes = modeArg === "both" ? ["codemode", "raw"] : [modeArg];
const results = [];
for (let run = 0; run < runs; run += 1) {
  for (const mode of modes) {
    for (const task of tasks) {
      process.stderr.write(`[${mode}] ${task.id}\n`);
      try {
        results.push(await runTask(mode, task));
      } catch (error) {
        results.push({ mode, task: task.id, correct: false, error: String(error) });
      }
    }
  }
}

mkdirSync("bench/results", { recursive: true });
const stamp = new Date().toISOString().replaceAll(":", "-");
const output = {
  model: MODEL,
  runs,
  generated_at: new Date().toISOString(),
  results,
};
writeFileSync(`bench/results/${stamp}.json`, JSON.stringify(output, null, 2));

const aggregate = {};
for (const result of results) {
  const key = result.mode;
  aggregate[key] ||= { runs: 0, correct: 0, tokens: 0, turns: 0, tool_calls: 0, duration_ms: 0 };
  const entry = aggregate[key];
  entry.runs += 1;
  entry.correct += result.correct ? 1 : 0;
  entry.tokens += result.usage?.total_tokens || 0;
  entry.turns += result.turns || 0;
  entry.tool_calls += result.tool_calls || 0;
  entry.duration_ms += result.duration_ms || 0;
}

console.log(`\nModel: ${MODEL}\n`);
console.log("| mode | runs | correct | avg tokens | avg turns | avg tool calls | avg duration ms |");
console.log("| --- | ---: | ---: | ---: | ---: | ---: | ---: |");
for (const [mode, entry] of Object.entries(aggregate)) {
  const div = (value) => (entry.runs ? Math.round(value / entry.runs) : 0);
  console.log(
    `| ${mode} | ${entry.runs} | ${entry.correct}/${entry.runs} | ${div(entry.tokens)} | ${div(entry.turns)} | ${div(entry.tool_calls)} | ${div(entry.duration_ms)} |`,
  );
}
console.log(`\nResults: bench/results/${stamp}.json`);
