import { win32 } from 'node:path';

const backgroundBrowserFlag = '--disable-backgrounding-occluded-windows';
const chromiumApplications = new Set(['chrome.exe', 'msedge.exe']);

/** Prepare observable launch options before the broker freezes the approval snapshot. */
export function prepareDesktopLaunchArguments(argumentsValue, { allowForeground = false } = {}) {
  const result = { ...argumentsValue, background: argumentsValue.background ?? !allowForeground };
  if (!result.background || !chromiumApplications.has(win32.basename(result.appPath).toLowerCase())) return result;
  const args = [...(result.args ?? [])];
  // Chromium can stop painting and hide its document accessibility tree when fully
  // occluded. This documented automation switch retains background page rendering;
  // other executables and existing user browser processes are never modified.
  if (!args.some(argument => argument === backgroundBrowserFlag || argument.startsWith(`${backgroundBrowserFlag}=`)))
    args.push(backgroundBrowserFlag);
  return { ...result, args };
}
