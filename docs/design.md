# xidl-codemode 设计文档

## 1. 文档定位

本文描述 `xidl-codemode` 的实际实现和背后的关键取舍。0.4 设计修订实现了
三处会改变核心边界的变更：

1. workspace 文档默认放在 `<cwd>/.cache/codemode-<random>/` 临时目录中，
   进程退出时删除；显式 `--docs-dir` 才使用持久目录。
2. xidl plugin 不再生成宿主解释的 `manifest.json`，而是生成可直接安装到
   QuickJS 的 glue 代码；宿主只提供带限额的 `callTool` 能力。
3. `timeout_ms` 分为配置硬上限和 execute 软上限；bridge 必须单独执行单次
   tool call 超时。

三者均已实现；本文以当前行为为准。

核心模型只有一句话：

> 上游 MCP server 是运行时事实来源，生成到临时 workspace 的 IDL 是给 AI
> 阅读和索引的契约视图，QuickJS glue 是由 IDL 派生、一次性安装进沙箱的
> 调用适配层。

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
                                              xidlc + xidl-codemode-glue
                                                                         |
                                                                         v
                                                         glue.js + glue.d.ts
                                                         （运行时 + 类型契约）
                                                                         |
                                                                         v
                                    QuickJS installXidlGlue(host)
                                                                         |
                                                                         v
                                                                  RuntimeServer
                                                                         |
MCP client --execute(code)--> QuickJS --host.callTool--> bridge 线程 --> 上游 MCP
```

一次启动会完成以下步骤：

1. 读取配置，过滤禁用和自身条目。
2. 逐个连接上游 server，完成 `initialize` 和分页 `tools/list`。
3. 把每个 server 的 tool schema 按 namespace 生成 IDL。
4. 生成 `agents.md`（仅当上游提供 `initialize.instructions`）。
5. 用 bundle IDL 调用 external xidl plugin，生成 QuickJS glue。
6. 在探针 context 安装 glue，并与 catalog 对账后注册 `mcp.<server>`。
7. 对外暴露 `execute` 和只读 `search_tools`。

## 3. 关键决策

| ID | 决策 | 一句话理由 |
| --- | --- | --- |
| D1 | Rust 单仓库、双二进制 | 主程序、MCP 协议、QuickJS 和 xidlc 都在 Rust 侧，边界最少 |
| D2 | 复用 `rmcp` 做 server 和 client | 不手写 initialize、分页、stdio framing 和生命周期 |
| D3 | 业务执行只暴露 `execute`，另提供只读 `search_tools` | code mode 收敛业务调用；发现走文档和只读目录 |
| D4 | IDL 落盘，`execute.code` 走 stdio | 文档可被文件工具索引，执行代码不产生临时脚本 |
| D5 | IDL 只生成，不支持手写 override | 避免生成契约与手写契约双写漂移 |
| D6 | QuickJS glue 由 external xidl plugin 从 IDL 生成 | 调用适配层也是派生物，宿主不再解释第二份映射格式 |
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
| D19 | 单个 server 的发现/调用适配失败隔离 | 一个上游坏掉不应拖垮其他 server |
| D20 | 默认文档使用 `<cwd>/.cache/codemode-*` | 文档是派生物，读 `$TMPDIR` 不应是正常路径，也不能删除其他工具缓存 |
| D21 | execute timeout 是软上限，配置 timeout 是硬上限 | AI 可在安全范围内缩短，不能放大风险 |
| D22 | 不提供 bug-report 子命令 | 无法映射时由人手动收集上下文并报告 |

### D1：Rust 单仓库、双二进制

主二进制是 `xidl-codemode`，另一个是 external xidl plugin
`xidl-codemode-glue`。两者放在同一个 Cargo package 中，由
`cargo build --bins` 一起构建。

这样做是因为 MCP server/client、QuickJS 和 xidlc 都是 Rust 生态；单仓库
便于共享 glue 和 catalog 类型、统一版本和发布。插件仍然是独立进程，通过 xidlc
的 external codegen 协议调用，不和主进程内部状态耦合。

### D2：复用 `rmcp`

下游使用 `rmcp` 的 stdio server；上游同时支持：

- stdio：`TokioChildProcess`；
- streamable HTTP：`StreamableHttpClientTransport`，支持
  `Authorization: Bearer` 和自定义 header。

配置里的 `sse`、`streamable-http`、`streamable_http` 目前都归一化为
streamable HTTP；没有单独的 SSE transport 实现。

### D3：业务执行只暴露 `execute`

下游 `tools/list` 返回 `execute` 和只读 `search_tools`。不提供 `list_apis`、
`read_api`、MCP resource 或任何其他读取 API 的工具。

模型通过普通文件工具读取工作区里的 IDL 来发现参数名、顺序、类型和
optional 属性。这样下游 MCP context 不会被 N 个上游 tool schema 占满。

`search_tools` 只读取内存中的 catalog 索引，返回截断后的 signature/参数摘要，
标记 `readOnlyHint`；QuickJS 内的 `catalog.*` 与它共享同一份索引，作为脚本内
兜底。

### D4：IDL 落盘，执行代码不落盘

运行时文档默认放在当前目录的 `.cache/codemode-*` 临时 run 目录中。

落盘内容：

```text
<cwd>/
  .cache/
    codemode-<random>/         # tempfile::TempDir，RAII 持有
      <server>/
        agents.md             # 仅当上游提供 initialize.instructions
        <namespace>.idl
      .internal/
        <server>.bundle.idl
