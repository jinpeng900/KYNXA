/* Render original TeX with the same KaTeX version and fallback sequence as the reference UI.
   All output stays in this local document; only measured image regions leave the worker. */
(() => {
  const root = document.getElementById('formulas');
  const nextPaint = () => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));

  window.renderFormulas = async (id, items) => {
    try {
      root.replaceChildren();
      const rows = items.map((item, index) => {
        if (typeof item.latex !== 'string' || item.latex.length > 32768) return { index, error: 'too-large' };
        const row = document.createElement('div');
        row.className = 'row';
        const formula = document.createElement('span');
        formula.className = 'formula ' + (item.block ? 'display' : 'inline');
        // Separate options/macros for each formula prevent definitions leaking into another reply.
        const options = { displayMode: !!item.block, throwOnError: true, trust: false, maxExpand: 1000, maxSize: 100 };
        try {
          formula.innerHTML = katex.renderToString(item.latex, options);
        } catch {
          try {
            formula.innerHTML = katex.renderToString(item.latex, { ...options, strict: 'ignore', throwOnError: false });
          } catch {
            return { index, error: 'parse' };
          }
        }
        const baseline = document.createElement('span');
        baseline.className = 'baseline';
        formula.append(baseline);
        row.append(formula);
        root.append(row);
        return { index, row, formula, baseline };
      });
      await document.fonts.ready;
      const results = rows.map(entry => {
        if (entry.error) return { index: entry.index, error: entry.error };
        const rect = entry.formula.getBoundingClientRect();
        const baseline = entry.baseline.getBoundingClientRect().top - rect.top;
        const result = { index: entry.index, x: rect.x, y: rect.y, width: rect.width, height: rect.height, baseline };
        if (rect.width > innerWidth || rect.height > innerHeight) result.error = 'too-large';
        else if (rect.bottom > innerHeight) result.error = 'batch-full';
        return result;
      });
      await nextPaint();
      chrome.webview.postMessage({ type: 'rendered', id, viewportWidth: innerWidth, viewportHeight: innerHeight, results });
    } catch {
      chrome.webview.postMessage({ type: 'rendered', id, viewportWidth: innerWidth, viewportHeight: innerHeight,
        results: items.map((_, index) => ({ index, error: 'render' })) });
    }
  };
})();
