# xidl-codemode 设计文档

## 1. 文档目的

`PLAN.md` 说明“做什么、分几步做”；本文说明“为什么这样做”。

本文记录关键架构决策、备选方案、取舍和影响。实现过程中如果发现某个前提
不成立，应更新对应决策，而不是只在代码里悄悄改变行为。

## 2. 决策总览

| ID | 决策 | 一句话理由 |
| --- | --- | --- |
| D1 | Rust 单二进制实现 | rmcp、rquickjs、xidlc 都在 Rust 侧，部署和沙箱控制最直接 |
| D2 | 复用 rmcp 作为 MCP server + client | 避免手写协议、分页、transport 和生命周期管理 |
| D3 | xidl 是契约视图，manifest 是运行时派发表 | 契约可读可校验，运行时不被 IDL 解析绑定 |
| D4 | 每个 MCP server 的 schema 按 namespace 拆成多个 `{namespace}.idl` | 大 schema 可导航，按语义分组，避免单文件膨胀 |
| D5 | Code mode 入口使用全局 `mcp.<server>.<tool>()` | API 直接注入 runtime，无 import/require 负担 |
| D6 | 每次 execute 新建 QuickJS runtime | 强隔离，避免跨执行状态泄漏 |
| D7 | MVP 使用同步宿主桥接，异步后置 | 先解决正确性，避免 QuickJS Promise 与 tokio 的复杂度 |
| D8 | 对外只暴露 `execute` | 严格保持单一执行入口，发现信息走工作区文件 + 注入索引 |
| D9 | 工具调用返回原始 `CallToolResult` | 不丢失 content/structuredContent/错误语义 |
| D10 | 配置 JSON 为主、TOML 兼容 | 当前 Home Manager 实际生成的是 `mcp.json` |
| D11 | Secret 默认不进入沙箱、日志和产物 | 配置里有 token/header，泄漏风险高 |
| D12 | JSON Schema 只保证可表达子集，其余降级 `any` | 先保证所有 MCP server 可接入，再逐步提高类型精度 |
| D13 | 名称统一 sanitize，原名写入 manifest | 兼容非法 tool/property 名，同时保留可调用性 |
| D14 | 大 server 的公共字段抽取后置 | 避免第一版就做复杂的 schema 归并 |
| D15 | 默认不继承父环境，不暴露 FS/网络/子进程 | 默认拒绝，降低沙箱逃逸和 secret 泄漏面 |
| D16 | 默认排除自身 server | 防止 `xidl-codemode` 连接自己形成递归 |
| D17 | 文档落盘，manifest 由插件生成到 internal，无磁盘缓存 | AI 可索引完整 schema；避免缓存失效和 secret 残留 |
| D18 | xidlc 本地 path 开发，发布时 pin 版本 | 兼顾本地联调和 Nix/可复现构建 |
| D19 | 每个 MCP 生成 `agents.md` 作为提示词注入载体 | 上游 instructions 和生成用法需要进入模型上下文 |
| D20 | `execute` 接收 stdio 内联代码 | 代码不落盘，避免 TOCTOU 和脚本生命周期 |
| D21 | stdio-only；IDL/agents.md 落盘，execute 代码不落盘 | 文档可被 AI 索引；执行代码保持临时和可审计 |
| D22 | tool 描述放 IDL `///`，但 `agents.md` 从同一 IR 渲染 | IDL 是可读契约；注入文档不能依赖重新解析 IDL |
| D23 | IDL 只生成，不支持手写 override；失败转 bug report | 避免生成物与手写契约双写；生成器问题必须被修 |
| D24 | manifest 由 external xidl plugin 从 IDL 生成 | 运行时映射以 IDL 为输入，避免 codemode 内部再养一套 manifest emitter |

---

## 3. 关键决策

### D1：Rust 单二进制实现

**背景**

项目同时需要：

- MCP server 和 MCP client。
- QuickJS 嵌入式沙箱。
- xidl 解析/生成。
- 子进程和 HTTP transport 管理。

**决策**

使用 Rust 实现一个 `xidl-codemode` 二进制，内部拆成 `lib` + `main`。

**备选**

- TypeScript/Node + `@modelcontextprotocol/sdk` + `quickjs-emscripten`。
- Go + 任意 QuickJS binding。

**理由**

- `rmcp`、`rquickjs`、`xidlc` 都是 Rust 生态，边界最少。
- 单二进制便于 Home Manager / Nix 分发，不要求用户额外装 Node 或
  QuickJS runtime。
- 子进程、信号、资源限制和 channel 桥接在 Rust 中更容易做严格控制。
- xidlc 直接作为 library 调用，避免运行时依赖外部 `xidlc` 二进制。

**代价**

- Rust 侧 QuickJS binding 的异步桥接比 JS 生态稍复杂。
- 首版开发速度可能慢于 Node 原型。