```

`serve` 每次启动创建一个新的 run-id。目录句柄由 RAII 持有，正常退出、
错误退出和 panic unwind 都会删除。单实例假设下，下一次启动只删除
`.cache/codemode-*`，覆盖 SIGKILL 留下的目录，不影响其他工具的缓存。

当客户端文件工具不能读取 workspace 内路径以外的系统临时目录时，这个布局
保证了正常路径仍可读；但它仍要求客户端允许读
`<cwd>/.cache/codemode-<random>`。
确实需要长期查看或导出文档时，用显式 `--docs-dir <path>` 切换到持久目录。
持久模式同样每次启动原子刷新并清理本次没有生成的旧 namespace 文件，
只是进程退出时不删除。

`execute` 的输入是内联字符串：

```json
{
  "code": "return mcp.github.get_issue('xidl', 'xidl', 361);",
  "timeout_ms": 10000
}
```

不提供 `path` 参数，也不把代码写入磁盘。IDL 和 `agents.md` 是给 AI 的文档，
不是执行输入。

### D5：IDL 只生成，不支持手写 override

工作区里的 `*.idl` 永远由 catalog 生成，文件内容没有用户可编辑的入口。
如果生成结果有语法或语义问题，应该修生成器或 xidl，而不是手写覆盖。

设计没有 bug-report 子命令；无法映射时由人手动收集上下文并报告。
只读 `doctor --json` 提供当前 server 状态和失败原因。

### D6：QuickJS glue 由 external xidl plugin 生成

插件信息：

```text
codegen 名称: codemode-glue
二进制名称:   xidl-codemode-glue
```

运行流程是：

```text
bundle IDL
   |
   v
xidlc::driver::Generator("codemode-glue")
   |
   v
xidl-codemode-glue
   |
   v
