(() => {
  const helper = {
    scroller: () => document.scrollingElement,
    bodyText: () => document.getElementById('messages').textContent,
    bottomDistance() {
      const scroll = this.scroller();
      return scroll.scrollHeight - scroll.clientHeight - scroll.scrollTop;
    },
    textNode(root, token) {
      const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
      for (let node; (node = walker.nextNode());) {
        if (!node.parentElement.closest('.katex-mathml') && node.nodeValue.includes(token)) return node;
      }
      throw new Error('Missing visible text: ' + token);
    },
    copyEvent() {
      const clipboard = new DataTransfer();
      const event = new ClipboardEvent('copy', { bubbles: true, cancelable: true, clipboardData: clipboard });
      document.dispatchEvent(event);
      if (!event.defaultPrevented) throw new Error('Custom copy event was not handled');
      return clipboard.getData('text/plain');
    },
    copyAcross(reverse) {
      const messages = document.getElementById('messages');
      const first = this.textNode(messages, 'USER_START');
      const last = this.textNode(messages, 'ASSISTANT_END');
      const selection = getSelection();
      selection.removeAllRanges();
      if (reverse) selection.setBaseAndExtent(last, last.nodeValue.length, first, 0);
      else selection.setBaseAndExtent(first, 0, last, last.nodeValue.length);
      return this.copyEvent();
    },
    copyFormula(partial) {
      const formulas = [...document.querySelectorAll('.math')];
      const formula = formulas.find(node => node.dataset.latex.includes(partial ? 'ABCDE' : '\\omega'));
      if (!formula) throw new Error('Formula copy fixture missing');
      const selection = getSelection();
      selection.removeAllRanges();
      if (partial) {
        const text = this.textNode(formula.querySelector('.katex-html'), 'ABCDE');
        const start = text.nodeValue.indexOf('ABCDE') + 1;
        selection.setBaseAndExtent(text, start, text, start + 2);
      } else {
        const range = document.createRange();
        range.selectNode(formula);
        selection.addRange(range);
      }
      return this.copyEvent();
    },
    copyUserWhitespace() {
      const node = this.textNode(document.querySelector('[data-role=user] .message-body'), '第二行');
      const start = node.nodeValue.indexOf('内容');
      const selection = getSelection(); selection.removeAllRanges();
      selection.setBaseAndExtent(node, start, node, node.nodeValue.length);
      return this.copyEvent();
    },
    copyCodeIndent() {
      const code = document.querySelector('pre code');
      const walker = document.createTreeWalker(code, NodeFilter.SHOW_TEXT);
      let indent = null, offset = -1;
      for (let node; (node = walker.nextNode());) {
        const found = node.nodeValue.indexOf('    ');
        if (found >= 0) { indent = node; offset = found; break; }
      }
      if (!indent) throw new Error('Indented code text fixture missing');
      const selection = getSelection(); selection.removeAllRanges();
      selection.setBaseAndExtent(indent, offset, indent, offset + 4);
      return this.copyEvent();
    },
    openReasoning(open) {
      getSelection().removeAllRanges();
      document.querySelector('.reasoning:not([hidden])').open = open;
      return true;
    },
    dragPoint(token) {
      getSelection().removeAllRanges();
      const node = this.textNode(document.getElementById('messages'), token);
      node.parentElement.scrollIntoView({ block: 'center' });
      const range = document.createRange(); range.setStart(node, 8); range.setEnd(node, 9);
      const box = range.getBoundingClientRect();
      return { x: box.x + box.width / 2, y: box.y + box.height / 2, height: innerHeight };
    },
    copyWholeCode() {
      const range = document.createRange(); range.selectNodeContents(document.querySelector('pre code'));
      getSelection().removeAllRanges(); getSelection().addRange(range);
      return this.copyEvent();
    },
    resizeGeometry() {
      const pre = document.querySelector('pre'), code = pre.querySelector('code');
      const range = document.createRange(); range.selectNodeContents(code);
      const table = document.querySelector('.table-scroll');
      return {
        viewport: innerWidth,
        messagesWidth: document.getElementById('messages').getBoundingClientRect().width,
        replyWidth: document.querySelector('[data-role=assistant] .message-body').getBoundingClientRect().width,
        pageClientWidth: document.documentElement.clientWidth, pageScrollWidth: document.documentElement.scrollWidth,
        preClientWidth: pre.clientWidth, preScrollWidth: pre.scrollWidth,
        preHeight: pre.getBoundingClientRect().height, preRight: pre.getBoundingClientRect().right,
        codeGlyphMaxRight: Math.max(...[...range.getClientRects()].map(rect => rect.right)),
        tableClientWidth: table.clientWidth, tableScrollWidth: table.scrollWidth,
        codeWhiteSpace: getComputedStyle(pre).whiteSpace, codeOverflowWrap: getComputedStyle(pre).overflowWrap,
      };
    },
    scrollUp() {
      this.scroller().dispatchEvent(new WheelEvent('wheel', { deltaY: -120, bubbles: true }));
      this.scroller().scrollTop = 50;
      return true;
    },
    scrollBottom() {
      this.scroller().scrollTop = this.scroller().scrollHeight;
      return true;
    }
  };
  window.__transcriptSmoke = helper;
})();
