# 任务结果与回复完成

HTTP/SSE 传输完成和任务核验结果是两个维度。现有 `completionStatus`、助手消息 `Status` 和 SSE `type` 保留 `completed` / `interrupted` 等生命周期合同；`completed` 表示本次回复已经结束，不能单独解释为来源、执行或整体任务已验证。

Rust `orchestration/progress.rs` 从实际检查回执、来源复验、未知副作用和模型任务状态生成 `taskCompletion`。Node 负责传递并保存该对象，不从正文警告推断成功。最终非流响应、流式终态与正式消息的 `ToolRun.taskCompletion` 使用同一个结果；重读已完成请求返回保存的结果，不重新派发模型或工具。

| `taskCompletion.completionState` | 含义 |
|---|---|
| `completed` | 本次任务正常收口，没有当前已知的核验失败、待核验或未知执行结果；仍不认证所有事实或整体正确性。 |
| `partial` | 存在未完成要求、缺少核验覆盖、预算/工具限制或模型明确的阻塞，已有回复和执行回执保留。 |
| `verification-failed` | 实际执行检查失败，或最终来源/记忆复验失败、变化或不可用。正文可以正常返回，不能标成已验证事实。 |
| `execution-unknown` | 存在未确认的副作用结果；只允许按既有恢复规则观察状态，不能重放未知操作。 |

`taskCompletion.state` 继续保留执行核验维度的 `checks-passed`、`checks-failed`、`checks-version-unverified`、`needs-validation`、`no-execution-checks`，以及旧恢复合同的 `incomplete` / `execution-unconfirmed`。`sourceValidation` 保存来源复验的 `current`、`checked`、`invalidSources` 等事实；`modelTaskState` 保存模型的完成/续接/阻塞声明。它们不能替代真实回执。

`correctnessCertified` 始终为 `false`。文件版本回执仅覆盖已授权且已观察的目标，不是原子整仓快照；来源仍有效也不证明其语义支持了答案。恢复与核验失败不删除原日志、不关闭聊天、不重派已经成功或结果未知的副作用。后续新请求继续使用同一正式聊天。

后续模型历史仍按回复生命周期配对，保留原助手消息的对象身份和正文，并传递 Rust 投影的最小结果元信息：`completionState`、执行检查 `state`、来源 `sourceCurrent` 和 `correctnessCertified: false`。正常任务 `completed` 保留该元信息，不另加说明消息；其余结果由 Rust 追加一条包含助手稳定 ID 的独立历史说明，不重复整篇正文。原工具调用及回执不会被删除，私有诊断和供应商不透明续接内容不进入该说明。摘录来源和语义摘要来源身份包含结果状态；状态变化使旧摘要失效。语义导航的 `programState.requestStates` 同样保留任务结果，不能仅凭回复 `Status=completed` 推断任务已经核验。

来源扫描的 `scanOpen` / `scanNext` 使用原生异步任务和请求级取消租约。用户 `AbortSignal` 与会话关闭都能使遍历循环停止；任务结算后释放监听器和租约，再清空并关闭本次扫描状态。文档 SDK 的资源等待仍经过实际准入，文本遍历和额度规则继续由 Rust 拥有。
