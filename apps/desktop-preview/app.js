const projects = [
  ["KYNXA 界面设计", ["侧栏布局与导航", "工作与聊天切换", "输入框交互细节"]],
  ["课程设计", ["需求整理与分工", "实现计划与验收"]],
  ["代码学习", ["理解模块结构", "讨论改进方向"]]
];
const planned = {
  知识库: "将用于整理资料和检索引用。当前预览尚未接入文档导入、索引和检索服务。",
  连接: "将用于管理外部应用和工具连接。当前可用的是“模型管理”中的模型 API 连接，外部工具尚未接入。",
  定时任务: "将用于设置计划和查看执行记录。当前尚未接入调度器，不会创建或执行后台任务。",
  技能: "将用于查看和管理可复用的任务能力。当前尚未接入技能执行服务。"
};
const $ = selector => document.querySelector(selector);
const elements = {
  window: $(".window"), modeSwitch: $(".mode-switch"), modes: [...document.querySelectorAll(".mode")],
  recents: $(".recents"), list: $("#recentList"), label: $("#sectionLabel"), newItem: $("#newItem"),
  prompt: $("#prompt"), composer: $(".composer"), workspace: $("#workspaceButton"), empty: $("#emptyState"),
  conversation: $("#conversation"), title: $("#conversationTitle"), messages: $("#messages"),
  popover: $("#popover"), toast: $("#toast"), search: $("#sidebarSearch"), searchStatus: $("#searchStatus"),
  latest: $("#jumpToLatest")
};
function newSession(title, workspace = null, key = crypto.randomUUID()) {
  return { id: crypto.randomUUID(), key, title, workspace, messages: [], pending: null, draft: "", permission: "ask", scrollTop: 0, followLatest: true };
}
let mode = "work";
const workSessions = new Map();
const initialWork = newSession("新的工作");
workSessions.set(initialWork.key, initialWork);
const chatSessions = [newSession("新聊天")];
const modeSessions = { work: initialWork, chat: chatSessions[0] };
const searchQueries = { work: "", chat: "" };
const expandedProjects = new Set();
let presentedSession = null;
let toastTimer, menuAnchor = null;
let composingPrompt = false, suppressComposingEnter = false;
let providers = [], selectedModel = null, serviceState = "loading", serviceError = "";
let modelRefreshSequence = 0, detailTab = "tasks", detailsOpen = false;
let modelBusy = false, modelOperation = null, testedRevision = null, formRevision = 0;
let editingProviderId = null, newConnectionEdited = false, newProviderPrefix = "custom-api";
const activeSession = () => modeSessions[mode];
const modelChoices = () => providers.flatMap(provider => provider.models.map(model => ({
  provider: provider.providerId, providerName: provider.displayName, model,
  label: `${provider.displayName} · ${model}`
})));

