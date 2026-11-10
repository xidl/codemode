# xidl-codemode 设计文档

## 1. 文档定位

本文描述 `xidl-codemode` 0.3.0 的**实际实现**和背后的关键取舍。它不是最初
的设想稿；`PLAN.md` 保留为实施计划和后续路线，具体行为以本文和
`README.md` 为准。

核心模型只有一句话：

> 上游 MCP server 是运行时事实来源，生成到工作区的 IDL 是给 AI 阅读和索引
> 的契约视图，manifest 是由 IDL 派生、只在内存中使用的运行时派发表。

## 2. 总体架构

```text
mcp.json / config.toml
        |
        v
  ConfigLoader
        |
        v
  UpstreamManager（单 bridge 线程）
        |
        +--> initialize + tools/list --> ServerCatalog
                                           |
                                           v
                                     idl::generate
                                           |
              +----------------------------+----------------------------+
              |                            |                            |
              v                            v                            v
     {server}/agents.md          {server}/{namespace}.idl     .internal/{server}.bundle.idl
     （仅有上游 instructions）          （AI 文档）                        |
                                                                         v
                                                    xidlc + xidl-codemode-manifest
                                                                         |
                                                                         v
                                                                   manifest.json
                                                                   （仅内存）
                                                                         |
                                                                         v
                                                                  RuntimeServer
                                                                         |
MCP client --execute(code)--> QuickJS --__xidl_call--> bridge 线程 --> 上游 MCP
```

一次启动会完成以下步骤：

1. 读取配置，过滤禁用和自身条目。
2. 逐个连接上游 server，完成 `initialize` 和分页 `tools/list`。
3. 把每个 server 的 tool schema 按 namespace 生成 IDL。
4. 生成 `agents.md`（仅当上游提供 `initialize.instructions`）。
5. 用 bundle IDL 调用 external xidl plugin，生成运行时 manifest。
6. 校验 manifest 与 catalog，注册 `mcp.<server>` 命名空间。
7. 对外只暴露一个 `execute` 工具。

## 3. 关键决策

| ID | 决策 | 一句话理由 |
| --- | --- | --- |
| D1 | Rust 单仓库、双二进制 | 主程序、MCP 协议、QuickJS 和 xidlc 都在 Rust 侧，边界最少 |
| D2 | 复用 `rmcp` 做 server 和 client | 不手写 initialize、分页、stdio framing 和生命周期 |
| D3 | 对外只暴露 `execute` | code mode 的核心是收敛工具数量，发现信息走工作区文档 |
| D4 | IDL 落盘，`execute.code` 走 stdio | 文档可被文件工具索引，执行代码不产生临时脚本 |
| D5 | IDL 只生成，不支持手写 override | 避免生成契约与手写契约双写漂移 |
| D6 | manifest 由 external xidl plugin 从 IDL 生成 | manifest 是派生视图，不在 codemode 内再养一套映射逻辑 |
| D7 | 一个 namespace 一个 IDL | 大 server 的 schema 可导航，避免单文件膨胀 |
| D8 | API 使用位置参数 | 避免模型猜测参数名大小写；参数顺序由 IDL 固定 |
| D9 | 每次 execute 新建 QuickJS runtime | 强隔离，避免跨执行状态、原型污染和未释放 Promise |
| D10 | MVP 使用同步桥接 | 先保证超时、计数和错误映射正确，异步并发后置 |
| D11 | 工具返回原始 `CallToolResult` | 不丢失 content、structuredContent 和 `isError` 语义 |
| D12 | 配置 JSON 为主、TOML 兼容 | 当前环境实际是 `mcp.json`，同时满足用户提出的 TOML 形式 |
| D13 | 对外 transport 只有 stdio | 当前是本地工具；远程场景需要额外认证和代码上传设计 |
| D14 | JSON Schema 只映射可表达子集 | 保证所有 server 可接入，无法表达的降级为 `any` |
| D15 | 名称 sanitize，保留原始名别名 | 兼容非法标识符，同时保证仍能调用 wire 上的原名 |
| D16 | `agents.md` 只承载上游 instructions | API 细节只放 IDL，避免签名和描述双写 |
| D17 | 不做磁盘 cache | 每次启动重新发现，避免缓存失效、旧文档和 secret 残留 |
| D18 | 从 crates.io 使用 xidlc/xidl-parser 0.111 | 发布和 CI 可复现，不依赖仓库外的本地路径 |
| D19 | 单个 server 的发现/manifest 失败隔离 | 一个上游坏掉不应拖垮其他 server |

