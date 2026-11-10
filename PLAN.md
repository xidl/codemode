# xidl-codemode 实施计划

## 结论

做成一个 Rust 单二进制 MCP server：

1. codemode 对外只支持本地 **stdio** transport。
2. 启动时读取 `~/.config/mcp/mcp.json`（兼容用户要求的 `config.toml`），
   启动配置中的 MCP server，调用 `initialize` + `tools/list`。
3. 把 AI 需要索引的文档写到文件系统：
   - `{mcpname}/agents.md`：上游 `initialize.instructions`（仅当提供时）；
   - `{mcpname}/{namespace}.idl`：按 namespace 分组的 schema；
   - bundle IDL 交给外部 xidl 插件 `xidl-codemode-manifest`，由插件从 IDL
     生成运行时 manifest 映射。
4. 对外只暴露一个 MCP 工具：`execute`。不提供 `list_apis`、`read_api`，
   也不提供 API resource。
5. `execute` 的 JS 通过 **stdio 传入**，作为内联 `code` 执行，不写磁盘。
6. 所有 MCP API 通过全局 `mcp` 对象注入 JS runtime，直接调用：
   `mcp.github.get_issue()`。
7. 沙箱不暴露文件系统、网络、子进程、环境变量；只暴露 `mcp` 和少量
   helper。工作区文件由 AI 的文件工具索引，不进入 QuickJS。

核心原则：**上游 MCP 协议是运行时事实来源，xidl 是 API 契约和文档视图，
manifest 是运行时派发表**。这些产物由同一个归一化 catalog 生成，避免手写
漂移。

关键决策与取舍见 [`docs/design.md`](docs/design.md)。

---

## 1. 现状调查

### 1.1 MCP 配置

- 当前实际文件是 `~/.config/mcp/mcp.json`，不是 `config.toml`。
- 它是 Home Manager 生成的 Nix store 文件：
  `~/.config/mcp/mcp.json -> /nix/store/...-mcp.json`
- Home Manager 源配置在：
  - `~/.config/home-manager/home/programs/mcp.nix:4`
    （`servers = { ... }`）
  - `~/.config/home-manager/home/programs.nix:329`
    （Pi 的 `mcp.json` 镜像）

当前配置包含 9 个 server：

| server | transport | 备注 |
| --- | --- | --- |
| `agent-browser` | stdio | 29 个 core tools，字段大量重复 |
| `codegraph` | stdio | 1 个 tool：`codegraph_explore` |
| `context7` | HTTP | `Authorization` header |
| `duckduckgo` | stdio | `uvx` 启动 |
| `github` | stdio | `GITHUB_PERSONAL_ACCESS_TOKEN` |
| `mobile-mcp` | stdio | `pnpx` 启动 |
| `paddleocr-mcp` | stdio | `uvx` 启动，环境变量配置 |
| `rxresu` | HTTP | `x-api-key` header |
| `stitch` | HTTP | `X-Goog-Api-Key` header |

结论：codemode 对外只提供 stdio；上游仍需同时支持 stdio 和 streamable
HTTP。必须把 header/env 视为 secret，不能进入 IDL、日志或 manifest。

### 1.2 xidl 能力

已确认 xidl 可以直接作为 Rust library 使用：

- `xidlc/src/lib.rs:48`：`generate_from_source(lang, idl, props)`。
- `xidlc/src/driver/mod.rs:70`：生成结果 `File { path, content }`。
- `xidlc/Cargo.toml`：可用 `default-features = false, features =
  ["gen-typescript"]` 降低依赖。

IDL 侧可用的关键能力：

- `struct`、`enum`、`union`、`sequence<T>`、`map<K,V>`、`any`。
- struct 继承：`struct Derived : Base { ... }`。
- `@optional`、`@rename`、`@default`、`@range`、`@min`、`@max`。
- `///` Markdown 文档注释。
- TypeScript target 生成 `.d.ts` + `.zod.ts`，但**不生成 interface 方法签名**；
  方法签名和 runtime 注入描述由本项目的 inject emitter 负责。

因此 xidl 在这里承担三件事：

1. 生成可读、可审阅的 API 契约。
2. 用 xidlc 做语法/语义校验。
3. 生成上游数据类型的 `.d.ts` / `.zod.ts`，供文档和编辑器使用。

运行时的函数派发不依赖重新解析 IDL，而使用同一份归一化 catalog 生成的
manifest，避免 xidlc 版本差异影响执行。

### 1.3 真实 tool schema 观察

- `codegraph_explore`：参数 `query`、`maxFiles`、`projectPath`，camelCase。
- `agent-browser`：29 个 tool，每个 tool 都带一大组公共字段（`session`、
  `timeoutMs`、`restore`、`allowedDomains` 等），需要拆分 schema 文件。
