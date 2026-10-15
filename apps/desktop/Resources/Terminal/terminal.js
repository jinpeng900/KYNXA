/* The browser renders VT output; Rust owns the shell, working directory and lifetime.
 * 浏览器只渲染 VT 输出；Rust 拥有 Shell、工作目录和会话生命周期。
 */
(() => {
  "use strict";
  const bridge = window.chrome.webview;
  const terminal = new Terminal({
    cols: 80, rows: 24, fontSize: 13,
    fontFamily: 'Cascadia Mono, Consolas, "Microsoft YaHei UI", monospace',
    cursorBlink: true, cursorStyle: "bar", scrollback: 10000,
    convertEol: false, allowProposedApi: false, disableStdin: true,
    theme: { background: "#fbfaf9", foreground: "#24272d", cursor: "#606772", selectionBackground: "#d6d6d680" }
  });
  const fit = new FitAddon.FitAddon();
  terminal.loadAddon(fit);
  terminal.open(document.getElementById("terminal"));
  let fitTimer;
  const post = message => bridge.postMessage(message);
  function fitTerminal() {
    if (document.documentElement.clientWidth < 40 || document.documentElement.clientHeight < 40) return;
    fit.fit();
  }
  function scheduleFit() {
    clearTimeout(fitTimer);
    fitTimer = setTimeout(fitTerminal, 60);
  }
  terminal.onData(data => post({ type: "input", data }));
  terminal.onResize(({ cols, rows }) => post({ type: "resize", cols, rows }));
  terminal.attachCustomKeyEventHandler(event => {
    if (event.type !== "keydown" || !event.ctrlKey || event.altKey) return true;
    if (event.key.toLowerCase() === "v") {
      event.preventDefault();
      post({ type: "paste" });
      return false;
    }
    if (event.shiftKey && event.key.toLowerCase() === "c") {
      event.preventDefault();
      if (terminal.hasSelection()) post({ type: "copy", text: terminal.getSelection() });
      return false;
    }
    // Plain Ctrl+C remains terminal input (ETX), including when a command is running.
    // 普通 Ctrl+C 保持 ETX 终端输入，执行中的命令可以正常中断。
    return true;
  });
  bridge.addEventListener("message", event => {
    const message = event.data;
    switch (message.type) {
      case "output":
        terminal.write(message.data, () => post({ type: "rendered", id: message.id }));
        break;
      case "connected":
        terminal.options.disableStdin = false;
        fitTerminal();
        terminal.focus();
        break;
      case "exited":
        terminal.options.disableStdin = true;
        break;
      case "paste":
        if (!terminal.options.disableStdin) terminal.paste(message.text);
        break;
      case "focus":
        fitTerminal(); terminal.focus();
        break;
      case "theme":
        terminal.options.theme = { ...terminal.options.theme, ...message.theme };
        document.documentElement.style.setProperty("--terminal-background", message.theme.background);
        break;
    }
  });
  new ResizeObserver(scheduleFit).observe(document.documentElement);
  requestAnimationFrame(() => {
    fitTerminal();
    post({ type: "ready", cols: terminal.cols, rows: terminal.rows });
  });
})();
