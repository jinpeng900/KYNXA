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

function send() {
  const value = elements.prompt.value.trim();
  if (!value) { elements.prompt.focus(); return; }
  elements.empty.classList.add("hidden");
  document.querySelector(".ambient").classList.add("hidden");
  elements.conversation.classList.remove("hidden");
  elements.title.textContent = mode === "work" ? "当前工作" : "新聊天";
  const user = document.createElement("div"); user.className = "message user"; user.textContent = value;
  const assistant = document.createElement("div"); assistant.className = "message assistant";
  assistant.textContent = "这是 Linux 预览层的交互占位回复。正式消息能力仍由 KYNXA 后端提供。";
  elements.messages.append(user, assistant);
  elements.prompt.value = "";
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
document.querySelector("#modelButton").addEventListener("click", (event) => showMenu(event.currentTarget, ["本地模型", "OpenAI Compatible", "模拟模型"], (value) => event.currentTarget.querySelector("span").textContent = value));
document.querySelector("#permissionButton").addEventListener("click", (event) => showMenu(event.currentTarget, ["请求批准", "智能批准", "完整访问"], (value) => event.currentTarget.querySelector("span").textContent = value));
elements.workspace.addEventListener("click", (event) => showMenu(event.currentTarget, projects.slice(0, 4).map(([name]) => name), (value) => event.currentTarget.querySelector("span").textContent = value));
elements.newItem.addEventListener("click", () => showToast(mode === "work" ? "预览：新建项目" : "预览：新建聊天"));
document.addEventListener("pointerdown", (event) => { if (!elements.popover.contains(event.target) && !event.target.closest("#modelButton, #permissionButton, #workspaceButton")) elements.popover.classList.add("hidden"); });

setMode("work");