### D1：Rust 单仓库、双二进制

主二进制是 `xidl-codemode`，另一个是 external xidl plugin
`xidl-codemode-manifest`。两者放在同一个 Cargo package 中，由
`cargo build --bins` 一起构建。

这样做是因为 MCP server/client、QuickJS 和 xidlc 都是 Rust 生态；单仓库
便于共享 manifest 类型、统一版本和发布。插件仍然是独立进程，通过 xidlc
的 external codegen 协议调用，不和主进程内部状态耦合。

### D2：复用 `rmcp`

下游使用 `rmcp` 的 stdio server；上游同时支持：

- stdio：`TokioChildProcess`；
- streamable HTTP：`StreamableHttpClientTransport`，支持
  `Authorization: Bearer` 和自定义 header。

配置里的 `sse`、`streamable-http`、`streamable_http` 目前都归一化为
streamable HTTP；没有单独的 SSE transport 实现。

### D3：只暴露 `execute`

下游 `tools/list` 永远只返回 `execute`。不提供 `list_apis`、`read_api`、
MCP resource 或任何其他读取 API 的工具。

模型通过普通文件工具读取工作区里的 IDL 来发现参数名、顺序、类型和
optional 属性。这样下游 MCP context 不会被 N 个上游 tool schema 占满。

### D4：IDL 落盘，执行代码不落盘

工作区默认是当前目录下的 `.codemode/`，可用 `--workspace` 覆盖。

落盘内容：

```text
<workspace>/
  <server>/
    agents.md                 # 仅当上游提供 initialize.instructions
    <namespace>.idl
  .internal/
    <server>.bundle.idl
```

`execute` 的输入是内联字符串：

```json
{
  "code": "return unwrap(mcp.github.get_issue('xidl', 'xidl', 361));",
  "timeout_ms": 10000
}
```

不提供 `path` 参数，也不把代码写入磁盘。IDL 和 `agents.md` 是给 AI 的文档，
不是执行输入。

### D5：IDL 只生成，不支持手写 override

工作区里的 `*.idl` 永远由 catalog 生成，文件内容没有用户可编辑的入口。
如果生成结果有语法或语义问题，应该修生成器或 xidl，而不是手写覆盖。

`bug-report` 子命令输出一个最小 Markdown 报告，包含版本、workspace、
server 错误和复现命令。它**不会**自动创建 GitHub issue。

### D6：manifest 由 external xidl plugin 生成

插件信息：

```text
codegen 名称: codemode-manifest
二进制名称:   xidl-codemode-manifest
```

运行流程是：

```text
bundle IDL
   |
   v
xidlc::driver::Generator("codemode-manifest")
   |
   v
xidl-codemode-manifest
   |
   v
manifest.json（返回给主进程，仅在内存）
```

主进程不会把 `manifest.json` 写到 `.codemode/.internal`；那里只写
`bundle.idl`。

启动时 `ensure_plugin_on_path()` 会优先把主程序所在目录加入 `PATH`
（如果同目录存在插件）。debug 构建下还会尝试 `target/debug`。

### D7：namespace 划分

MCP tool 名本身是扁平的。当前分组规则按顺序执行：

1. tool 名包含 `.`：取第一段；
2. tool 名包含 `:`：取第一段；
3. tool 名包含 `_` 或 `-`，且第一段被同 server 至少两个 tool 共享，
   并且第一段不等于 server 名：取第一段；
4. 否则归入 `default`。

结果经过 identifier sanitize。没有 `common` namespace，也没有跨 namespace
的公共类型抽取。

库层的 `ServerCatalog` 有 `namespace_map` 字段，但配置加载和 bridge 目前
始终传空 map，因此用户配置不能覆盖 namespace 分组。

### D8：位置参数

调用形式：

```js
const issue = mcp.github.get_issue("xidl", "xidl", 361);
return unwrap(issue);
```

参数顺序来自生成的 IDL，而 IDL 的参数顺序来自上游
`inputSchema.properties` 的 JSON 顺序。manifest 只记录每个位置对应的
IDL 参数名、wire 名和是否 required。

运行时行为：

- 参数个数多于 manifest 参数个数：抛错；
- required 位置缺失或值为 `undefined`：抛错；
- optional 位置缺失或值为 `undefined`：不写入 wire object；
- 不做运行时类型校验，类型由上游 MCP server 自己校验。

