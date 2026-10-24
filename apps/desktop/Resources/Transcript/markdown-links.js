/* Shared Markdown links preserve local anchors and send explicit clicks to the authorized host.
 * 共用 Markdown 链接保留页内锚点，只把用户点击交给授权宿主。 */
(() => {
  'use strict';
  function anchor(root, fragment) {
    let id; try { id = decodeURIComponent(fragment.replace(/^#/, '')); } catch { return false; }
    const target = root?.querySelector('#' + CSS.escape(id));
    if (!target) return false;
    target.scrollIntoView({ block: 'start', behavior: 'auto' });
    target.focus?.({ preventScroll: true });
    return true;
  }
  function follow(link, root, open) {
    const value = link?.getAttribute('href');
    if (!isSafe(value)) return false;
    if (value.startsWith('#')) return anchor(root, value);
    open(value); return true;
  }
  function isSafe(value) {
    if (typeof value !== 'string' || !value || /[\u0000-\u001f\u007f]/.test(value) || /^(?:\/\/|\\\\)/.test(value)) return false;
    return !/^[a-z][\w+.-]*:/i.test(value) || /^(?:https?:\/\/|mailto:|[a-z]:[/\\])/i.test(value);
  }
  window.kynxaMarkdownLinks = { anchor, follow, isSafe };
})();
