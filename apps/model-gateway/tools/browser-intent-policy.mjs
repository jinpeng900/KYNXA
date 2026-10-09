import { win32 } from 'node:path';
import { browserConnection, isBackgroundBrowserConnection } from './browser-connections.mjs';
import { currentBrowserInstructionText } from './browser-sessions.mjs';
import { toolFailure } from '../platform/tool-paths.mjs';
import { analyzeRequestClauses, requestInstructionText } from '../platform/request-clause-signals.mjs';

const browserName = '(?:浏览器|\\bbrowser\\b|\\b(?:chrome|msedge|edge|firefox|chromium|brave)\\b|\\bgoogle\\b\\s*(?:浏览器|\\bbrowser\\b))';
const browserControlAction = '(?:打开|开启|启动|操作|控制|显示|切换|激活|截图|截屏|截取|缩放|放大|缩小|调整|最小化|最大化|恢复|刷新|重新加载|关闭|滚动|点击|填写|输入|\\b(?:open|launch|start|operate|control|show|capture|screenshot|focus|activate|resize|zoom|reload|refresh|close|scroll|click|fill|type)\\b)';
const browserModifiers = '(?:(?:本机|本地|当前|已打开|我的|自己的|云端|远程|the|my|local|current|remote|cloud)\\s*)*';
const actionBeforeBrowser = new RegExp(`${browserControlAction}[^，。；,.!?;\\n]{0,24}(${browserName})`, 'giu');
const browserBeforeAction = new RegExp(`(${browserName})([^，。；,.!?;\\n]{0,16})${browserControlAction}`, 'giu');
const useBrowser = new RegExp(`(?:用|使用|通过|\\b(?:use|using|with|via)\\b)\\s*${browserModifiers}(${browserName})`, 'giu');
const inBrowser = new RegExp(`(?:在|\\bin\\b)\\s*${browserModifiers}(${browserName})`, 'giu');
const readBrowser = new RegExp(`(?:查看|读取|\\b(?:inspect|read)\\b)\\s*(${browserModifiers})(${browserName})`, 'giu');
const browserSearch = new RegExp(`(?:让|请)\\s*${browserModifiers}(${browserName})[^，。；,.!?;\\n]{0,8}(?:搜索|查询|读取|查看)`, 'giu');
const researchSuffix = /^\s*(?:(?:浏览器|browser)\s*)?(?:的\s*)?(?:最新(?:版本|新闻|消息)|版本|文档|官网|发行|发布说明|相关新闻|latest\b|version\b|docs?\b|documentation\b|release\b|news\b)/iu;
const researchPrefix = /^\s*(?:请\s*)?(?:搜索|查询|查找|检索|查|了解|研究|查看|读取|\b(?:search|find|read|inspect|research)\b|look\s+up)/iu;
const informationalQuestion = /怎么|如何|怎样|\bhow\b/iu;
const explicitExecutionRequest = /(?:帮我|替我|请你)\s*(?:实际|直接)?\s*(?:用|使用|操作|执行|演示|打开|启动|截图|截屏)|(?:请\s*)?(?:实际|直接)\s*(?:演示|操作|执行|用|使用)|\b(?:please\s+)?(?:actually\s+demonstrate|demonstrate|help\s+me\s+use|do\s+this\s+for\s+me)\b/iu;
const localBrowserForbidden = new RegExp(`(?:不要|别|不必|禁止|不能|不许|不允许|不希望|避免|无需|do not|don't|never|without)[^，。；,.!?;\\n]{0,24}(?:打开|开启|启动|操作|使用|用|开|open|launch|start|use|operate|control)[^，。；,.!?;\\n]{0,16}${browserName}|(?:不用|不使用)[^，。；,.!?;\\n]{0,16}${browserName}`, 'iu');
const browserExecutables = new Set(['chrome.exe', 'msedge.exe', 'firefox.exe', 'chromium.exe', 'brave.exe', 'opera.exe', 'vivaldi.exe']);
const browserExecutableNames = new Map([['chrome', 'chrome.exe'], ['edge', 'msedge.exe'], ['firefox', 'firefox.exe'],
  ['chromium', 'chromium.exe'], ['brave', 'brave.exe'], ['opera', 'opera.exe'], ['vivaldi', 'vivaldi.exe']]);

function browserInstructionText(message) {
  const browserLiteral = new RegExp(`^${browserName}$`, 'iu');
  const instruction = requestInstructionText(currentBrowserInstructionText(message),
    { preserveQuotedLiteral: value => browserLiteral.test(value.trim()) });
  // A quoted brand is a literal target; quoted operations remain source text rather than current commands.
  // 引号内完整品牌可作为字面目标；引文中的操作仍是资料文字，不能变成当前执行要求。
  return instruction.replace(new RegExp(`["“‘「『']\\s*(${browserName})\\s*["”’」』']`, 'giu'), '$1');
}