选择位置参数的原因是避免模型因为参数名大小写或拼写差异反复探测；代价是
长参数列表的可读性较差，必须依赖 IDL。

### D9：每次 execute 新建 QuickJS runtime

每次调用都创建新的 `Runtime` + `Context`，执行完立即销毁。QuickJS 侧没有
文件系统、网络、子进程或环境变量访问。

这样做的成本远低于一次 MCP tool call，但能避免跨执行的全局状态、定时器、
原型污染和未释放 Promise。

### D10：同步桥接

QuickJS 里的 tool 函数是同步函数。宿主通过全局 `__xidl_call` 进入一个独立
bridge 线程，再由该线程上的 tokio `current_thread` runtime 调用 `rmcp`。

结果：

- `await` 在用户代码中不可用。`execute.code` 被包进普通 `Function`，
  `await` 会产生语法错误；
- 返回 Promise 不会被等待，`JSON.stringify(Promise)` 得到 `{}`；
- `Promise.all` 不会带来真实并发；
- 所有上游 tool call 在单个 bridge 线程上串行执行。

这是当前实现的明确限制，不是文档遗漏。异步 Promise 桥接属于后续工作。

### D11：原始 `CallToolResult` 与 `unwrap`

`mcp.<server>.<tool>(...)` 返回上游 `tools/call` 的原始 JSON 结果：

```ts
{
  content: Array<{ type: string; text?: string; ... }>;
  structuredContent?: unknown;
  isError?: boolean;
}
```

`unwrap(result)`：

- `isError === true`：把 text content 拼成错误信息并抛出；
- 有 `structuredContent`：返回它；
- 否则返回整个 `content` 数组。

不直接返回 `structuredContent` 是为了保留多 content block、图片、资源引用
和错误标记。

### D12：配置格式

配置路径解析顺序：

1. `--config <path>`；
2. `$XDG_CONFIG_HOME/mcp/config.toml`；
3. `$XDG_CONFIG_HOME/mcp/mcp.json`；
4. 没有 `XDG_CONFIG_HOME` 时用 `~/.config/mcp/...`。

同一时间只接受一个 `--config`，不支持多文件合并或 `--allow-shadow`。

支持字段：

```toml
[mcpServers.github]
type = "stdio"          # 可选；有 command 时默认 stdio，有 url 时默认 http
command = "github-mcp"
args = []
cwd = "/optional/cwd"
disabled = false
enabled = true

[mcpServers.github.env]
TOKEN = "..."

[mcpServers.remote]
type = "http"
url = "https://example.com/mcp"

[mcpServers.remote.headers]
Authorization = "Bearer ..."
```

顶层 key 是 `mcpServers`，也接受 `servers` 别名。JSON 文件会先按 JSON
解析，失败后再尝试 TOML；`.toml` 文件只按 TOML 解析。未知字段目前会被
忽略，因此 `inheritEnv`、`namespace_map` 之类的字段不会生效。

自身排除规则：

- server 名恰好是 `xidl-codemode`；
- stdio command 字符串恰好等于当前可执行文件路径。

### D13：默认拒绝的沙箱边界

QuickJS 只提供：

- 标准 ECMAScript 内置对象；
- `mcp`；
- `unwrap`；
- `console.log/info/warn/error`。

没有 `fetch`、`process`、`fs`、`child_process`、`Deno` 或 `Bun`。

实现细节：宿主还会注入 `__xidl_manifest`、`__xidl_call`、`__xidl_log`
三个全局函数/字符串，用户代码可以直接访问。它们不是额外的文件或网络
能力，但 `__xidl_call` 可以绕过 position 映射直接调用已连接的上游 tool。
当前配置本身是本地可信输入，因此这不是提权路径；它确实是内部实现暴露，
后续应改为不可枚举或通过闭包封装。

### D14：JSON Schema 映射范围

当前只生成输入参数类型，所有 operation 的返回类型统一是
`McpToolResult`。`outputSchema` 会被 catalog 捕获，但不会生成专用输出
struct。

| JSON Schema | 生成的 IDL |
| --- | --- |
| `string` | `string` |
| `integer` | `int64` |
| `number` | `double` |
| `boolean` | `boolean` |
| `array` + `items` | `sequence<T>` |
| `object` + 非空 `properties` | 命名 `struct` |
| `object` + `additionalProperties` | `map<string, T>`；布尔形式为 `map<string, any>` |
| `type` 数组且只有一个非 null 类型 | 该类型 |
| 其他 `type` 数组、`$ref`、无法判断 | `any` |

