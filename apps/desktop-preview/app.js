const projects = [
  ["KYNXA 界面设计", ["侧栏布局与导航", "工作与聊天切换", "输入框交互细节", "浅色主题与字体"]],
  ["毕业论文", ["研究问题与提纲", "文献阅读与归纳", "数据清洗与分析"]],
  ["CodeRepair 开发", ["定位构建错误", "修复方案讨论", "补充回归验证"]],
  ["市场推广计划", ["目标用户分析", "内容选题规划", "活动页面文案"]],
  ["个人知识库", ["整理阅读笔记", "知识分类与标签"]]
];

const chats = [
  ["市场推广策略讨论", "2 小时前"], ["量子计算原理解释", "4 小时前"],
  ["产品命名方案", "1 天前"], ["总结研究论文", "2 天前"],
  ["设计落地页文案", "3 天前"], ["TypeScript 学习路线", "3 天前"],
  ["图像分割模型对比", "5 天前"], ["毕业论文选题建议", "6 天前"],
  ["深度学习训练技巧", "1 周前"], ["Linux 常用命令整理", "1 周前"],
  ["数据库期末复习", "1 周前"], ["SQL 语句优化", "1 周前"]
];

const elements = {
  modeSwitch: document.querySelector(".mode-switch"), modes: [...document.querySelectorAll(".mode")],
  recents: document.querySelector(".recents"), list: document.querySelector("#recentList"),
  label: document.querySelector("#sectionLabel"), newItem: document.querySelector("#newItem"),
  prompt: document.querySelector("#prompt"), composer: document.querySelector(".composer"),
  workspace: document.querySelector("#workspaceButton"), empty: document.querySelector("#emptyState"),
  conversation: document.querySelector("#conversation"), title: document.querySelector("#conversationTitle"),
  messages: document.querySelector("#messages"), popover: document.querySelector("#popover"),
  toast: document.querySelector("#toast")
};

let mode = "work";
let toastTimer;
let composingPrompt = false;
let suppressComposingEnter = false;
let providers = [];
let selectedModel = null;
let conversationId = crypto.randomUUID();
let sending = false;

async function api(path, options = {}) {
  const response = await fetch(path, options);
  const body = await response.json();
  if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
  return body;
}

async function refreshModels() {
  ({ providers } = await api("/api/models"));
  const available = providers.flatMap(provider => provider.models.map(model => ({
    provider: provider.providerId, model, label: `${provider.displayName} · ${model}`
  })));
  if (selectedModel && !available.some(item => item.provider === selectedModel.provider && item.model === selectedModel.model)) selectedModel = null;
  document.querySelector("#modelButton span").textContent = selectedModel?.model || "模型选择";
  return available;
}

function formConnection() {
  const form = document.querySelector("#modelForm");
  const values = new FormData(form);
  return {
    providerId: String(values.get("providerId")).trim(), displayName: String(values.get("displayName")).trim(),
    baseUrl: String(values.get("baseUrl")).trim(), apiKey: String(values.get("apiKey")),
    models: String(values.get("models")).split(/[\n,]/).map(value => value.trim()).filter(Boolean)
  };
}

function setConnection(provider) {
  const form = document.querySelector("#modelForm");
  for (const field of ["providerId", "displayName", "baseUrl"])
    form.elements.namedItem(field).value = provider?.[field] || "";
  form.elements.namedItem("models").value = provider?.models?.join("\n") || "";
  form.elements.namedItem("apiKey").value = "";
  document.querySelector("#modelStatus").textContent = provider?.hasApiKey ? "API Key 已保存；留空可保留。" : "填写连接信息后保存。";
}

function renderProviders() {
  const list = document.querySelector("#providerList");
  list.replaceChildren();
  providers.forEach(provider => {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = `${provider.displayName} · ${provider.models.length} 个模型`;
    button.addEventListener("click", () => setConnection(provider));
    list.append(button);
  });
}

async function openModels() {
  document.querySelector("#modelDialog").showModal();
  try { await refreshModels(); renderProviders(); }
  catch (error) { document.querySelector("#modelStatus").textContent = error.message; }
}

