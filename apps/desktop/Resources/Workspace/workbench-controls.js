// Workbench selectors share the host palette without changing preview-wide native controls.
// 工作台选择器沿用宿主配色，不改变其他预览页面的原生控件。
let nextSelectId = 0;
const OPTION_HEIGHT = 36, OPTION_GAP = 4, POPUP_INSET = 14, VISIBLE_OPTIONS = 4;

export function createSelectScope(root) {
  const document = root.ownerDocument, window = document.defaultView;
  const lifetime = new AbortController(), controls = new WeakMap();
  let opened = null, disposed = false;

  function close(restoreFocus = false) {
    const current = opened;
    if (!current) return;
    opened = null;
    current.events.abort(); current.disabledObserver.disconnect(); current.popup.remove();
    current.button.setAttribute('aria-expanded', 'false');
    current.button.removeAttribute('aria-activedescendant');
    if (restoreFocus && current.button.isConnected && !current.button.disabled)
      current.button.focus({ preventScroll: true });
  }

  function selectedLabel(control) { return control.items[control.selected]?.[1] || ''; }
  function updateValue(control, value) {
    control.selected = control.items.findIndex(([id]) => id === String(value));
    control.label.textContent = selectedLabel(control);
    control.button.setAttribute('aria-valuetext', selectedLabel(control));
    if (opened?.control !== control) return;
    opened.rows.forEach((row, index) => row.setAttribute('aria-selected', String(index === control.selected)));
    setActive(opened, control.selected < 0 ? 0 : control.selected);
  }

  function setActive(current, index) {
    if (opened !== current || !current.rows.length) return;
    current.active = Math.max(0, Math.min(current.rows.length - 1, index));
    current.rows.forEach((row, rowIndex) => row.dataset.active = String(rowIndex === current.active));
    const active = current.rows[current.active];
    current.button.setAttribute('aria-activedescendant', active.id);
    const optionBounds = active.getBoundingClientRect(), popupBounds = current.popup.getBoundingClientRect();
    if (optionBounds.top < popupBounds.top + 7) current.popup.scrollTop -= popupBounds.top + 7 - optionBounds.top;
    else if (optionBounds.bottom > popupBounds.bottom - 7) current.popup.scrollTop += optionBounds.bottom - popupBounds.bottom + 7;
  }

  function commit(current, index, restoreFocus = true) {
    if (opened !== current || current.button.disabled || !current.button.isConnected) { close(); return; }
    const value = current.control.items[index]?.[0];
    if (value === undefined) return;
    const changed = current.control.selected !== index;
    updateValue(current.control, value); close(restoreFocus);
    if (changed) {
      current.button.dispatchEvent(new Event('change', { bubbles: true }));
      current.control.action?.(value);
    }
  }

  function placePopup(current) {
    const bounds = current.button.getBoundingClientRect(), viewport = window.visualViewport;
    const leftEdge = (viewport?.offsetLeft || 0) + 8, topEdge = (viewport?.offsetTop || 0) + 8;
    const rightEdge = leftEdge + (viewport?.width || window.innerWidth) - 16;
    const bottomEdge = topEdge + (viewport?.height || window.innerHeight) - 16;
    const below = Math.max(0, bottomEdge - bounds.bottom - 4), above = Math.max(0, bounds.top - topEdge - 4);
    const rows = Math.min(VISIBLE_OPTIONS, current.rows.length);
    const optionHeight = current.rows[0]?.getBoundingClientRect().height || OPTION_HEIGHT;
    const desired = rows * optionHeight + Math.max(0, rows - 1) * OPTION_GAP + POPUP_INSET;
    const opensBelow = below >= desired || below >= above, available = opensBelow ? below : above;
    // Short viewports reduce the complete row count; they never expose a clipped fifth option.
    // 短窗口按完整行减少展示数量，不让第五个选项露出半行。
    const visibleRows = Math.min(rows, Math.max(1, Math.floor((available - POPUP_INSET + OPTION_GAP) / (optionHeight + OPTION_GAP))));
    const height = Math.min(available, visibleRows * optionHeight + Math.max(0, visibleRows - 1) * OPTION_GAP + POPUP_INSET);
    const popup = current.popup;
    popup.style.position = 'fixed'; popup.style.width = 'max-content';
    popup.style.minWidth = Math.max(0, Math.min(bounds.width, rightEdge - leftEdge)) + 'px';
    popup.style.maxWidth = Math.max(0, rightEdge - leftEdge) + 'px';
    popup.style.height = height + 'px'; popup.style.maxHeight = height + 'px';
    const width = popup.getBoundingClientRect().width;
    popup.style.left = Math.max(leftEdge, Math.min(bounds.left, rightEdge - width)) + 'px';
    popup.style.top = (opensBelow ? bounds.bottom + 4 : bounds.top - 4 - height) + 'px';
    popup.dataset.placement = opensBelow ? 'below' : 'above';
  }

  function open(control, initialIndex = control.selected) {
    if (disposed || control.button.disabled || !control.button.isConnected || !control.items.length) return;
    close();
    control.button.focus({ preventScroll: true });
    const popup = document.createElement('div'), events = new AbortController();
    popup.className = 'workbench-select-options'; popup.id = control.popupId;
    popup.setAttribute('role', 'listbox'); popup.setAttribute('aria-labelledby', control.button.id);
    const rows = control.items.map(([id, text], index) => {
      const row = document.createElement('div'); row.className = 'workbench-select-option';
      row.id = control.popupId + '-' + index; row.dataset.index = String(index); row.dataset.value = id;
      row.textContent = text; row.title = text; row.setAttribute('role', 'option');
      row.setAttribute('aria-selected', String(index === control.selected));
      popup.append(row); return row;
    });
    const disabledObserver = new MutationObserver(() => { if (control.button.disabled) close(); });
    const current = opened = { control, button: control.button, popup, rows, events, disabledObserver,
      active: initialIndex < 0 ? 0 : initialIndex, composing: false, search: '', searchTime: 0 };
    // Container queries may make the workbench a fixed-position containing block; keep the popup in viewport space.
    // 工作台的容器查询会改变 fixed 定位包含块；弹层挂页面根部，避免坐标偏移和内容裁剪。
    popup.style.setProperty('--workbench-font-size', window.getComputedStyle(root).getPropertyValue('--workbench-font-size'));
    document.body.append(popup); placePopup(current); setActive(current, current.active);
    control.button.setAttribute('aria-expanded', 'true');
    disabledObserver.observe(control.button, { attributes: true, attributeFilter: ['disabled'] });
    const signal = events.signal;
    popup.addEventListener('pointerdown', event => {
      if (event.target.closest?.('.workbench-select-option')) event.preventDefault();
    }, { signal });
    popup.addEventListener('pointermove', event => {
      if (event.pointerType === 'touch') return;
      const row = event.target.closest?.('.workbench-select-option');
      if (row && popup.contains(row)) setActive(current, Number(row.dataset.index));
    }, { signal });
    popup.addEventListener('click', event => {
      const row = event.target.closest?.('.workbench-select-option');
      if (row && popup.contains(row)) commit(current, Number(row.dataset.index));
    }, { signal });
    document.addEventListener('pointerdown', event => {
      if (!control.button.contains(event.target) && !popup.contains(event.target)) close();
    }, { capture: true, signal });
    window.addEventListener('scroll', event => { if (!popup.contains(event.target)) close(); }, { capture: true, passive: true, signal });
    window.addEventListener('resize', () => close(), { signal });
    window.visualViewport?.addEventListener('resize', () => close(), { signal });
    window.addEventListener('blur', () => close(), { signal });
  }

  function matchingOption(current, text) {
    const now = performance.now();
    current.search = now - current.searchTime < 700 ? current.search + text : text;
    current.searchTime = now;
    const folded = current.search.toLocaleLowerCase(), characters = [...folded];
    const query = characters.every(character => character === characters[0]) ? characters[0] : folded;
    const start = query.length === 1 ? current.active + 1 : current.active;
    for (let offset = 0; offset < current.control.items.length; offset++) {
      const index = (start + offset) % current.control.items.length;
      if (current.control.items[index][1].toLocaleLowerCase().startsWith(query)) { setActive(current, index); return; }
    }
  }

  function controlFor(event) {
    const button = event.target.closest?.('.workbench-select');
    return button && root.contains(button) ? controls.get(button) : null;
  }
  root.addEventListener('click', event => {
    const control = controlFor(event);
    if (!control) return;
    if (opened?.control === control) close(); else open(control);
  }, { signal: lifetime.signal });
  root.addEventListener('keydown', event => {
    const control = controlFor(event);
    if (!control || control.button.disabled || event.isComposing || event.keyCode === 229 || opened?.composing) return;
    const current = opened?.control === control ? opened : null;
    if (event.key === 'Tab') { if (current) commit(current, current.active, false); return; }
    if (event.key === 'Escape') { if (current) { event.preventDefault(); event.stopPropagation(); close(true); } return; }
    if (event.altKey && event.key === 'ArrowUp' && current) { event.preventDefault(); close(true); return; }
    if (['Enter', ' ', 'ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      if (!current) {
        const initial = event.key === 'Home' ? 0 : event.key === 'End' ? control.items.length - 1 : control.selected;
        open(control, initial); return;
      }
      if (event.key === 'Enter' || event.key === ' ') commit(current, current.active);
      else if (event.key === 'Home' || event.key === 'End') setActive(current, event.key === 'Home' ? 0 : current.rows.length - 1);
      else setActive(current, current.active + (event.key === 'ArrowDown' ? 1 : -1));
      return;
    }
    if (!event.ctrlKey && !event.metaKey && !event.altKey && [...event.key].length === 1) {
      event.preventDefault(); if (!current) open(control);
      if (opened?.control === control) matchingOption(opened, event.key);
    }
  }, { signal: lifetime.signal });
  root.addEventListener('compositionstart', event => { if (controlFor(event) && opened) opened.composing = true; }, { signal: lifetime.signal });
  root.addEventListener('compositionend', event => {
    const control = controlFor(event);
    if (!control) return;
    if (opened) opened.composing = false;
    if (event.data) { if (opened?.control !== control) open(control); if (opened) matchingOption(opened, event.data); }
  }, { signal: lifetime.signal });
  root.addEventListener('focusout', event => {
    if (opened && !opened.button.contains(event.relatedTarget) && !opened.popup.contains(event.relatedTarget)) close();
  }, { signal: lifetime.signal });

  return {
    select(items, value, action) {
      const button = document.createElement('button'), label = document.createElement('span');
      const chevron = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
      const stroke = document.createElementNS(chevron.namespaceURI, 'path');
      const popupId = 'workbench-select-options-' + ++nextSelectId;
      button.type = 'button'; button.className = 'preview-select workbench-select'; button.id = popupId + '-control';
      button.setAttribute('role', 'combobox'); button.setAttribute('aria-haspopup', 'listbox');
      button.setAttribute('aria-expanded', 'false'); button.setAttribute('aria-controls', popupId);
      label.className = 'workbench-select-value'; chevron.classList.add('workbench-select-chevron');
      chevron.setAttribute('viewBox', '0 0 16 16'); chevron.setAttribute('aria-hidden', 'true');
      stroke.setAttribute('d', 'M4 6l4 4 4-4'); chevron.append(stroke); button.append(label, chevron);
      const control = { button, label, popupId, action, items: items.map(([id, text]) => [String(id), String(text)]), selected: -1 };
      controls.set(button, control); button.disabled = !control.items.length;
      // Programmatic filtering updates only the visible value; it never repeats the user's action.
      // 程序清空筛选只同步显示值，不重复执行用户操作。
      Object.defineProperty(button, 'value', { configurable: true, get: () => control.items[control.selected]?.[0] || '',
        set: next => updateValue(control, next) });
      updateValue(control, value); return button;
    },
    close,
    dispose() { if (disposed) return; close(); disposed = true; lifetime.abort(); }
  };
}