### D2：复用 rmcp，而不是手写 MCP 协议

**背景**

需要同时作为 MCP client 连接上游 server，作为 MCP server 暴露给下游客户端。
MCP 涉及 initialize、分页、stdio framing、streamable HTTP、session、取消等
细节。

**决策**

使用 `rmcp`：

- 下游：`rmcp` server + `transport-io`。
- 上游 stdio：`rmcp` client + `transport-child-process`。
- 上游 HTTP：`rmcp` client + `transport-streamable-http-client-reqwest`。

**备选**

- 手写 JSON-RPC framing 和 transport。
- 使用 Node/Python 的 MCP SDK。

**理由**

- 协议细节多，手写容易在分页、初始化顺序、session 和取消上出错。
- `rmcp` 同时支持 server/client 和多种 transport，避免两套实现。
- `custom_headers` / `auth_header` 能覆盖当前配置中的 HTTP 认证。

**代价**

- 受 `rmcp` API 版本影响；需要 pin 版本并做 M0 编译验证。
- 某些非标准 MCP server 可能仍有兼容问题，需要 transport 适配层。

### D3：IDL 是契约，manifest 由 external xidl plugin 从 IDL 派生

**背景**

用户要求用 xidl 描述 API；但代码执行时真正需要的是：

- server 名；
- 原始 tool 名；
- 安全 JS 名；
- input/output JSON Schema；
- 调用映射。

如果 codemode 自己维护 manifest emitter，就会和 IDL 生成器形成第二套映射
逻辑；如果运行时用临时 parser 解析 IDL，又会和 xidlc 语义漂移。

**决策**

流水线：

```text
catalog -> agents.md + namespace IDL + bundle IDL
                                      |
                                      v
                    external plugin: xidl-codemode-manifest
                                      |
                                      v
                                 manifest.json
                                      |
                                      v
                            JS runtime mcp facade
```

- `agents.md` 和 namespace IDL 落盘，供 AI 索引。
- `bundle.idl` 是内部输入，交给 external xidl plugin。
- 插件实现 xidlc `Codegen` 协议，读取 HIR 和 `@mcp_*` annotation，输出
  `manifest.json`。
- codemode 读取 manifest 并与 `tools/list` catalog 交叉校验，然后构建
  `mcp` facade。
- 插件是独立二进制 `xidl-codemode-manifest`，target 名
  `codemode-manifest`；xidlc 通过 PATH 启动 `xidl-<lang>`。

**备选**

- codemode 内部维护 manifest emitter。
- 运行时用临时 parser 解析 IDL 生成 manifest。
- 只生成 IDL，不生成 manifest。

**理由**

- IDL 是唯一契约；manifest 是 IDL 的派生视图，不是第二份契约。
- 外部插件复用 xidlc 的 HIR 和 annotation 语义，不需要 codemode 再写一套
  IDL parser。
- 插件可以独立升级/替换，不和 codemode 主进程耦合。
- manifest 与 catalog 交叉校验可以发现 annotation、plugin 或 IDL 生成 bug。

**代价**

- 必须把插件安装到 PATH，或给 xidlc 子进程注入插件目录。
- IDL 必须携带完整的 `@mcp_*` annotation，否则 manifest 无法恢复映射。
- 插件版本和 xidlc engine version 需要兼容检查。
- 生成 IDL/插件失败不能靠手写绕过，必须修生成器或 xidl。

### D4：每个 MCP server 的 schema 按 namespace 拆成多个 `{namespace}.idl`

**背景**

用户要求结构为 `{mcpname}/{x}.idl`，且 `x` 按 namespace 分组。当前
`agent-browser` 有 29 个 tool，公共字段很多，单文件 IDL 会非常臃肿。

**决策**

默认工作区 `<cwd>/.codemode/`，可用 `--workspace` 覆盖。结构：

```text
<workspace>/{mcpname}/agents.md
<workspace>/{mcpname}/{namespace}.idl
```

- 一个 namespace 一份 IDL。
- MCP 没有原生 namespace，默认从 tool 名推导：
  - `foo.bar` / `foo:bar` -> `foo`；
  - 按 `_` / `-` 切分，第一段被多个 tool 共享 -> 第一段；
  - 无法判断 -> `default`；
  - `namespace_map` 可显式覆盖。
- `common` 是保留 namespace，用于跨 namespace 共享类型。
- 每个 server 在内存中额外生成 `bundle.idl` 用于 xidlc 校验；
  per-namespace IDL 是它的视图。
- `agents.md` 包含 namespace 索引、完整紧凑签名和用法。
- 生成物只读、原子写入；工作区不提交 git。

**备选**

- 一个 MCP server 一个 IDL。
- 一个 tool 一个 IDL。
- 所有 server 一个大 IDL。

