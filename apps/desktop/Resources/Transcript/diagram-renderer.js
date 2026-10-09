/* Render untrusted diagram text inside an opaque, script-only sandbox. Never bind diagram actions.
 * 在不透明且仅允许脚本的沙箱内渲染不可信图表文本，绝不绑定图内动作。 */
(() => {
  'use strict';
  const MAX_SOURCE_CHARACTERS = 50000;
  const MAX_SVG_CHARACTERS = 2000000;
  const UI_FONT_FAMILY = '"Segoe UI Variable Text", "Segoe UI", "Microsoft YaHei UI", "Microsoft YaHei", sans-serif';
  let rendering = false;
  const color = (palette, key, fallback) => /^#[0-9a-f]{6}$/i.test(palette?.[key]) ? palette[key] : fallback;

  function rendererOptions(palette) {
    const main = color(palette, 'main', '#ffffff');
    const soft = color(palette, 'soft', '#edf2fc');
    const accent = color(palette, 'accent', '#4568b2');
    const text = color(palette, 'text', '#24272d');
    const secondary = color(palette, 'secondary', '#606772');
    const border = color(palette, 'border', '#dce3ef');
    return {
      startOnLoad: false, securityLevel: 'strict', htmlLabels: false,
      suppressErrorRendering: true, maxTextSize: MAX_SOURCE_CHARACTERS, maxEdges: 800,
      deterministicIds: false, theme: 'base',
      fontFamily: UI_FONT_FAMILY,
      flowchart: { htmlLabels: false, useMaxWidth: false, wrappingWidth: 220 },
      // Sequence labels have separate family defaults; set them before layout so measured and displayed glyphs match.
      // 时序图标签有独立默认字体，布局前同步设置，确保文字测量与实际显示一致。
      sequence: { useMaxWidth: false, wrap: true, wrapPadding: 12, actorMargin: 45, width: 170,
        actorFontFamily: UI_FONT_FAMILY, noteFontFamily: UI_FONT_FAMILY, messageFontFamily: UI_FONT_FAMILY },
      state: { useMaxWidth: false }, class: { useMaxWidth: false },
      er: { useMaxWidth: false }, gantt: { useMaxWidth: false },
      mindmap: { useMaxWidth: false },
      themeVariables: {
        fontFamily: UI_FONT_FAMILY,
        background: main, primaryColor: soft, primaryTextColor: text, primaryBorderColor: accent,
        secondaryColor: main, secondaryTextColor: text, secondaryBorderColor: border,
        tertiaryColor: soft, tertiaryTextColor: text, tertiaryBorderColor: border,
        textColor: text, lineColor: secondary, mainBkg: soft, nodeBorder: accent,
        clusterBkg: main, clusterBorder: border, edgeLabelBackground: main,
        actorBkg: soft, actorBorder: accent, actorTextColor: text, actorLineColor: secondary,
        signalColor: secondary, signalTextColor: text, labelBoxBkgColor: soft,
        labelBoxBorderColor: accent, labelTextColor: text, loopTextColor: text,
        noteBkgColor: soft, noteBorderColor: border, noteTextColor: text,
        activationBkgColor: soft, activationBorderColor: accent,
        classText: text, titleColor: text, fontSize: '16px'
      }
    };
  }

  window.addEventListener('message', async event => {
    const command = event.data;
    if (event.source !== parent || rendering || command?.type !== 'kynxa-mermaid-render'
      || typeof command.token !== 'string' || !/^[a-z0-9-]{8,100}$/i.test(command.token)
      || typeof command.source !== 'string' || command.source.length > MAX_SOURCE_CHARACTERS) return;
    rendering = true;
    try {
      if (!window.mermaid?.render) throw new Error('renderer-unavailable');
      window.mermaid.initialize(rendererOptions(command.palette));
      const result = await window.mermaid.render('kynxa-diagram-' + command.token, command.source);
      if (typeof result.svg !== 'string' || result.svg.length > MAX_SVG_CHARACTERS) throw new Error('svg-limit');
      parent.postMessage({ type: 'kynxa-mermaid-result', token: command.token, svg: result.svg }, '*');
    } catch {
      parent.postMessage({ type: 'kynxa-mermaid-result', token: command.token, error: true }, '*');
    } finally {
      rendering = false;
      // Remove Mermaid's temporary output after each request, including failed parses.
      // 每次请求后清除 Mermaid 临时节点，包括解析失败时生成的内容。
      document.body.replaceChildren();
    }
  });
  parent.postMessage({ type: 'kynxa-mermaid-ready' }, '*');
})();
