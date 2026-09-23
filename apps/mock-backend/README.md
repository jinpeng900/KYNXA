# 聊天 UI 模拟接口

这个服务用于桌面聊天界面联调。它只监听本机，不调用模型、不需要 API Key；任何非空问题都在约 650ms 后得到 `你好`。

在仓库根目录分别启动服务和桌面：

```powershell
dotnet run --project apps/mock-backend/KYNXA.MockBackend.csproj
dotnet run --project apps/desktop/KYNXA.Desktop.csproj -p:Platform=x64 --no-launch-profile
```

默认地址是 `http://127.0.0.1:5217`。两端都支持通过环境变量 `KYNXA_MOCK_API_URL` 修改地址。

- `GET /health`：检查服务是否启动。
- `POST /api/chat`：接收 JSON 问题，返回 JSON 助手消息。

请求示例：

```json
{
  "conversationId": "fabd785a-00e3-4fb8-a6ee-77055337fb61",
  "message": "今天的天气怎么样？",
  "model": "任意模型名称，也可以不传",
  "permissionMode": "ask"
}
```

响应包含 `conversationId`、`requestId`、`role: "assistant"`、`content: "你好"`、`createdAt`。空白问题或空会话 ID 返回 400。

`permissionMode` 支持 `ask`（请求批准，默认）、`smart`（帮我批准）、`full`（完全访问权限）；其他值返回 400。桌面会保存选择，在发送时附带该配置，重试沿用原请求的权限配置。目前它只是模拟接口的元数据，尚未接入文件或网络操作执行器。

桌面会显示等待状态、保存双方消息，连接失败时可以重试。切换会话后回复仍写回发起请求的会话；删除会话会取消未完成的请求。未发送任何消息的临时聊天仍不保存。旧版字符串消息会按用户消息读取，保存时转换为带角色的结构。

尚未进入项目的工作输入框下方显示“选择项目”。菜单中的已有项目列表最多约 6 行、可独立滚动，底部操作固定显示。选择项目时保留输入草稿；选择“不使用文件夹”后，首次发送会创建无关联文件夹的工作任务，显示在工作侧栏的任务区域，普通聊天历史保持独立。

样式参考本机 Kimi：用户气泡 16px / 26px，内边距 12px / 10px，圆角 12px；助手正文 14px / 22.4px，头像 32px，正文区域加宽至最大 920px。工作聊天可展开空白右栏；分隔线可拖动，右栏宽度为 320–1600px，并为聊天区域至少保留 420px。主内容区域不足 900px 宽或 440px 高时自动隐藏右栏，手动收起状态和拖动宽度独立保存。

启动服务后运行接口与存储回归检查：

```powershell
dotnet run --project tests/chat-smoke/KYNXA.ChatSmoke.csproj
```