glue.js + glue.d.ts
```

宿主不解释中间 manifest。插件生成一段可安装的 JS 适配层（实际使用
JSON 字符串作为宿主边界，由 glue 侧做 stringify/parse），例如：

```js
function installXidlGlue(target, host) {
  const __server = Object.create(null);
  const __ns = Object.create(null);
  __ns["get_issue"] = function(...args) {
    if (args.length > 3) { throw new Error("too many arguments for github.get_issue (expected 3)"); }
    const __params = {};
    if (args.length > 0 && args[0] !== undefined) { __params.owner = args[0]; } else { throw new Error("missing required argument owner for github.get_issue"); }
    return JSON.parse(host.callTool(JSON.stringify({ server: "github", tool: "get_issue", params: __params })));
  };
  Object.freeze(__ns);
  __server["github"] = __ns;
  Object.freeze(__server);
  for (const [key, value] of Object.entries(__server)) {
    Object.defineProperty(target, key, { value: value, enumerable: true });
  }
}
```

`host` 是宿主注入的唯一能力入口，只提供 `callTool`，并统一执行调用数、
单次调用超时和错误包装。`host` 不注册为全局，只作为 install 参数被 glue
闭包捕获，用户代码无法直接访问。位置参数到 wire object 的转换由 glue
完成。

`glue.d.ts` 是同一份 IDL 的类型视图，供 AI 阅读和 CI 对生成产物做类型
检查。QuickJS 运行 `glue.js`；不要求每次启动执行 TypeScript compiler。

启动时 `ensure_plugin_on_path()` 优先把主程序所在目录加入 `PATH`
（如果同目录存在插件）。debug 构建下还会尝试 `target/debug`。

安装后仍要做一次审计：先在一个探针 QuickJS context 安装 glue，枚举生成的
`mcp` 对象，再与 catalog 对账。审计只保证 tool 集合和 JS 名一致；参数
契约由 IDL -> glue 的同一份编译产物保证，不再由运行时解释第二份 manifest。

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
return issue;
```

参数顺序来自生成的 IDL，而 IDL 的参数顺序来自上游
`inputSchema.properties` 的 JSON 顺序。glue 固化每个位置的 IDL 参数名、
wire 名和是否 required。

运行时行为：

- 参数个数多于生成的参数个数：抛错；
- required 位置缺失或值为 `undefined`：抛错；
- optional 位置缺失、值为 `undefined` 或 `null`：不写入 wire object；
- 不做运行时类型校验，类型由上游 MCP server 自己校验。

选择位置参数的原因是避免模型因为参数名大小写或拼写差异反复探测；代价是
长参数列表的可读性较差，必须依赖 IDL。

### D9：每次 execute 新建 QuickJS runtime

每次调用都创建新的 `Runtime` + `Context`，执行完立即销毁。QuickJS 侧没有
文件系统、网络、子进程或环境变量访问。

这样做的成本远低于一次 MCP tool call，但能避免跨执行的全局状态、定时器、
原型污染和未释放 Promise。

### D10：同步桥接与 await 语法兼容

QuickJS 里的 tool 函数是同步函数。glue 通过注入的 `host.callTool` 进入
一个独立 bridge 线程，再由该线程上的 tokio `current_thread` runtime 调用
`rmcp`。

结果：

- `execute.code` 在 async IIFE 中运行，`await` 语法可用；
- QuickJS pending jobs 由 `Promise::finish()` 驱动，纯 JS Promise 会完成；
- 没有 timer 或异步 host call 时，pending Promise 会立即返回明确的
  `await cannot make progress` 错误；
- `Promise.all` 在纯 JS 值上可用，但不会带来真实上游并发；
- 所有上游 tool call 在单个 bridge 线程上串行执行。

### D11：归一化的 codemode 返回值

`mcp.<server>.<tool>(...)` 只返回普通 JS/JSON 值：

| 上游结果 | codemode 返回值 |
| --- | --- |
| `isError: true` | text content 作为 JavaScript Error 抛出 |
| 有 `structuredContent` | 直接返回该 JSON 值 |
| 单个 text block | 字符串 |
| 单个 image/audio/resource block | 对应 codemode 值对象 |
| 多个 content block | 归一化后的数组 |
| 空 content | `null` |

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
- tool 调用直接返回普通 JS/JSON 值；
- `console.log/info/warn/error`。

没有 `fetch`、`process`、`fs`、`child_process`、`Deno` 或 `Bun`。

实现细节：宿主只在 bootstrap 期间临时暴露一个 log 函数（eval 完成后立即
删除）；`host.callTool` 不注册为全局，只作为 glue 的 install 参数被闭包
捕获，用户代码无法直接访问。

### D14：JSON Schema 映射范围

当前只生成输入参数类型，所有 operation 的返回类型统一是 `any`。
`outputSchema` 会被 catalog 捕获，但不会生成专用输出 struct。

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
2. `mcp.<server>.<tool>(arg1, arg2, ...)` 和普通 JSON 返回值的调用方式；
3. IDL 的 glob 路径；
4. 标记为 untrusted 的上游 instructions；
5. 启动时失败的 server 列表。