function renderRecents() {
  elements.list.replaceChildren();
  if (mode === "work") {
    for (const [name, children] of projects) {
      const group = document.createElement("div");
      group.className = "recent-group";
      const project = row(name, "/desktop-assets/Icons/work-folder.svg");
      const nested = document.createElement("div");
      nested.className = "project-children hidden";
      children.forEach((child) => nested.append(row(child, "/desktop-assets/Icons/chat.svg")));
      project.addEventListener("click", () => nested.classList.toggle("hidden"));
      group.append(project, nested);
      elements.list.append(group);
    }
  } else {
    chats.forEach(([name, time]) => elements.list.append(row(name, null, time)));
  }
}

function row(label, icon, detail) {
  const button = document.createElement("button");
  button.className = "recent-row";
  if (icon) {
    const image = document.createElement("img"); image.src = icon; image.alt = ""; button.append(image);
  }
  const text = document.createElement("span");
  text.textContent = label;
  if (detail) { const small = document.createElement("small"); small.textContent = detail; text.append(small); }
  button.append(text);
  button.addEventListener("dblclick", () => showToast(`已打开：${label}`));
  return button;
}

function setMode(nextMode) {
  mode = nextMode;
  elements.modeSwitch.classList.toggle("chat", mode === "chat");
  elements.modes.forEach((button) => {
    const active = button.dataset.mode === mode;
    button.classList.toggle("active", active);
    button.setAttribute("aria-selected", String(active));
  });
  elements.label.textContent = mode === "work" ? "项目" : "聊天";
  elements.newItem.title = mode === "work" ? "添加项目" : "新建聊天";
  elements.workspace.classList.toggle("hidden", mode === "chat");
  renderRecents();
}

function showMenu(anchor, choices, onSelect) {
  elements.popover.replaceChildren();
  choices.forEach((choice) => {
    const button = document.createElement("button"); button.textContent = choice;
    button.addEventListener("click", () => { onSelect(choice); elements.popover.classList.add("hidden"); });
    elements.popover.append(button);
  });
  const area = document.querySelector(".main-region").getBoundingClientRect();
  const box = anchor.getBoundingClientRect();
  elements.popover.style.left = `${Math.max(12, box.right - area.left - 250)}px`;
  elements.popover.style.top = `${box.bottom - area.top + 8}px`;
  elements.popover.classList.remove("hidden");
}

async function send() {
  const value = elements.prompt.value.trim();
  if (!value) { elements.prompt.focus(); return; }
  if (sending) return;
  if (!selectedModel) { showToast("请先配置并选择模型"); return; }
  sending = true;
  document.querySelector("#sendButton").disabled = true;
  elements.empty.classList.add("conversation-active");
  document.querySelector(".ambient").classList.add("hidden");
  elements.conversation.classList.remove("hidden");
  document.querySelector(".composer-block").classList.add("in-conversation");
  elements.title.textContent = mode === "work" ? "当前工作" : "新聊天";
  const user = document.createElement("div"); user.className = "message user"; user.textContent = value;
  const assistant = document.createElement("div"); assistant.className = "message assistant";
  assistant.textContent = "正在等待模型回复…";
  elements.messages.append(user, assistant);
  elements.prompt.value = "";
  try {
    const reply = await api("/api/chat", { method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ conversationId, message: value, provider: selectedModel.provider,
        model: selectedModel.model, permissionMode: "ask" }) });
    assistant.textContent = reply.content;
  } catch (error) { assistant.textContent = `调用失败：${error.message}`; }
  finally { sending = false; document.querySelector("#sendButton").disabled = false; }
}

function showToast(message) {
  clearTimeout(toastTimer); elements.toast.textContent = message; elements.toast.classList.remove("hidden");
  toastTimer = setTimeout(() => elements.toast.classList.add("hidden"), 1800);
}

