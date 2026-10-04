import { win32 } from 'node:path';

const backgroundBrowserFlag = '--disable-backgrounding-occluded-windows';
const chromiumApplications = new Set(['chrome.exe', 'msedge.exe']);

/**
 * Prepare observable launch options before the broker freezes the approval snapshot.
 * 在代理层冻结审批快照前，准备可观察的启动选项。
 */
export function prepareDesktopLaunchArguments(argumentsValue, { allowForeground = false } = {}) {
  const result = { ...argumentsValue, background: argumentsValue.background ?? !allowForeground };
  if (!result.background || !chromiumApplications.has(win32.basename(result.appPath).toLowerCase())) return result;
  const args = [...(result.args ?? [])];
  // Chromium can stop painting and hide its document accessibility tree when fully
  // occluded. This documented automation switch retains background page rendering;
  // other executables and existing user browser processes are never modified.
  // Chromium 被完全遮挡时可能停止绘制并隐藏文档无障碍树；此自动化开关保留后台渲染，不修改其他程序或已有用户浏览器进程。
  if (!args.some(argument => argument === backgroundBrowserFlag || argument.startsWith(`${backgroundBrowserFlag}=`)))
    args.push(backgroundBrowserFlag);
  return { ...result, args };
}
