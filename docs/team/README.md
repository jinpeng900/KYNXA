# KYNXA 五人开发分工

更新日期：2026-10-05。已提交基线为 `5637dc3`；本次重组在工作区，验证与提交状态按实际交付记录确认。正式链路保持 WinUI → Node.js 网关 → 模型接口，以及网关 → C# ToolHost。

| 成员 | 主责 | 可分配目录 | 任务入口 |
|---|---|---|---|
| A | 编排、基础合同、集成与打包 | gateway orchestration/platform；desktop Services/Integration | [A](A-队长与任务编排.md) |
| B | WinUI/WebView2 展示与交互 | desktop Views/Controls/ViewModels/资源/Services/Presentation | [B](B-桌面前端.md) |
| C | 模型、协议、流式、预算、请求上下文 | gateway models；desktop Services/Models；shared Chat | [C](C-模型与本地推理.md) |
| D | 工具、权限、MCP/Skill、浏览器与原生执行 | gateway tools/official-tools；desktop Services/Tools；shared Tools；tool-host 功能目录 | [D](D-权限与工具执行.md) |
| E | 会话、记忆、索引、路径与迁移 | gateway data；desktop Services/Data；shared Memory | [E](E-数据与验证.md) |

gateway、desktop、shared、tool-host 分别指 apps/model-gateway、apps/desktop、apps/shared、apps/tool-host。实际依赖与证据见 [团队边界](../architecture/team-boundaries.md)，集成流程见 [队长总览](队长总览.md)，本轮实测见 [重组验证记录](../architecture/reorganization-validation.md)。

日常分工简化为 A 编排、B 桌面、C 模型、D 工具、E 数据五个主要模块；配套客户端和契约归同一领域负责人。GitHub 邀请命令、目录审查人生成与主分支保护见 [GitHub 协作](github-collaboration.md)。目录负责人是审查归属，不是 Git 的文件夹写入权限。

各人负责自己的实现、测试与说明；A 负责集成和工具设置 HTTP 接线，E 负责数据一致性，不承担全队测试。B 是全部 ShellPage partial 的唯一主负责人；多个 partial 共享页面状态，不代表完整 MVVM。

当前已有正式聊天/确认记忆、三协议、流式工具循环、审批、MCP/Skill、浏览器及 ToolHost 执行。权限与隔离按 [工具架构](../architecture/agent-tools.md) 的实际实现说明。持久长任务、检查点、崩溃后自动恢复与确定性验证绑定属于后续工作，不能因本轮目录重组而宣称完成。

首轮交接目标是每人能定位主责源码、运行对应回归、按共享契约联调。旧独立 Host、Rust Authority 和六周排期不作为当前执行方案；后续按实际需求和团队容量安排。

## 主责清单与自动检查

[机器可读的模块主责清单](module-ownership.json) 维护每个目录/文件的唯一主责，不使用虚构 GitHub 账号。从仓库根运行：

```powershell
node tools/development/check-architecture.mjs
node tools/development/check-architecture.mjs --details
```

[依赖与归属守卫](../../tools/development/check-architecture.mjs) 检查静态相对引用、域依赖/循环和单一主责；`--details` 输出本轮 119 套测试的主责。守卫覆盖现有可识别语法，不检查计算生成的动态导入，也不代替功能测试。新增/移动模块同时更新清单与实际调用端。
