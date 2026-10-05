# 基础工具、MCP 与应用技能

本页描述当前工具闭环。设置中的“工具与技能”打开独立窗口，管理 MCP 服务、单个工具启停、应用技能与参数；扩展文件可单独选择存储目录。聊天输入区的三种权限参与真实执行判定。运行中聊天只展示简洁动作和来源链接；窗口截图在右侧小模块查看，完整工具记录保留在正式日志。

## 浏览器连接与启动回执

工具窗口的浏览器字段映射到已固定版本的官方 MCP 参数，未编辑的高级配置原样保留。Chrome DevTools 可选现有 Chrome（`--autoConnect`）、独立窗口或无头实例（`--headless`）、CDP 地址（`--browserUrl`/`--wsEndpoint`）；Playwright 对应官方扩展（`--extension`）、独立配置、CDP（`--cdp-endpoint`）或已有 Playwright 协议（`--endpoint`）。配置文件或环境驱动的连接显示自定义，避免简单选择器无声改变实际协议。配置保存和连接仍分开，认证引用不进入模型提示。

现有浏览器连接可以在用户授权下使用已登录网页，不导出 Cookie、密码或令牌。首次浏览器调试允许和站点验证由用户完成。独立本机浏览器与远程/云端浏览器没有隐含登录继承；服务端可用性、目标站点访问限制仍以实际回执为准。原生 `computer.read` 是 UI Automation 文字，页面 DOM 操作使用浏览器 MCP。

`browser-sessions.mjs` 根据已配置的实现识别 Chrome/Playwright，记录连接、当前聊天、标签页及快照代次，不根据第三方工具名称猜测浏览器身份。后台参数在审批前规范化，执行时重新核验并按连接串行处理选页状态；不偷偷调用选页来修正错误目标。点击和表单填入使用新鲜 DOM/可访问性引用，导航或动作后重新读取；跨域 iframe 仍走浏览器引用。首次快照缺少标签页 ID 时保留文字但不虚构 ID，须显式发现标签页后重新获取可用引用。

后台是默认浏览器偏好；用户明确禁止前台时，当前请求的原生激活和模拟输入也被拒绝。`computer.launch.background` 仅是最佳努力的启动提示，第三方应用可能覆盖，程序检查实际前台回执。GUI 非网页部分可使用有界 UIA 元素或区域，再按授权退回前台鼠标/键盘。密码控件不读名称、值或文本，但保留控件类型和定位；不把“不读密码”错误解释为“不能输入用户授权的密码”。文本模型没有截图视觉输入，不能声称仅凭图片就识别了画布中的控件。

GUI 启动用 `CreateProcessW` 且禁止继承句柄。网关不等应用退出；只等待助手回执和有界管道排空，取消、超时有独立结束期限。已完成回执即使与取消竞争也先归档；未确认的启动/输入记录为 `unknown` 后停止模型循环。启动 PID 可向已有浏览器进程委派，必须重新列举窗口，不能把任意窗口当成本次打开的页面。

只读操作没有写入结果待确认：UIA 默认 3 秒（可配置 500–5000ms）并提前拒绝未响应窗口，浏览器观察失败保留类型和工具配对后允许模型换连接或方法。同一读取目标连续三轮失败时禁用下一轮工具并生成阻碍说明，聊天不删除。导航、输入或脚本等可能产生副作用的超时/断连仍保存 `unknown` 并停止，恢复前核验已发生的操作；不通过重复执行验证。`computer.window` 支持调整客户区物理尺寸、最大化、最小化、恢复，报告实际状态；截图 `crop` 不重采样。

截图链统一接收原生 PNG、标准 MCP 图片和已确认的本机截图输出路径；路径先检查链接、范围，再兼容 Windows 8.3 与长名称。Chrome 将 `.jpg` 输出为 `.jpeg` 时仅兼容同一受控目录、同文件名的已知截图回执。远程连接不读取本机同名文件。正式结果保留原始类型；桌面通过归档加载 PNG/JPEG，切换聊天和隐藏侧栏时遵守已有生命周期，不下载任意 Markdown 图片、不向文本模型发送像素。