function forbiddenBrowserExecutables(clause) {
  const instruction = browserInstructionText(clause);
  const restriction = localBrowserForbidden.exec(instruction);
  if (!restriction) return [];
  const objectSuffix = instruction.slice(restriction.index + restriction[0].length);
  // Scope follows the prohibited object, not explanatory words elsewhere in the clause.
  // 限制范围跟随被禁止的对象，不能由子句其他位置的解释性词语扩大或缩小。
  const objectBeforeBrowser = /窗口|标签(?:页)?|页面|网页|\b(?:windows?|tabs?|pages?)\b/iu.test(restriction[0]);
  const objectAfterBrowser = /^\s*(?:(?:浏览器|browser)\s*)?(?:(?:的|['’]s)\s*)?(?:(?:新|当前|已有|已打开|这个|该|无痕|隐私|new|current|existing|private)\s*)*(?:窗口|标签(?:页)?|页面|网页|\b(?:windows?|tabs?|pages?)\b)/iu.test(objectSuffix);
  if (objectBeforeBrowser || objectAfterBrowser) return [];
  return [...browserExecutableNames].filter(([name]) => new RegExp(`\\b${name}\\b`, 'iu').test(restriction[0]))
    .map(([, executable]) => executable);
}

/** A continuation describes a page action; unrelated research does not retain browser control.
 * 后续消息可以描述页面操作；无关资料检索不能继承浏览器控制。
 */
export function isBrowserTaskFollowUp(message = '') {
  const text = browserInstructionText(message).trim();
  if (informationalQuestion.test(text) && !explicitExecutionRequest.test(text)) return false;
  if (researchPrefix.test(text) && !/(?:当前|这个|该|本页).{0,8}(?:网页|页面|网站|标签)|作业|账户|账号|二维码/u.test(text)) return false;
  return /^(?:再来|继续|重试|再次尝试|再次尝试打开|再试(?:一次)?|再试试|再尝试(?:一次)?|再次打开|打不开|还是打不开|try again|retry|continue)[。.!！?？\s]*$/iu.test(text) ||
    /(?:打开|进入|访问|登录|登陆|刷新|关闭|滚动|截图|截屏|点击|填写|输入|切换|查看|读取|open|navigate|log\s*in|sign\s*in|refresh|reload|close|scroll|screenshot|click|fill|type).{0,60}(?:网站|网页|页面|标签|网址|https?:\/\/|学习通|账户|账号|作业|二维码|按钮|输入框|当前|这个|page|tab|website|url|account|form|button)|^(?:向上|向下|往上|往下)?(?:滚动|刷新|关闭当前页面)|^(?:截图|截屏|填写|输入|登录|登陆)/iu.test(text);
}

function clauseRequestsBrowserOperation(clause) {
  // Asking how a browser works is not permission to operate it; an explicit live execution/demo remains a task.
  // 询问浏览器使用方法不授权实际操作；明确要求执行或现场演示的任务仍保留。
  if (informationalQuestion.test(clause) && !explicitExecutionRequest.test(clause)) return false;
  // Browser brands can be research subjects. Only control actions, explicit instruments or actual browser contents grant intent.
  // 浏览器品牌可以只是研究对象；仅控制动作、明确作为操作工具或读取浏览器自身内容表达操作意图。
  for (const pattern of [useBrowser, inBrowser, browserSearch])
    for (const match of clause.matchAll(pattern))
      if (!researchSuffix.test(clause.slice(match.index + match[0].length))) return true;
  for (const match of clause.matchAll(readBrowser)) {
    const remaining = clause.slice(match.index + match[0].length);
    const browserObject = /浏览器|\bbrowser\b/iu.test(match[2]) || /^\s*(?:浏览器|\bbrowser\b)/iu.test(remaining);
    const localObject = /本机|本地|当前|已打开|我的|自己的|\b(?:local|current|my)\b/iu.test(match[1]);
    if ((browserObject || localObject) && !researchSuffix.test(remaining)) return true;
  }
  if (researchPrefix.test(clause)) return false;
  for (const match of clause.matchAll(actionBeforeBrowser))
    if (!researchSuffix.test(clause.slice(match.index + match[0].length))) return true;
  for (const match of clause.matchAll(browserBeforeAction))
    if (!researchSuffix.test(match[2])) return true;
  return /(?:登录|填写|输入密码|log\s*in|sign\s*in).{0,24}(?:网页|网站|页面|https?:\/\/)|(?:网页|网站|页面).{0,24}(?:登录|填写|输入密码)/iu.test(clause);
}

function currentBrowserIntent(message) {
  const text = browserInstructionText(message);
  const projection = analyzeRequestClauses(text);
  const clauses = projection.activeText.split(/[，。；,.!?;\n]/u);
  let explicitBrowserTask = false, allowLocalBrowser = false;
  const excludedBrowserExecutables = [];
  // Excluded clauses never provide a positive operation; explicit brand restrictions still bind their object.
  // 已排除子句不能提供正向操作意图；明确的浏览器品牌限制仍约束其自身对象。
  for (const clause of projection.excludedClauses) {
    if (['explicit-task-exit', 'superseded-by-task-boundary'].includes(clause.basis)) continue;
    excludedBrowserExecutables.push(...forbiddenBrowserExecutables(clause.text));
  }
  for (const clause of clauses) {
    if (localBrowserForbidden.test(clause)) {
      excludedBrowserExecutables.push(...forbiddenBrowserExecutables(clause));
      continue;
    }
    if (!clauseRequestsBrowserOperation(clause)) continue;
    explicitBrowserTask = true;
    const remoteOnly = /(?:云端|远程).{0,12}(?:浏览器|chrome|edge)|\b(?:remote|cloud)\s+browser\b/iu.test(clause) &&
      !/(?:本机|本地|我的|自己的).{0,12}(?:浏览器|chrome|edge)|\blocal\s+browser\b/iu.test(clause);
    allowLocalBrowser ||= !remoteOnly;
  }
  // Negative clauses constrain their own object. A ban on a new window or on
  // Chrome must not cancel a positive instruction to use the current page/Edge.
  // 否定分句限定自身对象；禁止新窗口或 Chrome 不得取消使用当前页面/Edge 的正向指令。
  return { allowLocalBrowser, explicitBrowserTask, ...(excludedBrowserExecutables.length
    ? { excludedBrowserExecutables: [...new Set(excludedBrowserExecutables)] } : {}) };
}

export function inferBrowserTaskIntent(message = '', previousUserMessages = []) {
  const current = currentBrowserIntent(message);
  const text = browserInstructionText(message);
  if (analyzeRequestClauses(text).boundary !== 'none' || !isBrowserTaskFollowUp(message) ||
      new RegExp(browserName, 'iu').test(text)) return { ...current, inherited: false };
  let active;
  const history = [...previousUserMessages];
  if (history.at(-1) === message) history.pop();
  for (const previous of history) {
    const boundary = analyzeRequestClauses(currentBrowserInstructionText(previous)).boundary;
    if (boundary !== 'none') active = undefined;
    const intent = currentBrowserIntent(previous);
    if (intent.explicitBrowserTask && (new RegExp(browserName, 'iu').test(currentBrowserInstructionText(previous)) ||
        !isBrowserTaskFollowUp(previous) || !active)) active = intent;
    else if (boundary !== 'none' || !isBrowserTaskFollowUp(previous)) active = undefined;
  }
  if (active) return { ...active, inherited: true };
  return { ...current, inherited: false };
}

/** The same decision governs connection discovery, schemas and execution, never tool reasons.
 * 连接发现、工具声明与执行使用相同判定，工具理由和第三方工具描述不能授权本机浏览器。
 */
export function canUseBrowserServer(context, server) {
  if (!context || !server || !browserConnection(server)) return true;
  const intent = context.browserTaskIntent ?? inferBrowserTaskIntent(context.message);
  if (intent.excludedBrowserExecutables?.includes(configuredBrowserExecutable(server))) return false;
  return intent.allowLocalBrowser || isBackgroundBrowserConnection(server);
}

function configuredBrowserExecutable(server) {
  const args = server.args ?? [];
  for (let index = 0; index < args.length; index++) {
    const option = String(args[index]);
    if (/^--(?:executablePath|executable-path|browserExecutablePath|browser)(?:=|$)/u.test(option)) {
      const value = option.includes('=') ? option.slice(option.indexOf('=') + 1) : args[index + 1];
      if (typeof value !== 'string') continue;
      const executable = win32.basename(value).toLowerCase();
      if (browserExecutables.has(executable)) return executable;
      if (value.toLowerCase() === 'msedge') return 'msedge.exe';
      if (browserExecutableNames.has(value.toLowerCase())) return browserExecutableNames.get(value.toLowerCase());
    }
  }
  return browserConnection(server)?.engine === 'chrome-devtools' ? 'chrome.exe' : 'chromium.exe';
}

export function assertBrowserServerAllowed(context, server) {
  if (!canUseBrowserServer(context, server))
    throw toolFailure('普通网页搜索不能启动或操作本机浏览器；请使用搜索、公开网页读取或已配置的隔离后台浏览器。', 'BROWSER_TASK_NOT_AUTHORIZED', 403);
}

export function isBrowserApplicationPath(appPath) {
  return typeof appPath === 'string' && browserExecutables.has(win32.basename(appPath).toLowerCase());
}

export function assertBrowserLaunchAllowed(context, appPath) {
  const intent = context.browserTaskIntent ?? inferBrowserTaskIntent(context.message);
  if (isBrowserApplicationPath(appPath) && (!intent.allowLocalBrowser ||
      intent.excludedBrowserExecutables?.includes(win32.basename(appPath).toLowerCase())))
    throw toolFailure('只有用户明确要求打开或操作本机浏览器时才能启动浏览器；普通网页搜索使用后台检索。', 'BROWSER_TASK_NOT_AUTHORIZED', 403);
}
