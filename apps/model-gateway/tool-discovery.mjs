const computerAliases = {
  windows: 'windows window list 窗口 窗口列表 当前窗口',
  apps: 'apps applications programs chrome edge firefox 浏览器 软件 应用 程序 软件列表 本机浏览器',
  launch: 'launch open start chrome edge firefox browser 打开 启动 软件 应用 浏览器 本机浏览器 本地浏览器 可见窗口 打开界面',
  window: 'window resize maximize minimize restore 调整窗口 窗口大小 放大 缩小 最大化 最小化 恢复窗口',
  activate: 'activate focus foreground 激活 切换 前台 窗口',
  read: 'read inspect accessible UI text browser content 读取 查看 浏览器内容 网页内容 页面内容 窗口文本',
  screenshot: 'screenshot capture screen 截图 截屏 屏幕截图 窗口截图',
  move: 'move mouse 鼠标 移动',
  click: 'click mouse 点击 鼠标',
  scroll: 'scroll wheel 滚动 鼠标',
  drag: 'drag mouse 拖动 拖拽 鼠标',
  type: 'type input keyboard 输入 键盘 文本',
  key: 'key shortcut keyboard 按键 快捷键 键盘'
};

function normalized(value, maximum = 200) {
  return typeof value === 'string' ? value.slice(0, maximum).normalize('NFKC').trim().toLowerCase() : '';
}

/** Classification helps discovery only; it neither proves runtime availability nor grants permission. */
export function toolDiscoveryCategory(tool) {
  const name = normalized(tool?.name, 256), description = normalized(tool?.description, 2000);
  if (name.startsWith('computer.')) return 'computer';
  if (name === 'terminal.host.run') return 'host-terminal';
  if (name === 'terminal.run') return 'sandbox-terminal';
  if (name === 'web.fetch') return 'web-fetch';
  if (/playwright|chrome[-_.]?devtools|puppeteer|(?:^|[._-])browser(?:[._-]|$)/.test(name) ||
      /browser automation|browser debugging/.test(description)) return 'browser';
  if (/web[-_]?search|search[-_]?web|search[-_]?news|news[-_]?search/.test(name) ||
      /public web search|search (?:the )?(?:web|internet)/.test(description)) return 'web-search';
  if (/^filesystem\./.test(name)) return 'filesystem';
  return 'other';
}

function aliases(tool) {
  const name = normalized(tool.name, 256), category = toolDiscoveryCategory(tool);
  if (category === 'computer') return 'computer desktop local host 本机 本地 桌面 桌面控制 ' +
    (computerAliases[name.slice('computer.'.length)] ?? '');
  if (category === 'host-terminal') return 'host terminal local terminal visible terminal cmd powershell conda 本机终端 本地终端 可见终端 显示终端 终端窗口 宿主终端 本机命令 本地命令 命令提示符';
  if (category === 'sandbox-terminal') return 'sandbox terminal node 沙箱终端 隔离终端 运行代码 执行代码';
  if (category === 'web-search') return 'web search internet 搜索 联网 搜索网页 查资料 查证';
  if (category === 'web-fetch') return 'web fetch public webpage read url 页面 网页 网站 阅读网页 读取网页 网页内容 网址 获取正文';
  if (category === 'filesystem') return 'file filesystem 文件 文件夹 工作目录 ' +
    ({ read: '读取 查看', list: '列表 列出', search: '搜索 查找', write: '写入 创建', edit: '编辑 修改', delete: '删除', stat: '属性 信息', mkdir: '目录 创建文件夹' }[name.split('.').at(-1)] ?? '');
  if (category !== 'browser') return '';
  let value = 'browser chrome devtools playwright 浏览器 网页 网站 网址 浏览器自动化';
  if (/navigate|new_page|new[-_]tab/.test(name)) value += ' navigate open visit 导航 打开 访问 网页 页面';
  if (/snapshot|read|content|evaluate[-_]script/.test(name)) value += ' read inspect content 读取 查看 网页内容 页面内容 浏览器内容';
  if (/evaluate|script/.test(name)) value += ' evaluate script javascript 脚本 执行脚本';
  if (/screenshot|capture/.test(name)) value += ' screenshot capture 截图 截屏';
  if (/click/.test(name)) value += ' click 点击';
  if (/type|fill/.test(name)) value += ' type fill 输入 填写';
  return value;
}

