/* Run in the identified page, preserving main-document actions and numeric refs.
 * 在已确认的页面内执行，保留主文档操作及数字 ref。
 * Extra document text must not create cross-document action refs.
 * 附加文档正文不能生成跨文档操作引用。
 */
(function (maximum) {
  const max = Math.max(1, Math.min(64000, Number(maximum) || 16000));
  const bodyText = document.body?.innerText || "";
  const actionSelector = 'a,button,input,select,textarea,[role="button"]';
  const elements = Array.from(document.querySelectorAll(actionSelector)).slice(0, 1000)
    .filter(element => element.getClientRects().length).map((element, index) => {
      const bounds = element.getBoundingClientRect();
      return {
        ref: index, tag: element.tagName.toLowerCase(),
        name: (element.getAttribute("aria-label") || element.innerText || element.getAttribute("placeholder") || "").slice(0, 200),
        id: element.id, href: element.href || null, password: element.type === "password",
        value: element.type === "password" ? null : element.value || null,
        selector: currentSelector(element), role: element.getAttribute("role") || implicitRole(element),
        bounds: { x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height },
        inViewport: bounds.bottom > 0 && bounds.right > 0 && bounds.top < innerHeight && bounds.left < innerWidth
      };
    });

  function implicitRole(element) {
    if (element.tagName === "A") return "link";
    if (element.tagName === "BUTTON") return "button";
    if (element.tagName === "SELECT") return "combobox";
    if (element.type === "checkbox") return "checkbox";
    if (element.type === "radio") return "radio";
    return element.tagName === "INPUT" || element.tagName === "TEXTAREA" ? "textbox" : null;
  }

  function currentSelector(element) {
    if (element.id) {
      const selector = "#" + CSS.escape(element.id);
      if (document.querySelectorAll(selector).length === 1) return selector;
    }
    const pieces = [];
    for (let item = element; item && item.nodeType === 1; item = item.parentElement) {
      const siblings = item.parentElement ? Array.from(item.parentElement.children).filter(peer => peer.tagName === item.tagName) : [item];
      pieces.unshift(item.tagName.toLowerCase() + (siblings.length > 1 ? `:nth-of-type(${siblings.indexOf(item) + 1})` : ""));
    }
    return pieces.join(" > ") || null;
  }

  function readableText(node) {
    if (node.nodeType === 3) return (node.nodeValue || "").replace(/\s+/g, " ");
    if (node.nodeType !== 1) return "";
    const tag = node.tagName.toLowerCase();
    if (["script", "style", "noscript", "template", "input", "textarea", "button"].includes(tag)) return "";
    if (tag === "br") return "\n";
    if (tag === "pre") return "\n\n```\n" + node.textContent.trim() + "\n```\n\n";
    if (tag === "table") {
      const rows = Array.from(node.querySelectorAll("tr")).map(row =>
        Array.from(row.children).filter(cell => cell.matches("td,th")).map(cell =>
          readableText(cell).trim().replace(/\s*\n\s*/g, " ").replace(/\|/g, "\\|")));
      if (!rows.length) return "";
      const lines = rows.map(row => "| " + row.join(" | ") + " |");
      lines.splice(1, 0, "| " + rows[0].map(() => "---").join(" | ") + " |");
      return "\n\n" + lines.join("\n") + "\n\n";
    }
    const content = Array.from(node.childNodes).map(readableText).join("");
    if (tag === "a" && node.getAttribute("href")) {
      try {
        const address = new URL(node.getAttribute("href"), document.baseURI);
        if (["http:", "https:"].includes(address.protocol) && content.trim()) return `[${content.trim()}](${address.href})`;
      } catch { /* Invalid page links remain ordinary text. 中文：无效链接保留为普通文字。 */ }
    }
    if (/^h[1-6]$/.test(tag)) return "\n\n" + "#".repeat(Number(tag[1])) + " " + content.trim() + "\n\n";
    if (tag === "li") return "\n- " + content.trim();
    if (["p", "div", "section", "article", "main", "blockquote", "ul", "ol"].includes(tag)) return "\n" + content + "\n";
    return content;
  }

  let text = bodyText;
  let article = null;
  let extraction = "dom";
  try {
    // Readability mutates its document. The live page and action refs must remain untouched.
    // Readability 会改写文档；只解析克隆，保持真实页面及操作引用不变。
    const parsed = new Readability(document.cloneNode(true), { charThreshold: 140, maxElemsToParse: 50000, serializer: element => element }).parse();
    if (parsed?.content && parsed.textContent?.trim().length >= 140) {
      text = readableText(parsed.content).replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
      extraction = "article";
      article = { title: parsed.title, byline: parsed.byline, siteName: parsed.siteName,
        publishedTime: parsed.publishedTime, lang: parsed.lang, excerpt: parsed.excerpt };
    }
  } catch { /* Extraction failure falls back to the current visible DOM. 中文：正文提取失败时继续使用当前可见 DOM。 */ }

  const frames = [];
  const supplements = [];
  const frameNodes = Array.from(document.querySelectorAll("iframe"));
  for (const frame of frameNodes.slice(0, 12)) {
    if (!frame.getClientRects().length) continue;
    try {
      const inner = frame.contentDocument;
      if (!inner?.body) throw new Error("Frame is unavailable");
      const content = inner.body.innerText || "";
      frames.push({ title: frame.title || inner.title, url: inner.URL, readable: true, totalChars: content.length, actionableRefs: false });
      if (content.trim()) supplements.push("[iframe: " + (frame.title || inner.title || inner.URL) + "]\n" + content);
    } catch {
      frames.push({ title: frame.title || "", url: frame.src || null, readable: false, reason: "cross-origin-or-unavailable", actionableRefs: false });
    }
  }
  let shadowRoots = 0;
  const pendingRoots = [document];
  while (pendingRoots.length && shadowRoots < 16) {
    const root = pendingRoots.shift();
    for (const host of root.querySelectorAll("*")) {
      if (!host.shadowRoot || !host.getClientRects().length) continue;
      shadowRoots++;
      const content = Array.from(host.shadowRoot.children).filter(child => child.getClientRects().length)
        .map(child => child.innerText || "").filter(Boolean).join("\n");
      if (content.trim()) supplements.push("[open shadow DOM: " + host.tagName.toLowerCase() + "]\n" + content);
      pendingRoots.push(host.shadowRoot);
      if (shadowRoots >= 16) break;
    }
  }
  if (supplements.length) text += "\n\n" + supplements.join("\n\n");
  return {
    title: document.title, url: location.href, text: text.slice(0, max), totalChars: text.length,
    truncated: text.length > max, elements, article, extraction, domTotalChars: bodyText.length,
    viewport: { width: innerWidth, height: innerHeight, scrollX, scrollY },
    coverage: { elements: "main-document", frames, openShadowRoots: shadowRoots,
      frameLimitReached: frameNodes.length > 12, shadowRootLimitReached: shadowRoots >= 16,
      supplementaryContentIsReadOnly: true, crossOriginFramesRequireSeparateConnection: frames.some(frame => !frame.readable) }
  };
})