elements.modes.forEach((button) => button.addEventListener("click", () => setMode(button.dataset.mode)));
document.querySelector("#treeToggle").addEventListener("click", (event) => {
  const collapsed = elements.recents.classList.toggle("collapsed");
  event.currentTarget.setAttribute("aria-expanded", String(!collapsed));
});
document.querySelector("#expandComposer").addEventListener("click", () => elements.composer.classList.toggle("expanded"));
document.querySelector("#sendButton").addEventListener("click", send);
elements.prompt.addEventListener("compositionstart", () => { composingPrompt = true; });
elements.prompt.addEventListener("compositionend", () => {
  composingPrompt = false;
  suppressComposingEnter = true;
});
elements.prompt.addEventListener("keydown", (event) => {
  if (event.key !== "Enter") { suppressComposingEnter = false; return; }
  if (event.isComposing || composingPrompt || event.keyCode === 229 || suppressComposingEnter) {
    suppressComposingEnter = false;
    return;
  }
  if (event.shiftKey) return;
  event.preventDefault();
  send();
});
elements.prompt.addEventListener("keyup", () => {
  if (!composingPrompt) suppressComposingEnter = false;
});
document.querySelector("#modelButton").addEventListener("click", async (event) => {
  try {
    const choices = await refreshModels();
    showMenu(event.currentTarget, [...choices.map(item => item.label), "配置模型连接…"], value => {
      const choice = choices.find(item => item.label === value);
      if (choice) { selectedModel = choice; event.currentTarget.querySelector("span").textContent = choice.model; }
      else openModels();
    });
  } catch (error) { showToast(error.message); }
});
document.querySelector("#permissionButton").addEventListener("click", (event) => showMenu(event.currentTarget, ["请求批准", "智能批准", "完整访问"], (value) => event.currentTarget.querySelector("span").textContent = value));
elements.workspace.addEventListener("click", (event) => showMenu(event.currentTarget, projects.slice(0, 4).map(([name]) => name), (value) => event.currentTarget.querySelector("span").textContent = value));
elements.newItem.addEventListener("click", () => {
  if (mode === "work") { showToast("预览：新建项目"); return; }
  conversationId = crypto.randomUUID();
  elements.messages.replaceChildren();
  elements.conversation.classList.add("hidden");
  elements.empty.classList.remove("conversation-active");
  document.querySelector(".ambient").classList.remove("hidden");
  elements.prompt.value = "";
  elements.prompt.focus();
});
document.querySelector("#manageModels").addEventListener("click", openModels);
document.querySelector("#closeModels").addEventListener("click", () => document.querySelector("#modelDialog").close());
document.querySelector("#newProvider").addEventListener("click", () => setConnection(null));
document.querySelector("#ollamaPreset").addEventListener("click", () => setConnection({ providerId: "ollama", displayName: "Ollama", baseUrl: "http://127.0.0.1:11434/v1" }));
document.querySelector("#customPreset").addEventListener("click", () => setConnection({ providerId: "custom-api", displayName: "自定义 API" }));
document.querySelector("#probeModels").addEventListener("click", async () => {
  const status = document.querySelector("#modelStatus"); status.textContent = "正在测试…";
  try {
    const result = await api("/api/models/test", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(formConnection()) });
    if (result.models.length) document.querySelector("#modelForm").elements.namedItem("models").value = result.models.join("\n");
    status.textContent = `连接成功，${result.latencyMs} ms；发现 ${result.models.length} 个模型。`;
  } catch (error) { status.textContent = `测试失败：${error.message}。可手动填写 Model ID。`; }
});
document.querySelector("#modelForm").addEventListener("submit", async event => {
  event.preventDefault();
  const status = document.querySelector("#modelStatus"); status.textContent = "正在保存…";
  try {
    const result = await api("/api/models", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(formConnection()) });
    await refreshModels(); renderProviders(); setConnection(result.provider);
    status.textContent = `已保存 ${result.provider.displayName}。现在可以在输入框选择模型。`;
  } catch (error) { status.textContent = `保存失败：${error.message}`; }
});
document.addEventListener("pointerdown", (event) => { if (!elements.popover.contains(event.target) && !event.target.closest("#modelButton, #permissionButton, #workspaceButton")) elements.popover.classList.add("hidden"); });

setMode("work");
refreshModels().catch(() => {});
