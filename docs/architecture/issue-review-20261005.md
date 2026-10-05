# 2026-10-05 问题核查与修复

核查对象为本机工作区，基线为 GitHub main `5637dc3`，叠加尚未提交的五人目录重组与协作配置。保留所有既有改动；未修改用户正式 Data、扩展配置或日常聊天，不把旧报告中的测试数字当成本轮结果。

## 确认与修复

| 报告问题 | 本机确认 | 实施 |
|---|---|---|
| MockBackend 缺共享类型 | 编译复现 4 个 CS0246 | 补 shared Tools 契约引用，恢复原 HTTP 聊天验证 |
| MCP 成功后旧超时错误残留 | 原实现只记录失败，不结算成功恢复 | 成功清旧错误；连接/状态身份和诊断代次防止旧成功抹掉新失败、断连或替换后的旧结果污染新连接；不自动重放动作 |
| Ask 直接读取工作区 .env | 内置文件读取属于普通 scoped read，无敏感目标审批 | Ask/Smart 对已识别敏感目标单次审批；未批准或非交互时无内容/归档引用，普通目录搜索跳过敏感文件；Full 保留明确全权限行为，正式应用凭据仍禁止读取 |
| PDB 记录绝对源码路径 | 实际 Portable PDB 元数据包含仓库与 NuGet 用户缓存路径 | 根 Directory.Build.props/targets 映射 /_/KYNXA/ 和 /_/NuGet/，保留符号与行号；不改运行时存储路径 |
| 截图窗口关闭文案无英文 | UiText.Get("关闭") 无翻译键 | 补 Close，审批新增敏感读取说明也可即时切换中英文 |

敏感读取的审批说明明确告知：批准后内容可能进入工具记录并发送给当前模型。批准只针对本次固定调用，不是会话级开放。普通目录搜索的外部访问审批不会顺便开放目录中的凭据文件。已识别路径保护不等于任意文本的秘密识别或所有终端/MCP 的数据防泄漏系统；Full 下读取与结果留存仍由用户授予的权限决定。

## 本轮验证

| 实际检查 | 结果 |
|---|---|
| 完整网关，node --test --test-concurrency=4 apps/model-gateway/tests/*.test.mjs | 681/681，0 失败、0 跳过 |
| MCP/浏览器相关套件 | 95/95；含超时恢复与并发/断连/替换回归 |
| 文件读取/搜索、权限、分页及提示预算相关套件 | 34/34；修正审批说明后敏感文件 7/7 再次通过 |
| MockBackend 构建 | 0 警告、0 错误 |
| 原 HTTP chat-smoke | 通过；独立随机端口、模拟回复，无真实模型调用 |
| 语言 smoke，含新增审批翻译 | 2568 checks、649 labels、588 source references 通过 |
| Agent 设置 UI | 129 检查通过 |
| Computer/审批 UI，--computer-tools-only | 42 检查通过；含敏感读取说明的中英文即时切换，没有读取真实凭据或运行宿主命令 |
| 桌面 Debug x64 构建 | 0 警告、0 错误 |
| 最新 Debug 便携载荷 | 39/39；隔离拷贝、空 PATH、内置运行时及 ToolHost |
| 桌面 Release x64 编译 | 0 错误，100 条 IL2026/IL2104 裁剪警告；未抑制，未据此宣称正式发布验收完成 |
| 实际 Debug PDB 文档名 | desktop 160、ToolHost 23、MockBackend 7，均已映射，0 未映射文档 |
| 实际 Release PDB 文档名 | desktop 160、ToolHost 23，均已映射，0 未映射文档 |

本轮本机证据在 ignored `artifacts/issue-validation/` 与 `artifacts/architecture-validation/`。实际 PDB 元数据记录为 `pdb-debug-after.json`、`pdb-release-after.json`，原路径证据为 `pdb-before.json`。当前构建规则不会重写已经生成的旧 AppX/旧安装包；旧产物可能仍含历史 PDB，分发前必须使用重新生成的发布载荷。

尚未验收真实账号浏览器、全部默认第三方 MCP、完整安装包或跨设备 Release 行为。上述修复不构成安全认证，未更改 GitHub 权限或提交推送。
