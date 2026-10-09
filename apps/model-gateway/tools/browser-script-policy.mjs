import { parse } from 'acorn';
import { toolFailure } from '../platform/tool-paths.mjs';

/** Inspect executable syntax, not comments or strings containing documentation.
 * 检查可执行语法，避免把注释或文档字符串误当成激活窗口。
 */
export function scriptRequestsBrowserForeground(source) {
  let syntax;
  try { syntax = parse(source, { ecmaVersion: 'latest', sourceType: 'module', allowReturnOutsideFunction: true }); }
  catch {
    try { syntax = parse(`(${source})`, { ecmaVersion: 'latest', sourceType: 'module' }); }
    catch { throw toolFailure('浏览器脚本语法无法验证，请提供完整 JavaScript。', 'BROWSER_SCRIPT_INVALID', 400); }
  }
  const windowAliases = new Set(['window', 'globalThis', 'self']);
  const functionAliases = new Set();
  const memberName = node => node?.type === 'MemberExpression'
    ? node.computed ? node.property?.value : node.property?.name : undefined;
  const isWindow = node => node?.type === 'Identifier' && windowAliases.has(node.name) ||
    node?.type === 'MemberExpression' && ((['window', 'self', 'top', 'parent'].includes(memberName(node)) && isWindow(node.object)) ||
      memberName(node) === 'defaultView' && node.object?.type === 'Identifier' && node.object.name === 'document' ||
      memberName(node) === 'contentWindow');
  const isForegroundFunction = node => node?.type === 'Identifier' && functionAliases.has(node.name) ||
    node?.type === 'MemberExpression' && (memberName(node) === 'bringToFront' ||
      ['focus', 'open'].includes(memberName(node)) && isWindow(node.object));
  const visitChildren = (node, visitor) => Object.values(node).some(value =>
    Array.isArray(value) ? value.some(visitor) : value && typeof value === 'object' && visitor(value));
  // Follow simple static aliases and destructuring. This is a foreground policy
  // check, not a sandbox or a proof about arbitrary dynamically generated code.
  // 跟踪静态别名与解构；此处仅判断前台策略，不是任意动态代码的沙箱或安全证明。
  const collectAliases = node => {
    if (!node || typeof node !== 'object') return false;
    if (node.type === 'VariableDeclarator' && node.id?.type === 'Identifier') {
      if (isWindow(node.init)) windowAliases.add(node.id.name);
      if (isForegroundFunction(node.init)) functionAliases.add(node.id.name);
    }
    if (node.type === 'VariableDeclarator' && node.id?.type === 'ObjectPattern' && isWindow(node.init))
      for (const property of node.id.properties)
        if (['focus', 'open'].includes(property.key?.name ?? property.key?.value) && property.value?.type === 'Identifier')
          functionAliases.add(property.value.name);
    visitChildren(node, collectAliases); return false;
  };
  collectAliases(syntax);
  const visit = node => {
    if (!node || typeof node !== 'object') return false;
    if (node.type === 'CallExpression') {
      const callee = node.callee?.type === 'ChainExpression' ? node.callee.expression : node.callee;
      if (isForegroundFunction(callee) || callee?.type === 'Identifier' && ['bringToFront', 'eval', 'Function'].includes(callee.name)) return true;
      if (callee?.type === 'MemberExpression') {
        const member = memberName(callee);
        if (['call', 'apply', 'bind'].includes(member) && isForegroundFunction(callee.object) ||
            isWindow(callee.object) && (member === undefined || ['eval', 'Function'].includes(member))) return true;
      }
    }
    if (node.type === 'NewExpression' && node.callee?.type === 'Identifier' && node.callee.name === 'Function') return true;
    return visitChildren(node, visit);
  };
  return visit(syntax);
}