async function api(path, options = {}) {
  const signal = options.signal ?? AbortSignal.timeout(15000);
  const response = await fetch(path, { ...options, signal });
  let body;
  try { body = await response.json(); } catch { throw new Error("服务返回了无法识别的内容，请刷新后重试。"); }
  if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
  return body;
}
function renderServiceState() {
  const choices = modelChoices();
  const state = serviceState === "ready" ? (selectedModel ? "selected" : choices.length ? "ready" : "unconfigured") : serviceState;
  const label = state === "loading" ? "正在连接模型网关…" : state === "failed" ? "模型网关连接失败"
    : state === "unconfigured" ? "网关可用 · 尚未配置模型"
    : state === "selected" ? `${selectedModel.providerName} · ${selectedModel.model}` : "网关可用 · 请选择模型";
  $("#serviceStatus").dataset.state = state;
  $("#serviceLabel").textContent = label;
  $("#serviceStatus").title = state === "failed" ? serviceError : label;
  $("#refreshStatus").disabled = state === "loading";
  $("#configureStatus").textContent = state === "failed" ? "查看配置" : "配置模型";
  $("#modelButton span").textContent = selectedModel?.model || "模型选择";
  $("#modelButton").title = selectedModel?.label || "选择已配置的模型";
  renderDetails();
}
async function refreshModels() {
  const sequence = ++modelRefreshSequence;
  serviceState = "loading";
  renderServiceState();
  try {
    const result = await api("/api/models");
    if (!Array.isArray(result.providers)) throw new Error("模型列表格式无效。");
    if (sequence !== modelRefreshSequence) return modelChoices();
    providers = result.providers;
    const choices = modelChoices();
    if (selectedModel) selectedModel = choices.find(item => item.provider === selectedModel.provider && item.model === selectedModel.model) ?? null;
    serviceState = "ready"; serviceError = "";
    renderServiceState();
    return choices;
  } catch (error) {
    if (sequence === modelRefreshSequence) {
      serviceState = "failed"; serviceError = error.message;
      renderServiceState();
    }
    throw error;
  }
}
function formConnection() {
  const values = new FormData($("#modelForm"));
  return {
    providerId: String(values.get("providerId")).trim(), displayName: String(values.get("displayName")).trim(),
    baseUrl: String(values.get("baseUrl")).trim(), apiKey: String(values.get("apiKey")),
    protocol: String(values.get("protocol")),
    models: String(values.get("models")).split(/[\n,]/).map(value => value.trim()).filter(Boolean)
  };
}
function setModelStatus(text, error = false) {
  $("#modelStatus").textContent = text;
  $("#modelStatus").dataset.error = String(error);
}
function nextProviderId(prefix = "custom-api") {
  let suffix = 1, id = prefix;
  while (providers.some(item => item.providerId === id)) id = `${prefix}-${++suffix}`;
  return id;
}
function synchronizeNewProviderId() {
  if (editingProviderId !== null || newConnectionEdited) return;
  const field = $("#modelForm").elements.namedItem("providerId");
  if (!providers.some(item => item.providerId === field.value)) return;
  field.value = nextProviderId(newProviderPrefix);
  testedRevision = null; formRevision++;
}
function setConnection(provider, editing = false) {
  const form = $("#modelForm");
  editingProviderId = editing ? provider?.providerId ?? null : null;
  newConnectionEdited = false;
  newProviderPrefix = provider?.providerId || "custom-api";
  for (const field of ["providerId", "displayName", "baseUrl"])
    form.elements.namedItem(field).value = provider?.[field] || "";
  if (!editing) form.elements.namedItem("providerId").value = nextProviderId(newProviderPrefix);
  form.elements.namedItem("providerId").readOnly = editing;
  form.elements.namedItem("models").value = provider?.models?.join("\n") || "";
  form.elements.namedItem("protocol").value = provider?.protocol || "openai-completions";
  form.elements.namedItem("apiKey").value = "";
  testedRevision = null; formRevision++;
  setModelStatus(provider?.hasApiKey ? "API Key 已保存；地址不变时留空可保留。" : "填写连接后，测试或保存。");
}
function renderProviders() {
  const list = $("#providerList");
  list.replaceChildren();
  if (!providers.length) { const hint = document.createElement("p"); hint.className = "list-note"; hint.textContent = "还没有连接"; list.append(hint); }
  providers.forEach(provider => {
    const button = document.createElement("button"); button.type = "button";
    button.disabled = modelBusy;
    button.textContent = `${provider.displayName} · ${provider.models.length} 个模型`;
    button.addEventListener("click", () => setConnection(provider, true));
    list.append(button);
  });
}
async function openModels() {
  closeMenu();
  if (!$("#modelDialog").open) $("#modelDialog").showModal();
  if (!$("#modelForm").elements.namedItem("providerId").value) setConnection(null);
  const openingRevision = formRevision;
  try {
    await refreshModels(); renderProviders();
    if (formRevision === openingRevision) synchronizeNewProviderId();
  }
  catch (error) { setModelStatus(`无法连接网关：${error.message}。请确认本地模型网关已启动。`, true); }
}
function setModelBusy(busy) {
  modelBusy = busy;
  for (const field of $("#modelForm").querySelectorAll("input, textarea, select, button:not(#closeModels)")) field.disabled = busy;
}
function showInfo(title, description) {
  closeMenu();
  $("#infoTitle").textContent = title;
  $("#infoDescription").textContent = description;
  if (!$("#infoDialog").open) $("#infoDialog").showModal();
}
function row(label, icon, detail, selected = false) {
  const button = document.createElement("button"); button.className = `recent-row${selected ? " selected" : ""}`;
  if (icon) { const image = document.createElement("img"); image.src = `/desktop-assets/Icons/${icon}.svg`; image.alt = ""; button.append(image); }
  const text = document.createElement("span"); text.textContent = label;
  if (detail) { const small = document.createElement("small"); small.textContent = detail; text.append(small); }
  button.append(text);
  return button;
}
function searchMatch(title, session = null) {
  const query = searchQueries[mode].trim().toLocaleLowerCase();
  if (!query || title.toLocaleLowerCase().includes(query)) return { matches: true, detail: null };
  const message = session?.messages.find(item => item.content?.toLocaleLowerCase().includes(query));
  if (!message) return { matches: false, detail: null };
  const compact = message.content.replace(/\s+/g, " ");
  const index = compact.toLocaleLowerCase().indexOf(query);
  const start = Math.max(0, index - 10);
  return { matches: true, detail: `匹配消息：${start ? "…" : ""}${compact.slice(start, start + 58)}${compact.length > start + 58 ? "…" : ""}` };
}
function renderRecents() {
  elements.list.replaceChildren();
  const searching = Boolean(searchQueries[mode].trim());
  $("#clearSearch").classList.toggle("hidden", !searchQueries[mode]);
  let resultCount = 0;
  if (mode === "work") {
    for (const session of workSessions.values()) {
      if (session.workspace || (!searching && !session.messages.length && session !== activeSession())) continue;
      const match = searchMatch(session.title, session);
      if (!match.matches) continue;
      const item = row(session.title, "chat", match.detail || "本页工作", session === activeSession());
      item.addEventListener("click", () => selectSession(session));
      elements.list.append(item); resultCount++;
    }
    for (const [index, [name, children]] of projects.entries()) {
      const projectMatches = searchMatch(name).matches;
      const childEntries = children.map((title, childIndex) => ({ title, key: `demo-${index}-${childIndex}` }));
      const predefinedKeys = new Set(childEntries.map(item => item.key));
      for (const session of workSessions.values()) {
        if (session.workspace === name && !predefinedKeys.has(session.key)) childEntries.push({ title: session.title, key: session.key });
      }
      const matches = childEntries.map(item => ({ ...item, match: searchMatch(item.title, workSessions.get(item.key)) }))
        .filter(item => projectMatches || item.match.matches);
      if (!projectMatches && !matches.length) continue;
      const group = document.createElement("div"); group.className = "recent-group";
      const project = row(name, "work-folder", "演示", activeSession().workspace === name);
      const expanded = searching || expandedProjects.has(index);
      project.setAttribute("aria-expanded", String(expanded));
      const nested = document.createElement("div"); nested.className = `project-children${expanded ? "" : " hidden"}`;
      matches.forEach(({ title, key, match }) => {
        const button = row(title, "chat", match.detail, activeSession().key === key);
        button.addEventListener("click", () => selectWorkspace(name, title, key));
        nested.append(button);
      });
      project.addEventListener("click", () => {
        if (searching) { selectWorkspace(name); return; }
        if (expandedProjects.has(index)) expandedProjects.delete(index); else expandedProjects.add(index);
        renderRecents();
      });
      group.append(project, nested); elements.list.append(group); resultCount += matches.length || 1;
    }
  } else {
    for (const session of [...chatSessions].reverse()) {
      const match = searchMatch(session.title, session);
      if (!match.matches) continue;
      const item = row(session.title, "chat", match.detail || `${session.messages.filter(message => message.role === "user").length} 条提问 · 本页会话`, session === activeSession());
      item.addEventListener("click", () => selectSession(session));
      elements.list.append(item); resultCount++;
    }
  }
  elements.searchStatus.textContent = resultCount ? `找到 ${resultCount} 个会话入口` : "没有找到匹配的项目或会话。试试标题或消息中的词。";
  elements.searchStatus.classList.toggle("hidden", !searching);
}
function rememberConversationPosition() {
  if (presentedSession !== activeSession() || elements.conversation.classList.contains("hidden")) return;
  presentedSession.scrollTop = elements.conversation.scrollTop;
  presentedSession.followLatest = isNearLatest();
}
function preserveDraft() { activeSession().draft = elements.prompt.value; rememberConversationPosition(); }
function selectSession(session) {
  preserveDraft(); modeSessions[mode] = session;
  closeMenu(); renderSession(); renderRecents(); closeNarrowSidebar();
}
function selectWorkspace(name, title = `${name} · 新工作`, key = `demo-${name}-default`) {
  if (!workSessions.has(key)) workSessions.set(key, newSession(title, name, key));
  selectSession(workSessions.get(key));
}
function createCurrentSession() {
  const session = newSession(mode === "work" ? "新的工作" : "新聊天");
  if (mode === "work") workSessions.set(session.key, session); else chatSessions.push(session);
  searchQueries[mode] = ""; elements.search.value = "";
  selectSession(session); elements.prompt.focus();
}
function focusSidebarSearch() {
  closeMenu(); setSidebar(true);
  elements.recents.classList.remove("collapsed"); $("#treeToggle").setAttribute("aria-expanded", "true");
  elements.search.focus(); elements.search.select();
}
function setMode(nextMode) {
  preserveDraft(); mode = nextMode;
  elements.modeSwitch.classList.toggle("chat", mode === "chat");
  elements.modes.forEach(button => {
    const active = button.dataset.mode === mode;
    button.classList.toggle("active", active); button.setAttribute("aria-selected", String(active));
  });
  elements.label.textContent = mode === "work" ? "示例项目" : "本页聊天";
  $("#listNote").textContent = mode === "work" ? "演示项目不读取本机文件。" : "刷新页面后，这些预览会话会清空。";
  const action = mode === "work" ? "新建工作" : "新建聊天";
  elements.newItem.title = `${action} · Ctrl + N`; elements.newItem.setAttribute("aria-label", action);
  elements.search.value = searchQueries[mode];
  const searchLabel = mode === "work" ? "搜索项目和会话" : "搜索聊天和消息";
  elements.search.placeholder = searchLabel; elements.search.setAttribute("aria-label", searchLabel);
  $("#welcomeTitle").textContent = mode === "work" ? "从一件事开始" : "有什么想聊的？";
  $("#welcomeDescription").textContent = mode === "work" ? "描述你的目标，再选择模型开始对话。" : "选择一个模型，开始学习或讨论。";
  elements.workspace.classList.toggle("hidden", mode === "chat");
  $("#detailsToggle").classList.toggle("hidden", mode === "chat");
  closeMenu(); renderSession(); renderRecents(); setDetails(detailsOpen);
}
function closeMenu(restoreFocus = false) {
  elements.popover.classList.add("hidden");
  if (menuAnchor) {
    menuAnchor.setAttribute("aria-expanded", "false");
    if (restoreFocus) menuAnchor.focus();
  }
  menuAnchor = null;
}
function showMenu(anchor, choices, onSelect) {
  closeMenu(); menuAnchor = anchor;
  elements.popover.replaceChildren();
  choices.forEach(choice => {
    const button = document.createElement("button"); button.textContent = choice; button.setAttribute("role", "menuitem");
    button.addEventListener("click", () => { closeMenu(true); onSelect(choice); });
    elements.popover.append(button);
  });
  anchor.setAttribute("aria-expanded", "true");
  elements.popover.classList.remove("hidden");
  const box = anchor.getBoundingClientRect();
  const width = Math.min(300, innerWidth - 24);
  elements.popover.style.width = `${width}px`;
  elements.popover.style.maxHeight = `${Math.max(80, innerHeight - 24)}px`;
  const natural = elements.popover.scrollHeight + 2;
  const below = innerHeight - box.bottom - 20, above = box.top - 20;
  const upward = below < Math.min(natural, 200) && above > below;
  const height = Math.min(natural, Math.max(80, upward ? above : below), innerHeight - 24);
  elements.popover.style.maxHeight = `${height}px`;
  elements.popover.style.left = `${Math.max(12, Math.min(box.right - width, innerWidth - width - 12))}px`;
  elements.popover.style.top = `${Math.max(12, Math.min(upward ? box.top - height - 8 : box.bottom + 8, innerHeight - height - 12))}px`;
  elements.popover.firstElementChild?.focus();
}
function updateSendButton() {
  const pending = activeSession().pending;
  const button = $("#sendButton");
  button.classList.toggle("stop", Boolean(pending));
  button.title = pending ? "停止本地等待；服务端可能继续生成" : "发送";
  button.setAttribute("aria-label", pending ? "停止本地等待" : "发送");
  button.replaceChildren();
  if (pending) button.textContent = "■";
  else { const icon = document.createElement("img"); icon.src = "/desktop-assets/Icons/send-arrow.svg"; icon.alt = ""; button.append(icon); }
}
async function copyText(text, button) {
  if (button?.disabled) return;
  const label = button?.textContent;
  if (button) button.disabled = true;
  try {
    await navigator.clipboard.writeText(text);
    if (button) {
      button.textContent = "已复制"; button.dataset.copied = "true";
      setTimeout(() => { button.textContent = label; button.disabled = false; delete button.dataset.copied; }, 1400);
    }
  } catch {
    if (button) button.disabled = false;
    showToast("复制不可用，请选中文字后复制。");
  }
}
function isNearLatest() {
  return elements.conversation.scrollHeight - elements.conversation.scrollTop - elements.conversation.clientHeight <= 48;
}
function updateLatestButton() {
  elements.latest.classList.toggle("hidden", elements.conversation.classList.contains("hidden") || isNearLatest());
}
function goToLatest() {
  const session = activeSession();
  session.followLatest = true;
  elements.conversation.scrollTop = elements.conversation.scrollHeight;
  session.scrollTop = elements.conversation.scrollTop;
  updateLatestButton();
}
function renderSession(forceScroll = false) {
  const session = activeSession();
  if (presentedSession === session) rememberConversationPosition();
  const followLatest = forceScroll || session.followLatest;
  const previousScrollTop = session.scrollTop;
  const hasMessages = session.messages.length > 0;
  elements.empty.classList.toggle("conversation-active", hasMessages);
  elements.conversation.classList.toggle("hidden", !hasMessages);
  elements.title.textContent = session.workspace ? `${session.title} · 演示项目` : session.title;
  elements.workspace.querySelector("span").textContent = session.workspace ? `${session.workspace} · 演示` : "选择示例项目";
  elements.prompt.value = session.draft;
  $("#permissionButton span").textContent = "文本对话";
  $("#permissionButton img").src = "/desktop-assets/Icons/permission-ask.svg";
  elements.messages.replaceChildren();
  for (const message of session.messages) {
    const node = document.createElement("article"); node.className = `message ${message.role} ${message.status ?? ""}`;
    const content = document.createElement("div"); content.className = "message-content";
    content.textContent = message.status === "waiting" ? "正在等待模型回复…"
      : message.status === "failed" ? `调用失败：${message.error}`
      : message.status === "interrupted" ? "已停止本地等待。服务端可能仍在生成，请先确认状态再重新请求。"
      : message.content;
    node.append(content);
    const actions = document.createElement("div"); actions.className = "message-actions";
    if (message.content) {
      const copy = document.createElement("button"); copy.textContent = "复制";
      copy.addEventListener("click", () => copyText(message.content, copy)); actions.append(copy);
    }
    if (message.role === "assistant" && ["failed", "interrupted"].includes(message.status)) {
      const retry = document.createElement("button"); retry.textContent = message.status === "interrupted" ? "重新请求" : "重试";
      retry.disabled = Boolean(session.pending);
      retry.title = message.status === "interrupted" ? "上次请求可能仍在服务端运行，重新请求会发起一次调用。" : "重新请求这条回复";
      retry.addEventListener("click", () => performReply(session, message)); actions.append(retry);
    }
    if (actions.childElementCount) node.append(actions);
    elements.messages.append(node);
  }
  updateSendButton(); renderDetails();
  presentedSession = session;
  elements.conversation.scrollTop = followLatest ? elements.conversation.scrollHeight : previousScrollTop;
  session.scrollTop = elements.conversation.scrollTop; session.followLatest = followLatest;
  updateLatestButton();
}
async function performReply(session, message) {
  if (session.pending) return;
  const controller = new AbortController();
  const operation = { controller, message };
  session.pending = operation; message.status = "waiting"; message.error = "";
  if (activeSession() === session) renderSession();
  try {
    const reply = await api("/api/chat", {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify(message.request),
      signal: AbortSignal.any([controller.signal, AbortSignal.timeout(300000)])
    });
    if (session.pending !== operation) return;
    message.content = reply.content; message.status = "completed";
  } catch (error) {
    if (session.pending !== operation) return;
    message.status = controller.signal.aborted ? "interrupted" : "failed";
    message.error = error.name === "TimeoutError" ? "等待超时，服务端可能仍在生成。" : error.message;
  } finally {
    if (session.pending === operation) session.pending = null;
    if (activeSession() === session) renderSession();
    renderRecents();
  }
}
function stopWaiting() {
  const session = activeSession();
  if (!session.pending) return;
  session.pending.controller.abort();
  session.pending.message.status = "interrupted";
  session.pending = null;
  renderSession();
}
function send() {
  const session = activeSession();
  if (session.pending) { stopWaiting(); return; }
  const value = elements.prompt.value.trim();
  if (!value) { elements.prompt.focus(); return; }
  if (!selectedModel || serviceState !== "ready") { showToast(serviceState === "failed" ? "模型网关不可用，请刷新或检查配置。" : "请先配置并选择模型"); return; }
  const user = { role: "user", content: value };
  const assistant = { role: "assistant", content: "", status: "waiting", request: {
    conversationId: session.id, message: value, provider: selectedModel.provider,
    model: selectedModel.model, permissionMode: session.permission
  } };
  if (!session.messages.length && !session.workspace) session.title = value.length > 22 ? `${value.slice(0, 22)}…` : value;
  session.messages.push(user, assistant); session.draft = "";
  elements.prompt.value = ""; renderRecents();
  renderSession(true);
  void performReply(session, assistant);
}
function renderDetails() {
  const session = activeSession();
  $("#detailWorkspace").textContent = session.workspace ? `${session.workspace} · 演示项目` : "本页工作 · 未关联文件夹";
  $("#detailModel").textContent = selectedModel?.label || "尚未选择模型";
  const body = $("#detailBody"); body.replaceChildren();
  const section = (title, text) => {
    const container = document.createElement("section");
    const heading = document.createElement("h3"); heading.textContent = title;
    const paragraph = document.createElement("p"); paragraph.textContent = text;
    container.append(heading, paragraph); body.append(container); return container;
  };
  if (detailTab === "tasks") {
    const count = session.messages.filter(message => message.role === "user").length;
    const errors = session.messages.filter(message => ["failed", "interrupted"].includes(message.status)).length;
    section("当前对话", `${count} 条提问${session.pending ? " · 正在等待回复" : errors ? ` · ${errors} 条回复未完成` : ""}`).className = "detail-summary";
    section("任务清单待接入", "当前可以讨论目标与计划。任务执行和审批服务尚未接入，不会自动运行工具或修改文件。");
  } else if (detailTab === "files") {
    section("尚无关联文件", session.workspace ? "这是演示项目，没有读取本机文件。桌面端可手动刷新关联目录和预览小型文本文件；网页预览不读取本机文件。" : "当前工作未关联本机文件夹。浏览器预览不读写你的项目文件。");
  } else {
    const replies = session.messages.filter(message => message.role === "assistant" && message.status === "completed");
    if (!replies.length) section("还没有结果", "模型完成回复后，可在这里查看最近回复并复制。");
    else {
      const last = replies.at(-1);
      const result = section("最近的模型回复", last.content); result.className = "detail-summary";
      const copy = document.createElement("button"); copy.className = "subtle-button detail-copy"; copy.textContent = "复制回复";
      copy.addEventListener("click", () => copyText(last.content, copy)); result.append(copy);
      section("产物待接入", "模型文本回复不代表任务已经执行，也不会自动保存为项目文件。");
    }
  }
}
function setDetails(open) {
  detailsOpen = open;
  const visible = open && mode === "work";
  $("#contentLayout").classList.toggle("details-open", visible);
  $("#workDetails").hidden = !visible;
  $("#detailsToggle").setAttribute("aria-expanded", String(visible));
  renderDetails();
}
function setSidebar(open) {
  const narrow = matchMedia("(max-width: 760px)").matches;
  elements.window.classList.toggle("sidebar-open", narrow && open);
  elements.window.classList.toggle("sidebar-collapsed", !narrow && !open);
  $("#sidebarBackdrop").classList.toggle("hidden", !(narrow && open));
  $("#sidebarToggle").setAttribute("aria-expanded", String(open));
}
function closeNarrowSidebar() { if (matchMedia("(max-width: 760px)").matches) setSidebar(false); }
function showToast(message) {
  clearTimeout(toastTimer); elements.toast.textContent = message; elements.toast.classList.remove("hidden");
  toastTimer = setTimeout(() => elements.toast.classList.add("hidden"), 2600);
}