连接方式参考固定版本的 [Chrome DevTools MCP 配置](https://github.com/ChromeDevTools/chrome-devtools-mcp/blob/chrome-devtools-mcp-v1.10.1/docs/configuration.md) 与 [Playwright MCP](https://github.com/microsoft/playwright-mcp/tree/v0.0.83)。真实验收脚本为 `tests/native-desktop-smoke/browser-smoke.mjs`、`tests/screenshot-panel-ui-smoke/browser-artifacts-live-smoke.mjs`；`--remote-cdp` 使用隔离 loopback 端点验证远程协议，不代表某个商业云浏览器或用户登录账号已经验收。

## 官方工具包与用户工具

官方内容只有一份，位于安装包内的 `model-gateway/official-tools/`；源码对应 `apps/model-gateway/official-tools/`。当前包版本 `0.4.1`，集中提供 **35 个工具声明、11 个 MCP 连接预设、7 个技能**及其资源、来源和许可证。核心执行仍经过原 `ToolService`、文件服务、网页读取服务和沙箱，不新增另一套执行链；MCP 实现由固定版本外部发行包或远程服务提供，不把其运行环境重复塞进官方目录。

| 用户能力 | 官方执行入口 | 运行条件 |
|---|---|---|
| 文件增删改查、搜索、长源码分页 | `filesystem.*`，`workspace-inspect` / `safe-file-edit` | 随包；未挂载聊天使用持久独立工作目录 |
| 软件启动、窗口调整、截图、UIA 阅读与鼠键输入 | `computer.*`，`desktop-workflow` | 随包原生 ToolHost；交互式 Windows 桌面 |
| 本机 CMD / PowerShell / Conda，或隔离 Node / cmd | `terminal.host.run` / `terminal.run`，`terminal-workflow` | 宿主程序需已安装；沙箱先验证；无需额外 Python 启动工具链 |
| 公开网页正文与链接 | `web.fetch`，`web-research` | 随包 Node 与 `html-to-text`；需联网，无浏览器登录或脚本执行 |
| 浏览器结构、表单、导航、页面截图与调试 | Playwright / Chrome DevTools，`browser-workflow` | 官方预设默认启用；首次连接使用固定 npm 包，对应浏览器需可用；缺启动程序或配置时显示未就绪 |

七个官方技能为 `workspace-inspect`、`safe-file-edit`、`browser-workflow`、`desktop-workflow`、`terminal-workflow`、`web-research`、`internal-comms`。新增工作流不安装软件，也不授予权限。已有文件、终端和本机桌面实现均复用，不重复内置另一个 Filesystem 或 Windows MCP。Python Fetch、Git、MarkItDown 和 Windows Screenshot 仍是可选预设，其额外环境不属于基础工具要求。只有 Edge 的 Windows 用户可在浏览器设置选 `msedge`；预设存在不等于已经启动或连接成功。

`web.fetch` 使用原生 HTTP(S) 请求和固定版本 **html-to-text 10.0.0 / MIT** 的 HTML 解析、实体和链接转换，许可证随 `node_modules/html-to-text/LICENSE` 打包；接口不会执行网页 JavaScript、携带 Cookie 或认证头。每跳验证公共地址和全部 DNS 答案，连接固定已验证地址；拒绝本机/内网、带凭据 URL 及非标准端口。本地开发页、登录页、动态页交给已配置浏览器 MCP。最多五次重定向，压缩前后各限制 2 MiB，整次读取可取消和超时；HTTP 错误或不完整响应不能当作成功。

代理的 Fake-IP DNS 若将公共域名解析至保留/私有地址，轻量读取同样拒绝并明确提示改用已启用的浏览器；不把虚拟 IP 当作已验证公网地址。本机匿名外网探针实测遇到了这个边界，未据此宣称公网 TLS 端到端通过。浏览器路径沿用其已配置网络，不自动变更代理、登录状态或切换浏览器身份。

提取后的正文最多保留 262,144 UTF-16 单位，超限及解析深度省略明确标记；模型默认先读不超过 16,000 单位的页面。结果含实际最终 URL、标题、时间与来源链接，完整有界提取文本沿用聊天的 `tool-results` 归档。`offset` / `nextOffset` / `hasMore` 支持实时网页分页；每次 `web.fetch` 重新获取，稳定回看使用结果引用和 `tool.result.read`，不把当前页面误称为以前的快照。独立读取可并行，网页活动直接显示网址，不展示参数/结果 JSON。Ask/Smart 仍需外部读取审批，Full 沿用已有授权。

`filesystem.read` 的可选 `offset` 为 UTF-16 单位，范围 `0–1,048,576`；原 `maxBytes` / `maxChars` 保留。返回 `nextOffset`、`totalCharacters`、`hasMore`，分页不拆代理对，每页重新计算完整文件 SHA-256。模型应按 `hasMore` 判断结束，发现 SHA 改变即停止拼接并重新读取；尾页的 `truncated:true` 表示这一页不是整个文件，不代表还有下一页。

技能摘要最多十二项，并按本次剩余提示空间注入；小窗口可以全部按需加载。权限和执行规则保留，所有启用技能仍可用 `skill.list/read` 查看，不用扩大旧连接窗口才能载入桌面工具。格式与转换依据：[html-to-text 官方源码](https://github.com/html-to-text/node-html-to-text)、[Playwright 固定版本配置](https://github.com/microsoft/playwright-mcp/tree/v0.0.83)、[Chrome DevTools 工具清单](https://github.com/ChromeDevTools/chrome-devtools-mcp/blob/chrome-devtools-mcp-v1.10.1/docs/tool-reference.md)。

2026-10-05 官方能力补齐验收：完整网关 **658/658**、真实 WinUI/WebView2 **196**、复制载荷后清空 PATH/无外部 .NET 的运行检查 **38** 项通过，桌面 x64 构建零警告零错误。固定版本 Chrome DevTools 与 Playwright 在临时配置的本地测试页分别完成跨源 iframe 导航、实际点击、DOM 读回与截图，两条工作流通过；另有 **23** 项检查确认 PNG/JPEG 归档与浏览器源文件逐字节相等，图片已查看。三协议的旧 8K 连接均可发现七个技能并加载桌面工具，未改变用户窗口设置。最初一次桌面能力检测短暂不可用，未捕获当时底层状态；同一二进制复查及最终全量回归通过，未放宽检测或跳过测试。全部使用临时数据、合成页面或独立浏览器配置，未读取日常账号、调用付费模型；云端 CDP、登录/MFA 和所有可选第三方服务未在本轮验收。

```text
KYNXA 安装目录/
└─ model-gateway/
   └─ official-tools/                 随版本发布，运行时不改写
      ├─ manifest.json                包版本、来源、许可证与内容清单
      ├─ LICENSE.txt
      ├─ Tools/catalog.mjs            核心目录，引用 computer.mjs 本机工具声明
      ├─ MCP/catalog.mjs              11 个固定版本或 hosted 预设
      └─ Skills/技能名称/               SKILL.md、参考资源与许可证

用户工具目录/                          设置中可选择和迁移
├─ Agent/config.json                  用户服务、官方差量覆盖、技能目录与启停
├─ Skills/                            用户导入的技能
├─ MCP/                               用户工具缓存、浏览器及可选运行环境
└─ Backups/Extensions/                用户配置迁移恢复记录
```

设置中的路径行显示“用户工具”，只迁移下面这一层。官方路径在工具窗口中只读显示，由应用位置计算；不写死盘符或用户名，也不随用户目录迁移。官方目录整体受已有文件代理的写保护，官方身份不授予额外工具权限。发布按明确资源类型选择官方文件，ToolHost 输出排除 PDB/TMP；用户工具、配置和聊天不在官方文件清单中。

`GET /api/agent/config` 返回合并后的有效配置：官方默认加用户自定义服务，服务器标明 `origin:official|user`，官方项附 `presetId`、`overridden`，根对象附官方/用户目录和官方包版本。11 个官方 MCP 默认启用；启用表示允许按现有连接流程使用，不代表已经连接或运行环境齐全。打开设置、保存开关和显示清单不会启动服务。已有用户连接保留 ID、版本、参数和认证引用，明确关闭或隐藏的选择继续生效；同发布者或相同连接优先复用已有配置，避免另外启动重复默认服务。

连接前检查所需账号环境变量和必填配置路径占位符；缺启动程序、认证变量或必填路径时保留启用选择，显示未就绪。Python/uv、浏览器等依赖仍按预设要求准备，浏览器工具目录发现成功不代表浏览器已安装或登录。连接失败保留脱敏诊断，不把未就绪服务或旧目录当成模型可用能力。实际发现且启用的工具仍经过原有按需声明预算和工具级禁用规则，默认启用不扩大模型上下文或执行权限。

每次目录发现最多并行连接 4 个独立服务，返回工具顺序保持配置顺序；不同聊天仍按各自工作范围建连，总连接上限保持 32。配置重置会立即使旧目录发现失效，停止后续批次，并在清理完成前拒绝新建连接；重叠重置共用同一清理过程，避免后启动的进程丢失所有者。首次下载、慢服务及超时仍可能增加准备时间，尚未实现所有服务完全按需冷启动。

保存仍使用 `expectedRevision` 和完整有效 `mcpServers` 数组，程序仅将新增用户项与官方参数差异写入用户 `Agent/config.json`：`officialMcpOverrides:[{presetId,id,changes}]`。只改启停不会复制或冻结整份默认定义；官方升级后未修改字段使用新默认，明确修改的参数继续保留。`disabledOfficialMcpServers` 保存显式隐藏的默认项，旧版本缺少新字段时按兼容默认读取；未知/暂时不可用的官方覆盖保留为未激活配置。来源字段由后端重建，客户端不能用标签改变授权。

工具窗口可筛选官方或用户来源。官方项支持启停、参数覆盖和恢复默认；用户自定义项继续新增、编辑和删除。技能标明官方、用户或项目来源，按需加载及原有权限不变。官方技能 ID 由包 ID 与相对路径生成，升级或安装位置变化不改变 ID；本次搬迁兼容旧位置的禁用 ID，并在配置视图中归一为稳定 ID。用户目录技能继续沿用已有路径身份及迁移映射。

分层借鉴本机参考项目的 profile 配置补丁与分范围技能发现，以及 [pi 的设置合并](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/settings-manager.ts)、[资源来源与去重](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/resource-loader.ts)、[OpenCode 配置来源](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/config/config.ts)。只使用适合现有加载器的设计，不依赖参考源码目录，也不复制整套框架。敏感工作文件读取策略按用户要求留到后续安全专项，本轮不把分层描述成已解决该问题。

2026-10-04 上一阶段分层与任务链验收：完整网关 **507/507** 通过，原生工具窗口 **31** 项、客户端 **46** 项、用户工具迁移 **124** 项通过；聊天流、会话客户端、模型预算与网关冷启动检查通过，桌面构建零警告零错误。该阶段官方包 **14** 个明确发布资源与内容清单对应，技能原文字节和许可证保留。模型与网页资料使用隔离模拟，三协议均验证一次搜索、两个来源并行读取、直接来源链接、保存重开与完整工具配对；代码链使用真实 Windows AppContainer，验证失败测试、再次修复、通过测试、保存重开且不重放。另用隔离匿名 Exa 服务实际搜索公开 SDK 文档成功，未读取用户认证或调用付费模型；这些结果不能代替真实付费模型的任务成功率和耗时评估。

## 从模型请求到执行

桌面发送 `permissionMode`、聊天 ID 和请求 ID。Runtime 从正式 catalog 查找聊天所属工作和挂载目录，为这次请求固定范围与权限，提供函数目录。本轮的模型连接同样固定，避免用户编辑连接时把一种协议的工具参数发送到另一种协议的接口。模型生成完整参数后，工具服务检查路径、参数、版本与审批，再执行操作；结果以提供商原生工具结果格式返回模型继续生成正文。

支持 OpenAI Chat Completions、OpenAI Responses 和 Anthropic Messages。Gemini、DeepSeek、Ollama 等使用现有 OpenAI 兼容接口时，需要实际模型及服务支持 function calling。没有 `permissionMode` 的旧调用仍走文本回复接口。工具名称在上游映射为合法函数别名，返回时反查本次目录，再交给内部权限服务；模型不能通过别名绕过权限。

流式事件增加 `tool_call`、`approval_required`、`tool_result` 和 `content_snapshot`，聊天展示简洁动作、网页链接和公开阶段回复，完整工具数据由正式记录保留。每轮完成可修订流式草稿；后续工具或模型中断也会保留已修订的正文。Responses 的加密推理项及 Anthropic 的签名块使用受保护的原生续接档案，不进入公开聊天接口。

运行中展示公开阶段正文及紧凑动作、命令或网页链接，工具文字为 12px 中灰色，可换行；主聊天不展示参数与结果 JSON，也没有“查看截图”按钮。最终正文完成渲染后移除过程，只保留最终正文和整轮用时。完整活动仍保存于正式记录，截图在右侧栏的内容标签中查看。增量更新复用未变化活动节点，不随正文 token 重绘整组。

连续执行保留现有模型→工具→模型闭环，默认每个请求最多 64 轮、256 次工具调用、估算生成 1048576 tokens 与 30 分钟，不要求用户在普通步骤间再次发送“继续”。`runLimits` 可在聊天 API 请求中设置受限预算；服务器的请求生命周期仍是外层硬限额。估算生成量包含可见正文、思考及工具参数，并非供应商精确计费或累计输入用量。`ToolRun` 进度与回执保存于该助手消息的正式 JSONL，`GET /api/conversations/:chatId/runs/:requestId` 返回阶段和计数。已完成结果先保存并发送，再更新进度；预算、取消或保存失败阻止下一次操作。

`ToolRun.diagnostics` 补充每轮模型、工具与审批等待的单调时钟耗时、计数和最近 64 个模型/256 个工具计时记录。只保存数值、轮次及已有调用 ID，不记录参数、网址、结果或私有模型字段。`totalToolMs` 是各调用扣除审批等待后的累计处理耗时，并发时不能与模型耗时相加当作总墙钟时间；整轮实际用时仍取 `DurationMs`。诊断随既有进度保存和上述接口读取，不增加逐 token 写盘或主聊天卡片。

`tool-observations.mjs` 管理同一请求的观察缓存与无进展检测。仅明确的无状态搜索和 HTTP(S) Fetch 可复用 15 秒内成功的小结果，最多 32 项、总计 256 KiB；文件、历史和结果分页仍重新读取。缓存不跨请求，绑定 MCP 连接实例，状态操作、配置变化、失效连接与结束请求使其失效。复用仍经过本次调用的参数、原因、权限、单次审批、归属与 MCP 目录校验，为当前调用重新保存归属正确的结果引用；记录复用标记及原观察时间，不伪装成再次联网。错误、保存失败、大结果及媒体结果不缓存，原始类型和私有元数据保留在原有归档边界。

成功只读调用只有业务参数与公开观察都重复时才累计无进展轮次，随机结果引用和审批理由不制造新进展。同轮重复调用仅计一轮；新参数/分页、新内容、错误以及写入或未知操作重置连续计数。连续两轮无进展给模型一次改换策略的提示，第三轮后提供一次无工具的总结机会；模型仍要求调用工具时，在执行前以 `TOOL_RUN_NO_PROGRESS` 中断并保留已有回执。运行提示只进入请求上下文，正式用户消息和配对历史不被改写。该规则不代替语义验收，不据此宣称所有相似查询都会被识别或真实模型总能及时停止。

2026-10-04 上一轮运行效率验收：完整后端 **485/485** 通过，桌面聊天流客户端检查通过，桌面构建零警告零错误。三协议实际本机模拟 HTTP 验证四次重复读取后第五次模型请求进入无工具总结；模型拒绝总结时不执行第五次工具，已完成回执和原生配对历史仍保留。覆盖缓存独立引用、每次审批、配置/连接/工作范围变化、取消、文件重新读取、分页、TTL/容量及诊断日志重读。重新启动后 **55** 个打包网关模块与源码哈希一致。测试未调用付费模型，未以模拟上游声称固定真实提速幅度；

切换到其他聊天不取消正在生成的请求。关闭应用、网关中断或显式停止会保留已知结果；当前不提供后台子代理调度，也不自动重放中断任务或恢复供应商私有续接。进度记录用于准确说明已经执行到哪里，不能把它当作能安全重复副作用的恢复检查点。

工具发现与发送给模型的目录分开：单个 MCP 服务仍最多发现 128 个远端工具，资源能力最多额外增加 3 个本机包装；每次模型请求统一最多声明 96 个，并受模型上下文中的 schema 预算约束。schema 预算不超过 24000 tokens、实际输入预算的 40%，并扣除当前消息、工具系统提示和安全预留。`tool.search/load/result.read` 优先，明确的桌面任务再优先选择本机工具，普通代码/网页请求默认不带桌面 schema。模型用 `tool.search` 查询，`tool.load` 保留发现工具及所请求的定义，在同一预算内替换普通核心或远端工具；加载失败保留原目录。禁用工具不会进入搜索或模型声明，也不能通过直接调用重新启用。若初始工具定义无法装入模型预算，保留原本有效的文本聊天，不因发现数量过多而失败。

每轮输入同时估算工具 schema 与原生续接字段。接近预算先缩小旧结果，再压缩较早完整历史轮次；最新结果过大时也可转换成有 `resultRef` 的较大预览，完整公开结果仍在本地，按 `tool.result.read` 分页读取。只改输出/历史正文投影，不改变工具调用 ID、匹配结果、当前请求、权限、原生签名与加密续接字段，不重新执行已完成操作。没有可靠结果引用、必要字段仍过大则返回 `TOOL_CONTEXT_BUDGET_EXCEEDED`。

`conversation.history.search({query?,offset?,limit?})` 与 `conversation.history.read({messageId,offset?,limit?})` 是免额外审批的当前聊天只读工具。查询最多返回 20 条带真实 ID 的公开消息片段；读取最多 16000 字符并返回下一页位置。仅当前有效聊天，读取前后检查归属与归档；不读取兄弟聊天、Reasoning、ToolActivities、连接配置或回收站，不绕过正式记录所有者。`extractive-v2` 的原文导航可通过它回源，不成为新的权限或确认记忆。

`GET /health` 的 `agentProtocol` 为 `5`、`toolStreamProtocol` 为 `3`，`officialToolsProtocol:2` 表示官方/用户分层及本机工具和无挂载工作区接线，`extensionStorageProtocol:1` 和 `extensionRoot` 表示扩展存储接口与当前实际目录。桌面拒绝复用缺少当前接口的旧版工具网关，防止它丢弃字段或仍然写入旧目录；升级应在当前回复结束后关闭旧网关再启动。本机助手的实际可用性还须经过能力探测，health 版本不等于当前桌面已解锁或工具执行成功。

## 范围与权限

| 模式 | 自动执行 | 需要批准 |
|---|---|---|
| 请求批准 | 当前挂载或本聊天独立目录内读取、查询；已发现技能的按需读取 | 写入、修改、删除、终端、范围外访问、MCP、本机桌面操作 |
| 帮我批准 | 上述读取、范围内可逆修改、已验证的 AppContainer 命令 | 删除、范围外访问、MCP、本机桌面操作 |
| 完全访问权限 | 系统权限允许的文件操作及已启用工具 | 不弹逐次批准；范围外文件、MCP、本机桌面操作仍须说明原因 |

默认使用挂载目录。完全访问不能提升为管理员，也不能关闭终端沙箱。文件工具不读取模型连接密钥、Agent 连接配置或其备份，不直接改写正式聊天、记忆、索引和应用配置。其他正式 Data 文本可在说明原因后只读访问；Ask/Smart 需要批准。应用创建的 `Data/Desktop/Projects/<所属项目ID>` 是用户工作文件夹，在正式归属和精确目录匹配时允许正常工作文件操作；任意挂载 `Data/Chats` 等目录不会获得相同例外。

审批绑定聊天、请求、工具调用和单次令牌。参数在等待前冻结，批准不能替换路径或命令。拒绝、超时、关闭和取消都不执行该操作。批准后再次核对工作归属及配置版本；工作迁移或配置刷新使旧工具快照失效。MCP 自报的 `readOnlyHint` 等注解不改变审批策略。

第三方 MCP 的业务参数与本机审批字段分开。模型输入为 `{arguments: 原工具参数, policy: {reason: 调用理由}}`，审批窗口显示完整包装，SDK 只将 `arguments` 发给服务。第三方自己的数字、可选或字符串 `reason` 不会被覆盖。基础文件工具仍沿用已有参数格式。

## 文件与终端工具

`filesystem.list/read/search/stat/write/edit/delete/mkdir` 支持目录、UTF-8 文本和 SHA-256 版本检查。创建文件要求 `expectedHash: null`；替换、编辑和删除文件要求刚读取的准确 hash。编辑只替换唯一匹配，写入使用临时文件与原子替换。删除仅支持单个文件或空目录，不支持递归删除。拒绝链接、设备、网络共享、歧义路径和文件替代流；查询、结果、文件大小和遍历都有上限。

`terminal.run` 由独立 .NET `KYNXA.ToolHost` 创建 Windows AppContainer，使用 Job Object 管理最多 16 个进程、单进程 256 MiB/总计 768 MiB 内存上限、时间及输出限制。命令在临时工作副本中运行，没有网络能力，副本不会自动写回真实工作。正式 Data、模型凭据、链接和常见敏感/庞大目录不进入快照；精确匹配的应用管理工作目录仍能正常复制。完成后清理请求副本。

第一版支持 Node.js 及受限 `cmd` 内置命令。Node 脚本可使用内置模块，测试建议 `node --test --test-isolation=none`；默认子进程测试模式可能受沙箱限制。cmd 参数为 `['/d','/c','一条命令']`，已验证 `echo`、`type` 和副本内重定向；`dir` 的卷根探测可能被系统拒绝，目录和内容搜索请用文件工具。PowerShell、Python、任意可执行文件、调试端口、提权和安装流程尚不支持。启动或验证沙箱失败时返回错误，没有宿主执行回退。

AppContainer 是终端的系统隔离边界，Job Object 负责资源和生命周期。文件 CRUD 是网关的权限代理操作。用户启用的 MCP 服务程序是受信任的外部依赖，会以当前用户身份启动，**不在这个终端沙箱中**；不要把 MCP 调用审批描述成对服务程序本身的系统隔离。

## 无关联文件夹的聊天与本机控制

`terminal.host.run` 是独立的本机执行通道，参数为 `shell`（cmd/powershell）、`script`、可选 `cwd`/`timeoutMs` 和必需的 `reason`。缺省 cwd 为当前有效工作目录，无挂载聊天同样可用。它继承当前用户环境，可调用用户已安装的 conda 等程序；缺少程序时明确报命令错误。Ask/Smart 始终单次批准，Full 不弹批准；原沙箱失败不会转到此通道。审批展示命令、终端、目录和理由。

默认执行在后台，正式右侧栏不渲染终端或创建终端标签。原生 stdout/stderr 按既有编码策略分行解码，短时间突发合并为有序增量；SSE `terminal_output.terminal` 带工具调用身份、递增 sequence、stream、text 和 replace。桌面保留当前调用身份与增量顺序的协议校验，不把输出混入聊天正文。最终正式输出与退出状态仍由工具回执拥有，聊天沿用简洁工具活动及当前任务的取消操作，不宣称实现了持久交互 shell。

右侧内容区当前只展示截图标签，一次启用一个查看器；新截图加入后台标签，不改变已有选择，没有打开内容时才自动显示。关闭标签只隐藏图片，打开列表可恢复，也不强制展开被用户收起的右侧栏。标签与图片随右侧宽度调整，标签栏支持横向滚动。关闭图片不取消后台任务，也不删除正式工具结果；附件查看器尚未接入。

需要看见终端窗口时使用同一工具的 `visible:true`，网关发送独立原生操作 `host_terminal_visible`；即使助手在能力查询之后被换成旧版，旧版也会拒绝未知操作，不会忽略字段而后台执行。默认仍是后台管道捕获；可见模式由随包 ToolHost 伴随进程创建独立控制台，CMD/PowerShell 继承真实控制台输入输出。原生握手验证可见窗口和控制台句柄后才运行命令，窗口代理句柄不能当作可见证据。伴随进程通过当前用户私有 named pipe 传回回执，父进程核对连接者 PID；解释器及子进程仍受同一个 Job 管理。无需额外 Python，不开放通用桌面启动通道来代替终端执行。

可见回执的 `consoleText` 是有界的控制台屏幕预览，`outputCapture:console-screen`、`transcriptComplete:false`、`streamsSeparated:false`；stdout/stderr 为空，不宣称获得精确全量流。运行中保留最近预览，取消及超时后继续保留已有信息。`keepOpenMs` 只用于可见模式，默认命令结束后保留 5 秒，范围 0–30 秒且包含在总超时中。命令已结束后取消窗口停留，仍保留真实退出码；命令未结束而窗口关闭/取消，结果为 `unknown`，不能自动重放。结束清理进程树，当前没有无限 `/k` 会话或后续输入工具。用户明确要求保持后台时不允许打开可见终端；默认后台模式不变。健康协议版本 3 保留实时输出事件的兼容判定，当前界面不将它显示到右侧；原生能力版本 2 单独声明 `visibleTerminal`，旧助手不支持时在执行前报错。

本机终端固定系统 CMD/Windows PowerShell，使用 Job 控制最多 32 个进程并在结束时清理子树；网关同时最多启动 8 个助手。脚本最多 16,384 字符，超时 100–120,000 ms，原始 stdout/stderr 合计最多 256 KiB。正常非零退出表示命令结束但执行失败；取消、超时、超限及执行后缺失/无效回执保留 `unknown` 和已返回输出，循环先保存回执再中断后续调用，不能声称回滚或自动重做。Windows 短路径先规范化，防止正常执行被误判为目录身份不符。CMD 分行识别 UTF-8/OEM，同一行混合编码不保证无损；详见 `tests/native-host-terminal-smoke/README.md`。

旧版精确匹配所属项目的 `Data/Desktop/Projects/<项目ID>` 若带有不安全链接祖先，当前聊天改用真实独立工作区。catalog 路径与旧文件保持原样，提示模型旧内容未迁移；外部挂载链接仍被拒绝。批准后再次检查正式路径和有效工作区，链接状态改变也会撤销旧审批。

没有关联文件夹的工作聊天和普通聊天，首次准备工具请求时懒创建 `<模型数据目录>/Workspaces/<聊天ID>/`。标准布局为 `Data/Models/Workspaces/<聊天ID>/`，旧字母 ID 使用确定的 GUID 目录映射，正式 ID 不变。`.workspace-owner.json` 校验真实归属，文件工具不能读取或修改它；目录及父目录链接、身份冲突和改挂载后的过期审批会被拒绝。相同聊天重开沿用生成文件，不改 catalog 的关联文件夹或共享记忆关系。聊天移到另一工作后目录仍以稳定聊天 ID 保存；聊天删除暂不自动删除这些生成文件，避免把项目文件当聊天日志清理。

`ConversationWorkspaces` 管理这部分生成文件；正式 JSONL、Memory 和连接配置由原服务管理。Data 迁移复制并校验整个 Models 树，带走目录和归属记录；用户工具路径迁移不移动这些文件。终端仍只对授权目录做 AppContainer 快照，没有网络和自动回写，正式代码修改使用带哈希的文件工具。自定义旧模型根通过网关固定的 `conversationWorkspaceHome` 角色校验；模型参数无法改变该根，不能把任意 Data/Extensions 子树声明成可执行目录。

本机控制复用随包的 `KYNXA.ToolHost` 原生 Windows 窗口、UI Automation、PNG 和 SendInput 实现，**无需 Python**。桌面打包将助手及自包含 .NET/WindowsDesktop 依赖纳入 `ToolHost/`，Node 网关同样随包提供。第三方 MCP 可能依赖 Python、uv、账号或浏览器运行时；这些依赖只属于对应扩展，默认启用但可能未就绪的 Windows Screenshot MCP 不是内置截图的执行依赖。

桌面构建从 `apps/desktop/Build/node-runtime.json` 读取分平台固定版本与官方 SHA-256，校验完整 ZIP 及其中的 Node/npm/npx 文件后放入安装包 `runtime/`。x64、ARM64 使用 Node 24.14.0，仍保留的 x86 目标使用支持该架构的 Node 22.22.0；版本不在构建文件里另设一份。首次构建需下载，后续可复用校验后的 `obj` 缓存，运行用户不需自行安装 Node。网关启动优先使用随包 `node.exe` 并将该运行目录加入子进程 PATH，随包文件缺失时拒绝静默换用其它 Node。

原始 Node/npm 许可证随运行时保留。ToolHost 的 .NET 与 WindowsDesktop 原始许可证和可用第三方声明从本次 restore 实际选择的 runtime pack 复制至 `ToolHost/licenses/`；其 PDB 不进入桌面工具载荷。构建还按明确清单复制官方扩展及网关依赖，不复制用户 Data、扩展配置或密钥。`tests/node-runtime-smoke/run.ps1 -BundleRoot <新构建输出>` 在带空格的临时目录复制真实载荷、清空 PATH、指定不存在的 DOTNET_ROOT，验证内置 Node/npm/npx、原生助手及隔离网关。

| 工具 | 当前能力 |
|---|---|
| `computer.apps`、`computer.windows` | 从系统应用入口发现可启动软件，列出可见窗口、窗口 ID、PID、程序路径及客户区尺寸；不递归扫描磁盘 |
| `computer.launch`、`computer.activate` | 打开明确的本地 GUI exe，或切换到已识别窗口；不接受命令解释器、控制台脚本或提权回退 |
| `computer.read` | 按目标 HWND/PID 有界读取可访问的窗口文字、元素坐标和页面地址；密码控件跳过 |
| `computer.screenshot` | 仅截图目标窗口客户区，最多 4 MiB PNG/16M 像素；不回退截取整桌面 |
| `computer.move/click/scroll/drag/type/key` | 前台目标客户区内鼠标与受限键盘输入，中英文文本不经过剪贴板；坐标是客户区物理像素 |

本机操作在 **宿主桌面** 执行，与 AppContainer 终端分开。Ask/Smart 需一次性审批，Full 不弹逐次批准但仍校验理由、配置、聊天范围、目标窗口与进程。输入前和每批操作重新核对前台、可见范围和 UIPI；锁屏、焦点丢失、高完整性进程或 Windows 拒绝激活时返回错误，不绕过系统限制。输入事件已发送仅证明发送完成，模型须再次读取验证应用结果；部分输入或无法确认的取消保存为 unknown，不当作成功或自动重放。

截图以 typed image 保存到本聊天 `tool-results/`；模型请求只带元信息和引用，现阶段文本模型不会获得图片视觉能力。右侧截图小模块显示缩略图，点击图片复用已有本机结果预览；聊天中没有截图按钮、base64 或参数 JSON。桌面审批显示动作、目标与实际启动参数，输入文本只显示字数。成功后的过程仍从正文收起，正式记录继续保留。电脑工具声明按桌面意图或 `tool.search/load` 加载，普通代码/网页问题不默认占用额外 schema 预算。

`computer.read` 适用于浏览器辅助功能暴露的当前可见内容，不能宣称读取完整 DOM、后台标签或所有浏览器内容。仓库 Playwright/Chrome DevTools 官方预设默认 `headless + isolated`，不连接用户日常浏览器；需要完整网页能力时使用该 MCP，接已有会话须显式配置其官方连接方式。实现参考 [Windows-MCP 的能力划分](https://github.com/CursorTouch/Windows-MCP)、[Microsoft UI Automation](https://learn.microsoft.com/en-us/dotnet/framework/ui-automation/ui-automation-overview)与 [SendInput/UIPI](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)；未引入其 Python 服务作为必需运行时。

右栏 `ConversationScreenshotsPanel` 从当前聊天正式完成的截图回执投影查看器，不建立第二套截图记录。通过顶部标签选择图片，新截图不切走当前选中内容，点击图片放大。仅在侧栏可见且图片被选中时读取归档；切换聊天、折叠及关闭标签时取消请求并丢弃过期结果。预览按显示尺寸和 DPI 有界升级，最多缓存三张及 16M 解码像素，正文增量不会反复加载图片。已有截图的普通聊天也可打开右栏，沿用用户侧栏开关和窄窗口隐藏规则；重开聊天仍从正式记录恢复。

2026-10-04 本机工具验收：完整网关 **531/531**，后续相关工具与官方资源 **20/20** 通过。原生助手 **26**、输入扩展 **7**、部分输入回执及释放 **24**、真实 Edge/Chrome 本机隔离页面 **29** 项通过。右栏独立 WinUI **49** 项覆盖迟到结果、同步开栏不重复读取、多图缓存、文本增量、错误重读、窄栏和语言切换；主聊天 DOM **74**、原生 Transcript **21**、共享审批/图片查看器 **27**、语言 **2340** 项通过。x64 桌面最终构建零警告、零错误，真实新构建载荷复制后在空 PATH、无有效 DOTNET_ROOT 环境中通过 **27** 项内置运行时、原生助手和隔离网关检查。全部使用临时数据和模拟模型；不代表发布安装包、ARM64 设备或付费模型已验收，改动未提交。

## MCP 与 Skill

使用官方 TypeScript SDK 2.3.0 的 stdio 与 Streamable HTTP 客户端，默认固定 `2025-11-25` 兼容协议；明确配置为 `2026-07-28` 时使用新版发现与协商。stdio 支持用户配置 `cwd`、非敏感常量 `env` 和秘密环境变量引用 `envRefs`；HTTP 支持 `url`、`headerEnv`、Bearer 环境变量认证及 OAuth client credentials。凭据在连接时解析，不进入模型或诊断；HTTP 只允许 HTTPS 或本机 loopback HTTP，拒绝 URL 内嵌凭据和重定向。OAuth 绑定明示 issuer，令牌只在当前客户端内存保存；浏览器交互式 OAuth 登录尚未实现。

只运行当前有效配置中启用且通过连接前检查的程序和参数，用户明确关闭的官方项不会被默认值重新打开。打开窗口和普通列表查询不会启动服务；带工具权限的聊天准备目录时，或用户连接、刷新时才连接，缺启动程序、认证变量或必填路径时显示未就绪。配置禁用、删除、刷新和服务关闭会结束对应客户端。Windows stdio 清理限定 SDK 自有 PID，结束整个子进程树，避免 npx 超时后留下占用目录的进程。`startupTimeoutMs` 默认 15 秒、可配置 1–120 秒，Playwright 预设使用 60 秒。服务工具目录通知会刷新发现目录；执行前核对原始工具 schema 与操作，变化则拒绝旧调用并要求重新准备。模型本轮目录保持快照，新工具进入下一次消息准备的目录。断线更新状态并移除过期工具，用户可断开、重连；下次有明确连接需求也可新建连接，不重放上次工具调用。连接、认证与目录刷新失败只显示服务 ID 和安全错误码，其他工具仍保留。

单工具开关使用服务的原始工具名保存在 `disabledTools`，不改服务自身配置。窗口保存同样检查 revision；冲突保留当前编辑。管理目录展示禁用项，执行目录排除禁用项。声明 resources 能力的服务额外暴露 `mcp.<serverId>.kynxa_resources_list`、`mcp.<serverId>.kynxa_resources_templates_list`、`mcp.<serverId>.kynxa_resources_read` 包装，资源 URI 只转发指定服务，不在宿主自行 fetch。包装遵循相同的外部调用理由、审批、结果保存和工具启停规则；与服务同名工具冲突时保留原工具。

官方预设目录来自网关单一来源。Playwright 使用固定版本 `@playwright/mcp@0.0.83`、headless 和 isolated 模式；GitHub 使用官方 hosted MCP，Authorization 通过环境变量引用提供。官方预设默认启用；添加或读取配置本身不启动进程、不访问服务，再次添加复用现有配置与工具开关。缺启动程序、认证变量或必填路径的预设显示未就绪；其他运行环境按预设要求准备。已有文件操作、聊天/工作记忆和终端能力继续使用原服务，没有安装功能重叠的 filesystem/memory MCP。第三方服务器由其发行方式安装或按固定版本启动，浏览器运行时和用户凭据不会自动下载、填写或复制。

应用技能来自打包的基础技能、`Extensions/Skills`（未单独配置时为 `Data/Skills`）、用户配置的技能目录，以及当前工作 `.kynxa/skills`。先提供有限的名字和用途，通过 `skill.list/read` 按需读取 SKILL.md；`skill.resource.read` 从所选技能根目录解析资源路径，支持 UTF-8 分页和二进制元数据，拒绝越界、链接、硬链接及敏感文件。`skill.inspect/check` 展示资源清单、格式、运行器和依赖诊断，检查不会执行宿主命令或安装包。来源顺序和同名冲突明确；规范化后的实际路径去重，Windows 忽略大小写并合并长短路径别名，保留优先来源 ID。`disabledSkills` 禁用项不进入模型目录，也不能绕过开关读取、运行。

内置 `workspace-inspect` 与 `safe-file-edit` 引导读取、定位和带 hash 的精确修改；`browser-workflow` 指导使用已有浏览器 MCP，`desktop-workflow`、`terminal-workflow` 与 `web-research` 分别指导本机窗口、执行边界和资料查证。复用的 Apache-2.0 `internal-comms` 保留上游许可证、固定 commit、来源和修改说明，并通过现有资源工具读取例子。技能文本属于参考资料，不授予权限。仓库 `.agents/skills/kynxa-development` 是开发本项目时使用的编码技能，属于另一个用途。

SKILL.md frontmatter 使用固定版本 `yaml` 的 YAML 1.2 Core 解析器，支持引号、行尾注释、多行描述和嵌套元信息。已有宽松格式保留可读性并显示诊断；新增包严格检查标准的 name/description/compatibility 上限、名字与父目录、metadata 和 allowed-tools 类型。`allowed-tools` 仅作描述，不扩大权限。拒绝别名、重复键、自定义标签、合并键与危险对象键，限制头部大小、深度及节点数量。坏技能隔离并保留原文件，不阻断其他技能或基础工具。

用户选择目录导入技能，服务严格验证包并复制完整白名单到原子 staging，重新核验 hash 后才提交到当前扩展根的 `Skills/<name>`。同内容重复导入复用已存在的包；同名不同内容拒绝覆盖。导入不会执行技能、自动授权或清除禁用状态。

`skill.run` 只执行用户已发现、启用技能中的指定 Node 脚本。审批前固定最多 128 文件/8 MiB 的 SHA-256 manifest；原生 helper 只复制该清单并复核 hash，变化即拒绝执行。技能包副本 ACL 只读，工作副本可写，无网络、不自动写回宿主。请求结束清理副本；无真实 AppContainer 则拒绝，不回退宿主。未知兼容性或未验证依赖需要先处理；Python、Bash、PowerShell 技能脚本未支持并明确返回阻塞原因。

技能资源入口同样保护正式 Data 和私有工具结果，不能把包含 Data 的祖先目录注册为技能后绕过保密边界。原生 helper 明示 `skillExecution` 能力，复制时逐文件核验清单，结果包含选定脚本 hash、文件数和只读/hash 校验声明；旧 protocol-1 终端 helper 缺少该能力时明确拒绝技能运行，普通终端兼容不受影响。

## 使用入口与代码归属

工具管理窗口只保留 MCP、技能两页。MCP 页直接提供名称、连接方式、程序或地址、启停与保存；服务 ID、JSON 参数、认证引用和可信进程说明在默认收起的高级设置内，单个工具开关放在可展开的工具列表。技能页直接提供导入、列表与启停，来源、内容、运行环境和外部目录按需展开。高级项只改变展示方式，已有配置、诊断和权限能力保留。

设置的“存储”区域包含两行：数据存储、用户工具。两行共用相同的灰色样式，显示当前路径和“更改位置”按钮；后者打开文件夹选择器，要求空的本机目录。更改扩展位置会暂停调用、复制并校验扩展文件、更新自有目录内的配置路径与技能禁用 ID，最后原子切换指针。原文件保留；成功后网关关闭旧工具客户端并加载新目录，后续更改位置无需重启应用。

扩展位置由 `KYNXA_EXTENSION_HOME`、用户 `~/.kynxa/extensions.json` 的 `extensionRoot`、原正式 Data 根依次决定。未单独配置时保留已有 `Data/Agent` 和 `Data/Skills`，避免升级后旧配置消失。显式指定 Data/Model 环境变量且未指定扩展位置时使用对应 Data 根；测试可通过 `KYNXA_EXTENSION_POINTER` 指定隔离指针。环境控制路径的界面不覆盖启动配置。已单独配置扩展根时，更改 Data 不再移动这一套扩展；仍使用旧 Data 默认根时，Data 迁移同时处理 Agent、Skills 与 MCP 缓存。

扩展目录保存 MCP 连接配置、导入技能、npm 下载缓存、浏览器缓存及 uv/Python 运行环境；默认 stdio 缓存环境变量指向该目录，用户显式环境映射优先。内置技能和核心工具随应用发布，用户指定的外部技能目录或第三方可执行程序保持原位置。远程 MCP 本身运行在其服务端；改变本机目录不会迁移远端服务。聊天、记忆和完整工具结果继续属于 Data。

在设置中打开“工具与技能”，默认列表已有启用的官方 MCP 预设；先补齐显示未就绪项目需要的环境变量引用、运行时或配置，再点击连接。用户可关闭任意预设，已有明确关闭的项需要用户重新打开；添加操作用于恢复被隐藏的预设。GitHub 预设的 `KYNXA_GITHUB_AUTHORIZATION` 值是完整的 `Bearer <token>`；直接选择 Bearer 认证时，`tokenEnv` 引用的变量只放 token。变量应在启动 KYNXA 前设置，已运行的网关不会自动取得另一个终端中新设置的变量。Playwright 首次连接需要可用的 Node/npm 和浏览器运行时；添加预设不会提前安装浏览器或启动服务。

技能页可选择包含 SKILL.md 的文件夹导入，查看格式、来源、同名冲突、资源和运行环境，并切换启停。标准包目录见下方 Skills；导入成功仅表示文件包已校验保存，脚本执行仍检查依赖和当前聊天权限。内置浏览器工作流使用已有的 Playwright 工具，沟通写作技能使用已有的文件和资源工具。

| 文件 | 职责 |
|---|---|
| `apps/shared/Tools/AgentApiContracts.cs`、`apps/desktop/Services/Tools/AgentApiClient.cs` | 桌面和网关共享合同、请求与错误处理 |
| `apps/desktop/ToolManagementWindow.xaml(.cs)` | 配置、诊断和启停界面 |
| `apps/desktop/Controls/StorageLocationRow.cs`、`apps/desktop/Services/Data/ExtensionPaths.cs`、`apps/desktop/Services/Data/ExtensionStorageMigrationService.cs` | 共用存储行、扩展路径解析、校验迁移与指针提交 |
| `apps/model-gateway/orchestration/agent-http-routes.mjs` | 工具管理 HTTP 接线，调用 Tools 业务服务，不拥有模型授权 |
| `extension-storage.mjs` | 扩展根解析；网关在维护完成后与 Data 根一起切换运行时 |
| `agent-config.mjs`、`agent-config-layers.mjs`、`mcp-config.mjs`、`mcp-presets.mjs` | 用户配置持久化、官方差量合并、连接校验与预设去重 |
| `official-tools.mjs`、`official-tools/manifest.json`、`official-tools/Tools/catalog.mjs`、`official-tools/MCP/catalog.mjs` | 安装包定位、官方清单、稳定技能身份及单一工具/预设定义 |
| `mcp-client.mjs`、`mcp-transport.mjs` | SDK 发现/调用/资源包装、认证、连接与进程生命周期 |
| `skill-service.mjs`、`skill-frontmatter.mjs`、`skill-resources.mjs`、`skill-package.mjs` | 发现与开关、YAML 格式、包内资源、导入和脚本清单 |
| `tool-service.mjs`、`tool-policy.mjs`、`tool-catalog.mjs`、`tool-result-store.mjs` | 统一调用协调、权限与审批、模型目录预算、完整结果保存 |
| `tool-storage-boundary.mjs`、`tool-system-prompt.mjs`、`desktop-launch-options.mjs` | 存储身份与保护策略、有界能力提示、审批前可观察的后台启动参数 |
| `tool-loop.mjs`、`tool-run.mjs`、`tool-observations.mjs` | 模型与工具循环、受限耗时诊断、请求内观察复用及无进展控制 |
| `sandbox-runner.mjs`、`sandbox-skill.mjs`、`apps/tool-host/Sandbox/SkillSnapshot.cs` | 原生沙箱通信、统一能力/回执判定、哈希核验及只读技能快照 |
| `sandbox-workspaces.mjs`、`tool-host-path.mjs`、`desktop-runner.mjs`、`official-tools/Tools/computer.mjs` | 独立聊天工具目录、原生助手定位、本机协议与回执、桌面工具声明 |
| `apps/tool-host/Desktop*.cs`、`apps/desktop/Services/Presentation/ComputerToolPresentation.cs` | 原生窗口读取/输入与程序启动、无参数 JSON 的动作与审批展示 |
| `apps/desktop/Controls/ConversationScreenshotsPanel.cs`、`apps/desktop/Services/Presentation/ConversationScreenshot*.cs`、`apps/desktop/Services/Presentation/ToolResultImageDecoder.cs` | 当前聊天截图投影、可取消的按需读取、有界图片解码与右栏缩略图 |

上述短文件名均位于 `apps/model-gateway`。技能文本不新增运行器；第三方 MCP 通过原客户端连接，正式聊天和记忆仍由现有服务保存。

代码职责和本轮生命周期修缮见 [代码组织](code-organization.md)。GUI 默认后台启动，原生助手观察自身窗口并放到当前前台窗口后面，窗口保持未最小化。独立可见终端也采用不激活启动，仍有真实控制台和正式结果，不进入右侧栏。窗口读写、截图及状态调整不要求先激活；物理输入仍须明确前台和目标身份。

后台 Chrome/Edge 会在审批前显式加入 `--disable-backgrounding-occluded-windows`，与其他启动参数一并冻结、显示和保存。实际隔离浏览器验证发现，完全遮挡可能让 Chromium 暂停网页绘制并隐藏页面辅助功能树；这项开关恢复后台正文和目标截图。依据 [Chrome 启动工具参数说明](https://github.com/GoogleChrome/chrome-launcher/blob/main/docs/chrome-flags-for-tools.md) 及 [ChromeDriver 默认参数变更](https://chromium.googlesource.com/chromium/src/+/23902d9051098fbc6a3a669fc0988c6899f38b65%5E%21/)。已有浏览器进程可能忽略后续参数；不能改动其既有启动状态或用前台激活掩盖失败。观察窗口有时限，第三方软件仍可能晚到或主动抢焦点，回执中的实际位置、焦点和截图结果才是依据。

## 数据与恢复

以下 Data 树中的 Agent、Skills 是未独立配置扩展位置时的兼容布局；独立配置后使用下方 Extensions 树。

```text
Data/
├─ Agent/
│  ├─ config.json                  MCP 连接/认证引用、服务/工具/技能启停、技能目录、revision
│  └─ skill-imports/               导入临时包，成功/失败后清理
├─ Skills/
│  └─ 技能目录/                    用户应用技能及资源、脚本、许可证
│     ├─ SKILL.md
│     ├─ references/               可选
│     ├─ scripts/                  可选
│     └─ assets/                   可选
├─ Desktop/Projects/项目ID/        应用创建的用户工作文件
├─ Projects/项目ID/Sessions/聊天ID/
│  ├─ events.jsonl                 正式消息和工具活动、结果引用
│  └─ tool-results/结果ID.json      完整工具结果，首次执行后按需创建
└─ Chats/聊天ID/
   ├─ events.jsonl                 普通聊天正式记录
   └─ tool-results/结果ID.json
```

原聊天、记忆及索引结构见 [聊天与工作记忆](chat-work-memory.md)。ToolActivities 放在现有 assistant 消息里，由同一 `message.upsert` 日志保存；完整结果文件不是第二份聊天历史。执行前保存 running；工具返回后先保存结果与终态，再响应停止，后续工具和模型请求不继续。已确认完成的操作保持 completed；有可信原生取消结果时记录 cancelled，已开始但无法确认结果的中断记录 unknown。已完成请求重放回复，不再执行工具。中断或失败且已有工具记录的请求不能用同一请求 ID 自动重做，界面隐藏普通重试按钮；检查结果后发送新消息。它不是跨崩溃精确续跑或多文件事务系统。

单独配置扩展目录后，其组织如下；上面的 Data/Agent、Data/Skills 是兼容的默认位置，旧副本保留用于恢复，网关只使用当前指针指向的一套配置。

```text
Extensions/                       用户选择的扩展根
├─ Agent/
│  ├─ config.json                 MCP、技能目录和启停配置
│  └─ skill-imports/              导入临时包
├─ Skills/
│  └─ 技能名称/
│     ├─ SKILL.md
│     ├─ references/              可选
│     ├─ scripts/                 可选
│     └─ assets/                  可选
├─ MCP/
│  ├─ npm-cache/                  stdio/npm 下载缓存
│  └─ browser-cache/              Playwright 浏览器缓存
├─ Backups/Extensions/Migrations/ 扩展迁移恢复记录
└─ extension-layout.json          扩展目录框架版本
```

机器级小指针 `~/.kynxa/extensions.json` 不保存令牌或聊天。迁移锁与 Data 共用 `~/.kynxa/storage-migration.lock`，两种迁移互斥；扩展服务另用操作锁防止并发复制。扩展配置与其备份仍受凭据保护，选择独立目录不会扩大模型文件读取或终端权限。

MCP 原始结果保留全部 content 类型、structuredContent 及私有 `_meta`，按聊天归属以原子文件保存，引用带结果 ID、字节数和 SHA-256。模型和本机详情的公开投影都移除 `_meta`；模型只收到文本、结构化结果、媒体类型与资源 URI 等摘要，图片/音频不被伪装成已交给模型的多模态输入。媒体字节仅在本机显式“查看媒体与资源”时读取，支持受限图片预览；音频与不支持的类型显示元信息，URI 不自动打开或执行。普通文件工具不能绕过投影读取原始结果文件。

单个完整结果最多 8 MiB。消息里的大结果是合法 JSON 外壳，含状态、预览、总长度、截断标记和引用，不截切整个 JSON。`tool.result.read` 按每段最多 16000 字符读取当前聊天的公开结果；本机窗口分页可查看归档历史，模型工具仍拒绝归档范围。完整保存失败或超限时保留已知执行状态、提示 `TOOL_RESULT_SAVE_FAILED`，不伪造完整引用或把已完成写入说成取消。结果随聊天移动、删除到 Trash、撤销恢复及 Data 迁移一起处理；删除聊天不会因迟到保存而重建。

本机接口为 `GET /api/conversations/:chatId/tool-results/:resultId`，返回 `{result}`；带 `offset` / `limit` 时返回分页 `{id,text,totalCharacters,offset,nextOffset,truncated,resultRef}`。关闭详情或切换聊天会取消并忽略迟到结果。分页显示 JSON，可复制；媒体请求单独触发，避免在 SSE 中输送大段 base64。

网关首次使用和激活新扩展位置时，自动建立上述框架并幂等补齐缺失目录，不覆盖已有配置或技能；只读位置解析不会创建文件，迁移维护期间暂停初始化。布局版本未知、目录被替换为文件或链接时保留原文件并拒绝写入。设置迁移复制全部自有配置、技能、缓存及扩展恢复记录，核验源和目标文件哈希、目录清单，完成配置路径与技能禁用 ID 重写后才切换指针。默认共享 Data 时，只迁移 Backups/Extensions，正式聊天和模型备份仍属于 Data。外部工作目录、外部技能目录和 MCP 可执行程序保持用户配置的路径，不假定另一台电脑存在相同位置。配置损坏保留原文件并报错，revision 冲突不覆盖其他窗口的新配置。

## 本机与联网能力

MCP 是工具通信协议，不要求访问互联网。stdio 在本机父子进程的输入输出上通信；HTTP 也可以只连接 localhost。具体服务是否联网由其功能决定：本地文件、数据库等可以离线，云端接口和访问外部网站需要网络。使用本地模型与已经安装好的本地工具可组成离线流程；通过 npx 初次下载依赖通常需要网络，要固定离线运行应配置已安装程序的路径。[官方连接说明](https://ts.sdk.modelcontextprotocol.io/v2/clients/connect)

内置终端运行经验证的 AppContainer Node/cmd；独立的 `computer.*` 通道在宿主桌面上启动 GUI 软件、读取窗口、截图和输入，不受终端 AppContainer 保护。原生截图限定目标窗口客户区，未实现全桌面截屏。Playwright MCP 启用且浏览器依赖可用时能截取其控制的网页。图片可以在本机详情中预览，模型续接目前只有文本、结构化数据和媒体引用，没有图像理解输入；截图保存本身不依赖视觉模型。[Playwright 截图说明](https://playwright.dev/mcp/tools/screenshots)

## 验证入口

- 网关：`node --test tests/*.test.mjs`，从 `apps/model-gateway` 运行。记忆效果测试依据模拟模型实际收到的 system 回答；工具测试读取真实临时工作文件、续接三种协议、检查审批身份、配置冲突及重放去重。MCP 测试启动官方 SDK 的本地测试服务并真实发现/调用两个协议版本，另验证模型声明、SDK 调用、结果续接的完整链路。迁移清理失败不会产生未处理拒绝，health 以固定错误码报告失败。
- 客户端：`tests/agent-client-smoke`、`tests/chat-stream-smoke`、`tests/storage-migration-smoke`、`tests/gateway-startup-smoke`。
- 界面：`tests/agent-ui-smoke` 的独立原生窗口与 `tests/agent-transcript-smoke` 的独立浏览器 DOM 验收；都不使用实际用户数据。
- 系统沙箱：`tests/sandbox-smoke` 实际检查副本读写、外部文件拒绝、loopback 网络拒绝、父子进程取消、输出和内存限额。不是仅检查函数返回的 sandbox 字段。
- 本机控制：`tests/native-desktop-smoke` 使用自建窗口和临时浏览器配置，实际检查窗口/PID、PNG、UIA、前台输入和按键清理；不读取日常窗口或浏览器资料。`sandbox-workspaces.test.mjs` 验证每聊天目录、真实 Data 迁移，以及标准/自定义模型根的实际 AppContainer 隔离。

这些验证不保证所有模型会正确选择工具，也不代表所有系统版本、DPI 或终端命令已经兼容。

2026-10-03 框架初始化基线网关完整回归 294/294 通过，覆盖工具预算、完整结果、取消后记录、MCP 连接与目录、标准技能及保密边界，并验证首次启动、迁移维护、未来版本、布局修复、原子版本发布和路径切换。C# 扩展迁移 96 项、原生窗口及存储 UI 129 项、语言 1994 项通过；Data 迁移检查包含源晚到修改、目标空目录丢失和初始化期间篡改凭据。桌面构建零警告、零错误，39 个输出模块哈希与源码相同，7 个关键模块独立导入成功。聊天流式、网关启动与拒绝旧协议、模型上下文、正式聊天客户端检查通过。测试使用独立临时数据和模拟模型，不调用用户付费接口。

随后上下文与输出更新完整回归 336/336，通过三协议请求压缩与实际 HTTP 大结果续接、工具配对/私有原生字段保留、首个大结果回源、260 条正式历史检索、归属/归档竞态与删除拒绝、硬必要预算无副作用、截断工具参数不执行和取消回执。技能提示仍保持 12 条标题及 2500 token 测试边界；不为新回源工具放宽旧预算。新模型配置原生窗口 53 项和语言 2019 项通过，桌面构建零警告零错误。

此前自包含 ToolHost 已通过实际 Windows AppContainer 技能只读/hash 校验，工具展示 DOM 20 项亦有通过记录，本次未更改其脚本。官方 Playwright MCP 在实际迁移的新缓存根连接发现 25 个工具，校验复制的 194 个包文件；GitHub hosted 服务未使用用户凭据连接。本次给现有 D 盘扩展目录补齐框架时再次校验全部 199 个已有文件哈希及两个存储指针，原文件、正式 Data 和运行中的网关均保持不变。

实现依据：[OpenAI 函数调用](https://developers.openai.com/api/docs/guides/function-calling)、[Claude 工具结果](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls)、[MCP SDK v2](https://ts.sdk.modelcontextprotocol.io/v2/)、[MCP 协议版本](https://ts.sdk.modelcontextprotocol.io/v2/protocol-versions)、[Agent Skills 格式](https://agentskills.io/specification)、[yaml 解析器](https://github.com/eemeli/yaml)、[Microsoft AppContainer](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer)、[Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)。复用来源：[Playwright MCP](https://github.com/microsoft/playwright-mcp)、[GitHub MCP](https://github.com/github/github-mcp-server/blob/main/docs/remote-server.md)、[沟通写作技能固定版本](https://github.com/anthropics/skills/tree/8a1541c4a3ffa5a20a5a91de0dcf3f0bab1d1ef4/skills/internal-comms)。

## 公开能力的直接复用

### 有序消息与检索效率（2026-10-03）

`assistant-segments.mjs` 为每轮模型调用建立稳定消息段，`toolStreamProtocol:3` 传输段开始、增量身份与完成快照。工具调用、审批和结果使用固定 `round` / `order`，并发结果可乱序完成。正式消息的 `AssistantSegments` 保存全部公开阶段，`Content` 只保存最终正文，后续模型配对历史由独立内部投影负责。主聊天运行时紧凑展示近期阶段与活动，整轮成功且有有效最终正文后只显示最终结果与真实总用时；思考、工具与中间说话从显示移除，不删除正式记录。旧 v2 日志沿用无边界 `Content`，不猜测正文中的阶段分隔。单轮内公开思考与正文仍分别聚合，不声称能重建缺少原始块边界的任意交错流。

显示策略由桌面的 `TranscriptPresentation` 和浏览器 `message-presentation.js` 协作：保留全部已有正文阶段，与最终正文使用同一排版；可见工具总计最近八个，待批准动作及其阶段额外保留。同类相邻活动合并，网页活动保留直接 URL，过程不堆叠独立折叠卡。成功收束依赖消息 `completed` 与有效 `final_answer`，不能仅依赖某一模型轮次完成；最终正文与公式先渲染，再移除过程。失败、中断、截断及缺少有效最终答案时保留已有正文和明确状态。正式 `DurationMs` 包含整轮模型生成和工具执行，HTTP/SSE 的 `durationMs` 与重放结果一致；旧记录仅在已有可靠终态时间时投影兼容，不按当前时钟推算。复制、历史重开、语言切换、选区冻结与滚动锚点按同一可见投影处理，独立轨迹界面尚未实现。

工具动作、状态、命令采用 12px 灰字。桌面 `tool-presentation.js` 将真实活动投影为搜索资料、读取网页、读取文件、修改文件、运行命令、使用技能或执行操作；不在聊天里显示技术工具名、参数/结果 JSON、调用 ID、哈希、审批或沙箱元信息。普通项展开仅含必要意图、实际命令/路径和可读错误。网站单项直接列出可点击网址；`tool-web-links.js` 只处理明确的网站 MCP 工具，使用公开搜索 URL、Fetch 请求地址、浏览器最终/当前页面字段，过滤私有元信息、审批理由和无关后台页。网页正文里随意出现的网址不自动成为来源。审批和独立结果查看器代码仍保留，聊天中不再提供复杂结果入口。正式结果与执行记录的保存不受该展示投影影响。

中文和英文事实查询软优先已有多结果搜索、Fetch，代码文档查询优先已有文档工具，原 schema/token 预算与按需加载不变。`tool-scheduling.mjs` 只将确定的内置读取和指定无状态搜索、HTTP(S) Fetch 标为可并行，最多四个一批；浏览器、写入、终端、技能和未知 MCP 仍串行。每个调用仍独立校验权限与审批；开始按分配顺序发布，完成回执即时保存，每批全部结算后才处理取消或失败。提示词要求简短进展、最终只答当前问题、可靠证据充分即停，并按任务复杂度决定细节；256K 是输出上限，不是目标长度。是否遵守停止指导须用实际模型验收，模拟测试不证明固定提速倍数。

普通 MCP 目录刷新复用进程，相同配置保存不撤销请求。纯技能设置变化撤销旧请求和审批，但保持浏览器；真实 MCP 配置修改和显式断开/重连仍保守全局失效。循环遇到 `AGENT_CONFIG_CHANGED` / `MCP_CATALOG_CHANGED` 保存失败回执后停止，不继续调用旧目录。SDK 列表和资源显式读取使用 `cacheMode:'refresh'`。

Chrome DevTools 1.10.1 可能仅以 `Unable to navigate…` 文本报告已捕获的导航错误；网关对 `navigate_page` 明确失败行标记 `MCP_BROWSER_NAVIGATION_FAILED`，原始结果仍完整保留。`evaluate_script` 的稳定等待发生在脚本执行后；浏览器技能要求读取前确认 URL、标题与页面就绪。独立动态页实测两个查询的 URL、正文和哈希均变化，未复用旧浏览器结果；线上 Bing 重复正文的具体原因仍未确定。验证码和无关页面不算查证成功。

采用以下公开方法，适配到现有存储和审批，不另装 Agent 核心：

| 来源 | 采用的原则 |
|---|---|
| [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) | 核对本机 turn/step 与过程组，助手回答和工具分开 |
| [Codex 消息协议](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/items.rs) | commentary 与 final_answer 分离；缺少原生 phase 时由循环做展示投影 |
| [pi 界面](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/modes/interactive/interactive-mode.ts) | 每轮助手独立块，工具按 call ID 更新；模型回合结束与任务结束分离 |
| [OpenCode processor](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/processor.ts) | 稳定 Part 身份、文字与工具分别更新 |
| [Kimi CLI](https://github.com/MoonshotAI/kimi-cli/blob/main/src/kimi_cli/ui/shell/visualize/_live_view.py) | 思考、正文、工具切换时结束前一块 |
| [OpenClaw agent loop](https://github.com/openclaw/openclaw/blob/main/docs/concepts/agent-loop.md) | 助手、工具和运行生命周期独立；循环检测不代替证据判断 |
| [Claude 流式接口](https://platform.claude.com/docs/en/build-with-claude/streaming) | thinking/text/tool_use 内容块有边界；签名只供协议续接，未声称读取其产品内部 UI |

[ReAct](https://arxiv.org/abs/2210.03629) 支持根据行动观察继续下一步的交替循环；[Token-Budget-Aware LLM Reasoning](https://arxiv.org/abs/2412.18547) 支持按问题复杂度提供适当软预算。论文没有证明 KYNXA 每个模型能按固定比例提速，也不要求展示全部内部推理。

本次分段、配置刷新和并行读取更新的网关完整回归 378/378 通过，使用 `node --test --test-concurrency=4 tests/*.test.mjs` 控制独立测试进程并发。测试覆盖三协议真实模拟 HTTP 续接、阶段持久化与重放、四并发乱序结果、写入屏障、取消后保留回执、目录刷新和资源缓存；未使用用户付费模型。聊天流 C# 检查通过，原生 WinUI/WebView2 125 项、浏览器 DOM 43 项及语言 2063 项通过，桌面构建零警告零错误。2026-10-04 重新部署启动后，48 个网关模块及三个展示/技能资源哈希与源码一致，运行中 health 确认 `toolStreamProtocol:3`，Data 和扩展位置保持不变。另以隔离的真实 Chrome DevTools MCP 和动态本机页面验证导航失败识别、稳定等待时机与查询内容变化；该验证不代表已复测 Bing 线上页面或实际模型的搜索耗时。

### 跨消息的工具依据（2026-10-04）

排查一次「上一答已经搜索，下一答却声称上一答没查来源」的正式记录，首答确有五次成功搜索，后答否定其检索依据与执行记录不符。旧输入投影只有助手最终 `Content`，未保留任何上一答的工具观察。这是能确认的信息断层，不能单凭记录断言模型自我否定只有这一种原因。

此前 `execution-receipts.mjs` 仅投影最近三个有效完成轮次、最多 12 条和 768 估算 tokens 的执行元信息，不能代替工具观察全文。本次主 Runtime 改用下面的规范配对历史，不再以该固定短回执限制历史依据。历史回执模块仍保留兼容测试。调用完成不等于结论正确，缺少回执不等于没有查证，也不能据历史重放工具或恢复副作用。

工具提示要求后续追问承接原主题，只引用工具实际返回或读取的直接来源 URL；纠错说明具体旧事实、新证据及差异，不虚构「上一轮仅凭印象/未查证」。这些是软提示，验证输入信息和规则存在不等于保证模型永不出错。

对照参考实现的跨轮输入，而非只看其 UI：

| 实现 | 后续请求保留的依据 | 历史减少方式 |
|---|---|---|
| [Harness 请求投影](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/core/agent-loop/src/agent.ts)、[会话投影](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/core/session/src/surface.ts) | 从日志投影助手与工具结果，核对本机对应源码 | 先裁减工具输出，再压缩旧区间并保护配对边界 |
| [pi 会话历史](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/session-manager.ts#L439) | 当前分支的 assistant/toolResult 原消息 | 摘要加 firstKeptEntryId 起的近期原文，切点保护工具配对 |
| [OpenCode 消息投影](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/message-v2.ts#L296) | 文本、工具 input/output/callID | 旧输出可用占位文字代替，调用结构继续保留，再做摘要 |
| [Codex 输入历史](https://github.com/openai/codex/blob/main/codex-rs/core/src/context_manager/history.rs#L599) | 正规化 ResponseItem 序列，包括工具调用与结果 | 截短结果、压缩历史，另核对调用/结果配对 |
| [Kimi CLI 公开实现](https://github.com/MoonshotAI/kimi-cli/blob/main/src/kimi_cli/soul/kimisoul.py#L1389) | 助手和工具消息追加到 context.history | 摘要旧内容并保留近期尾巴，不据此推断网页内部实现 |
| [OpenClaw 会话裁减](https://docs.openclaw.ai/concepts/session-pruning) | 保留近期助手和工具结果的配对 | 独立裁减旧工具输出，再按需压缩对话 |
| [Claude 工具历史要求](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls) | 原 assistant tool_use 与紧邻 tool_result | 上下文编辑/压缩是独立机制，内置搜索另有原生引用与加密内容 |

上述短回执补丁是此前低成本阶段；本次继续接入 `model-transcript.mjs` 与 `model-history.mjs`，在正式助手消息内保存内部规范轮次 `ModelTranscript`，包含各轮公开正文、调用 ID、工具名和参数。工具结果与执行状态始终取既有 `ToolActivities`，完整内容仍来自同一 `tool-results` 档案，不建立第二套聊天或执行记录。模型轮次在副作用前保存，实际返回结果先保存再响应取消。

下一用户请求按目标协议重建助手调用与结果配对，历史 ID 在请求内重新命名，避免不同请求重复 call ID 冲突。旧工具未在当前目录或普通请求不声明工具时改用标明来源与信任边界的公开调用/观察文本，不补旧 schema 或重新授权。旧日志从已有公开段落与工具回执恢复可证明的结构，不能猜测不存在的原生签名。失败或中断请求保留已执行观察并补明确未知/未开始状态，草稿不提升为最终成功答案，也不自动重做调用。

近期公开结果按真实输入预算读取，读取字节总量包含档案外壳余量，不能用许多微小引用绕过 IO 预算；大文件在读取前按回执大小拒绝超预算读取。结果归配核对原请求、调用 ID、工具名、字节数与 SHA-256，错绑引用从请求投影移除，正式记录保留供诊断。较旧大结果先保留首尾预览和回源引用，再按整段调用/结果或整轮用户对话缩减。`buildContext` 对投影后的全部协议消息计算预算，不能只计算最终 `Content`。原日志不变，历史不限 200 条；压缩是同一聊天的请求投影，超过窗口不意味着必须新开聊天。`conversation.history.read/search(includeTools:true)` 可分页回查公开阶段和调用观察，`tool.result.read` 回查完整公开结果，均保持聊天归属和归档/删除限制。

必要供应商原生续接写入受保护结果档案的 `_meta`，正式消息只存 opaque 引用。公开消息/目录接口剥离 `ModelTranscript`，公开结果与历史读取不返回原生载荷；文件工具仍不能直接读取保护档案。同一正在运行的模型轮保留原生续接，跨用户轮默认重建公开配对，不沿用绑定旧前缀的 thinking 签名或加密块。即使 provider/model 相同，系统时间、记忆、目录或此前消息变化也可能使签名失效，参见 [Claude 签名规则](https://platform.claude.com/docs/en/build-with-claude/thinking-troubleshooting#a-400-error-says-a-thinking-block-signature-is-invalid)。这不是供应商原生历史恢复、完整检查点或跨崩溃续跑。

预算参考 [pi 的 16K 预留与 20K 近期原文](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/compaction/compaction.ts#L126)、[Harness 的阶段压缩阈值](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/compaction/compaction-basic/src/config.ts#L153)、[Codex 的有效窗口与自动阈值](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/openai_models.rs#L390)及 [OpenCode 的独立输入预算](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/overflow.ts#L8)。KYNXA 在扣实际输出和安全余量后的输入预算 90% 触发循环压缩、85% 为目标；大窗口输出分配默认最多占可用窗口 30%，仍受用户配置和已知供应商能力约束。64 轮/256 调用/30 分钟的默认运行上限保留，生成预算 1048576 只累计估算输出，不是包含重复输入的总计费预算。详细输入、输出公式见 [聊天上下文](chat-work-memory.md#请求上下文与恢复)。

[The Complexity Trap](https://arxiv.org/html/2508.21433v3#S3.SS1) 支持先减少旧观察体积；[LongMemEval](https://arxiv.org/html/2410.10813v2#S4.SS2) 指出用摘要替代原文会丢细节，因此保留回源。[MemGPT](https://arxiv.org/abs/2310.08560) 与 [Lost in the Middle](https://arxiv.org/abs/2307.03172) 支持工作上下文、近期材料和外部归档分层，不能据论文声称每个 KYNXA 模型都有固定节省比例或无限上下文。

本轮完整后端 386/386、隔离浏览器 DOM 55 项、原生 WinUI/WebView2 148 项通过，桌面构建零警告零错误。三协议本地模拟 HTTP 后续请求验证真实执行状态/结果 ID 进入下一轮，历史阶段、推理、原始参数/结果及私密路径不进入投影；220 轮样例与工具目录预算退化仍遵守输入预算。网站链接检查含点击发送宿主导航消息、来源去重、最终/当前页、完整 URL 复制、选区冻结、即时语言切换及窄窗换行。重新部署后 49 个网关模块与四个展示资源哈希一致，应用与 v3 网关均运行正常；没有调用用户付费模型，不据模拟上游保证真实模型永不无据纠错。

随后配对历史与简化工具展示的完整回归为 **425/425**。覆盖三协议实际本机模拟 HTTP 的下一消息配对、关闭工具/普通请求降级、旧 v3 无 kind 段恢复、失败与未开始状态、跨请求 ID、私有原生档案保护、档案身份/大小/哈希、错绑引用移除、受控读取量、旧观察优先与完整轮次压缩，以及官方输入/输出能力夹取。200 个大引用和 200 个小引用均有 IO 预算回归；240 条正式日志在缩减、重启与继续消息后仍逐字保留。工具界面原生 153 项、浏览器 DOM 59 项、模型配置原生 64 项及语言 2163 项通过，完整桌面构建零警告零错误。测试未调用用户付费模型，不能据模拟模型保证真实模型永不错误纠正或固定提速。

2026-10-04 配对历史更新重新部署并启动完成，53 个网关模块及五个聊天展示资源的 SHA-256 与源码一致。运行中的网关确认 `contextProtocol:3` 和 `toolStreamProtocol:3`，桌面主窗口正常启动，Data 与扩展位置保持不变。

2026-10-04 紧凑过程与最终回答收束验收：完整网关 **436/436**、隔离浏览器 DOM **51**、原生 WinUI/WebView2 **188**、语言 **2180** 项通过，聊天流及正式聊天客户端检查通过，桌面构建零警告零错误。覆盖真实 HTTP/SSE 总耗时、三协议普通与工具循环、已完成重放、取消回执和截断；界面覆盖近期三阶段、八个普通工具与待批准保留、同类合并、网页四链接及余数、最终 Markdown/公式、空最终回复、失败部分内容、整条复制、选区冻结与收束滚动锚点。测试采用临时 Data 和模拟上游，未调用用户付费模型。重新启动后 54 个网关模块及六个展示资源哈希与源码一致，health 确认 `replyTimingProtocol:1`，模型预算与 Data/扩展位置保持原值。实际模型是否在关键节点产生公开进展仍由其输出决定，界面不编造阶段说话；独立轨迹页面尚未实现。

2026-10-04 中间正文保留与排版修正：隔离浏览器 DOM **57**、原生 WinUI/WebView2 **195** 项通过。中间正文采用最终正文相同的字号、颜色及 Markdown 样式；运行中、失败或缺少最终答案时保留全部已有正文，最终阶段先完成但请求仍运行时也不收束。最终正文与公式完成渲染后才移除过程，覆盖全正文复制、选区冻结、历史重开与滚动锚点。本轮使用临时数据和模拟记录，仅重跑相关展示检查，未调用付费模型。

MCP 目录提供 Playwright、GitHub、Fetch、Git、Context7、Chrome DevTools、Exa、Brave Search、DBHub、MarkItDown 与 Windows Screenshot 共 11 个预设。直接连接上游服务，不另写同类实现；原有文件、记忆、结果分页和终端继续复用。包版本、许可证、依赖、网络要求及数据库只读模板由 `mcp-preset-catalog.mjs` 提供。该阶段预设默认禁用，现行规则为默认启用且检查连接条件，见上方官方工具包说明；连接完成不等于所有操作均已验证。DBHub 需要用户 DSN，GitHub/Brave 需要账号变量，Windows Screenshot 需要 Python 3.14 且该阶段尚未验真，不能据预设存在或开关启用声称这些条件已满足。

当前本机扩展根内已导入 29 个完整上游技能包，保留原文、资源、许可证、固定 commit 和文件 SHA-256；11 个指令型包启用，其余因脚本、网络、账号或办公运行环境尚未满足而禁用。来源是 OpenAI skills、Anthropic 开放技能、Kimi CLI、DeepSeek Harness 的 MIT Office 包、pi-skills、Superpowers 和 codex-research。Anthropic 四个非开放办公包未纳入。Office 仅修正共同检查脚本的包内引用，完整原始内容和适配记录保留于 UPSTREAM。

本机安装记录位于扩展根 `Agent/public-skills.json`、`Agent/public-mcp.json`；它们记录来源和验证情况，不包含 API 密钥，不是权限或正式执行记录。外部包不进入仓库，也不自动执行资源脚本。首次安装验收启用了 Playwright、Fetch、Git、Context7、Chrome DevTools、Exa 和 MarkItDown；依赖在扩展根预热，实际发现 7 个 ready 连接。用户后续可调整启停，启用数量不代表当前连接已就绪。两种浏览器均实际打开隔离的公开测试页面。MCP 外部进程沿用其信任边界，不属于终端 AppContainer。

公开源码可以按许可证复制、修改与复用；公开可读不自动等于开放许可。复用包保留声明，适配层只处理本机存储、标准协议、审批与展示。不会把整个其他 Agent 的会话/记忆系统并排装入当前正式存储，也不声称已安装其闭源或账号专属能力。

本轮公开包复用与连续执行更新：网关 349/349、MCP客户端 46、扩展迁移120项通过；真实临时CPython/stdlib虚拟环境迁移核验3436文件，移走旧目录后新位置两种解释器实际运行成功。工具展示原生WebView2 114项、工具DOM29项、小号灰色字体与窄窗33项、模型输出表单55项、语言2056项通过。运行环境迁移仅适配自有MCP目录中普通pyvenv.cfg和uv-receipt.toml的确定路径字段；保留原文件、注释、外部解释器路径及原有链接拒绝规则，不盲改任意二进制启动器。已安装的本机Python目录别名采用普通目录，uv包文件使用copy模式，实际MCP服务通过uvx与托管物理解释器启动。

## 2026-10-05 敏感读取补充

内置文件工具在 Ask/Smart 中读取 `.env`、常见凭据文件或私钥目标先请求单次审批，审批说明告知内容可能留存并发送给模型；普通目录搜索跳过敏感内容，外部目录审批不隐式授权嵌套凭据。Full 保留用户授予的读取范围，正式应用连接密钥和私有结果仍走已有保护。[本轮核查与验证](issue-review-20261005.md)。
