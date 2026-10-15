# 命名与双语注释

适用于 KYNXA 自有 Rust 网关与资源服务、C#/WinUI 与 ToolHost、WebView JavaScript、PowerShell 构建及对应测试。当前入口和职责见[原生网关架构](../../../../docs/architecture/native-gateway.md)；规则参考公开团队指南，再按当前语言与契约调整，不要求切换技术栈。

规范化的验证与收尾遵循[最低必要验证与推进节奏](../../kynxa-development/references/validation.md#最低必要验证与推进节奏)，不为命名、注释或格式修改自动增加探针、快照及全套验收。

## 语义和兼容性

名称优先表达用途。`output` 若实际持有终端标准输出，使用 `stdoutBuffer`；预算中的 `used` 使用 `usedTokens`；截图的设备上下文使用 `windowDeviceContext`。不为已经清晰的 `path`、`error`、`index` 增加重复修饰。

在修改前查清以下关系：声明与引用、同名局部作用域、闭包捕获、对象简写、模式解构、反射、序列化、事件和 XAML 绑定。不能只查文字命中后机械替换。

```js
// Keep the public JSON key while giving the local value a specific unit.
// 保留公开 JSON 字段，局部变量则明确使用 token 单位。
const { budget: inputBudgetTokens } = request;
return { budget: inputBudgetTokens };
```

导出方法、DTO/record 属性、JSON/schema 键、工具 ID、SSE 事件、错误码、命令参数、环境变量和 Win32 ABI 保持现有合同。确实需要改变时走独立的两端兼容修改，不当作纯变量重命名。数据日志、聊天、记忆和来源 ID 不做大小写迁移。

偏移名称要区分 `offset_bytes`、UTF-16 单元、行号与 token；公开字段已经叫 `offset` 时保留字段并在说明或局部别名中注明实际单位。原始 JSON 转义、内容哈希输入和日志字节不能为了美观重新编码；常量只改内部名称，不顺手调整预算、TTL 或重试额度。

## Rust

- 类型、枚举和 trait 使用 UpperCamelCase；模块、函数、局部变量和字段使用 snake_case；常量使用 SCREAMING_SNAKE_CASE。单位写进名称，例如 `generated_tokens`、`duration_ms`。
- JSON 与 IPC 的 camelCase 字段继续保持，通过 `serde` 映射或明确的 JSON 投影衔接，不因内部命名修改 wire 合同。
- Rust 同时拥有确定性规则和已登记的原生 I/O 接线；按 `docs/team/module-ownership.json` 的 `allowedDomainDependencies`、`integrationModules`、`executionModules` 区分职责。模型 HTTP/流、MCP、进程和资源 IPC 留在所属 Rust 接线层，正式文件和 SQLite 留在事务所有者；不重新引入 Node/NAPI 适配或重复账本。
- 可预期错误返回 `Result` 与稳定错误码；`expect` 只用于静态、已验证的不变量。取消后已经返回的执行事实不能丢弃；IPC 故障不能重置副作用账本再继续派发。
- 私有 helper 可以表达实际可复用步骤；抽取时保留 `await`/`select!` 分支、锁范围、等待者登记、通知与 `Drop` 顺序。`Mutex` 内状态改成具名字段时仍保持同一锁和计数语义，不凭命名整理增加并发或改变关闭通道的含义。
- FFI 的 `repr(C)`、`extern` ABI、原生字段顺序和函数入口名保持原样；自有封装可用语义名称。涉及 `unsafe` 时说明谁拥有缓冲区/句柄、有效期及匹配的释放函数，不能仅替换名称后更换释放方式。
- 使用 rustfmt，4 空格、UTF-8/CRLF。新说明性注释保留中英文两层；锁定依赖与 Cargo.lock，随包提供许可证原文。

## C#

| 对象 | 项目约定 | 示例 |
|---|---|---|
| 类型、方法、属性、事件、常量 | PascalCase | `ChatStreamReader`、`ReadAsync`、`MaximumEventCharacters` |
| 接口 | `I` 前缀及 PascalCase | `IAgentApi` |
| 参数和局部变量 | camelCase | `conversationId`、`eventData`、`processHandle` |
| 私有实例字段 | `_camelCase` | `_requestCancellation` |
| 私有静态配置/共享只读字段 | 保留本项目 PascalCase 约定 | `JsonOptions`、`ExcludedDirectories` |
| 异步 Task 方法 | 动词及 `Async` 后缀 | `LoadConversationAsync` |
| 泛型参数 | 通用 `T` 或 `T` 加语义 | `TMessage` |

事件处理函数、框架 override、生成控件名称、已有 namespace 和原生声明遵守既有调用合同。句柄参数在自有业务代码可叫 `windowHandle`；`DllImport` 入口、`StructLayout` 字段和原生常量按原生声明保留。record 主构造参数是属性，不当作可自由改名的局部参数。

数值带单位，例如 `durationMs`、`maximumOutputTokens`、`offsetBytes`。布尔值表达肯定条件。`ID/URL/HTTP` 的既有对外拼写不强制迁移；新局部名称使用 `Id/Url/Http` 的 camelCase 组合。

控制流沿用 Allman 大括号、4 空格，保留已清楚的局部类型推断。异步改名不改变 await 顺序、取消传递、UI 线程回写或 Dispose 所有权。

WinUI 视图和控件负责展示，API 客户端负责 HTTP/SSE，ToolHost 负责 Windows 执行；`apps/shared/` DTO 保持传输合同。整理不得把正式聊天/索引写入搬到 UI 缓存，也不更改 WebView 消息类型、会话身份、原生句柄布局或跨 UI 线程的调用方式。

## JavaScript / ESM

- 函数/变量/参数 lowerCamelCase，类 PascalCase；文件沿用小写连字符和 `.mjs`/`.js`。
- 上限、协议版本等固定模块常量用 UPPER_SNAKE_CASE；并非每个 `const` 都是这种常量。
- 私有 helper 用动词和对象，例如 `archiveBrowserScreenshot`；布尔判断优先 `is/has/can/should`。
- 使用 `const` 表达不重新赋值的绑定，确实重赋值才用 `let`。纯规范化不借机改变可变性、export 或回调执行时机。
- 2 空格、现有单引号及分号风格；开发 `.mjs` 保持 ESM 显式扩展名，WebView `.js` 保持现有加载方式，不为统一格式把普通脚本改成模块。无强制 80 字符裁切，长语句按实际可读性拆分。

对象简写和解构需要特别核对：`{ output }` 改局部名称后应显式保留 `{ output: stdoutBuffer }`。嵌入浏览器脚本中的参数与函数名也可能是调用合同，不机械替换模板字符串。

WebView 的 DOM、`window` 导出、C#↔WebView 消息字段、流式合并刷新和选择/滚动状态属于既有界面合同。开发检查可以使用 Node，发布核心不因此依赖 Node；私有 Node 的第三方 MCP/Skill 包与许可证不做自有代码批量改写。

## PowerShell

- 自有函数优先批准动词加名词，例如 `Get-RuntimeManifest`、`Test-RuntimeCache`；既有脚本入口和参数名保持兼容。
- 局部变量用明确的 camelCase 或 PascalCase 现有风格；`$processStartInfo` 比 `$info` 更清楚，涉及范围时用任务前缀。
- 不复用 `$HOME`、`$PID`、`$Error` 等自动或系统变量；变量改名不改变参数绑定与环境变量名。
- 保留 LiteralPath、确定的路径边界、隐藏后台启动和现有退出码。规范化不能引入额外 shell 或更改命令语义。
- 入口继续使用 `tools/development/start-native-gateway.ps1` 和 `apps/desktop/Build/prepare-native-runtime.ps1` 的既有参数；Node 扩展准备位于 `prepare-mcp-runtime.ps1`。启动路径、随包路径、校验哈希与下载来源是发布合同，不能为缩短脚本绕过校验或恢复 `node server.mjs`。
- 需兼容 Windows PowerShell 5.1 且含中文等非 ASCII 执行字符串的历史脚本，保留原 UTF-8 BOM。例如设计文档构建脚本由 `.editorconfig` 单独声明；不能为格式统一移除 BOM，导致旧宿主按 ANSI 解码。

## 注释写法

优先解释为何这样处理：为何在取消后仍保存已返回的回执、为何不能拆开工具与结果、谁释放资源、什么情况下拒绝旧状态。英中文含义相同，不把限制译成成功保证。

```cs
/// <summary>
/// Reads SSE frames across packet and UTF-8 character boundaries.
/// 跨网络分包和 UTF-8 字符边界读取完整 SSE 事件。
/// </summary>
public static class ChatStreamReader { }
```

```js
// Persist a returned receipt before stopping the cancelled request.
// 已返回的执行回执先落盘，再停止被取消的请求。
```

```powershell
# Use only this fixture's owned process for cleanup.
# 清理仅针对本夹具自己创建的进程。
```

英文多行说明可以配一段中文，不必逐行复制。短中文说明可紧邻英文；XML/JSDoc 文档保持合法结构，不能把中文插进标签属性、参数名、工具定义字符串或示例代码中的字符串。中文也不能附带新的运行承诺。

以下内容不按普通注释改写：第三方源文件及 skill 原文、许可证、版权、自动生成文件、压缩库、source map、pragma/分析器指令、数据和代码字符串、外部文档标题、单纯单位或字段标记。现有纯中文说明可保留；以后修改该非平凡说明时补齐英文层。

## 工具与验收

`.editorconfig` 给出格式与 C# 命名提示。提示帮助编辑，不代替作用域判断、构建或业务验证；只格式化本轮相关文件。Rust 使用 `cargo fmt`/`rustfmt` 及受影响回归，C# 使用对应桌面/ToolHost/smoke 构建，WebView/开发 JavaScript 使用现有检查；开发期 Node 检查不构成产品后端依赖。文件移动或跨域调用变化运行 `node tools/development/check-architecture.mjs`，同时核对调用端与随包路径。

纯注释修改应只有注释差异；命名修改应核对符号引用以及未变化的键、字面量、分支、调用顺序和参数。按改动影响选择必要的编译或验证，不因纯文档、注释修改重跑功能测试。需要运行网关、客户端、原生工具或真实展示检查时，只使用临时数据、模拟凭据及自有窗口，不调用用户真实模型。

## 一手参考与本项目选择

核对日期：2026-10-05。

- [Microsoft C# 标识符命名](https://learn.microsoft.com/en-us/dotnet/csharp/fundamentals/coding-style/identifier-names)：参考语义命名和 C# 大小写；本项目继续保留既有私有静态只读字段 PascalCase。
- [.NET Runtime C# 风格](https://github.com/dotnet/runtime/blob/main/docs/coding-guidelines/coding-style.md)：参考清晰书写、局部风格和维护约定；不把其全部字段前缀强制应用到既有合同。
- [Google JavaScript 指南](https://google.github.io/styleguide/jsguide.html) 与 [Google TypeScript 指南](https://google.github.io/styleguide/tsguide.html)：参考命名、模块及说明注释；JS 指南已停止维护，参考通用原则而不据此迁移 KYNXA 到 TypeScript。
- [Google 代码审查关注点](https://google.github.io/eng-practices/review/reviewer/looking-for.html)：参考可读性、复杂度、测试与解释原因；中英文双层是 KYNXA 的项目要求。
- [PowerShell 批准动词](https://learn.microsoft.com/en-us/powershell/scripting/developer/cmdlet/approved-verbs-for-windows-powershell-commands)：新建自有函数采用动词名词，现有脚本公共参数不随风格修改。
- [.NET EditorConfig 命名规则](https://learn.microsoft.com/en-us/dotnet/fundamentals/code-analysis/style-rules/naming-rules)：使用 IDE 可识别的提示，不升级为全仓阻断规则。