上游 instructions 会被截断到总共约 32 KiB，并明确标注为不可信数据。

### D17：不做磁盘 cache

每次启动都重新连接上游、重新执行 `tools/list`、重新生成 IDL 和调用适配层。
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
- 调用适配层生成或审计失败：该 server 不注册，但已经写入的 IDL/
  `agents.md` 会留在工作区；
- workspace 写入失败：中止整个启动。

IDL 生成函数本身不返回 `Result`，不会单独失败；bundle IDL 的解析错误会在
调用适配层生成阶段表现为该 server 失败。所以“单 server 失败不拖垮其他
server”覆盖连接、发现和调用适配层生成/审计失败；workspace I/O 失败没有
per-server 隔离。

### D20：临时文档目录

文档与 bundle 一样是启动快照的派生物，默认不应该长期留在用户项目中。
因此 runtime 文档放在 `<workspace>/.cache/<run-id>/`。目录由
`tempfile::TempDir` 创建并持有，drop 时递归删除。

约束：

- run-id 必须包含随机成分，不能只用 PID；
- instructions 和 `execute` description 只暴露当前 run 目录绝对路径，
  不暴露 `.cache/` 通配；
- `doctor` 默认只读，不创建 run 目录；
- 默认不扫描删除同前缀临时目录；单实例假设下仅在下一次启动时清空
  `.cache/`；
- `--docs-dir` 是显式 fallback，不是默认路径。

### D21：Timeout 软硬分离

配置提供每个 execute 的硬上限 `execute_timeout_hard_ms`，默认 60 秒。
`execute.timeout_ms` 是请求软上限，默认 10 秒。运行时使用
`min(requested_soft || default_soft, hard_max)`，并把实际生效值返回在
`ExecuteOutcome.timeout_ms` 中。

对外 schema 的 `maximum` 动态写为配置硬上限；客户端绕过 schema 时运行时
仍然 clamp。整体脚本超时错误使用 `execute_timeout`。

bridge 还必须为单次上游调用设置独立超时，默认 10 秒，最大不能超过本次
execute 的剩余 deadline。单次调用超时返回 `tool_timeout`。这避免 hung
upstream 阻塞到 QuickJS interrupt 才失败。

### D22：没有 bug-report 子命令

无法映射的 schema、xidl 拒绝的生成结果或 glue 审计失败都只是当前 server
失败，不生成自动化报告。诊断信息由只读 `doctor --json` 输出；用户手动
复制相关 schema、IDL、错误和版本来提 issue。

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

`output_schema` 目前只用于记录，不参与 IDL 生成，也不进入 glue。

### 4.2 IDL 形状

每个 namespace 文件包含：

- `#pragma xidlc package <server>`；
- `#pragma xidlc version 1.0.0`；
- 该 namespace 用到的嵌套 struct；
- 一个带 `@mcp_server` / `@mcp_namespace` 的 interface。

示例：