- 这意味着 IDL 生成需要：
  - 保留原始 JSON key（camelCase 通常就是合法 IDL identifier）。
  - 对公共字段做可选抽取（base struct + 继承），否则 agent-browser 的
    schema 文档会有大量重复。
  - 按 namespace 分组，一个 namespace 一个 IDL 文件；无法分组的 tool 归入
    `default` namespace。文件落盘，方便 AI 索引。

### 1.4 技术选型

- MCP server/client：`rmcp`（官方 Rust SDK；stdio、child process、
  streamable HTTP、custom headers 都有支持）。
- JavaScript 沙箱：`rquickjs`（QuickJS-NG，支持 memory limit、stack limit、
  interrupt handler、module loader、async host function）。
- IDL：`xidlc` library。
- 异步运行时：`tokio`。
- CLI/config：`clap` + `serde` + `serde_json` + `toml`。

---

## 2. 核心设计

### 2.1 总体数据流

```text
mcp.json / config.toml
        |
        v
  ConfigLoader  (normalize, redact, reject self)
        |
        v
  McpConnector  (rmcp: stdio / streamable HTTP)
        |
        v
  tools/list    (pagination, schema capture)
        |
        v
  ToolCatalog   (server -> tools -> JSON Schema)
        |
        +--> AgentsEmitter    -> on-disk {mcpname}/agents.md
        +--> IdlEmitter       -> on-disk {mcpname}/{namespace}.idl
        +--> BundleIdl        -> .codemode/.internal/{mcpname}.bundle.idl
        +--> XidlExternalPlugin
               xidlc gen codemode-manifest bundle.idl
               -> manifest.json (runtime mapping)
        +--> XidlcValidator   -> parse check / optional .d.ts
        |
        v
  Workspace files (AI-indexable) + ApiRegistry (mcp.<server>.<tool>)
        |
        v
  execute(code) -> QuickJS -> host bridge -> upstream MCP
```

### 2.2 配置加载

路径解析顺序：

1. `--config <path>`（可重复，按顺序合并）。
2. `$XDG_CONFIG_HOME/mcp/config.toml`（用户要求的形式）。
3. `$XDG_CONFIG_HOME/mcp/mcp.json`（当前环境实际形式）。
4. `~/.config/mcp/config.toml` / `~/.config/mcp/mcp.json` 兜底。

格式：

- JSON：Home Manager 原生格式。
- TOML：字段与 JSON 等价，顶层支持 `mcpServers` 或 `servers`。
- 同名 server 冲突时报错；`--allow-shadow` 才允许后者覆盖。

归一化后的 server 模型：

```rust
enum Transport {
    Stdio { command: String, args: Vec<String>, env: BTreeMap<String, String>, cwd: Option<PathBuf> },
    Http  { url: String, headers: BTreeMap<String, String> },
    Sse   { url: String, headers: BTreeMap<String, String> },
}
```

规则：

- 缺省 `type`：有 `command` 视为 stdio，有 `url` 视为 http。
- 支持 `disabled = true` / `enabled = false`。
- HTTP headers 转成 `HeaderName` / `HeaderValue`；`Authorization: Bearer X`
  走 rmcp 的 `auth_header("X")`，其余走 `custom_headers`。
- 子进程默认 `env_clear()`，只传 `PATH`、`HOME` 和配置里的 env；可选
  `inheritEnv = true` 才继承父环境。
- 默认跳过 server 名 `xidl-codemode`，以及 command 等于当前可执行文件的
  server，防止递归。
- 配置文件本身可能是 Nix store 只读文件；只读不写。

Secret 处理：

- 配置中的 env value、header value 进入 `SecretSet`。
- 日志、错误、`doctor`、manifest、IDL 全部做 redaction。
- 不把 env/header 写进生成的任何文件。
- `--trace-args` 默认关闭；即使开启，也要先 redaction。

### 2.3 MCP 连接与 catalog

启动时并发连接所有 server：

- 每个 server：`initialize` -> `notifications/initialized` -> `tools/list`。
- `tools/list` 支持分页，循环 `nextCursor` 直到结束。
- 单个 server 超时（默认 15s）只标记为 `error`，不阻塞其他 server。
- 记录：tool name、title、description、inputSchema、outputSchema、
  annotations（`readOnlyHint` 等）。
- 记录 `initialize.instructions`，用于生成 `{mcpname}/agents.md`。
- 连接成功后保留 `Peer`，供 `execute` 调用；子进程设置 kill-on-drop，
  server 退出时清理。
- 启动失败可 lazy retry：`execute` 首次调用该 server 的 tool 时重试一次。

`catalog` 同时保留每台 server 的：

- `server_name`（原始配置名）
- `workspace_dir`（该 server 的落盘目录）
- `tools`（原始名 + 安全 JS 名 + schema）
- `agents_md`（落盘文档 + 注入内容）
- `namespace_idls`（namespace -> IDL 文件路径）
- `namespaces`（namespace 分组结果）
- `status`（ok / error）
- `warnings`（schema 降级、名称冲突等）

### 2.4 工作区与 namespace