**理由**

- 符合用户提出的 `{mcpname}/{x}.idl` 结构，且 `x` 是语义 namespace。
- 一个 tool 一个文件会产生过多碎片；一个 server 一个文件又会过大。
- namespace 分组更接近人类对 API 的组织方式（issues、pulls、core 等）。
- 落盘后 AI 可以直接索引 namespace 文件，不需要把所有 schema 塞进 prompt。

**代价**

- MCP 没有原生 namespace，推导规则需要可配置，否则可能分组不理想。
- 共享类型需要 `common` namespace 或重复定义，M1 需要验证 xidl import。
- 多项目/多实例共享 workspace 会互相覆盖；默认每项目一个 `.codemode/`。

### D5：Code mode 入口使用全局 `mcp.<server>.<tool>()`

**决策**

沙箱内直接调用：

```js
const issue = mcp.github.get_issue("xidl", "xidl", 361);
return unwrap(issue);
```

宿主在创建 QuickJS context 时注入全局 `mcp` 对象：

```text
mcp
  ├── github
  │     ├── get_issue
  │     └── search_issues
  └── agent_browser
        └── agent_browser_open
```

MVP 中 tool 函数同步返回 `CallToolResult`；`await` 普通值仍然可用，
真正的 Promise 并发放到后续阶段。

**备选**

- `require("/server/server.idl")`。
- ES module `import("/server/server.idl")`。
- `mcp["/server/server.idl"]` 路径对象。

**理由**

- 用户明确要求 `mcp.github.get_issue()` 这种直接调用形式。
- 不需要实现模块 loader；宿主直接构造命名空间对象，最可靠。
- API 在 runtime 中已经就绪，脚本不需要 imports，代码更短。
- 与“schema 文档在内存、API 注入 runtime”的职责划分一致。

**代价**

- 全局命名空间可能和用户变量冲突；用 `mcp` 这个保留名并在 instructions
  中说明。
- 不是标准 ESM；不能按路径懒加载。
- server/tool 名需要 sanitize，且要保留原名 bracket access。

### D6：每次 execute 新建 QuickJS runtime

**决策**

每次 `execute` 创建新的 `Runtime` + `Context`，执行结束后销毁。

**备选**

- 复用同一个 runtime/context，按执行清空全局变量。

**理由**

- 复用 runtime 容易残留原型污染、全局状态、定时器和未释放的 Promise。
- 新建 runtime 的成本远低于一次 MCP tool call 的网络/进程成本。
- 隔离边界清晰，测试也更容易。

**代价**

- 每次执行有少量初始化开销。
- 无法跨执行缓存 JS 编译结果；后续可在不牺牲隔离的前提下做 compiled
  module cache。

### D7：MVP 同步宿主桥接，异步后置

**决策**

QuickJS 中的 tool 函数同步返回结果；宿主通过独立 bridge task 和阻塞 channel
完成 MCP 调用。

```text
QuickJS thread -> std::sync::mpsc -> tokio bridge task -> rmcp call_tool
```

**备选**

- 一开始就用 `Promise::wrap_future` / `AsyncRuntime`。
- 在 QuickJS 线程里直接 `Handle::block_on`。

**理由**

- 避免在 tokio worker 中嵌套 `block_on` 导致的死锁/panic 风险。
- 同步模型更容易做超时、调用计数和错误映射。
- `await` 一个普通值仍然可用，所以常见脚本写法不受影响。

**代价**

- `Promise.all` 不会真正并发，只会顺序执行。
- 长 tool call 会阻塞当前脚本；由 MCP 调用超时和执行 deadline 兜底。
- 需要 M5 再做真正的异步桥接。

### D8：对外只暴露 `execute`

**决策**

codemode MCP server 只注册一个工具：

```json
{
  "name": "execute",
  "inputSchema": {
    "type": "object",
    "properties": {
      "code": { "type": "string" },
      "timeout_ms": { "type": "integer" }
    },
    "required": ["code"]
  }
}
```

不提供 `list_apis`、`read_api`、MCP resources 或其他读取 API 的工具。
输入 schema 不包含 `path`；JS 通过 stdio 内联传入，不落盘。

发现信息有两条路径：

1. codemode 的 `instructions` 注入工作区路径、server/namespace 索引和简短
   签名。
2. 模型用文件工具索引工作区里的 `agents.md` / `*.idl`。

**备选**

- 暴露 `execute` + `list_apis` + `read_api`。
- 把每个上游 tool 原样暴露给下游。

**理由**

- 用户明确要求只提供 `execute`，这是 code mode 的核心约束。
- 单一工具避免下游 MCP context 被 N 个上游 tool 撑爆。
- 工作区文件 + `agents.md` 已经能提供发现信息，不需要额外 MCP 工具。
- 少两个工具就少两套参数校验、分页和权限逻辑。