```idl
#pragma xidlc package github
#pragma xidlc version 1.0.0

@mcp_server("github")
@mcp_namespace("issues")
interface GithubIssuesApi {
    /// Get a single issue.
    @mcp_tool("get_issue")
    @mcp_read_only
    any get_issue(
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
文件是 bundle 的视图；生成流程必须先用 bundle 做解析、glue 生成和探针
审计，再把同一份快照的 namespace 视图落盘。

## 5. Glue

运行时用 glue 派生调用适配层。插件输入仍是 per-server bundle IDL；输出是
`glue.js` 与 `glue.d.ts`。

### 5.1 生成

`glue::generate(server, bundle)` 通过
`xidlc::driver::Generator::new("codemode-glue")` 调用 external plugin。

插件需要携带的 annotation：

- interface：`@mcp_server("...")`、`@mcp_namespace("...")`；
- operation：`@mcp_tool("...")`；
- 可选 flags：`@mcp_read_only`、`@mcp_destructive`、`@mcp_idempotent`、
  `@mcp_open_world`；
- 参数：`@optional` 决定 required，`@rename` 决定 wire 名。

### 5.2 QuickJS 形状

插件生成一次 `installXidlGlue(target, host)`。每个 tool 函数固化：

- JS 名和原始 wire tool 名；
- 参数个数；
- IDL 参数名到 wire 参数名的映射；
- required/optional 判定。

宿主只注入 `host.callTool(call)`。它统一执行调用数、单次调用超时、结果
大小、错误包装和调用记录。位置参数到 wire object 的转换完全由 glue
完成；宿主不再读取 `manifest.json`。

### 5.3 安装审计

宿主在探针 QuickJS context 安装 glue，枚举 `mcp` object，并要求：

- 每个 catalog tool 恰好出现一次；
- 没有额外 tool；
- 跨 namespace JS 名唯一；
- 原始名和 sanitized 名别名正确。

审计失败时该 server 不注册。参数本身不做运行时第二解释；IDL -> glue 是
同一份编译产物。`.d.ts` 在 CI 中对 fixture 做类型检查，用于尽早发现生成
契约漂移。

## 6. 工作区

默认根目录已移除；写入布局是：

```text
<cwd>/.cache
```

写入流程：

1. 解析 catalog 快照并生成 bundle IDL；
2. 生成 glue、在探针 context 审计，失败则该 server 不注册；
3. 创建 `<cwd>/.cache/codemode-<random>/<server>/`；
4. 有上游 instructions 时写 `agents.md`，否则删除旧文件；
5. 逐个写 `<namespace>.idl` 和 `.internal/<server>.bundle.idl`。

默认 run 目录由 `tempfile::TempDir` 创建并持有，drop 时递归删除。
持久 `--docs-dir` 写入使用临时文件 `.name.tmp` + rename，Unix 下最终权限
设为 `0444`；每次启动同样清理本次没有生成的旧 namespace 文件。

当前行为：

- 默认 run 目录退出删除，单实例假设下下一次启动只删除 `.cache/codemode-*`
  覆盖 SIGKILL 泄漏；持久 `--docs-dir` 清理本次没有生成的旧 namespace 文件
  和 bundle；
- 从配置中移除 server 后，持久模式下该 server 的旧目录整体保留，
  只清理仍存在 server 的 stale namespace 文件；
- 不对 server 名做 `..` 路径包含检查；配置被视为本地可信输入；
- 多个 codemode 实例共享同一 workspace 时没有锁，后写覆盖先写；
- `serve` 创建临时 run 目录；`generate` 只在显式 `--docs-dir` 时导出
  持久文档；`doctor` 只读，不写任何文件；`bug-report` 已删除。

## 7. QuickJS 执行

### 7.1 限制

| 项目 | 当前值 |
| --- | --- |
| 代码大小 | 256 KiB（字节） |
| 默认 execute 软超时 | 10 秒 |
| 配置硬上限 | 60 秒 |
| 请求范围 | 100 ms – 配置硬上限 |
| 单次 tool call 默认超时 | 10 秒 |
| QuickJS 内存 | 128 MiB |
| QuickJS 栈 | 1 MiB |
| 最多 tool call | 100 |
| 最大结果 | 1 MiB（`JSON.stringify` 后的字符串长度） |
| console 日志 | 100 条 / 64 KiB |

`execute.code` 目前只检查大小，不拒绝 NUL 或其他控制字符。

### 7.2 执行包装

用户代码被放进 async IIFE：

```js
(async function() {
  const fn = Object.getPrototypeOf(async function() {}).constructor(source);
  const value = await fn();
  return JSON.parse(JSON.stringify(value));
})()
```

因此：

- 顶层 `return` 可用；
- `await` 可用于语法兼容和纯 JS Promise；
- `undefined` 变成 `null`；
- 循环引用、BigInt 等序列化失败会变成 `js_error`；
- 没有 timer 或异步 host call 的 pending Promise 返回明确的
  `await cannot make progress` 错误。

### 7.3 Timeout 与错误分类

配置的 `execute_timeout_hard_ms` 是硬上限，默认 60 秒。execute 请求中的
`timeout_ms` 是软上限，默认 10 秒。运行时使用
`min(requested_soft || default_soft, hard_max)`，实际生效值返回在
`ExecuteOutcome.timeout_ms`。对外 schema 的 `maximum` 动态写为配置硬上限；
客户端绕过 schema 时仍会 clamp。

bridge 必须为单次上游调用设置独立超时，默认 10 秒，最大不超过本次
execute 剩余 deadline。这避免 hung upstream 阻塞到 QuickJS interrupt 才
失败。

`ExecuteOutcome.error.code` 实际可能是：

- `js_error`：语法错误、运行时异常、tool call 抛错、序列化失败；
- `execute_timeout`：整个 execute 的软/硬 deadline 到期；
- `tool_timeout`：单次上游调用在 bridge 侧超时；
- `oom`：QuickJS 报 out of memory；
- `limit_exceeded`：代码或结果超限；
- `internal`：runtime 创建、glue 安装等宿主失败。

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
  "timeout_ms": 10000,
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
invalid params。schema 的 `maximum` 动态写为配置硬上限，运行时仍会
clamp。未知 tool 名同样返回 invalid params。

## 9. 安全与已知缺口

已经实现的安全边界：

- 对外只有 stdio，不监听网络端口；
- QuickJS 不暴露 FS、网络、子进程、环境变量；
- 上游 stdio 子进程 `env_clear()`，只注入 `PATH`、`HOME` 和配置里的 env；
- 默认跳过自身条目；
- 代码、执行时间、内存、栈、tool call 数、结果和日志都有上限；
- 配置里的 env/header 值不会写入 IDL、`agents.md` 或 glue；
- 上游 instructions 被标记为 untrusted 并限长。

尚未实现、需要在文档中明确的安全能力：

- 没有 `SecretSet` / 统一 redaction。错误字符串、日志和
  `tool_calls[].error` 没有系统化脱敏；配置本身不要放不该出现在错误里的
  敏感值；
- 没有 workspace 路径包含检查；
- 没有 `--read-only`、`--allow-tools`、`--deny-tools` 过滤器；
- 没有对 `execute.code` 的 NUL/控制字符检查；
- 上游 instructions 是 prompt injection 的潜在载体，当前只做了边界标记
  和长度限制。

## 10. 当前未实现

以下能力出现在早期计划中，尚未实现：

- TOML/JSON 多配置文件合并、同名冲突检测、`--allow-shadow`；
- `inheritEnv`；
- 配置驱动的 `namespace_map`；
- 连接失败后的 lazy retry、`--refresh`；
- `--read-only` / allow/deny tool 过滤；
- 独立 SSE transport；
- `.zod.ts` 生成（glue `.d.ts` 已实现）；
- `$ref` / `$defs` / enum / `allOf` / `oneOf` / `outputSchema` 的精确映射；
- 跨 namespace `common` 类型和公共字段抽取；
- 真正异步的 Promise tool call（当前只做 `await` 语法兼容）；
- 结果截断（当前只有超限报错）；
- 多实例 workspace 锁和旧文件清理。

## 11. 架构不变量

1. 对外只注册 `execute`，不注册上游 tool 或读取 API。
2. 所有 `mcp.<server>.<tool>` 映射必须来自 IDL 派生的 glue；glue 必须由
   bundle IDL 经 `xidl-codemode-glue` 生成并通过探针审计。
3. 所有 tool 调用必须经过封装后的 `host.callTool`、bridge 线程和统一限额；
   用户代码无法直接访问宿主对象。
4. 每次 execute 使用新的 QuickJS runtime，不跨执行复用 JS 状态。
5. IDL、`agents.md` 和 glue 必须来自同一次 catalog 快照。
6. `execute.code` 只通过 stdio 传入，不写入工作区。
7. IDL 只生成，不支持手写 override。
8. 配置 env/header 值不写入生成文件。
9. 上游 instructions 必须以 untrusted 区块注入并限长。
10. 单个 server 的连接/调用适配失败不能阻止其他 server 注册；I/O 失败
    目前会中止整个启动。
11. 运行时文档默认位于 `<workspace>/.cache/<run-id>/` 临时 run 目录，
    进程退出删除；显式 `--docs-dir` 才允许持久保存。
12. execute timeout 是配置硬上限内的软上限；单次上游调用有独立
    `tool_timeout`。