codemode 把 AI 需要索引的文档写到文件系统。默认工作区：

```text
<cwd>/.codemode/
```

可用 `--workspace <dir>` 覆盖。布局：

```text
<workspace>/
  github/
    agents.md
    issues.idl
    pulls.idl
  agent-browser/
    agents.md
    core.idl
    network.idl
  codegraph/
    agents.md
    default.idl
```

文件路径为 `/{mcpname}/agents.md` 和 `/{mcpname}/{namespace}.idl`（相对
工作区根目录）。`agents.md` 同时会注入 codemode 的 `instructions`；IDL
主要给 AI 用文件工具索引。

MCP 的工具名本身是扁平的，没有标准 namespace 字段。分组规则：

1. tool 名形如 `foo.bar` / `foo:bar`：取 `foo` 为 namespace。
2. 否则按 `_` / `-` 切分；如果第一段被同 server 多个 tool 共享，取第一段。
3. 无法判断时归入 `default`。
4. 配置里允许 `namespace_map` 显式覆盖（例如 agent-browser 的
   core/network/state profiles）。

一个 namespace 一份 IDL。`common` 是保留 namespace，用于跨 namespace
共享类型。

`agents.md` 只承载上游 MCP server 自己提供的 `initialize.instructions`。
没有 instructions 的 server 不写 `agents.md`。它不是 tool 索引，也不重复
IDL 里的参数签名。

工作区规则：

- 生成物只读，文件头标记 `generated by xidl-codemode; do not edit`。
- 原子写入（临时文件 + rename）；避免写一半被索引。
- 默认单 codemode 实例对应一个工作区；多实例/多配置并发写同一目录不在
  MVP 支持范围内。
- 工作区不应提交到 git；建议 `.codemode/` 加入 `.gitignore`。
- 参数名、顺序、类型、optional 只在 IDL 文件里；AI 用 `rg` / `read` 获取。
- codemode 的 `instructions` 只说明 workspace 约定和调用方式，不放索引。

JS 命名空间：

```text
mcp.<serverNs>.<toolFn>(arg1, arg2, ...)
```

- `serverNs` 默认由 server 名 sanitize 成合法 JS identifier，例如
  `agent-browser` -> `agent_browser`。
- 同时保留原名访问：`mcp["agent-browser"]`。
- `toolFn` 优先保留原始 tool 名；非法时 sanitize，并保留 bracket access。
- `mcp` 和每个 server namespace 使用 null-prototype 对象并冻结。

### 2.5 JSON Schema -> xidl

按 namespace 生成 IDL；每个 namespace 一份 IDL 文件，包含该 namespace 下
所有 tool 的 input/output/result 和 interface method。

| JSON Schema | xidl |
| --- | --- |
| `object` + properties | `struct` |
| `required` 中的字段 | 非 `@optional` |
| 非 required 字段 | `@optional` |
| `description` | `///` 注释 |
| `default` | `@default(...)` |
| `minimum` / `maximum` | `@min(...)` / `@max(...)`，两者都有时 `@range(...)` |
| `type: string` | `string` |
| `type: integer` | 默认 `int64`；范围明确且较小时可收窄 |
| `type: number` | `double` |
| `type: boolean` | `boolean` |
| `type: array` + `items` | `sequence<T>` |
| `type: object` + `additionalProperties` 且无固定 properties | `map<string,T>` |
| `$ref` / `$defs` | 生成命名 struct，支持递归 |
| 字符串 enum | 安全时生成 xidl `enum`，否则 `string` |
| `oneOf` / `anyOf` 含 null | 能降级则降级，否则 `any` |
| `allOf` | 全为 object 时合并，否则 `any` |
| 无法表达的结构 | `any` + warning |

规则：

- JSON key 是合法 IDL identifier 时原样保留（例如 `projectPath`）。
- 非法 key 转合法 identifier，并加 `@rename("<original>")`。
- tool 名合法时原样作为 IDL method 名；非法时 sanitize，原始 MCP tool 名写入
  `@mcp_tool("...")`，由 manifest 插件读取。
- struct 名加 server/tool 前缀，避免跨 namespace 冲突。
- 跨 namespace 共享类型放 `common` namespace。
- 每个 server 生成一个内部 `bundle.idl`（`.codemode/.internal/`）用于
  xidlc 校验和 manifest 生成；落盘的 per-namespace IDL 是它的视图，避免
  `common` import 语义未验证时生成失败。

IDL 策略：

- **只生成，不支持手写 IDL override**。
- 每个 server 的所有 IDL 必须通过 xidlc 校验后才写入工作区。
- JSON Schema 中无法表达的结构降级为 `any` + warning，这是 schema 映射
  预期内的降级，不算 bug。
- 如果生成的 IDL 本身无效（xidlc 拒绝）或语义错误（调用参数/类型不符合
  MCP schema），这是 bug，不允许静默 fallback。