**代价**

- 下游客户端需要能读工作区；否则只能依赖注入索引。
- 工作区是多实例并发写的风险点，需要原子写入和单实例假设。
- 模型需要先读文档、再 execute，但代码本身不用落盘。

### D9：保持原始 `CallToolResult`

**决策**

注入的 tool 函数返回 MCP 原始结果：

```ts
{ content, structuredContent?, isError? }
```

提供 `unwrap(result)` 做便捷解包。

**备选**

- 直接返回 `structuredContent`。
- 把 `content` 文本拼成字符串。
- 失败时统一 throw。

**理由**

- MCP 结果可能包含多个 content block、图片/资源、错误标记和 structured data。
  直接扁平化会丢失语义。
- 返回原始结果让脚本可以按需处理 `isError`，`unwrap` 只是便利层。
- 未来 outputSchema 类型化时，原始 envelope 仍然兼容。

**代价**

- 模型需要知道 `.structuredContent` / `.content` 的结构。
- instructions 和 IDL 必须明确说明返回形态。

### D10：配置 JSON 为主，TOML 兼容

**背景**

用户提出读取 `~/.config/mcp/config.toml`，但实际 Home Manager 生成的是
`~/.config/mcp/mcp.json`。

**决策**

- 优先支持当前实际存在的 `mcp.json`。
- 同时实现 TOML 解析，字段与 JSON 对齐。
- 自动按扩展名/内容探测，也支持 `--config` 显式指定。

**备选**

- 只支持 `config.toml`。
- 只支持 `mcp.json`。

**理由**

- 只支持 TOML 会导致当前环境开箱不可用。
- 只支持 JSON 又不符合用户明确提出的 TOML 需求。
- 双格式的解析成本很低，归一化后共用同一个模型。

**代价**

- 需要维护两种格式的测试和错误信息。
- TOML 的 table 结构与 JSON 的 nesting 需要明确文档。

### D11：Secret 默认不进入沙箱、日志和产物

**决策**

- 配置中的 env/header 值收集为 `SecretSet`。
- 日志、错误、`doctor`、manifest、IDL、cache 全部 redaction。
- 子进程默认 `env_clear()`，只传 `PATH`、`HOME` 和配置 env。
- 沙箱本身不读取环境变量。

**备选**

- 继承父环境，方便子进程。
- 把配置原样写入缓存便于调试。

**理由**

- 当前配置包含 GitHub token、Context7 key、Stitch key 等敏感值。
- 沙箱内代码是模型生成的，不能假设其可信。
- 默认拒绝比“记得脱敏”更可靠。

**代价**

- 某些依赖父环境变量的 MCP server 需要显式配置 `inheritEnv` 或 env。
- 调试时需要额外开 `--trace-args` 并接受脱敏后的信息。

### D12：JSON Schema 只保证可表达子集，其余降级 `any`

**决策**

支持：

- object/struct、required/optional、description、default、min/max；
- string/integer/number/boolean、array、map；
- `$ref`/`$defs`、常见 enum、可合并的 `allOf`；
- 含 null 的 `anyOf` 做 optional/any 降级。

无法可靠表达的结构降级为 `any`，并在 catalog 中记录 warning。

**备选**

- 试图完整映射所有 JSON Schema。
- 遇到不支持的结构直接拒绝该 server。

**理由**

- MCP server 的 schema 质量参差，完整映射成本高且容易生成非法 IDL。
- 降级 `any` 能保证所有 server 可接入，同时明确告知精度损失。
- 原始 JSON Schema 保留在 manifest，后续可以改进映射而不丢信息。

**代价**

- 部分 tool 的 IDL 类型不够精确。
- 需要测试非法/边界 schema，避免生成器崩溃。

### D13：名称统一 sanitize，原名写入 manifest

**决策**

- 工作区目录/文件名和 IDL identifier 使用 sanitize 后的名字。
- 原始 server/tool/property 名保存在 manifest。
- 合法 identifier 尽量原样保留（包括 camelCase）。
- 非法 key 加 `@rename("<original>")`。

**备选**

- 直接用原始名，遇到非法字符就失败。
- 全部转 snake_case。

**理由**

- MCP tool 名可能包含 `-`、`.`、空格或数字开头。
- camelCase 原样保留能让生成的 `.d.ts` 与 wire JSON 一致。
- manifest 保证运行时仍能精确调用原始 tool。

**代价**

- 需要在 IDL、manifest、JS runtime 三处保持命名映射一致。
- 冲突处理需要确定性测试。

### D14：大 server 的公共字段抽取后置

**背景**

`agent-browser` 的 29 个 tool 共享大量字段。直接生成会导致 IDL 重复和上下文
浪费。