function queryTerms(query) {
  const terms = new Set(query.match(/[a-z0-9]+(?:[_-][a-z0-9]+)*/g) ?? []);
  for (const phrase of query.match(/\p{Script=Han}{2,}/gu) ?? []) {
    terms.add(phrase);
    const characters = Array.from(phrase);
    for (let index = 0; index < characters.length - 1 && terms.size < 32; index++)
      terms.add(characters[index] + characters[index + 1]);
  }
  return [...terms].slice(0, 32);
}

/** Enabled descriptors, deterministic ranking and no execution. An empty query preserves catalog order for paging. */
export function searchTools(descriptors, query = '') {
  const enabled = descriptors.filter(tool => tool.enabled !== false), text = normalized(query);
  if (!text) return enabled;
  const terms = queryTerms(text);
  const identityQuery = /^[a-z0-9_-]+(?:\.[a-z0-9_-]+)+$/.test(text);
  return enabled.map((tool, index) => {
    const name = normalized(tool.name, 256), description = normalized(tool.description, 8000), alias = aliases(tool);
    // A complete dotted tool identity is not a bag of generic provider/name words.
    // If that identity is absent or disabled, unrelated tools must not look like replacements.
    if (identityQuery && !name.includes(text)) return { tool, index, score: 0 };
    let score = name === text ? 100000 : name.includes(text) ? 10000 : 0;
    if (description.includes(text) || alias.includes(text)) score += 1000;
    let matches = 0;
    for (const term of terms) {
      const inName = name.includes(term), inDescription = description.includes(term), inAlias = alias.includes(term);
      if (inName || inDescription || inAlias) {
        matches++;
        score += inName ? 40 : inAlias ? 24 : 8;
      }
    }
    if (matches) score += matches * 100 + (matches === terms.length ? 300 : 0);
    return { tool, index, score };
  }).filter(item => item.score > 0).sort((left, right) => right.score - left.score || left.index - right.index)
    .map(item => item.tool);
}

function currentSignals(message) {
  const text = normalized(message, 2000), terms = queryTerms(text);
  const browser = /浏览器|网页|网站|网址|页面|chrome|firefox|playwright|devtools|\bedge\b|\bbrowser\b/.test(text);
  const remoteBrowser = /(?:云端|远程).{0,16}浏览器|\b(?:remote|cloud)\s+browser\b/.test(text);
  const signals = { terms,
    web: /最新|最近|当前|现在|今天|今日|新闻|官网|官方|上线|发布|搜索|查询|查证|联网|搜一下|是谁|什么时候|什么时间|多少钱/.test(text) ||
      /\b(?:latest|current|recent|today|news|official|released?|announced?|search|who|when|price|weather)\b|\blook\s+up\b/.test(text),
    docs: /文档|接口|代码|编程|开发|框架|库的|库怎么/.test(text) ||
      /\b(?:api|sdk|docs?|documentation|library|libraries|framework|programming|code|typescript|python|dotnet|winui|react)\b/.test(text),
    desktop: /截图|截屏|屏幕|鼠标|键盘|桌面|打开软件|打开应用|控制本机|浏览器|记事本|计算器|打开界面|可见窗口|调整窗口|窗口大小|最大化|最小化|恢复窗口/.test(text) ||
      /(?:打开|访问|浏览|查看|看看).{0,30}(?:网站|网页|网址|页面)/.test(text) ||
      /(?:打开|启动|显示|使用|控制).{0,30}(?:chrome|edge|firefox|google)|\b(?:open|launch|show|use|control)\s+(?:the\s+)?(?:local\s+)?(?:chrome|edge|firefox|google)\b/.test(text) ||
      /(?:打开|访问|浏览|查看|看看|\bopen\b|\bvisit\b).{0,40}(?:[a-z0-9-]+\.)+[a-z]{2,}/.test(text) ||
      /(?:本机|本地).{0,30}(?:登录|界面|窗口|软件|应用)/.test(text) ||
      /\b(?:screenshot|desktop|mouse|browser|notepad|calculator|launch|computer)\b/.test(text),
    browser, remoteBrowser,
    hostTerminal: /本机终端|本地终端|本机命令|本地命令|宿主终端|conda|powershell|cmd|命令提示符/.test(text) ||
      /\b(?:host|local|visible)\b.*\b(?:terminal|command)\b|\bterminal\b.*\b(?:window|visible)\b/.test(text) ||
      /(?:打开|显示|可见|可以看到|看得到).{0,20}终端|终端.{0,20}(?:窗口|可见|可以看到|看得到)/.test(text),
    url: /https?:\/\/\S+/i.test(text) };
  if (remoteBrowser && !/本机桌面|本地窗口|\blocal desktop\b/.test(text)) signals.desktop = false;
  return signals;
}