elements.modes.forEach(button => button.addEventListener("click", () => { setMode(button.dataset.mode); closeNarrowSidebar(); }));
elements.search.addEventListener("input", () => {
  searchQueries[mode] = elements.search.value; renderRecents();
});
$("#clearSearch").addEventListener("click", () => {
  searchQueries[mode] = ""; elements.search.value = ""; renderRecents(); elements.search.focus();
});
elements.conversation.addEventListener("scroll", () => { rememberConversationPosition(); updateLatestButton(); }, { passive: true });
elements.latest.addEventListener("click", goToLatest);
new ResizeObserver(() => {
  if (presentedSession === activeSession() && presentedSession.followLatest && !elements.conversation.classList.contains("hidden")) goToLatest();
  else updateLatestButton();
}).observe(elements.conversation);
$("#treeToggle").addEventListener("click", event => {
  const collapsed = elements.recents.classList.toggle("collapsed");
  event.currentTarget.setAttribute("aria-expanded", String(!collapsed));
});
$("#sidebarToggle").addEventListener("click", () => setSidebar($("#sidebarToggle").getAttribute("aria-expanded") !== "true"));
$("#sidebarBackdrop").addEventListener("click", () => setSidebar(false));
$("#expandComposer").addEventListener("click", event => {
  const expanded = elements.composer.classList.toggle("expanded");
  event.currentTarget.setAttribute("aria-expanded", String(expanded));
});
$("#sendButton").addEventListener("click", send);
elements.prompt.addEventListener("input", () => { activeSession().draft = elements.prompt.value; });
elements.prompt.addEventListener("compositionstart", () => { composingPrompt = true; });
elements.prompt.addEventListener("compositionend", () => { composingPrompt = false; suppressComposingEnter = true; });
elements.prompt.addEventListener("keydown", event => {
  if (event.key !== "Enter") { suppressComposingEnter = false; return; }
  if (event.isComposing || composingPrompt || event.keyCode === 229 || suppressComposingEnter) { suppressComposingEnter = false; return; }
  if (event.shiftKey || activeSession().pending) return;
  event.preventDefault(); send();
});
elements.prompt.addEventListener("keyup", () => { if (!composingPrompt) suppressComposingEnter = false; });
document.querySelectorAll("[data-prompt]").forEach(button => button.addEventListener("click", () => {
  elements.prompt.value = button.dataset.prompt; activeSession().draft = elements.prompt.value; elements.prompt.focus();
}));
$("#modelButton").addEventListener("click", async event => {
  const anchor = event.currentTarget;
  try {
    const choices = await refreshModels();
    showMenu(anchor, [...choices.map(item => item.label), "配置模型连接…"], value => {
      const choice = choices.find(item => item.label === value);
      if (choice) { selectedModel = choice; renderServiceState(); } else void openModels();
    });
  } catch { showMenu(anchor, ["重新连接网关", "配置模型连接…"], value => {
    if (value === "重新连接网关") void refreshModels().catch(error => showToast(error.message)); else void openModels();
  }); }
});
$("#permissionButton").addEventListener("click", () => showInfo("当前能力", "目前可以与模型进行文本对话。文件读写、联网工具和自动执行暂未开放，选择项目不会授予模型操作电脑的权限。"));
elements.workspace.addEventListener("click", event => showMenu(event.currentTarget, projects.map(([name]) => name), value => selectWorkspace(value)));
elements.newItem.addEventListener("click", createCurrentSession);
document.querySelectorAll("[data-planned]").forEach(button => button.addEventListener("click", () => showInfo(`${button.dataset.planned} · 规划中`, planned[button.dataset.planned])));
$("#settingsButton").addEventListener("click", () => showInfo("关于界面预览", "此页面用于预览 Windows 桌面界面。模型连接和文本调用使用本地网关；项目与任务示例不代表真实执行。工作和聊天分别保留本页会话，刷新后清空。完整项目管理与存储设置请使用 WinUI 桌面端。"));
$("#attachmentButton").addEventListener("click", () => showInfo("文件功能待接入", "当前预览尚未接入文件选择、上传和项目文件操作。可以粘贴代码或文字讨论；文件浏览将在桌面端实现。"));
$("#manageModels").addEventListener("click", openModels);
$("#configureStatus").addEventListener("click", openModels);
$("#refreshStatus").addEventListener("click", () => void refreshModels().catch(error => showToast(error.message)));
$("#closeInfo").addEventListener("click", () => $("#infoDialog").close());
$("#confirmInfo").addEventListener("click", () => $("#infoDialog").close());
$("#detailsToggle").addEventListener("click", () => setDetails(!detailsOpen));
$("#closeDetails").addEventListener("click", () => { setDetails(false); $("#detailsToggle").focus(); });
document.querySelectorAll("[data-detail]").forEach(button => button.addEventListener("click", () => {
  detailTab = button.dataset.detail;
  document.querySelectorAll("[data-detail]").forEach(item => {
    item.classList.toggle("active", item === button); item.setAttribute("aria-selected", String(item === button));
  });
  renderDetails();
}));
$("#closeModels").addEventListener("click", () => $("#modelDialog").close());
$("#modelDialog").addEventListener("close", () => { modelOperation?.abort(); $("#modelForm").elements.namedItem("apiKey").value = ""; testedRevision = null; formRevision++; });
$("#newProvider").addEventListener("click", () => setConnection(null));
$("#ollamaPreset").addEventListener("click", () => setConnection({ providerId: "ollama", displayName: "Ollama", baseUrl: "http://127.0.0.1:11434/v1" }));
$("#customPreset").addEventListener("click", () => setConnection(null));
$("#modelForm").addEventListener("input", () => {
  if (editingProviderId === null) newConnectionEdited = true;
  formRevision++;
  if (testedRevision !== null && testedRevision !== formRevision) { testedRevision = null; setModelStatus("配置已修改，上次测试结果已过期。"); }
});
$("#probeModels").addEventListener("click", async () => {
  if (modelBusy || !$("#modelForm").reportValidity()) return;
  const input = formConnection();
  setModelBusy(true); setModelStatus("正在测试模型列表接口…");
  const controller = new AbortController(); modelOperation = controller;
  try {
    const result = await api("/api/models/test", { method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify(input), signal: AbortSignal.any([controller.signal, AbortSignal.timeout(15000)]) });
    if (result.models.length) $("#modelForm").elements.namedItem("models").value = result.models.join("\n");
    setModelBusy(false);
    testedRevision = formRevision;
    setModelStatus(`模型列表接口可用 · ${result.latencyMs} ms · ${result.models.length} 个模型。测试时间 ${new Date().toLocaleTimeString()}；尚未测试生成。`);
  } catch (error) { if (!controller.signal.aborted) setModelStatus(`测试失败：${error.message}。可手动填写 Model ID。`, true); }
  finally { if (modelOperation === controller) modelOperation = null; setModelBusy(false); }
});
$("#modelForm").addEventListener("submit", async event => {
  event.preventDefault();
  if (modelBusy) return;
  const input = formConnection();
  setModelBusy(true); setModelStatus("正在保存连接…");
  const controller = new AbortController(); modelOperation = controller;
  try {
    if (editingProviderId === null) {
      // A new form must check the latest catalog before using an ID as a create target.
      await refreshModels(); renderProviders();
      if (controller.signal.aborted) return;
      if (providers.some(provider => provider.providerId === input.providerId)) {
        setModelStatus(`连接 ID “${input.providerId}” 已存在。请在高级设置填写其他 ID，或从连接列表选择已有连接进行编辑。`, true);
        return;
      }
    }
    const result = await api("/api/models", { method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify(input), signal: AbortSignal.any([controller.signal, AbortSignal.timeout(15000)]) });
    if (controller.signal.aborted) return;
    setConnection(result.provider, true);
    try { await refreshModels(); renderProviders(); }
    catch { setModelStatus(`已保存 ${result.provider.displayName}，但刷新列表失败。请返回主界面刷新。`, true); return; }
    setModelStatus(`已保存 ${result.provider.displayName}。返回聊天页，选择模型即可使用。`);
  } catch (error) { if (!controller.signal.aborted) setModelStatus(`保存失败：${error.message}`, true); }
  finally { if (modelOperation === controller) modelOperation = null; setModelBusy(false); }
});
document.addEventListener("pointerdown", event => {
  if (!elements.popover.contains(event.target) && !event.target.closest("#modelButton, #permissionButton, #workspaceButton")) closeMenu();
});
document.addEventListener("keydown", event => {
  if ($("#modelDialog").open || $("#infoDialog").open || event.isComposing || event.keyCode === 229) return;
  if (event.key === "Escape" && menuAnchor) { event.preventDefault(); event.stopPropagation(); closeMenu(true); }
  else if (event.key === "Escape" && elements.window.classList.contains("sidebar-open")) { event.preventDefault(); setSidebar(false); $("#sidebarToggle").focus(); }
  else if (menuAnchor && ["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
    const items = [...elements.popover.querySelectorAll("button")];
    const index = items.indexOf(document.activeElement);
    const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1
      : (index + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
    items[next]?.focus(); event.preventDefault();
  }
  else if (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey) {
    const key = event.key.toLocaleLowerCase();
    if (!["n", "l", "f"].includes(key)) return;
    event.preventDefault(); closeMenu();
    if (key === "n") { if (!event.repeat) createCurrentSession(); }
    else if (key === "l") { closeNarrowSidebar(); elements.prompt.focus(); }
    else focusSidebarSearch();
  }
});
addEventListener("resize", () => { closeMenu(); setSidebar(!matchMedia("(max-width: 760px)").matches && !elements.window.classList.contains("sidebar-collapsed")); });
addEventListener("pagehide", () => {
  for (const session of [...workSessions.values(), ...chatSessions]) session.pending?.controller.abort();
  modelOperation?.abort();
});
setMode("work");
setSidebar(!matchMedia("(max-width: 760px)").matches);
void refreshModels().catch(() => {});