**决策**

第一版先按 schema property 顺序生成位置参数；嵌套 object 仍生成命名
struct。第二版再对**嵌套 struct** 做公共字段识别：

- 相同 schema 的字段集合出现超过阈值 -> 生成公共 nested struct。
- 嵌套字段用 struct 继承复用；顶层参数不做对象包装。

**备选**

- 第一版就做复杂 schema 归并。

**理由**

- schema 归并涉及 required、default、description、rename 和冲突处理，容易
  在第一版引入隐蔽 bug。
- 先保证每个 tool 可调用，再用快照测试驱动优化。
- 用户的核心目标是 code mode，不是最小 IDL。

**代价**

- 第一版 agent-browser IDL 较大。
- 需要压缩描述、抽取公共字段，但**不能省 tool 签名**；否则 `agents.md`
  索引用不起来，模型无法调用被省略的 API。

### D15：默认安全边界

**决策**

- 沙箱不提供 FS、网络、子进程、环境变量。
- 只允许调用 registry 中的 tool。
- `execute.code` 大小不超过 256 KiB；拒绝 NUL/非法控制字符。
- 工作区生成路径限制在 `--workspace` 根目录内，拒绝 `..` 逃逸。
- tool 调用数、执行时间、内存、栈、结果、日志都有上限。
- `--read-only` 可按 `readOnlyHint` 过滤。

**备选**

- 提供 `fetch` 让脚本自行访问网络。
- 把 execute 代码写入磁盘再执行。

**理由**

- code mode 的执行者是模型生成的代码，默认应视为不可信。
- 上游 MCP tool 已经提供了受控能力；额外开放 FS/网络会扩大攻击面。
- 资源上限防止无限循环、内存炸弹和上下文爆炸。

**代价**

- 某些需要文件/网络的脚本不能直接做，必须通过 MCP tool 或宿主扩展。
- 用户需要显式配置 allow/deny 或 `inheritEnv` 才能支持特殊 server。

### D16：默认排除自身 server

**决策**

读取配置时跳过：

- server 名为 `xidl-codemode`；
- command 指向当前可执行文件的 server；
- 明确标记 `self = true` 的 server（如果后续配置支持）。

**理由**

一旦把 `xidl-codemode` 加入 `programs.mcp.servers`，它读取同一份配置时会看到
自己；不排除会导致启动递归、进程爆炸或无限连接。

**代价**

- 如果用户真的想嵌套 codemode，需要显式 `--allow-self`，但默认不推荐。

### D17：文档落盘，manifest 由插件生成到 internal，不做磁盘缓存

**决策**

- 每次启动重新连接上游、重新生成 `CatalogSnapshot`。
- `agents.md`、namespace IDL 原子写入工作区，供 AI 索引。
- manifest 由插件生成到 `.codemode/.internal`，读入内存；不作为 AI 文档。
- 不做磁盘 cache；snapshot 带 `config_hash` / `tool_hash` 仅用于日志和
  诊断。

**理由**

- 文档落盘是为了 AI 索引；缓存不是目标。
- 本地 stdio 启动成本可接受；上游 tool 变化不会读到旧文档。
- 无缓存就没有 cache invalidation 和 secret 残留问题。

**代价**

- 每次启动都要等待上游 `tools/list`，慢 server 会拖慢启动。
- 无法离线启动；进程重启后必须重新发现并重写工作区。
- 工作区写入需要原子替换，避免 AI 索引到半成品。

### D18：xidlc 本地 path 开发，发布时 pin 版本

**决策**

- 本地开发：`xidlc = { path = "../xidl/xidlc" }`。
- 发布/Nix：使用 crates.io 版本或固定 git rev。
- 只启用 `gen-typescript`，避免编译所有 generator。

**理由**

- 本地联调需要和 `~/xidl` 的最新改动同步。
- Nix sandbox 不能依赖仓库外的 `../xidl`。
- 只编译 TypeScript generator 可以减少构建时间和二进制体积。

**代价**

- 两套依赖来源需要 CI 同时覆盖。
- 如果本地 xidl 有未发布改动，需要临时 patch 或 vendor。

### D19：每个 MCP 生成 `agents.md` 作为提示词注入载体

**背景**

上游 MCP `initialize` 可能返回 `instructions`。codemode 只暴露 `execute`，
没有 `read_api`；文档需要落盘给 AI 索引，同时也要让模型知道工作区位置。

**决策**

每个 MCP server 在工作区生成 `{mcpname}/agents.md`，内容包括：

- server 摘要和 namespace 索引；
- **完整、紧凑的可调用签名**；
- `mcp.<server>.<tool>()` 示例；
- namespace IDL 文件路径；
- 沙箱限制和返回结果形态。