不映射的内容包括：`enum`、`default`、`minimum`/`maximum`、`allOf`、
`oneOf`、`$defs`、递归引用和 `outputSchema`。这些不是生成失败；它们会
以 `any` 或普通基础类型降级，并在 IDL 中失去部分精度。

其他 IDL 规则：

- tool description 和字段 description 写进 `///`；
- description 最多保留 12 行；
- required 参数不带 `@optional`，其余带 `@optional`；
- 非法 identifier sanitize，并用 `@rename("original")` 保留 wire 名；
- operation 一定带 `@mcp_tool("原始 tool 名")`；
- flags 只在为 true 时写 `@mcp_read_only`、`@mcp_destructive`、
  `@mcp_idempotent`、`@mcp_open_world`。

### D15：名称 sanitize 与原名别名

server namespace 同时注册：

- `mcp["<原始 server 名>"]`；
- `mcp.<sanitize 后的 server 名>`（如果不同）。

tool 函数同时注册：

- `mcp.server["<原始 tool 名>"]`；
- `mcp.server.<sanitize 后的 tool 名>`（如果不同）。

namespace 对象会被冻结。这样即使 tool 名包含 `.`、`-` 或其他非法字符，
也仍然可以调用。

### D16：`agents.md` 只承载上游 instructions

`agents.md` 的内容就是上游 `initialize.instructions` 的 trim 结果，没有
生成索引、签名或工具描述。上游没有 instructions 时不写文件；如果上一次
运行留下旧文件，会删除。

codemode 的 server instructions 包含：

1. workspace 绝对路径；
2. `mcp.<server>.<tool>(arg1, arg2, ...)` 和 `unwrap` 的调用方式；
3. IDL 的 glob 路径；
4. 标记为 untrusted 的上游 instructions；
5. 启动时失败的 server 列表。

上游 instructions 会被截断到总共约 32 KiB，并明确标注为不可信数据。

### D17：不做磁盘 cache

每次启动都重新连接上游、重新执行 `tools/list`、重新生成 IDL 和 manifest。
没有 snapshot 缓存，也没有“离线启动”。这样不会读到旧工具契约，也不会
在缓存里留下配置 secret。

### D18：xidlc 版本

当前依赖：

```toml
xidlc = { version = "0.111", default-features = false }
xidl-parser = "0.111"
```

不从 `../xidl` path 依赖。这样 CI、release 和 Nix 构建都不需要仓库外的
本地目录。

### D19：失败隔离

单 server 的边界：

- 连接或 `tools/list` 失败：该 server 不注册，记录到 instructions 的
  Errors 区和 `doctor`/`generate` 摘要；
- manifest 生成或校验失败：该 server 不注册，但已经写入的 IDL/
  `agents.md` 会留在工作区；
- workspace 写入失败：中止整个启动。

IDL 生成函数本身不返回 `Result`，不会单独失败；bundle IDL 的解析错误会在
manifest 生成阶段表现为该 server 失败。所以“单 server 失败不拖垮其他
server”覆盖连接、发现和 manifest 生成/校验失败；workspace I/O 失败没有
per-server 隔离。

## 4. 数据模型

### 4.1 `ServerCatalog`

每个 server 在内存中有一份：

```rust
struct ServerCatalog {
    name: String,
    js_name: String,
    instructions: Option<String>,
    tools: Vec<ToolInfo>,
    namespace_map: BTreeMap<String, String>,
}

struct ToolInfo {
    name: String,
    description: String,
    input_schema: Value,
    output_schema: Option<Value>,
    read_only: bool,
    destructive: bool,
    idempotent: bool,
    open_world: bool,
}
```

`output_schema` 目前只用于记录，不参与 IDL 生成，也不进入 manifest。

### 4.2 IDL 形状

每个 namespace 文件包含：

- `#pragma xidlc package <server>`；
- `#pragma xidlc version 1.0.0`；
- 该 namespace 用到的嵌套 struct；
- 一个 `McpToolResult`；
- 一个带 `@mcp_server` / `@mcp_namespace` 的 interface。

示例：

```idl
#pragma xidlc package github
#pragma xidlc version 1.0.0

struct McpToolResult {
    boolean isError;
    sequence<any> content;
    any structuredContent;
};

@mcp_server("github")
@mcp_namespace("issues")
interface GithubIssuesApi {
    /// Get a single issue.
    @mcp_tool("get_issue")
    @mcp_read_only
    McpToolResult get_issue(
        /// Repository owner.
        string owner,
        /// Repository name.
        string repo,
        /// Issue number.
        int64 issue_number
    );
};
```

