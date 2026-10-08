import { wireCatalog } from '../models/tool-protocols.mjs';

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
  type: 'type input fill enter text paste caret focused field keyboard 输入 键入 填写 填入 填进 粘贴 光标 输入框 键盘 文本',
  key: 'key shortcut keyboard 按键 快捷键 键盘'
};

function normalized(value, maximum = 200) {
  return typeof value === 'string' ? value.slice(0, maximum).normalize('NFKC').trim().toLowerCase() : '';
}

/**
 * Classification helps discovery only; it neither proves runtime availability nor grants permission.
 * 分类只辅助发现，不证明运行时可用性，也不授予权限。
 */
export function toolDiscoveryCategory(tool) {
  const name = normalized(tool?.name, 256), description = normalized(tool?.description, 2000);
  if (name.startsWith('computer.')) return 'computer';
  if (name.startsWith('terminal.host.')) return 'host-terminal';
  if (name === 'terminal.run') return 'sandbox-terminal';
  if (name === 'web.fetch') return 'web-fetch';
  if (/playwright|chrome[-_.]?devtools|puppeteer|(?:^|[._-])browser(?:[._-]|$)/.test(name) ||
      /browser automation|browser debugging/.test(description)) return 'browser';
  if (/web[-_]?search|search[-_]?web|search[-_]?news|news[-_]?search/.test(name) ||
      /public web search|search (?:the )?(?:web|internet)/.test(description)) return 'web-search';
  if (/^filesystem\./.test(name)) return 'filesystem';
  return 'other';
}