- 失败处理：
  1. 该 server 标记为 `error`，不注册 `mcp.<server>` namespace；其他 server
     继续工作。
  2. 生成可提交的 bug report：server/tool、原始 JSON Schema（redacted）、
     生成的 IDL、xidlc 错误、codemode/xidlc 版本、复现命令。
  3. 如果生成 IDL 符合映射规则但被 xidlc 拒绝，提 xidl issue；如果映射逻辑
     产出了错误 IDL，提 codemode issue。
  4. CLI 提供 `bug-report` 子命令输出 Markdown，便于直接贴 issue；不自动
     创建 GitHub issue。

### 2.5.1 manifest 由外部 xidl 插件生成

不使用 codemode 内部的 manifest emitter。新增外部插件：

```text
target: codemode-manifest
binary: xidl-codemode-manifest
```

`xidlc` 的 external plugin 机制会启动 `xidl-<lang>` 子进程，通过 NDJSON
`Codegen` 协议传递 HIR。插件实现：

```rust
fn get_engine_version() -> "*"
fn get_properties() -> {}          // 默认 unit-scoped HIR
fn generate(input: CodegenInput, path, props) -> Vec<Artifact>
```

插件只读 `CodegenInput::RpcHir`，遍历 interface/operation 和
`@mcp_*` annotation，返回一个 `File { path: "manifest.json", content }`。

IDL 必须携带运行时映射所需的 annotation：

- `@mcp_server("github")`：interface 级。
- `@mcp_namespace("issues")`：interface 级。
- `@mcp_tool("get_issue")`：operation 级，保存原始 MCP tool 名；即使 JS 名
  与 MCP 名相同也必须写，避免插件靠命名推断。
- `@mcp_read_only` / `@mcp_destructive` / `@mcp_idempotent` /
  `@mcp_open_world`：operation 级 flags。

xidl lint 明确允许 unknown annotation 由 external plugin 解释
(`xidl-parser/src/lint/check.rs`)，所以这些 annotation 不需要改 xidl 核心。

manifest 示例：

```json
{
  "version": 1,
  "server": "github",
  "namespaces": {
    "issues": {
      "idl": "/github/issues.idl",
      "interface": "GithubIssuesApi",
      "tools": [
        {
          "js": "get_issue",
          "mcp": "get_issue",
          "output": "GithubIssueResult",
          "params": [
            { "name": "owner", "wire": "owner", "required": true },
            { "name": "repo", "wire": "repo", "required": true },
            { "name": "issue_number", "wire": "issue_number", "required": true }
          ],
          "readOnly": true,
          "destructive": false
        }
      ]
    }
  }
}
```

生成流程：

1. codemode 生成 per-server `bundle.idl`。
2. 通过 xidlc external plugin 调用 `codemode-manifest`：
   `xidlc gen --out-dir <internal> codemode-manifest <bundle.idl>`。
3. 插件返回 `manifest.json`；codemode 读入内存并校验：
   - 每个 `tools/list` tool 都有 manifest entry；
   - 每个 entry 都能映射回真实 MCP tool；
   - namespace/IDL 路径一致；
   - JS 名无冲突。
4. 校验失败或插件失败 -> server error + bug report；不静默 fallback。

插件通过 PATH 被发现（`xidl-codemode-manifest`）。安装时必须保证插件和
codemode 在同一个 bin 目录，或由 codemode 给 xidlc 子进程注入插件目录。

嵌套 object 的公共类型抽取（可选优化）：

- 顶层 tool 参数按 schema properties 顺序生成位置参数，不再包一层 input struct。
- 嵌套 object 仍生成命名 struct；公共字段抽取只针对这些嵌套 struct。
- 第一版先不抽取，保证正确性；第二版再加。

返回类型：

- 通用 `McpToolResult`：

```idl
struct McpToolResult {
    boolean isError;
    sequence<any> content;
    any structuredContent;
};
```

- 有 `outputSchema` 时生成 `<Tool>Output`，并为该 tool 生成专用 result
  struct；没有 outputSchema 时使用通用结果。

IDL 示例（示意）：

```idl
#pragma xidlc package agent-browser
#pragma xidlc version 1.0.0

struct AgentBrowserOpenResult {
    boolean isError;
    sequence<any> content;
    any structuredContent;
};

interface AgentBrowserCoreApi {
    /// MCP tool: agent_browser_open
    AgentBrowserOpenResult agent_browser_open(
        @optional sequence<string> allowedDomains,
        @optional string session,
        @optional int64 timeoutMs,
        @optional any restore,
        @optional string url,
        @optional boolean headed,
        @optional boolean webgpu
    );
};
```

### 2.6 Code mode API

沙箱内直接使用注入的全局 `mcp` 对象，不需要 import/require：

```js
const issue = mcp.github.get_issue("xidl", "xidl", 361);
return unwrap(issue);
```

`mcp` 对象：

- 第一层 key 是 server namespace；每个 namespace 是 null-prototype 冻结对象。
- 第二层是 tool 函数，参数按 MCP input schema 的 property 顺序位置传入；
  runtime 把它们映射为 MCP args object。