bundle IDL 是同一份内容的合并版本，供 xidlc/插件一次性解析。namespace
文件是 bundle 的视图；当前写入 namespace 文件前没有逐个调用 xidlc 校验，
bundle 本身会在 manifest 生成时被解析。

## 5. manifest

### 5.1 生成

`manifest::generate(server, bundle)` 通过
`xidlc::driver::Generator::new("codemode-manifest")` 调用 external plugin，
从返回的 `manifest.json` artifact 反序列化。

插件需要携带的 annotation：

- interface：`@mcp_server("...")`、`@mcp_namespace("...")`；
- operation：`@mcp_tool("...")`；
- 可选 flags：`@mcp_read_only`、`@mcp_destructive`、`@mcp_idempotent`、
  `@mcp_open_world`；
- 参数：`@optional` 决定 required，`@rename` 决定 wire 名。

### 5.2 形状

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
          "output": "McpToolResult",
          "params": [
            { "name": "owner", "wire": "owner", "required": true },
            { "name": "repo", "wire": "repo", "required": true },
            { "name": "issue_number", "wire": "issue_number", "required": true }
          ],
          "readOnly": true,
          "destructive": false,
          "idempotent": false,
          "openWorld": false
        }
      ]
    }
  }
}
```

manifest **不包含**完整 JSON Schema、参数类型或 outputSchema。运行时
position 映射只依赖 `params` 的顺序和 `wire` 名。

### 5.3 校验范围

`manifest::validate` 目前检查：

- `version == 1`；
- `server` 与 catalog 名一致；
- namespace 非空且 tools 非空；
- 每个 `mcp` 名字都能在 catalog 中找到；
- 同一个 MCP tool 不重复；
- JS 名非空且跨 namespace 唯一；
- manifest 覆盖的 tool 数量与 catalog 一致。

它**不检查**：

- `params` 的名称、顺序、required 是否与 inputSchema 一致；
- annotation flags 是否与 catalog 一致；
- `idl` 路径和 `interface` 是否与工作区文件一致；
- manifest 的 namespace 是否与 IDL 分组一致。

这些是后续可以加强的校验点；当前的行为是“能调用”，不是“完整契约
一致性证明”。

## 6. 工作区

默认根目录：

```text
<cwd>/.codemode
```

写入流程：

1. 创建 `<workspace>/<server>/`；
2. 有上游 instructions 时写 `agents.md`，否则删除旧文件；
3. 逐个写 `<namespace>.idl`；
4. 写 `<workspace>/.internal/<server>.bundle.idl`；
5. 生成并校验 manifest。

写入使用临时文件 `.name.tmp` + rename，Unix 下最终权限设为 `0444`。

已知行为：

- 旧 namespace 文件不会被删除；上游删除 tool/namespace 后，本次没有生成
  的旧 `.idl` 仍可能留在目录里；
- 不对 server 名做 `..` 路径包含检查；配置被视为本地可信输入；
- 多个 codemode 实例共享同一 workspace 时没有锁，后写覆盖先写；
- `serve`、`generate`、`doctor`、`bug-report` 都会执行同一套 build 流程，
  因此都会连接上游并重写工作区。

## 7. QuickJS 执行

### 7.1 限制

| 项目 | 当前值 |
| --- | --- |
| 代码大小 | 256 KiB（字节） |
| 默认超时 | 10 秒 |
| 超时范围 | 100 ms – 60 秒 |
| QuickJS 内存 | 128 MiB |
| QuickJS 栈 | 1 MiB |
| 最多 tool call | 100 |
| 最大结果 | 1 MiB（`JSON.stringify` 后的字符串长度） |
| console 日志 | 100 条 / 64 KiB |

`execute.code` 目前只检查大小，不拒绝 NUL 或其他控制字符。

### 7.2 执行包装

用户代码被放进普通 `Function`：

```js
const fn = new Function(source);
const value = fn();
return JSON.parse(JSON.stringify(value));
```

因此：

- 顶层 `return` 可用；
- `await` 不可用；
- `undefined` 变成 `null`；
- 循环引用、BigInt 等序列化失败会变成 `js_error`；
- 返回 Promise 不会被等待。

### 7.3 错误分类

`ExecuteOutcome.error.code` 实际可能是：

- `js_error`：语法错误、运行时异常、tool call 抛错、序列化失败；
- `timeout`：执行 deadline 到期；
- `oom`：QuickJS 报 out of memory；
- `limit_exceeded`：代码或结果超限；
- `internal`：runtime 创建、manifest 注入等宿主失败。

没有独立的 `tool_error` 或 `serialization_error` 分类。

### 7.4 输出

`execute` 返回一个 MCP text content block，内容是 `ExecuteOutcome` 的 JSON
字符串：

```json
{
  "ok": true,
  "value": null,
  "logs": [],
  "tool_calls": [],
  "duration_ms": 12,
  "truncated": false,
  "error": null
}
```

`truncated` 目前始终是 `false`；结果超限时直接返回 `limit_exceeded`，
不会截断后继续。

## 8. 对外 MCP 接口

唯一工具：

```json
{
  "name": "execute",
  "inputSchema": {
    "type": "object",
    "properties": {
      "code": { "type": "string" },
      "timeout_ms": {
        "type": "integer",
        "minimum": 100,
        "maximum": 60000,
        "default": 10000
      }
    },
    "required": ["code"],
    "additionalProperties": false
  }
}
```

`ExecuteArgs` 使用 `deny_unknown_fields`，传 `path` 或其他字段会得到
invalid params。未知 tool 名同样返回 invalid params。

## 9. 安全与已知缺口

已经实现的安全边界：

- 对外只有 stdio，不监听网络端口；
- QuickJS 不暴露 FS、网络、子进程、环境变量；
- 上游 stdio 子进程 `env_clear()`，只注入 `PATH`、`HOME` 和配置里的 env；
- 默认跳过自身条目；
- 代码、执行时间、内存、栈、tool call 数、结果和日志都有上限；
- 配置里的 env/header 值不会写入 IDL、`agents.md` 或 manifest；
- 上游 instructions 被标记为 untrusted 并限长。

尚未实现、需要在文档中明确的安全能力：

- 没有 `SecretSet` / 统一 redaction。错误字符串、日志和
  `tool_calls[].error` 没有系统化脱敏；配置本身不要放不该出现在错误里的
  敏感值；
- 没有 workspace 路径包含检查；
- 没有 `--read-only`、`--allow-tools`、`--deny-tools` 过滤器；
- 没有对 `execute.code` 的 NUL/控制字符检查；
- 没有 manifest 参数级一致性校验；
- 用户代码可以直接访问 `__xidl_call` 等内部全局；
- 上游 instructions 是 prompt injection 的潜在载体，当前只做了边界标记
  和长度限制。

## 10. 当前未实现

以下能力出现在早期计划中，但 0.3.0 没有实现：

- TOML/JSON 多配置文件合并、同名冲突检测、`--allow-shadow`；
- `inheritEnv`；
- 配置驱动的 `namespace_map`；
- 连接失败后的 lazy retry、`--refresh`；
- `--read-only` / allow/deny tool 过滤；
- 独立 SSE transport；
- `.d.ts` / `.zod.ts` 生成；
- `$ref` / `$defs` / enum / `allOf` / `oneOf` / `outputSchema` 的精确映射；
- 跨 namespace `common` 类型和公共字段抽取；
- manifest 的参数/flag/IDL 路径深度校验；
- `bug-report` 中的原始 JSON Schema、生成 IDL 和 xidlc 错误详情；
- 真正异步的 Promise tool call；
- 结果截断（当前只有超限报错）；
- 多实例 workspace 锁和旧文件清理。

## 11. 架构不变量

1. 对外只注册 `execute`，不注册上游 tool 或读取 API。
2. 所有 `mcp.<server>.<tool>` 映射必须来自 manifest；manifest 必须由
   bundle IDL 经 `xidl-codemode-manifest` 生成并与 catalog 校验。
3. 所有 tool 调用必须经过 `__xidl_call`、bridge 线程和统一限额。
4. 每次 execute 使用新的 QuickJS runtime，不跨执行复用 JS 状态。
5. IDL、`agents.md` 和 manifest 必须来自同一次 catalog 快照。
6. `execute.code` 只通过 stdio 传入，不写入工作区。
7. IDL 只生成，不支持手写 override。
8. 配置 env/header 值不写入生成文件。
9. 上游 instructions 必须以 untrusted 区块注入并限长。
10. 单个 server 的连接/manifest 失败不能阻止其他 server 注册；I/O 失败
    目前会中止整个启动。