上游 `initialize.instructions` 默认**不注入**。它是不可信内容，可能包含
prompt injection。需要时显式 `--inject-upstream-instructions`，并放在
明确的“untrusted upstream documentation”区块。

codemode 的 `instructions` 注入：

- 工作区绝对路径；
- server/namespace 索引；
- 每个 tool 的短签名；
- 提示模型用文件工具读取 `agents.md` / `*.idl` 获取完整描述。

**备选**

- 全部塞进 `execute` 的 tool description。
- 完全不注入，只靠文件路径。
- 提供一个 `read_prompt` 工具（与“只提供 execute”冲突）。

**理由**

- 落盘文档可被 AI 索引，完整 schema 不占系统提示。
- 注入索引让模型知道去哪里找细节。
- 默认不注入上游 instructions，避免第三方 MCP server 控制系统提示。
- 不需要额外的 MCP 工具，符合单一 `execute` 约束。

**代价**

- 工作区所有权/并发写需要处理；默认每项目一个 `.codemode/`。
- 文档可能过期，需要启动时重写。
- 上游 instructions 被默认丢弃，用户可能觉得信息不足；需要 opt-in。

### D20：`execute` 接收 stdio 内联代码

**背景**

用户明确：IDL 落盘方便 AI 索引，但 `execute` 的 JS 走 stdio、不落盘。

**决策**

```json
{ "code": "return mcp.github.get_issue('xidl', 'xidl', 361);", "timeout_ms": 10000 }
```

- `code` 必填；不提供 `path` 参数。
- JS 通过 MCP stdio 请求传入，直接放入 QuickJS，不写磁盘。
- 默认最大 256 KiB；拒绝 NUL/非法控制字符。
- 不在日志中记录完整 code（可能含敏感数据）；只记录 hash/长度。

**备选**

- 只接受文件路径。
- 同时接受 `code` 和 `path`。

**理由**

- 用户明确要求 execute 代码不落盘。
- 无需处理路径、symlink、TOCTOU 和脚本生命周期。
- 文档落盘 + 代码走 stdio，职责清晰：文件系统用于索引，stdio 用于执行。

**代价**

- 代码不可复用/不可审计，除非客户端自己保存。
- 大脚本受 stdio 消息和 256 KiB 上限约束。
- 不能靠文件路径做 per-script 权限控制，只能靠 code 内容/工具 allowlist。

### D21：stdio-only；IDL/agents.md 落盘，execute 代码不落盘

**背景**

远程 HTTP 客户端无法共享服务端文件系统；用户明确选择本地 stdio，并希望
IDL 落盘方便 AI 索引。

**决策**

- codemode 对外只提供 stdio transport。
- 上游 server 仍可配置为 stdio 或 streamable HTTP。
- `agents.md` / namespace IDL 写工作区，供 AI 索引。
- manifest 由 external plugin 从 IDL 生成到 `.codemode/.internal`，读入内存。
- `execute` 的 JS 通过 stdio 传入，不落盘。

**备选**

- 对外 HTTP transport + 上传脚本/resource。
- 只注入文档，不落盘。

**理由**

- 本地 stdio 是当前使用场景的最小实现。
- 文档落盘让 AI 可以索引完整 schema，不把上下文撑爆。
- 执行代码不落盘，避免临时脚本生命周期和审批对象不清。

**代价**

- 远程客户端不可用。
- 每次启动重新发现并重写工作区，无离线缓存。
- 多实例/多项目共享工作区需要隔离；默认每项目 `.codemode/`。
- 如果未来要支持远程，仍需设计代码上传方式；inline code 已经具备迁移基础。

### D22：tool 描述放 IDL `///`，`agents.md` 从同一 IR 渲染

**背景**

MCP tool 的 `description`、参数说明和输出说明需要同时出现在：

- 人类可读的 IDL 契约；
- 注入给模型的 `agents.md`。

如果两边各自维护，就会出现描述漂移；如果运行时重新解析 IDL，又把执行路径
和 xidlc HIR 绑定。

**决策**

- 在内存中维护一份 `ToolDoc` IR，包含 tool 名、namespace、description、
  参数/字段描述、签名。
- IDL emitter 把 `description` 渲染到 operation 前的 `///`，把字段描述渲染
  到 struct member 前的 `///`。
- `agents.md` emitter 从同一份 `ToolDoc` IR 渲染完整紧凑签名和压缩描述。
- 运行时派发只读 manifest，不解析 IDL。
- IDL 必须通过 xidlc 校验；`///` 会作为 `@doc` annotation 保留。

示例：

```idl
interface GithubIssuesApi {
    /// Get a single issue by owner/repo/number.
    ///
    /// Returns the issue body, labels, and state.
    /// Repository owner.
    string owner,
    /// Repository name.
    string repo,
    /// Issue number.
    int64 issue_number
};
```