- MVP 同步返回原始 `CallToolResult`；`await` 普通值仍然可用。
- 原始 server/tool 名通过 bracket access 保留。
- 不提供 `mcp.list()` / `mcp.read()` 之类的读取 API；发现信息来自工作区
  IDL 文件，以及 `instructions` 里的 workspace 约定。

函数返回值保持 MCP 原始 `CallToolResult`：

```ts
interface CallToolResult {
  content: Array<{ type: string; text?: string; data?: string; mimeType?: string }>;
  structuredContent?: unknown;
  isError?: boolean;
}
```

提供全局 `unwrap(result)`：

- `isError === true` 时抛错。
- 有 `structuredContent` 时返回它。
- 否则返回 `content`。

### 2.7 QuickJS 沙箱

`execute({ code })` 的宿主流程：

1. 从 MCP stdio 请求中读取 `code` 字符串。
2. 校验大小、NUL/非法字节和 timeout。
3. 在新建的 QuickJS `Runtime` + `Context` 中执行。

JS 不写磁盘；工作区只放 `agents.md` / IDL。沙箱本身没有文件系统权限。

每次 `execute` 创建全新 `Runtime` + `Context`：

- 不跨执行复用 JS 状态。
- MCP client 连接可以复用；参数和结果只以 JSON 字符串跨边界。

资源限制（默认值，可配置）：

| 限制 | 默认 |
| --- | --- |
| 执行时间 | 10s（最大 60s） |
| 内存 | 128 MiB |
| 栈 | 1 MiB |
| 执行代码大小 | 256 KiB |
| 单次 tool 调用数 | 100 |
| 返回结果 | 1 MiB |
| console 日志 | 100 条 / 64 KiB |

沙箱全局：

- 标准 ECMAScript 对象。
- `console.log/info/warn/error`：写入执行结果，不写 stdout。
- `mcp`：按 server/tool 注入的 API 命名空间。
- `unwrap`：结果解包 helper。
- 不提供 `fetch`、`process`、`fs`、`child_process`、`Deno`、`Bun`。

中断与桥接：

- QuickJS `set_interrupt_handler` 检查 deadline / cancellation flag。
- 超时返回 `code = "timeout"`；内存超限返回 `code = "oom"`。
- 宿主桥接沿用独立 tokio bridge task + 阻塞 channel，避免嵌套 `block_on`。

错误分类：

- `js_error`、`timeout`、`oom`、`tool_error`、`serialization_error`、
  `limit_exceeded`。

### 2.8 对外 MCP 接口

只暴露一个工具：

`execute`

- 输入：
  - `code: string`：通过 stdio 传入的 JavaScript，必填；
  - `timeout_ms?: integer`：执行超时，默认 10000，最大 60000。
- 输出：
  `{ ok, result, logs, tool_calls, duration_ms, truncated, error? }`。
- description 动态包含：
  - server namespace 列表；
  - `mcp.<server>.<tool>()` 示例；
  - 工作区绝对路径和 namespace 文件索引；
  - 沙箱限制。

不提供：

- `list_apis`
- `read_api`
- MCP resources
- 任何其他读取 API 的工具

发现流程：

1. codemode 启动后把 namespace IDL 写到工作区；有上游 instructions 的
   server 额外写 `agents.md`。
2. codemode 的 `instructions` 只说明 workspace 约定、`mcp.<server>.<tool>`
   调用方式和 `unwrap`；上游 instructions 以不可信区块注入。
3. 模型用自己的文件工具 `rg` / `read` IDL，获取参数名、顺序和类型。
4. 模型把 JS 作为 `execute({ code })` 通过 stdio 传入；JS 不落盘。

工作区是 API 细节来源；`instructions` 不放索引或签名。若客户端无法读
工作区，只能依赖上游 instructions，API 发现能力会受限。

### 2.9 工作区与启动

- 工作区写 namespace IDL；有上游 instructions 时写 `agents.md`。manifest
  由插件生成到
  `.codemode/.internal`，读入内存后作为运行时映射。
- `.codemode/.internal` 不是 AI 文档区，不参与索引。
- 不做磁盘 cache；进程重启后重新连接上游并重写工作区。
- 启动时并发连接所有 server，单 server 超时标记为 error，不阻塞其他
  server。
- `execute` 首次调用某个失败 server 时可以 lazy retry 一次。
- 上游 tool 变化通过重启或 `--refresh` 触发。
- 工作区写入使用临时文件 + rename；生成物只读。
- 可选 CLI `--print-docs` 输出文档到 stdout，便于调试。

### 2.10 安全模型

默认拒绝：

