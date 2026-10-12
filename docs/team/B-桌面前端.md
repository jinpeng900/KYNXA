# B：桌面前端

更新日期：2026-10-06。[团队边界](../architecture/team-boundaries.md)。交接分支：`kynxa_team/b-ui`。

当前负责人账号为 `hui33844`。直接推送原仓库的界面分支，由队长验收并决定是否合并进 `main`；具体写入权限和历史保护见 [规则集](rulesets/README.md)。跨领域修改仍需协调，分支权限不等于文件夹权限。

## 代码范围

apps/desktop 的 Views、Controls、ViewModels、UI 模型/布局/资源与 Services/Presentation。B 是全部 ShellPage partial 的唯一主负责人。Presentation 包含本地化、侧栏、Markdown/公式、审批与结果、终端和截图展示。

检索界面 `Views/RetrievalSettingsWindow.cs`、`RetrievalSettingsWindow.Layout.cs`、`ShellPage.Retrieval.cs` 也归 B；检索 API 客户端与共享数据合同归 E。聊天 WebView2 入口是 `Resources/Transcript/transcript.js` / `transcript.css`，截图和附件沿现有右侧面板展示，不把终端重新渲染为右侧卡片。

## 首轮交付

检查移动后的命名空间、服务引用和打包资源。保留稳定聊天身份、单一未发送草稿、打开不更新 MRU、选择与展开独立。ProjectStore 仍为网关客户端，页面不写正式助手快照。

维护一个 WebView2 Transcript 展示路径；AssistantSegments 保持阶段/思考/调用/结果的真实顺序。成功收束只改变投影，不删除正式记录；失败、取消、截断不收束为成功。保护跨消息选择复制、源 Markdown/TeX、上滚锚点和流式选区冻结。

工具/记忆/模型通过 D/E/C 客户端接入；revision 冲突重载并让用户决定，切换/取消后的迟到结果只更新其原会话请求。功能客户端由相应成员维护，B 负责 UI 接线。

## 验证

桌面构建；按影响选择 work-sidebar、project-tree、ui-layout、ui-language、transcript-markdown/transcript-ui smoke。选择/复制/滚动/语言需要真实 WinUI/WebView2 验收，构建不替代交互检查。

Services 仍在原程序集与命名空间，客户端对 UiText 等展示类型的既有引用保留；多个 partial 不代表完整 MVVM。持久长任务恢复 UI 属后续工作。