`agents.md` 中对应：

```md
## /github/issues.idl
- `mcp.github.get_issue(owner: string, repo: string, issue_number: number)`
  Get a single issue by owner/repo/number.
```

**备选**

- 运行时解析 IDL 的 `///` 生成 `agents.md`。
- IDL 和 `agents.md` 各自维护描述。
- 把完整 description 只放 `agents.md`，IDL 不写。

**理由**

- 一份 IR 同时渲染两份文档，避免漂移。
- 人类可以在 IDL 里看到完整描述；模型在 `agents.md` 里看到签名和压缩描述。
- 不把 xidlc HIR 引入运行时派发路径。
- xidl 的 `///` 本来就是正式文档模型，TypeScript target 也会输出 JSDoc
  （数据类型的字段；interface method 不输出，但 IDL 本身保留）。

**代价**

- 需要维护 `ToolDoc` IR 和两个 renderer。
- 上游 description 是不可信文本，进入 `agents.md` 前要压缩、标记和限长。
- `agents.md` 不能为了省空间丢掉签名；只能压缩 description。
- xidl TypeScript target 不生成 interface method 的 JSDoc，所以 tool 方法的
  文档价值主要在 IDL 和 `agents.md`，不要指望 `.d.ts`。

### D23：IDL 只生成，不支持手写 override，失败转 bug report

**背景**

手写 IDL override 会引入第二份契约：用户改 IDL、生成器改 manifest，二者
必然漂移。用户明确要求不支持手写 IDL；生成 IDL 有问题就修生成器或 xidl。

**决策**

- 不提供 override 目录、override 配置或“保留用户修改”模式。
- 工作区 IDL 永远由 catalog 生成，文件头标记 `generated; do not edit`。
- 所有 IDL 写盘前通过 xidlc 校验。
- JSON Schema 无法表达的结构降级 `any` + warning：这是映射精度问题，不是
  bug。
- 生成的 IDL 被 xidlc 拒绝，或语义错误导致 MCP 调用失败：
  1. 该 server 标记 `error`，不注册 `mcp.<server>`。
  2. 生成 bug report，包含 server/tool、原始 JSON Schema（redacted）、
     生成的 IDL、xidlc 错误、codemode/xidlc 版本、复现命令。
  3. 符合映射规则但被 xidlc 拒绝 -> 提 xidl issue；映射逻辑产出错误 IDL ->
     提 codemode issue。
- CLI `bug-report` 输出 Markdown；不自动创建 issue，避免权限和误报。

**备选**

- 支持手写 override。
- 生成失败时静默回退到 `any` IDL。
- 跳过 xidlc 校验，先把文件写出去。

**理由**

- 单一事实来源：catalog -> IDL/manifest/agents.md，避免双写。
- 生成器 bug 必须暴露并修复，不能用手写文件掩盖。
- 失败隔离到 server，其他 server 仍可用。

**代价**

- 生成器/上游 schema 的问题会直接阻断某个 server，没有手工绕过路径。
- 需要维护 bug-report 模板和归因逻辑（xidl vs codemode）。
- 用户需要参与提 issue；自动化创建 issue 不在 MVP。

### D24：manifest 由 external xidl plugin 从 IDL 生成

**背景**

codemode 需要 manifest 做运行时派发，但 manifest 的字段（server、namespace、
原始 tool 名、flags）已经在生成的 IDL 和 annotation 中。再写一个内部
manifest emitter 会产生两套映射逻辑。

**决策**

- 新增外部 xidl plugin：

```text
target: codemode-manifest
binary: xidl-codemode-manifest
```

- 插件实现 xidlc `Codegen`：
  - `get_engine_version()` 返回 `*`（MVP）；
  - `get_properties()` 返回空，使用 unit-scoped HIR；
  - `generate(CodegenInput::RpcHir, path, props)` 遍历 HIR。
- 对每个 server 生成一个内部 `bundle.idl`，通过 xidlc external plugin
  机制运行：

```bash
xidlc gen --out-dir .codemode/.internal codemode-manifest \
  .codemode/.internal/github.bundle.idl
```

- 插件输出 `manifest.json` File artifact，内容：

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

- IDL 必须带 annotation：`@mcp_server`、`@mcp_namespace`、`@mcp_tool`、
  `@mcp_read_only`、`@mcp_destructive`、`@mcp_idempotent`、
  `@mcp_open_world`。`@mcp_tool` 即使和 JS 名相同也必须写，避免插件靠命名
  推断。xidl lint 明确允许 unknown annotation 由 external plugin 解释。
- codemode 读取 manifest 后与 `tools/list` catalog 交叉校验：缺项、多项、
  JS 名冲突、namespace/IDL 路径不一致都是 bug。

**备选**

