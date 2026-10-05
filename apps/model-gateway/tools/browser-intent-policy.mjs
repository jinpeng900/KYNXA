import { win32 } from 'node:path';
import { browserConnection, isBackgroundBrowserConnection } from './browser-connections.mjs';
import { currentBrowserInstructionText } from './browser-sessions.mjs';
import { toolFailure } from '../platform/tool-paths.mjs';

const browserName = '(?:浏览器|\\bbrowser\\b|\\b(?:chrome|msedge|edge|firefox|chromium|brave)\\b|\\bgoogle\\b\\s*(?:浏览器|\\bbrowser\\b))';
const browserControlAction = '(?:打开|开启|启动|操作|控制|显示|切换|激活|截图|截屏|截取|缩放|放大|缩小|调整|最小化|最大化|恢复|点击|填写|输入|\\b(?:open|launch|start|operate|control|show|capture|screenshot|focus|activate|resize|zoom|click|fill|type)\\b)';
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

/** Only a bare retry may retain the immediately preceding browser task.
 * 仅不带新任务的重试可以延续紧邻浏览器任务；新搜索或引用文本不能继承本机浏览器权限。
 */
export function isBrowserTaskFollowUp(message = '') {
  return /^(?:再来|继续|重试|再次尝试|再次尝试打开|再试(?:一次)?|再试试|再尝试(?:一次)?|再次打开|打不开|还是打不开|try again|retry|continue)[。.!！?？\s]*$/iu
    .test(currentBrowserInstructionText(message).trim());
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
  const text = currentBrowserInstructionText(message);
  const clauses = text.split(/[，。；,.!?;\n]/u), localForbidden = localBrowserForbidden.test(text);
  let explicitBrowserTask = false, allowLocalBrowser = false;
  for (const clause of clauses) {
    if (localBrowserForbidden.test(clause) || !clauseRequestsBrowserOperation(clause)) continue;
    explicitBrowserTask = true;
    const remoteOnly = /(?:云端|远程).{0,12}(?:浏览器|chrome|edge)|\b(?:remote|cloud)\s+browser\b/iu.test(clause) &&
      !/(?:本机|本地|我的|自己的).{0,12}(?:浏览器|chrome|edge)|\blocal\s+browser\b/iu.test(clause);
    allowLocalBrowser ||= !remoteOnly;
  }
  return { allowLocalBrowser: allowLocalBrowser && !localForbidden, explicitBrowserTask };
}

export function inferBrowserTaskIntent(message = '', previousUserMessages = []) {
  const current = currentBrowserIntent(message);
  if (!isBrowserTaskFollowUp(message)) return { ...current, inherited: false };
  for (const previous of previousUserMessages.slice(-3).reverse()) {
    if (isBrowserTaskFollowUp(previous)) continue;
    return { ...currentBrowserIntent(previous), inherited: true };
  }
  return { ...current, inherited: false };
}

/** The same decision governs connection discovery, schemas and execution, never tool reasons.
 * 连接发现、工具声明与执行使用相同判定，工具理由和第三方工具描述不能授权本机浏览器。
 */
export function canUseBrowserServer(context, server) {
  if (!context || !server || !browserConnection(server)) return true;
  const intent = context.browserTaskIntent ?? inferBrowserTaskIntent(context.message);
  return intent.allowLocalBrowser || isBackgroundBrowserConnection(server);
}

export function assertBrowserServerAllowed(context, server) {
  if (!canUseBrowserServer(context, server))
    throw toolFailure('普通网页搜索不能启动或操作本机浏览器；请使用搜索、公开网页读取或已配置的隔离后台浏览器。', 'BROWSER_TASK_NOT_AUTHORIZED', 403);
}

export function isBrowserApplicationPath(appPath) {
  return typeof appPath === 'string' && browserExecutables.has(win32.basename(appPath).toLowerCase());
}

export function assertBrowserLaunchAllowed(context, appPath) {
  if (isBrowserApplicationPath(appPath) && !(context.browserTaskIntent ?? inferBrowserTaskIntent(context.message)).allowLocalBrowser)
    throw toolFailure('只有用户明确要求打开或操作本机浏览器时才能启动浏览器；普通网页搜索使用后台检索。', 'BROWSER_TASK_NOT_AUTHORIZED', 403);
}