- 只支持 stdio；不监听 HTTP。
- 只允许配置中列出的 server；不允许连接自身。
- 沙箱不访问文件系统、网络、环境变量、子进程。
- `execute.code` 大小不超过 256 KiB；拒绝 NUL/非法控制字符。
- 工作区只写生成物，路径限制在 `--workspace` 根目录内；拒绝 `..` 逃逸。
- 子进程 env 默认白名单；不继承父环境。
- tool 调用参数/结果大小有上限；日志和错误做 secret redaction。
- 上游 `initialize.instructions` 以**不可信文本**注入：加边界标记、限长；
  它不能被当作系统指令。
- `--read-only` 可选：只暴露 `annotations.readOnlyHint == true` 的 tool。
- `--allow-tools` / `--deny-tools` 支持按 server/tool glob 过滤。
- `destructiveHint` 只作为元数据展示，不自动信任；高风险场景由用户配置
  allow/deny。

---

## 3. 代码结构

```text
xidl-codemode/
  Cargo.toml
  src/bin/
    xidl-codemode-manifest.rs
  src/
    main.rs
    lib.rs
    cli.rs
    config/
      mod.rs
      model.rs
      redact.rs
    catalog/
      mod.rs
      model.rs
      client.rs
    idl/
      mod.rs
      schema.rs
      names.rs
      namespace.rs
      emit.rs
      validate.rs
    docs/
      mod.rs
      agents.rs
      render.rs
    workspace/
      mod.rs
      layout.rs
      write.rs
    manifest/
      mod.rs
      generate.rs
      validate.rs
    bug_report.rs
    inject/
      mod.rs
      runtime.rs
    sandbox/
      mod.rs
      bridge.rs
      console.rs
      limits.rs
    server/
      mod.rs
      tools.rs
      instructions.rs
  tests/
    fixtures/
      fake_mcp.rs
    config.rs
    idl_snapshot.rs
    sandbox.rs
    e2e_stdio.rs
  docs/
    design.md
  PLAN.md
```

主 crate 做 codemode 编排和 MCP server；`xidl-codemode-manifest` 是同一
package 下的独立 external xidl plugin binary。两者在同一个 `cargo build`
中构建，安装时必须一起进入 PATH。

依赖草案：

```toml
rmcp = { version = "3.9", features = [
  "server", "client", "transport-io", "transport-child-process",
  "transport-streamable-http-client-reqwest",
] }
rquickjs = { version = "0.14", features = ["loader", "futures"] }
xidlc = { path = "../xidl/xidlc", default-features = false, features = ["gen-typescript"] }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.9"
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = "0.3"
thiserror = "2"
anyhow = "1"
sha2 = "0.10"
directories = "6"
```

说明：`xidlc` 先用本地 path，方便和 `~/xidl` 同步；Nix 打包时改用 crates.io
版本或固定 git rev，避免 Nix sandbox 访问 `../xidl`。

`xidl-codemode-manifest` 额外依赖：

```toml
xidlc = { path = "../../xidl/xidlc", default-features = false }
xidl-parser = { path = "../../xidl/xidl-parser" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

---

## 4. 里程碑

### M0：技术验证（0.5–1 天）

- `cargo init`，验证 `rmcp`、`rquickjs`、`xidlc` 三个依赖能一起编译。
- 读取真实 `mcp.json`，连接 `codegraph` 和 `agent-browser`，打印
  `tools/list` 数量。
- 用真实 tool schema 生成一份 IDL，并调用
  `xidlc::generate_from_source("typescript", ...)` 验证。
- 写一个最小 external plugin `xidl-codemode-manifest`，验证
  `xidlc gen codemode-manifest` 能通过 NDJSON `Codegen` 协议生成
  `manifest.json`。
- QuickJS 验证：eval、memory limit、interrupt、同步 host function、全局
  `mcp` 注入。

验收：

- `xidl-codemode doctor` 能看到 9 个 server 的状态（允许部分失败）。
- 一份真实 schema 生成的 IDL 能通过 xidlc 校验。
- `while(true){}` 能在超时后中断。

### M1：配置 + catalog + IDL（1–2 天）

- ConfigLoader（JSON/TOML、type 推断、secret redaction、self-exclusion）。
- McpConnector（stdio + HTTP、分页 `tools/list`、并发、超时、部分失败）。
- Schema -> xidl IR、namespace 分组、`agents.md`、manifest；文档落盘到
  workspace；manifest 由 external xidl plugin 从 IDL 生成。
- IDL 生成 `@mcp_server` / `@mcp_namespace` / `@mcp_tool` / flags
  annotation；manifest 插件读取 HIR 生成映射。
- manifest 与 `tools/list` catalog 交叉校验。
- CLI：`generate`（写 workspace）、`servers`、`doctor`、`bug-report`。
- 快照测试：codegraph、agent-browser、边界 schema。

验收：

- 每个 server 生成 `{mcpname}/agents.md` 和
  `{mcpname}/{namespace}.idl`。
- 所有 IDL 通过 xidlc TypeScript 生成。
- 输出中不出现配置里的 secret。
- IDL 包含完整位置参数签名；有上游 instructions 的 server 生成
  `agents.md`。
- `xidlc gen codemode-manifest` 生成的 manifest 能覆盖所有 tool；
  缺项/多项/名字冲突都报错。

### M2：QuickJS 执行（2–3 天）

- Sandbox runtime、console、全局 `mcp` 注入、`unwrap`。
- 宿主桥接 `__xidl_call`，支持 fake MCP tool 调用。
- `execute({ code })` MCP tool、stdio 内联代码校验、结果 envelope、
  超时/OOM/序列化错误。
- 集成测试：启动 fake MCP，通过 `execute({ code })` 执行并校验结果。

验收：

- `mcp.fake.echo()` 能调用 fake tool。
- `execute({ code: "return mcp.fake.echo(...)" })` 能执行脚本。
- 超时、内存、调用次数、结果大小限制都有测试。
- 沙箱内无 `fetch`/`process`/文件系统。

### M3：真实 MCP 接入（1–2 天）

- HTTP transport + headers。
- 真实 server 的 initialize / tools/list / 只读调用。
- 分页、lazy retry、refresh、self-exclusion。
- 大 server 的 namespace 分组和 `agents.md` 注入预算。

验收：

- `codegraph` 和 `agent-browser` 可执行只读 tool。
- `context7` / `stitch` / `rxresu` 至少能完成 initialize + tools/list。
- 配置中同时存在 `xidl-codemode` 自身时不会递归。
- 工作区包含可索引的 `agents.md` / namespace IDL；`execute` 不落盘。

### M4：固化与交付（1–2 天）

- `execute` 的单工具接口、`agents.md` 注入、stdio 代码大小校验。
- `--read-only`、allow/deny filter、redaction 测试。
- 文档、Home Manager snippet、Nix package/flake。
- 安装时同时提供 `xidl-codemode` 和 `xidl-codemode-manifest`，保证插件在
  PATH 中。
- 端到端 MCP stdio 测试，真实配置 smoke。

验收：

- `cargo test`、`cargo clippy -- -D warnings`、`cargo fmt --check` 通过。
- `nix build .#xidl-codemode` 通过（如提供 flake）。
- 真实客户端通过 stdio 启动并执行示例脚本。