function aliases(tool, { includeDeviceState = true } = {}) {
  const name = normalized(tool.name, 256), category = toolDiscoveryCategory(tool);
  if (category === 'computer') return 'computer desktop local host 本机 本地 桌面 桌面控制 ' +
    (computerAliases[name.slice('computer.'.length)] ?? '');
  if (category === 'host-terminal') return 'host terminal local terminal visible terminal background process job cmd powershell conda 本机终端 本地终端 可见终端 显示终端 终端窗口 宿主终端 本机命令 本地命令 命令提示符 后台进程 后台任务 监控进程 ' +
    ({ start: 'start launch 启动 运行', read: 'read inspect poll output 读取 查看 输出 状态', stop: 'stop cancel terminate 停止 关闭 终止 取消' }[name.split('.').at(-1)] ?? '') +
    (includeDeviceState && name === 'terminal.host.run' ? ' device status current network my ip address public ip proxy dns adapter interfaces ipconfig tasklist processes listening port netstat get-nettcpconnection 本机状态 我的ip 当前网络 网络配置 代理设置 代理配置 dns设置 dns配置 dns服务器 网卡 网络适配器 本机进程 进程列表 端口占用 监听端口 哪个程序占端口' : '');
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
  if (/type|fill/.test(name)) value += ' type fill input enter text paste caret focused field 输入 键入 填写 填入 填进 粘贴 光标 输入框';
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

/** Local state and public-IP explanation are different discovery subjects, not different permission levels.
 * 本机状态与公网 IP/网络概念属于不同发现主题，不改变终端权限、审批或运行时可用性。 */
function deviceStateTopic(text) {
  const command = /\b(?:ipconfig|netstat|tasklist|get-process|get-nettcpconnection|get-netadapter|get-netipconfiguration|ping|tracert|traceroute|nslookup|resolve-dnsname|test-netconnection)\b/.test(text);
  const topic = command || /代理|网络|网卡|适配器|进程|端口|\b(?:ip(?:v[46])?|dns|proxy|network|adapters?|interfaces?|process(?:es)?|ports?)\b/.test(text);
  if (!topic) return null;
  const localOwner = /(?:我的|本机|本地|这台(?:电脑|计算机|机器|设备)|当前(?:电脑|设备|机器)).{0,24}(?:ip(?:v[46])?\b|dns\b|代理|网络|网卡|适配器|进程|端口)/.test(text) ||
    /\b(?:my|this (?:computer|machine|device|pc)|local(?:host)?|host)\b.{0,40}\b(?:ip(?:v[46])?|dns|proxy|network|adapters?|interfaces?|process(?:es)?|ports?)\b/.test(text);
  const inspection = /查(?:询|看|一下|查)?|检查|检测|看看|列出|显示|读取|获取|监控|\b(?:check|inspect|list|show|read|detect|find|fetch|monitor)\b/.test(text);
  const explanation = /科普|原理|概念|教程|定义|区别|行业|趋势|(?:如何|怎样|怎么).{0,16}(?:查看|查询|配置|设置|检查|检测)|(?:解释|介绍).{0,20}(?:ip|dns|代理|网络|网卡|进程|端口|netstat|tasklist|ping|nslookup)/.test(text) ||
    /(?:ip|dns|代理|网络|网卡|进程|端口|netstat|tasklist|ping|nslookup).{0,12}(?:是什么|什么意思|有什么作用|工作原理)/.test(text) && !localOwner ||
    /\b(?:explain|tutorial|concept|meaning|difference|trends?)\b|\bhow\s+(?:to|does|do)\b/.test(text) ||
    /\bwhat\s+is\s+(?:an?\s+|the\s+)?(?:ip(?:v[46])?|dns|a proxy|a network|a process|a port)\b/.test(text);
  const scriptOnly = /(?:写|生成|提供|给我).{0,12}(?:代码|脚本)|\b(?:write|generate|provide)\b.{0,20}\b(?:code|script)\b/.test(text) &&
    !/(?:并|然后)(?:执行|运行)|\brun\s+(?:it|the script)\b/.test(text);
  if (scriptOnly || explanation) return 'explanation';
  const publicLookup = /归属|属地|运营商|所在地|地理位置|哪里的|是谁的|\b(?:whois|geolocation|ownership)\b|\b(?:who owns|where is|ip location|lookup this ip)\b/.test(text);
  const specifiedIp = /(?:^|[^0-9])(?:[0-9]{1,3}\.){3}[0-9]{1,3}(?:$|[^0-9])/.test(text) ||
    /\b(?:ipv6|ip address)\b.{0,12}[a-f0-9]+:[a-f0-9:]+/u.test(text);
  if (specifiedIp && publicLookup || !localOwner && (publicLookup || specifiedIp && !command)) return 'external';
  const localStatus = /当前(?:的)?(?:ip|网络|代理|dns|网卡|进程)|现在(?:的)?网络|当前(?:使用|运行).{0,12}(?:代理|dns|进程)|网络(?:配置|状态)|代理(?:配置|设置)|dns(?:配置|设置|服务器)|网卡|网络适配器|进程列表|运行中的进程|列出(?:所有|当前|本机|本地)?进程|端口占用|监听端口/.test(text) ||
    /\b(?:current (?:network|proxy|dns|ip|processes)|network (?:status|configuration)|proxy (?:settings|configuration)|dns (?:settings|servers?|configuration)|network adapters?|running processes|list processes|listening ports?)\b/.test(text);
  const directStatus = /^(?:(?:帮我|请)\s*)?(?:查看|检查|检测|查询|查一下|看看|列出|显示|读取)\s*(?:当前(?:的)?网络|代理(?:配置|设置)?|dns(?:配置|设置)?|网卡|网络适配器|本机进程|进程列表|端口占用)[呢吗?？!！。.\s]*$/.test(text) ||
    /^(?:当前(?:的)?(?:ip|网络)|网络(?:配置|状态)|代理(?:配置|设置)|dns(?:配置|设置)|进程列表|端口占用|current network|network status)[呢吗?？!！。.\s]*$/.test(text) ||
    /^(?:please\s+)?(?:check|inspect|list|show|read)\s+(?:(?:my|current|local)\s+)?(?:proxy|dns|network|network adapters?|processes|listening ports?)[?.!\s]*$/.test(text);
  const portOwner = /(?:哪个|什么|谁).{0,12}(?:程序|进程|软件|应用)?.{0,12}(?:占用|占了|占|监听).{0,16}(?:端口|port)|端口.{0,12}(?:谁|哪个(?:程序|进程)).{0,12}(?:占用|占了|占|监听)/.test(text) ||
    /\b(?:which|what)\s+(?:process|program|application|app)\b.{0,32}\b(?:port|listening)\b|\bwhat(?:'s| is) using (?:port|[0-9]{2,5})\b/.test(text);
  return localOwner || command || inspection && localStatus || directStatus || portOwner ? 'device' : null;
}

/**
 * Enabled descriptors, deterministic ranking and no execution. An empty query preserves catalog order for paging.
 * 仅检索已启用描述符，排序确定且不执行；空查询按目录原顺序分页。
 */
export function searchTools(descriptors, query = '') {
  const enabled = descriptors.filter(tool => tool.enabled !== false), text = normalized(query);
  if (!text) return enabled;
  const terms = queryTerms(text);
  const deviceTopic = deviceStateTopic(text);
  const identityQuery = /^[a-z0-9_-]+(?:\.[a-z0-9_-]+)+$/.test(text);
  const wireIdentityQuery = /^k_[a-z0-9_]+_[a-f0-9]{8}$/.test(text);
  return enabled.map((tool, index) => {
    const name = normalized(tool.name, 256), description = normalized(tool.description, 8000), alias = aliases(tool,
      { includeDeviceState: !['external', 'explanation'].includes(deviceTopic) });
    // A complete dotted tool identity is not a bag of generic provider/name words.
    // If that identity is absent or disabled, unrelated tools must not look like replacements.
    // 完整带点工具身份不能拆成通用名称关键词；该身份缺失或禁用时，不能把无关工具当成替代项。
    if (identityQuery && !name.includes(text)) return { tool, index, score: 0 };
    if (wireIdentityQuery) return { tool, index, score: normalized(wireCatalog([tool])[0].wireName, 256) === text ? 100000 : 0 };
    let score = name === text ? 100000 : name.includes(text) ? 10000 : 0;
    if (deviceTopic === 'device' && name === 'terminal.host.run') score += 5000;
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
  const deviceTopic = deviceStateTopic(text);
  const deviceState = deviceTopic === 'device' &&
    (!remoteBrowser || /本机|本地|我的电脑|\blocal (?:computer|machine|device)\b/.test(text));
  const signals = { terms,
    web: /最新|最近|当前|现在|今天|今日|新闻|官网|官方|上线|发布|搜索|查询|查证|联网|搜一下|是谁|什么时候|什么时间|多少钱/.test(text) ||
      /\b(?:latest|current|recent|today|news|official|released?|announced?|search|who|when|price|weather)\b|\blook\s+up\b/.test(text),
    docs: /文档|接口|代码|编程|开发|框架|库的|库怎么/.test(text) ||
      /\b(?:api|sdk|docs?|documentation|library|libraries|framework|programming|code|typescript|python|dotnet|winui|react)\b/.test(text),
    desktop: /截图|截屏|屏幕|鼠标|键盘|光标|输入框|桌面|打开软件|打开应用|控制本机|浏览器|记事本|计算器|打开界面|可见窗口|调整窗口|窗口大小|最大化|最小化|恢复窗口/.test(text) ||
      /(?:输入|键入|填写|填入|填进|粘贴).{0,24}(?:窗口|界面|页面|输入框)|\b(?:type|enter|paste)\b.{0,40}\b(?:caret|focused field|text field|input field)\b/.test(text) ||
      /(?:打开|访问|浏览|查看|看看).{0,30}(?:网站|网页|网址|页面)/.test(text) ||
      /(?:打开|启动|显示|使用|控制).{0,30}(?:chrome|edge|firefox|google)|\b(?:open|launch|show|use|control)\s+(?:the\s+)?(?:local\s+)?(?:chrome|edge|firefox|google)\b/.test(text) ||
      /(?:打开|访问|浏览|查看|看看|\bopen\b|\bvisit\b).{0,40}(?:[a-z0-9-]+\.)+[a-z]{2,}/.test(text) ||
      /(?:本机|本地).{0,30}(?:登录|界面|窗口|软件|应用)/.test(text) ||
      /\b(?:screenshot|desktop|mouse|browser|notepad|calculator|launch|computer)\b/.test(text),
    browser, remoteBrowser, deviceState,
    hostTerminal: /本机终端|本地终端|本机命令|本地命令|宿主终端|conda|powershell|cmd|命令提示符/.test(text) ||
      /后台.{0,12}(?:终端|进程|任务)|(?:终端|进程).{0,12}(?:后台|监控)|\b(?:background|persistent)\s+(?:terminal|process|job)\b/.test(text) ||
      /\b(?:host|local|visible)\b.*\b(?:terminal|command)\b|\bterminal\b.*\b(?:window|visible)\b/.test(text) ||
      /(?:打开|显示|可见|可以看到|看得到).{0,20}终端|终端.{0,20}(?:窗口|可见|可以看到|看得到)/.test(text),
    url: /https?:\/\/\S+/i.test(text) };
  if (deviceTopic === 'external') signals.web = true;
  if (deviceState) {
    signals.hostTerminal = true;
    // Reading device state does not imply opening a GUI or performing an ordinary web search.
    // 读取设备状态不意味着打开界面或做普通网页搜索；用户明确要求的浏览器/界面动作仍保留。
    if (!browser && !/截图|截屏|屏幕|鼠标|键盘|光标|窗口|界面|打开|启动|\b(?:screenshot|mouse|keyboard|window|launch|open)\b/.test(text)) signals.desktop = false;
    if (!browser && !signals.url && !/归属|属地|运营商|\b(?:whois|geolocation|ownership)\b/.test(text)) signals.web = false;
  }
  if (remoteBrowser && !/本机桌面|本地窗口|\blocal desktop\b/.test(text)) signals.desktop = false;
  return signals;
}

function browserBoundary(text) {
  const local = [...text.matchAll(/(?:本机|本地|可见|我的|自己的).{0,16}(?:浏览器|chrome|edge|窗口)|\blocal\s+(?:browser|chrome|edge)\b/g)].at(-1);
  const remote = [...text.matchAll(/(?:云端|远程).{0,16}浏览器|\b(?:remote|cloud)\s+browser\b/g)].at(-1);
  if (local || remote) return local && (!remote || local.index > remote.index) ? 'local' : 'remote';
  // A plain request to open an installed browser is a visible local launch;
  // an explicit remote boundary in the same message takes precedence above.
  // 普通的打开已安装浏览器请求表示本机可见启动；同条消息明确指定远程边界时，以远程边界为准。
  return /(?:打开|启动|显示).{0,12}(?:chrome|edge|firefox|google)|\b(?:open|launch|show)\s+(?:the\s+)?(?:chrome|edge|firefox)\b/.test(text) ? 'local' : null;
}

/**
 * Recent subject hints select schemas only. They never import old approval, arguments or capability status.
 * 近期主题提示只选择 schema，不继承历史审批、参数或能力状态。
 * deviceState marks the current device-inspection subject or its short retry, never an execution grant.
 * deviceState 表示当前本机状态查询或其短续问，不表示执行授权或宿主工具就绪。
 */
export function toolSelectionSignals(message, { historySignals = [], previousToolNames = [] } = {}) {
  const text = normalized(message, 2000), signals = currentSignals(text);
  const currentBoundary = browserBoundary(text);
  const recent = Array.isArray(historySignals) ? historySignals.slice(-3).filter(item => typeof item === 'string')
    .map(item => normalized(item, 1000)) : [];
  const priorNames = Array.isArray(previousToolNames) ? previousToolNames.slice(-32)
    .filter(name => typeof name === 'string' && /^[a-zA-Z0-9._-]{1,256}$/.test(name)) : [];
  const previous = recent.map(currentSignals);
  const followup = text.length <= 240 && (/继续|再来|重试|再次|再试|尝试|打不开|还是|那|查看|看看|访问|打开|登录|界面|本机|本地|\b(?:again|retry|continue|it|that|this|open|view|visit)\b/.test(text) ||
    previous.some(item => item.deviceState) && /再(?:查|看)|刷新|现在呢|\b(?:refresh|now)\b/.test(text));
  const explicitNewTask = /新任务|换个话题|另外一件|另一个问题|\b(?:new task|different topic)\b/.test(text) ||
    signals.docs && !signals.desktop && !signals.browser && !signals.hostTerminal ||
    signals.deviceState && !signals.desktop && !signals.browser || ['external', 'explanation'].includes(deviceStateTopic(text));
  const retainedNames = new Set();
  let boundary = currentBoundary;
  if (followup && !explicitNewTask) {
    const recentSubject = [...previous].reverse().find(item => item.desktop || item.browser || item.hostTerminal || item.deviceState || item.docs || item.web);
    const deviceFollowup = recentSubject?.deviceState && !recentSubject.browser && !recentSubject.desktop && !signals.browser && !signals.desktop;
    const subjects = deviceFollowup ? [recentSubject] : previous;
    for (const key of ['desktop', 'browser', 'hostTerminal'])
      signals[key] ||= subjects.some(item => item[key]);
    signals.deviceState ||= Boolean(deviceFollowup);
    if (!deviceFollowup) boundary ??= [...recent].reverse().map(browserBoundary).find(item => item !== null) ?? null;
    for (const name of priorNames) {
      // A retry of the latest device inspection must not resurrect an older browser topic.
      // 最新设备查询的重试不能复活更早的浏览器主题；原浏览器续问规则保持。
      const category = toolDiscoveryCategory({ name });
      if (deviceFollowup && ['computer', 'browser'].includes(category)) continue;
      retainedNames.add(name);
      if (category === 'computer') signals.desktop = true;
      else if (category === 'host-terminal') signals.hostTerminal = true;
      else if (category === 'browser') signals.browser = true;
    }
  }
  // The latest explicit boundary wins over older text and attempted tool names.
  // Names can keep a schema discoverable, but cannot turn a local retry into remote control.
  // 最新明确边界优先于旧文本和曾尝试的工具名称；名称可维持可发现性，但不能把本机重试变成远程控制。
  if (boundary) {
    signals.remoteBrowser = boundary === 'remote';
    signals.browser = true;
    signals.desktop = boundary === 'local';
  }
  if (signals.remoteBrowser && !/本机桌面|本地窗口|\blocal desktop\b/.test(text)) signals.desktop = false;
  if (signals.deviceState) signals.hostTerminal = true;
  return { ...signals, retainedNames };
}