- codemode 内部直接生成 manifest。
- 运行时临时解析 IDL。
- 让 manifest 成为第二份手写契约。

**理由**

- IDL 是唯一契约，manifest 是派生视图。
- 外部插件复用 xidlc 的 HIR/annotation 语义，避免重复 parser。
- 插件边界清晰，可以单独测试、发布和版本兼容检查。

**代价**

- 插件二进制必须在 PATH 中，或由 codemode 注入插件目录。
- IDL annotation 不完整会直接导致 manifest 错误。
- 插件与 xidlc engine version 的兼容需要维护。
- 多一个进程启动和 NDJSON 往返成本。

---

## 4. 架构不变量

以下约束不应在实现中被打破：

1. 任何生成 IDL 都必须通过 xidlc 解析/生成校验。
2. 任何 `mcp.<server>.<tool>` 映射都必须来自 manifest；`execute.code` 必须
   经过大小和字符校验。
3. 任何 tool 调用都必须经过统一的 bridge 和限额检查。
4. 每次 execute 的 JS 状态必须隔离。
5. 配置、env、header 中的 secret 不得出现在 IDL、manifest、日志。
6. 单个上游 server 失败不得导致整个 codemode server 崩溃。
7. 上游 tool 原始名和 JS 安全名之间的映射必须可在 manifest 中审计。
8. 默认配置必须排除自身，除非显式允许。
9. 结果和日志必须有大小上限，不能无界返回给模型。
10. IDL、`agents.md` 必须来自同一次 catalog 快照；manifest 必须由该 IDL
    经 external plugin 生成并回查 catalog。
11. codemode 只把 AI 文档写到 workspace；manifest 只在 internal 目录，
    execute code 不落盘。
12. IDL 只生成，不支持手写 override；生成 IDL 失败必须暴露为 bug。
13. 运行时 manifest 只能由 `xidl-codemode-manifest` 插件从 IDL 生成。

## 5. 未选择的方案

| 方案 | 为什么不选 |
| --- | --- |
| 用 TypeScript/Node 实现 | 部署需要 Node，xidlc 调用和沙箱边界更绕；不是不能做，但不是 MVP 最优 |
| 把每个上游 tool 直接暴露成 MCP tool | context 爆炸，失去 code mode 的聚合价值 |
| 额外提供 `list_apis` / `read_api` | 用户明确要求只提供 `execute`；发现信息走工作区文件和注入索引 |
| `execute` 只接受文件路径 | 用户明确要求 execute JS 走 stdio、不落盘；路径会引入 TOCTOU 和脚本生命周期 |
| 只把文档注入 prompt、不落盘 | 用户要求落盘方便 AI 索引；完整 schema 不应该全塞进上下文 |
| 支持手写 IDL override | 会形成第二份契约，与 manifest/生成器双写漂移；生成问题应提 bug |
| codemode 内部再写 manifest emitter | 会和 IDL 生成器形成两套映射逻辑；manifest 应从 IDL 插件派生 |
| 运行时临时 parser 解析 IDL | 与 xidlc HIR/annotation 语义漂移；应使用 external plugin |
| 对外提供 HTTP transport | 当前场景是本地 stdio；远程还需要额外的代码上传/认证设计 |
| 运行时重新解析 IDL 做派发 | 与 xidlc HIR 强耦合，版本升级风险高 |
| 在沙箱里提供 `fetch` / `fs` | 扩大攻击面，破坏默认拒绝模型 |
| 复用单个 QuickJS runtime | 跨执行状态泄漏，难以证明隔离 |
| 直接返回 `structuredContent` | 丢失 content blocks、图片/资源、`isError` 语义 |
| 第一版就做完整 JSON Schema -> xidl 映射 | 工作量和边界 bug 太大，先保证可接入 |
| 把配置和 token 写入缓存便于调试 | 不可接受的 secret 泄漏风险 |

## 6. 后续需要重新评估

这些决策有明确的升级路径，但不应在第一版同时做：

- `agents.md` 的索引/签名注入策略是否需要按客户端调整。
- MCP 没有原生 namespace，`namespace_map` 的默认推导是否足够。
- 多 codemode 实例共享同一项目 `.codemode/` 时是否需要 per-config 子目录。
- `execute` 的 256 KiB 上限是否需要按 server/tool 调整。
- 同步桥接是否要升级为 Promise 并发。
- 如果未来要支持远程 HTTP，是否增加认证和代码上传通道。
- 是否为 xidl 上游增加一个正式 `mcp`/`codemode` target。
- manifest 插件的 engine version 兼容范围和升级策略。
- `@mcp_*` annotation 是否要进入 xidl 正式 annotation 集合。
- 是否让 `bug-report` 可选地自动创建 GitHub issue。
- 是否需要 Windows/macOS 的 transport 和路径兼容层。