### M5：可选增强

- 异步 tool call（Promise / `AsyncRuntime`）。
- 大 server 按 profile/前缀进一步合并或拆分 namespace。
- 上游 instructions 的注入上限 / trust 标记 / 分 server 注入策略。
- 可选：失败时自动创建 GitHub issue（需要 token 和仓库配置，默认关闭）。
- BDD 场景（`behave`/cucumber）覆盖端到端流程。

---

## 5. 测试计划

### 单元测试

- 配置解析：JSON/TOML、缺省 type、disabled、同名冲突、self-exclusion。
- Secret redaction：env/header 值不出现在日志/manifest/IDL/错误中。
- namespace 提取：点号/冒号前缀、下划线/中划线前缀、`default` 回退、
  `namespace_map` 覆盖、冲突处理。
- IDL 位置参数签名、`agents.md` 上游 instructions 注入预算。
- 每个 namespace 一个 IDL、`common` 共享类型、`bundle.idl` 校验。
- manifest 插件：HIR fixture -> manifest 快照；server/namespace/tool/
  input/output/flags 映射；unknown annotation 由插件解释。
- Schema 映射：primitive、array、object、required/optional、`$ref`、
  `oneOf`/`anyOf`、enum、additionalProperties、递归。
- 名称 sanitize：tool/property 非法标识符、collision、原始名映射。
- IDL 快照：codegraph、agent-browser、边界 schema。
- 工作区写入：目录布局、只读权限、原子重命名、路径逃逸、无 secret。
- 生成 IDL 失败：server 标记 error、不注册 namespace、生成 bug report；
  不允许静默 fallback。

### 集成测试

- fake MCP stdio server：提供 `echo`、`add`、`fail`、`slow`、`large` 工具。
- 启动 `xidl-codemode serve`，用 rmcp client 完成：
  - `initialize`
  - `tools/list`
  - 检查对外工具只有 `execute`
  - 通过 `execute({ code })` 调用 fake tools
- `execute` 输入 schema 只有 `code` / `timeout_ms`，传 `path` 应被拒绝。
- 断言：workspace 有 `agents.md` / namespace IDL；执行代码没有落盘；
  `instructions` 包含索引和签名。
- `xidlc gen codemode-manifest` 外部插件集成：插件在 PATH 中时能产出
  manifest；插件缺失时 server error + 明确错误。
- manifest 与 catalog 不一致（缺 tool、多 tool、JS 名冲突）时生成 bug
  report。
- HTTP fake server（可选）：验证 header 和 streamable HTTP。

### 安全/鲁棒性测试