function browserBoundary(text) {
  const local = [...text.matchAll(/(?:本机|本地|可见|我的|自己的).{0,16}(?:浏览器|chrome|edge|窗口)|\blocal\s+(?:browser|chrome|edge)\b/g)].at(-1);
  const remote = [...text.matchAll(/(?:云端|远程).{0,16}浏览器|\b(?:remote|cloud)\s+browser\b/g)].at(-1);
  if (local || remote) return local && (!remote || local.index > remote.index) ? 'local' : 'remote';
  // A plain request to open an installed browser is a visible local launch;
  // an explicit remote boundary in the same message takes precedence above.
  return /(?:打开|启动|显示).{0,12}(?:chrome|edge|firefox|google)|\b(?:open|launch|show)\s+(?:the\s+)?(?:chrome|edge|firefox)\b/.test(text) ? 'local' : null;
}

/** Recent subject hints select schemas only. They never import old approval, arguments or capability status. */
export function toolSelectionSignals(message, { historySignals = [], previousToolNames = [] } = {}) {
  const text = normalized(message, 2000), signals = currentSignals(text);
  const currentBoundary = browserBoundary(text);
  const recent = Array.isArray(historySignals) ? historySignals.slice(-3).filter(item => typeof item === 'string')
    .map(item => normalized(item, 1000)) : [];
  const priorNames = Array.isArray(previousToolNames) ? previousToolNames.slice(-32)
    .filter(name => typeof name === 'string' && /^[a-zA-Z0-9._-]{1,256}$/.test(name)) : [];
  const followup = text.length <= 240 && /继续|再来|重试|再次|再试|尝试|打不开|还是|那|查看|看看|访问|打开|登录|界面|本机|本地|\b(?:again|retry|continue|it|that|this|open|view|visit)\b/.test(text);
  const explicitNewTask = /新任务|换个话题|另外一件|另一个问题|\b(?:new task|different topic)\b/.test(text) ||
    signals.docs && !signals.desktop && !signals.browser && !signals.hostTerminal;
  const retainedNames = new Set();
  let boundary = currentBoundary;
  if (followup && !explicitNewTask) {
    const previous = recent.map(currentSignals);
    for (const key of ['desktop', 'browser', 'hostTerminal'])
      signals[key] ||= previous.some(item => item[key]);
    boundary ??= [...recent].reverse().map(browserBoundary).find(item => item !== null) ?? null;
    for (const name of priorNames) {
      retainedNames.add(name);
      const category = toolDiscoveryCategory({ name });
      if (category === 'computer') signals.desktop = true;
      else if (category === 'host-terminal') signals.hostTerminal = true;
      else if (category === 'browser') signals.browser = true;
    }
  }
  // The latest explicit boundary wins over older text and attempted tool names.
  // Names can keep a schema discoverable, but cannot turn a local retry into remote control.
  if (boundary) {
    signals.remoteBrowser = boundary === 'remote';
    signals.browser = true;
    signals.desktop = boundary === 'local';
  }
  if (signals.remoteBrowser && !/本机桌面|本地窗口|\blocal desktop\b/.test(text)) signals.desktop = false;
  return { ...signals, retainedNames };
}