- `while(true){}` 超时。
- 大数组导致 OOM。
- `execute({ code })` 超过 256 KiB 失败。
- `execute` 中带 NUL/非法控制字符失败。
- 工作区生成时 `..` 路径逃逸失败。
- `typeof fetch === "undefined"`、`typeof process === "undefined"`。
- `typeof mcp === "object"`，且 `mcp.fake.echo` 可调用。
- 返回循环引用/函数/BigInt -> `serialization_error`。
- 上游 tool 返回 `isError: true` -> raw result + `unwrap` 抛错。
- 上游 server 启动失败 -> 其他 server 不受影响。
- 配置含 secret -> 所有输出 redaction。

### 手工验证

```bash
xidl-codemode doctor --config ~/.config/mcp/mcp.json
xidl-codemode generate --config ~/.config/mcp/mcp.json
xidl-codemode serve --config ~/.config/mcp/mcp.json
```

用真实客户端通过 stdio 发送内联代码：

```js
return unwrap(mcp.codegraph.codegraph_explore(
  "MCP tool handler",
  12,
  "/path/to/project",
));
```

```json
{ "code": "return unwrap(mcp.codegraph.codegraph_explore('MCP tool handler', 12, '/path/to/project'));" }
```

---

## 6. 风险与应对

| 风险 | 影响 | 应对 |
| --- | --- | --- |
| JSON Schema 表达力超过 xidl | 生成失败或类型失真 | 支持常见子集；无法表达降级 `any`；保留原始 schema 在 manifest；逐 tool 校验 |
| agent-browser schema/签名的上下文过大 | 模型不知道被省略的 API | namespace 分组；完整签名优先；压缩描述；include/exclude 缩减；超预算启动失败 |
| tool/property 名字非法或冲突 | 调用错 tool | 统一 sanitize + manifest 原名映射；冲突加后缀；测试覆盖 |
| `execute` 代码不可控 | 代码过大/控制字符注入 | 256 KiB 上限；拒绝 NUL/非法控制字符；沙箱超时/内存限制 |
| 工作区所有权 | 多项目/多实例互相覆盖 IDL；AI 索引到旧文档 | 默认项目内 `.codemode/`；原子写入；单实例假设；可选 per-config 目录 |
| 生成的 IDL 无效 | 模型看到错误契约，调用失败 | 写工作区前用 xidlc 校验；失败则 server error + bug report，不提权 fallback |
| manifest 插件缺失/版本不兼容 | 无法生成运行时映射 | 启动时检查 `xidl-codemode-manifest`；版本不匹配明确报错；打包保证同 bin 目录 |
| IDL annotation 缺失 | manifest 无法恢复原始 tool 名/flags | IDL 生成器强制写 `@mcp_*`；manifest 校验缺项即 bug |
| 上游 instructions 被当系统提示 | prompt injection | 注入时标记为 untrusted 数据、加边界和长度上限；API 发现仍靠 IDL |
| QuickJS 异步桥复杂 | tool call 阻塞/死锁 | MVP 用独立 bridge task + 阻塞 channel；异步作为 M5 |
| HTTP MCP header/协议差异 | 3 个 HTTP server 无法连接 | rmcp `custom_headers`/`auth_header`；M0 先验证 context7；必要时实现自定义 HTTP client |
| xidlc path 依赖 | Nix/发布困难 | 本地 path；Nix 用 crates.io/git pin；或改用 xidlc CLI 子进程 |
| 子进程泄漏 | 资源占用 | kill-on-drop；shutdown 统一 close；超时重试有上限 |
| secret 泄漏 | 严重安全风险 | SecretSet redaction；不写 artifact；测试含 secret 的配置 |
| 自身递归 | 无限启动/连接 | 按名字和可执行文件路径双重排除 |
| 结果/日志过大 | 内存/上下文压力 | 大小上限 + truncated 标记；日志条数上限 |

---

## 7. 待确认的默认决策

下面是计划采用的默认值；如果用户有不同偏好，在 M0 前调整：

1. **实现语言**：Rust（推荐，单二进制、rmcp/rquickjs/xidlc 都是 Rust）。
2. **Code mode 入口**：全局 `mcp.<server>.<tool>()`；不提供 `require` /
   `import`。
3. **IDL 粒度**：按 namespace 分组，一个 namespace 一个
   `{mcpname}/{namespace}.idl`；共享类型放 `common` namespace。
4. **返回值**：保持 MCP 原始 `CallToolResult`，用 `unwrap` 取
   `structuredContent` / `content`。
5. **执行模型**：MVP tool call 同步；Promise 并发放到 M5。
6. **对外工具**：只提供 `execute({ code, timeout_ms? })`，不提供读取 API
   的工具/resource。
7. **配置格式**：JSON 为主，TOML 兼容；当前实际路径是
   `~/.config/mcp/mcp.json`。
8. **安全默认**：不继承父环境；不暴露 FS/网络；默认跳过自身；支持
   `--read-only` 和 allow/deny；`execute.code` 限制 256 KiB。
9. **传输与落盘**：codemode 对外只提供 stdio；`agents.md` / IDL 落盘到
   工作区供 AI 索引；`execute.code` 通过 stdio 传入，不落盘。
