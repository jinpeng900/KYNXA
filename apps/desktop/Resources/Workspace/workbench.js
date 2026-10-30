import { element as el, button, input, field, number } from './preview-ui.js';
import { createNotice } from './preview-notice.js';
import { createSelectScope } from './workbench-controls.js';

const defaults = [
  { name: '资料报告', body: '根据 {{资料路径}} 中的资料，制作 {{报告主题}} 报告。引用真实来源，保存到 {{输出目录}}，展示最终报告。' },
  { name: '三维作品', body: '使用 Blender 制作 {{作品描述}}，重点达到 {{质量要求}}。保存工程，并导出可交互预览 GLB 与最终渲染图，在右侧展示最终成果。' },
  { name: '表格分析', body: '分析 {{表格路径}}，回答 {{分析问题}}。先核对字段与缺失值，再给出统计和图表，保留计算依据，不扩大工作范围。' },
  { name: '网页制作', body: '制作 {{网页用途}} 网页，风格 {{视觉要求}}，支持手机和电脑，保存源码并在右侧展示可运行成果。' },
  { name: '媒体转录整理', body: '整理 {{媒体路径}}：{{整理要求}}。需要转录时先使用可用转录能力，保留时间点；没有能力时如实说明，不编造语音内容。' }
];
function readStore(key, fallback) { try { return JSON.parse(localStorage.getItem(key)) ?? fallback; } catch { return fallback; } }
function writeStore(key, value) { try { localStorage.setItem(key, JSON.stringify(value)); } catch { /* Native project storage remains authoritative. / 本机项目存储不依赖视图缓存。 */ } }
function textArea(value = '', placeholder = '') { const e = el('textarea'); e.value = value; e.placeholder = placeholder; return e; }
const symbols = {
  work: 'M3 7h18v13H3zM8 7V4h8v3M3 12h18M10 12v3h4v-3',
  files: 'M14 3H5v18h14V8zM14 3v5h5M8 12h8M8 16h6',
  research: 'M4 4h7v16H4zM13 4h7v16h-7M7 8h1M16 8h1M7 12h1M16 12h1',
  templates: 'M4 4h16v16H4zM4 10h16M10 10v10',
  batch: 'M7 3h14v14M3 7h14v14H3zM7 11h6M7 15h6',
  connect: 'M8 12H4m16 0h-4M9 8V5a3 3 0 0 1 6 0v3M9 16v3a3 3 0 0 0 6 0v-3M8 8h8v8H8z',
  diagnostics: 'M3 18h18M6 14V9M12 14V4M18 14v-3',
  notes: 'M4 3h16v18H4zM8 7h8M8 11h8M8 15h5',
  export: 'M12 3v12M8 11l4 4 4-4M4 16v5h16v-5',
  delete: 'M4 7h16M9 7V4h6v3M6 7l1 14h10l1-14M10 11v6M14 11v6',
  shortcut: 'M4 5h16v14H4zM7 9h1M11 9h1M15 9h1M7 13h1M11 13h5'
};
function icon(name) {
  const node = document.createElementNS('http://www.w3.org/2000/svg', 'svg'), path = document.createElementNS(node.namespaceURI, 'path');
  node.setAttribute('viewBox', '0 0 24 24'); node.setAttribute('aria-hidden', 'true'); node.classList.add('workbench-icon');
  path.setAttribute('d', symbols[name] || symbols.work); node.append(path); return node;
}
function card(title, description, symbol = 'files') { const node = el('section', 'workbench-card'), heading = el('h2'); heading.append(icon(symbol), el('span', '', title)); node.append(heading); if (description) node.append(el('p', 'workbench-card-description', description)); return node; }
function resultRows(result) { if (Array.isArray(result)) return result; return result?.files || result?.entries || result?.items || result?.messages || result?.results || result?.versions || result?.artifacts || result?.matches || []; }
function filePath(item) { return item.path || item.relativePath || item.reference || item.name || item.title || ''; }

export async function open(options) {
  const root = el('div', 'workbench'), header = el('header', 'workbench-header'), nav = el('nav', 'workbench-nav'), track = el('div', 'workbench-nav-track'), indicator = el('span', 'workbench-nav-indicator'), page = el('div', 'workbench-page'), feedback = createNotice(root, 'workbench-status', { dismissAfterMs: 5000 });
  const choices = createSelectScope(root), select = choices.select;
  const storedFontSize = Number(readStore('kynxa.workbench.fontsize', 15));
  root.style.setProperty('--workbench-font-size', (Number.isFinite(storedFontSize) ? Math.min(24, Math.max(12, storedFontSize)) : 15) + 'px');
  const identity = options.conversationId || 'local', storeKey = 'kynxa.workbench.' + identity;
  let state = readStore(storeKey, { templates: defaults, bookmarks: [], collections: [], notes: '' }), disposed = false, activePage = options.state?.workbench?.page || 'home', revision = 0, speech;
  state.templates ||= defaults; state.bookmarks ||= []; state.collections ||= [];
  function save() { writeStore(storeKey, state); }
  function report(text, error = false, marker = revision) { if (!disposed && marker === revision) feedback.show(text, error); }
  const notice = (text, error = false) => report(text, error);
  async function request(action, payload = {}, marker = revision) {
    if (disposed) throw new DOMException('工作台已关闭', 'AbortError');
    try { report('正在读取…', false, marker); const value = await options.request(action, payload); report('', false, marker); return value; }
    catch (error) { if (error?.name !== 'AbortError') report(error.message || String(error), true, marker); throw error; }
  }
  function pageActions(marker) {
    const scopedRequest = (action, payload) => request(action, payload, marker);
    return { request: scopedRequest, notice: (text, error = false) => report(text, error, marker),
      async saveProject() { save(); await scopedRequest('project.save', { item: { notes: state.notes, collections: state.collections } }); } };
  }
  const busyControls = new WeakSet();
  function run(fn, canEnable = () => true) {
    return (...args) => {
      if (disposed) return Promise.resolve();
      const marker = revision, target = args[0]?.currentTarget;
      const control = target && typeof target === 'object' && 'disabled' in target ? target : null;
      if (control && busyControls.has(control)) return Promise.resolve();
      if (control) { busyControls.add(control); control.disabled = true; }
      return Promise.resolve().then(() => fn(...args)).catch(error => {
        if (error?.name !== 'AbortError') report(error?.message || String(error), true, marker);
      }).finally(() => { if (control) { busyControls.delete(control); control.disabled = !canEnable(); } });
    };
  }
  function action(name) { options.onAction?.(name); }
  function exportText(filename, content, mimeType = 'text/plain') { options.onExport?.({ filename, content, mimeType, encoding: 'utf8' }); }
  function row(...nodes) { const n = el('div', 'workbench-row'); n.append(...nodes); return n; }
  function actionRow(...controls) { const n = row(...controls); n.classList.add('workbench-actions'); return n; }
  function primary(control) { control.classList.add('primary'); return control; }
  function destructiveButton(text, action) { const control = button('', action, text); control.classList.add('danger'); control.append(icon('delete'), el('span', '', text)); return control; }
  function showList(parent, items, render) { parent.replaceChildren(); if (!items.length) { const empty = el('div', 'workbench-empty'); empty.append(icon('files'), el('span', '', '暂无内容')); parent.append(empty); } items.forEach((item, i) => { const n = el('div', 'workbench-list-item'); render(n, item, i); parent.append(n); }); }
  function quote(text, title = '工作台') { const value = String(text || '').trim(); if (!value) { notice('先填写或选中需要引用的内容。', true); return; } options.onSelection?.({ kind: 'workbench', title, text: value.slice(0, 24000) }); notice('已放入输入框，可继续编辑后发送。'); }
  const tabs = [['home', '工作', '工作', 'work'], ['files', '成果', '成果与版本', 'files'], ['research', '资料', '资料', 'research'], ['templates', '模板', '模板', 'templates'], ['batch', '批量', '批量处理', 'batch'], ['connect', '辅助', '连接与辅助', 'connect'], ['diagnostics', '状态', '状态', 'diagnostics']];
  const heading = el('div', 'workbench-heading'), identityMark = el('span', 'workbench-mark'), title = el('div', 'workbench-title'), exportProject = button('', () => exportText('项目工作台.json', JSON.stringify(state, null, 2), 'application/json'), '导出项目记录');
  identityMark.append(icon('work')); title.append(el('h1', '', '创作工作台'), el('p', '', '整理资料，继续创作。')); heading.append(identityMark, title);
  exportProject.classList.add('workbench-export'); exportProject.setAttribute('aria-label', '导出项目记录'); exportProject.append(icon('export'), el('span', '', '导出记录')); header.append(heading, exportProject);
  indicator.setAttribute('aria-hidden', 'true'); track.append(indicator); nav.append(track);
  root.append(header, nav, feedback.node, page); options.container.append(root);
  for (const [id, label, accessibleLabel, symbol] of tabs) { const b = button('', () => navigate(id), accessibleLabel); b.dataset.page = id; b.id = 'workbench-tab-' + id; b.setAttribute('role', 'tab'); b.setAttribute('aria-label', accessibleLabel); b.setAttribute('aria-controls', 'workbench-page'); b.append(icon(symbol), el('span', '', label)); track.append(b); }
  nav.setAttribute('role', 'tablist'); nav.setAttribute('aria-label', '工作台页面'); page.id = 'workbench-page'; page.setAttribute('role', 'tabpanel');
  function positionIndicator() { const selected = track.querySelector('[aria-selected=true]'); if (!selected || disposed) return; indicator.style.width = selected.offsetWidth + 'px'; indicator.style.height = selected.offsetHeight + 'px'; indicator.style.transform = `translate(${selected.offsetLeft}px, ${selected.offsetTop}px)`; return selected; }
  function revealTab(selected, smooth) {
    if (!selected || !nav.clientWidth) return;
    const left = selected.offsetLeft - 6, right = selected.offsetLeft + selected.offsetWidth + 6;
    const target = left < nav.scrollLeft ? left : right > nav.scrollLeft + nav.clientWidth ? right - nav.clientWidth : nav.scrollLeft;
    if (Math.abs(target - nav.scrollLeft) > .5) nav.scrollTo({ left: Math.max(0, target), behavior: smooth && !matchMedia('(prefers-reduced-motion: reduce)').matches ? 'smooth' : 'instant' });
  }
  const resizeObserver = new ResizeObserver(() => revealTab(positionIndicator(), false)); resizeObserver.observe(nav); resizeObserver.observe(track);
  nav.addEventListener('wheel', event => {
    if (event.ctrlKey || Math.abs(event.deltaX) > Math.abs(event.deltaY) || nav.scrollWidth <= nav.clientWidth) return;
    const next = Math.max(0, Math.min(nav.scrollWidth - nav.clientWidth, nav.scrollLeft + event.deltaY));
    if (Math.abs(next - nav.scrollLeft) < .5) return;
    event.preventDefault(); nav.scrollLeft = next;
  }, { passive: false });
  nav.addEventListener('keydown', event => { const buttons = [...track.querySelectorAll('button')], index = buttons.indexOf(document.activeElement); if (index < 0 || !['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return; event.preventDefault(); const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : (index + (event.key === 'ArrowRight' ? 1 : -1) + buttons.length) % buttons.length; buttons[next].focus(); navigate(buttons[next].dataset.page); });
  function navigate(id) { if (disposed) return; choices.close(); activePage = id; revision++; speech?.stop?.();
    track.querySelectorAll('button').forEach(b => { const selected = b.dataset.page === id; b.setAttribute('aria-selected', selected); b.tabIndex = selected ? 0 : -1; if (selected) { page.setAttribute('aria-labelledby', b.id); revealTab(b, true); } }); positionIndicator(); page.replaceChildren(); notice('');
    options.onState?.({ workbench: { page: id } }); run(({ home, files, research, templates, batch, connect, diagnostics }[id] || home))(); }
  async function home() {
    const marker = revision, { request, notice, saveProject } = pageActions(marker);
    const stored = await request('project.read'); if (marker !== revision) return;
    if (typeof stored.notes === 'string') state.notes = stored.notes;
    if (Array.isArray(stored.collections)) state.collections = stored.collections;
    page.append(el('p', 'workbench-subtitle', '从当前聊天出发，管理资料、作品与下一步。'));
    const grid = el('div', 'workbench-grid');
    const creation = card('输入与制作', '将附件、工作目录与工具接入当前聊天。', 'work');
    creation.append(actionRow(button('添加附件', () => action('attach')), button('关联文件夹', () => action('folder')), button('浏览文件', () => action('files')), button('打开终端', () => action('terminal')), button('工具与技能', () => action('extensions'))));
    const works = card('继续修改成果', '在预览中选中文字、表格或三维对象，引用后描述要修改的部分。'); works.append(actionRow(button('查看成果与版本', () => navigate('files')), button('使用工作模板', () => navigate('templates'))));
    const notes = card('项目笔记', '记录作品要求、版本说明与重要决定。', 'notes'); const note = textArea(state.notes || '', '项目要点、版本说明、重要决定…'); note.setAttribute('aria-label', '项目笔记'); note.onchange = run(async () => { state.notes = note.value.slice(0, 200000); await saveProject(); }); notes.append(note, actionRow(button('引用笔记', () => quote(note.value, '项目笔记'))));
    const access = card('快捷操作', '常用入口，随时回到创作。', 'shortcut'); for (const [key, desc] of [['Ctrl+L', '输入框'], ['Ctrl+N', '新聊天'], ['Ctrl+F', '当前聊天查找'], ['Ctrl+Shift+K', '历史会话查找']]) { const shortcut = el('div', 'workbench-shortcut'); shortcut.append(el('span', '', desc), el('kbd', 'workbench-key', key)); access.append(shortcut); } access.append(button('辅助与连接', () => navigate('connect')));
    grid.append(creation, works, notes, access); page.append(grid);
    const importFile = el('input'); importFile.type = 'file'; importFile.accept = '.json'; importFile.hidden = true;
    importFile.onchange = run(async () => {
      const f = importFile.files?.[0]; if (!f || f.size > 4 * 1024 * 1024) throw new Error('请选择 4 MiB 以内的项目记录');
      const v = JSON.parse(await f.text());
      if (!Array.isArray(v.templates) || !Array.isArray(v.bookmarks) || v.templates.some(t => typeof t?.name !== 'string' || typeof t?.body !== 'string') || v.bookmarks.some(b => typeof b?.url !== 'string' || !/^https?:\/\//i.test(b.url))) throw new Error('项目记录格式不正确');
      const imported = { templates: v.templates.slice(0, 200), bookmarks: v.bookmarks.slice(0, 200), collections: (Array.isArray(v.collections) ? v.collections : []).filter(c => typeof c?.name === 'string' && Array.isArray(c.paths) && c.paths.every(p => typeof p === 'string')).slice(0, 200), notes: String(v.notes || '').slice(0, 200000) };
      for (const item of imported.templates) await request('templates.save', { item: { name: item.name, body: item.body } });
      for (const item of imported.bookmarks) await request('bookmarks.save', { item: { title: String(item.title || ''), url: item.url, note: String(item.note || '') } });
      state = imported; state.customTemplatesUsed = true; await saveProject(); if (!disposed && marker === revision) navigate('home');
    });
    page.append(row(button('导入项目记录', () => importFile.click()), importFile));
  }
  async function files() {
    const marker = revision, { request, notice, saveProject } = pageActions(marker);
    const stored = await request('project.read'); if (marker !== revision) return;
    if (Array.isArray(stored.collections)) state.collections = stored.collections;
    const path = input('目录路径，留空为当前工作目录'), filter = input('筛选已载入的文件名或路径'), list = el('div', 'workbench-list');
    list.classList.add('workbench-files');
    const versions = el('div', 'workbench-list'), diff = el('div', 'workbench-diff'), editor = textArea('', '选择文件的“编辑文本”后载入内容');
    const listing = el('span', 'preview-muted'), selectedLabel = el('p', 'workbench-current-file', '尚未选择版本文件'), editingLabel = el('p', 'workbench-current-file', '尚未载入编辑文件');
    const pageText = el('span', 'preview-muted'), pageSize = 40;
    let editingPath = '', editingHash = '', readTruncated = false, readingEditor = false, selected = '', fileItems = [], filePage = 0;
    let listRequest = 0, versionRequest = 0, editRequest = 0, listedPath = '.', listingTruncated = false, deliverables = false;
    const collection = input('作品名称', '当前作品'), groups = el('div', 'workbench-list');
    function folder(item) { return item.isDirectory || item.kind === 'directory' || item.type === 'directory'; }
    function name(item) { return String(item.name || filePath(item).split(/[\\/]/).pop() || filePath(item)); }
    function kind(item) {
      if (folder(item)) return 'folder';
      const extension = name(item).split('.').pop().toLowerCase();
      if (['glb','gltf','blend','obj','stl'].includes(extension)) return 'model';
      if (['png','jpg','jpeg','webp','gif','bmp','svg','tif','tiff'].includes(extension)) return 'image';
      if (['mp4','webm','mov','mp3','wav','flac','m4a','ogg'].includes(extension)) return 'media';
      if (['pdf','doc','docx','ppt','pptx','xls','xlsx','csv','tsv','md','txt','mmd'].includes(extension)) return 'document';
      return 'other';
    }
    const category = select([['all','全部类型'],['folder','文件夹'],['document','文档与表格'],['model','三维模型'],['image','图片'],['media','音视频'],['other','其他']], 'all', resetList);
    const sort = select([['name','名称 A → Z'],['name-desc','名称 Z → A']], 'name', resetList);
    category.setAttribute('aria-label', '文件类型'); sort.setAttribute('aria-label', '文件排序'); filter.setAttribute('aria-label', '筛选已载入文件'); path.setAttribute('aria-label', '工作台目录');
    function changeFilePage(delta) {
      filePage += delta; renderFiles();
      list.querySelector('button')?.focus({ preventScroll: true });
      list.scrollIntoView({ block: 'start', behavior: 'instant' });
    }
    const previous = button('上一组', () => changeFilePage(-1)), next = button('下一组', () => changeFilePage(1));
    function resetList() { filePage = 0; renderFiles(); }
    function fileHeading(parent, item) {
      const p = filePath(item), heading = el('div', 'workbench-file-heading'), mark = icon(folder(item) ? 'work' : 'files'), text = el('div', 'workbench-file-label');
      const title = el('strong', '', name(item)); title.title = p; text.append(title, el('span', 'workbench-file-path', p));
      heading.append(mark, text); parent.append(heading); return p;
    }
    function renderFiles() {
      const term = filter.value.trim().toLocaleLowerCase();
      const visible = fileItems.filter(item => (!term || filePath(item).toLocaleLowerCase().includes(term) || name(item).toLocaleLowerCase().includes(term)) && (category.value === 'all' || kind(item) === category.value));
      visible.sort((a,b) => Number(!!folder(b))-Number(!!folder(a)) || name(a).localeCompare(name(b), 'zh-CN', {numeric:true}) * (sort.value === 'name-desc' ? -1 : 1));
      const pages = Math.max(1, Math.ceil(visible.length / pageSize)); filePage = Math.min(Math.max(0,filePage), pages-1);
      listing.textContent = `${visible.length} / ${fileItems.length} 项${deliverables ? ' · 已生成成果' : ' · 当前载入'}${listingTruncated ? ' · 列表已截断，可进入子目录查找' : ''}`;
      pageText.textContent = `${filePage+1} / ${pages}`; previous.disabled = filePage === 0; next.disabled = filePage === pages-1;
      showList(list, visible.slice(filePage*pageSize, (filePage+1)*pageSize), (n,item) => {
        const p = fileHeading(n,item), actions = row(button(folder(item) ? '进入' : '打开', run(async () => { if (folder(item)) { path.value=p; await load(); } else options.onOpen?.(p); })));
        if (!folder(item)) {
          const more = el('details', 'workbench-file-more'), summary = el('summary', '', '更多');
          more.append(summary, row(button('授权手机下载', run(async () => { await request('remote.registerDownload', {path:p}); notice('已将此文件授权给当前会话配对的手机。'); })), button('关联到作品', run(async () => {
            const title = collection.value.trim() || '当前作品'; let c = state.collections.find(c=>c.name===title);
            if (!c) { c={name:title,paths:[]}; state.collections.push(c); } if (!c.paths.includes(p)) c.paths.push(p);
            await saveProject(); if (marker!==revision) return; notice('已关联到“'+title+'”'); renderCollections();
          }))));
          actions.append(button('版本与比较', run(()=>inspect(p))), button('编辑文本', run(()=>edit(p))), more);
        }
        n.append(actions);
      });
      if (!visible.length) {
        const empty=list.firstElementChild; empty?.replaceChildren(icon('files'), el('span','',fileItems.length ? '没有匹配的文件，试试其他关键词或类型。' : '这里还没有文件。'));
        empty?.append(button(fileItems.length ? '清除筛选' : '浏览工作文件', ()=> { if(fileItems.length){ filter.value='';category.value='all';resetList(); } else action('files'); }));
      }
    }
    async function load() {
      const sequence=++listRequest, requestedPath=path.value.trim()||'.', result=await request('files.list',{path:requestedPath,limit:500}); if(marker!==revision || sequence!==listRequest) return;
      listedPath=requestedPath; deliverables=false; fileItems=resultRows(result); listingTruncated=!!result.truncated || !!result.hasMore; resetList();
    }
    async function inspect(p) {
      const sequence=++versionRequest; selected=p; selectedLabel.textContent=p+' · 正在读取版本…';
      const result=await request('versions.list',{path:p}); if(marker!==revision || sequence!==versionRequest) return;
      selectedLabel.textContent=p; selectedLabel.title=p; diff.replaceChildren();
      const expectedHash=result.currentHash || result.version || result.currentVersion || '';
      showList(versions,resultRows(result),(n,item)=> { const hash=item.version || item.sha256 || item.hash;
        n.append(el('strong','',`${item.createdAt || item.savedAt || ''} ${String(hash || '').slice(0,16)}`));
        n.append(actionRow(button('比较',run(async()=> { const r=await request('versions.compare',{path:p,version:hash}); if(marker!==revision || sequence!==versionRequest) return; diff.replaceChildren(el('pre','',r.beforeText ?? r.before ?? r.savedText ?? ''),el('pre','',r.afterText ?? r.after ?? r.currentText ?? '')); })),
          button('恢复此版本',run(async()=> { if(sequence!==versionRequest) return; await request('versions.restore',{path:p,version:hash,expectedHash}); if(marker!==revision || sequence!==versionRequest) return; notice('版本已恢复'); await inspect(p); if(marker===revision) options.onOpen?.(p); }))));
      });
      if(!resultRows(result).length) versions.replaceChildren(el('p','preview-muted','还没有快照，可先保存当前版本。'));
    }
    const saveEdit=button('保存修改',run(async()=> {
      if(!editingPath || !editingHash) throw new Error('先载入要编辑的文件'); if(readTruncated) throw new Error('内容不完整，不能覆盖原文件');
      const p=editingPath, hash=editingHash, content=editor.value, sequence=editRequest;
      const r=await request('files.write',{path:p,expectedHash:hash,encoding:'utf8',content}); if(marker!==revision || sequence!==editRequest) return;
      editingHash=r.sha256 || r.version || ''; notice('已保存修改并保留旧版快照'); if(marker===revision) options.onOpen?.(p);
    },()=>!readingEditor && !!editingPath && !!editingHash && !readTruncated)); saveEdit.disabled=true; primary(saveEdit);
    async function edit(p) {
      const sequence=++editRequest; readingEditor=true; saveEdit.disabled=true;
      try {
        const version=await request('versions.list',{path:p}), r=await request('files.read',{path:p,maxChars:64000}); if(marker!==revision || sequence!==editRequest) return;
        const text=r.text ?? r.content ?? r.result?.text; if(typeof text!=='string') throw new Error('此文件未返回可编辑文本');
        editingPath=p; editingHash=r.sha256 || r.version || version.currentHash; editor.value=text; readTruncated=!!r.truncated || !!r.hasMore;
        editingLabel.textContent=p+(readTruncated ? ' · 部分内容，只可引用' : ''); editingLabel.title=p;
        if(readTruncated) notice('当前只读取部分内容，可引用分析，不能覆盖保存。'); editor.scrollIntoView({behavior:matchMedia('(prefers-reduced-motion: reduce)').matches?'instant':'smooth',block:'center'});
      } finally { if(sequence===editRequest) { readingEditor=false; saveEdit.disabled=readTruncated || !editingHash || busyControls.has(saveEdit); } }
    }
    function renderCollections() { showList(groups,state.collections,(n,c)=> { n.append(el('strong','',c.name)); const links=actionRow(); for(const p of c.paths || []) { const open=button(p.split(/[\\/]/).pop(),()=>options.onOpen?.(p),p); open.classList.add('workbench-associated-file');links.append(open); } links.append(button('移除关联组',run(async()=> { const i=state.collections.indexOf(c);if(i<0)return;state.collections.splice(i,1);await saveProject();if(marker===revision)renderCollections(); }))); n.append(links); }); }
    path.onkeydown=e=> { if(e.key==='Enter') { e.preventDefault();run(load)(); } }; filter.oninput=resetList;
    editor.onkeydown=e=> { if((e.ctrlKey || e.metaKey) && e.key.toLowerCase()==='s'){e.preventDefault();if(!saveEdit.disabled)saveEdit.click();} };
    const up=button('上级目录',run(async()=> {
      const current=listedPath.replace(/[\\/]+$/,'');
      if(current==='.' || !current || /^[A-Za-z]:$/.test(current) || /^\\\\[^\\]+\\[^\\]+$/.test(current)) { path.value='.'; }
      else { const index=Math.max(current.lastIndexOf('/'),current.lastIndexOf('\\'));path.value=index<0?'.':current.slice(0,index+1); }
      await load();
    }));
    const browsing = card('工作文件', '浏览当前目录，按名称或类型查找成果。');
    browsing.append(row(path,primary(button('读取文件',run(load))),up,button('已生成成果',run(async()=> { const sequence=++listRequest,r=await request('deliverables.list');if(marker!==revision || sequence!==listRequest)return;fileItems=resultRows(r);deliverables=true;listingTruncated=!!r.truncated || !!r.hasMore;resetList(); }))),
      row(filter,category,sort),listing,list,row(previous,pageText,next));
    const associated = card('关联作品', '将相关文件整理到同一作品中。'); associated.append(row(collection),groups);
    const history = card('版本记录', '保存快照后，可比较内容或恢复已有版本。');
    history.append(selectedLabel,actionRow(button('保存选中文件快照',run(async()=> { if(!selected)throw new Error('先选择文件的“版本与比较”');const p=selected;await request('versions.create',{path:p});if(marker===revision)await inspect(p); }))),versions,diff);
    const editing = card('文本与源码编辑', '载入完整内容后保存修改，原有版本继续保留。', 'notes');
    editing.append(editingLabel,editor,row(saveEdit,button('让模型修改选中内容',()=>quote(`${editingPath}\n\n${editor.value.slice(editor.selectionStart,editor.selectionEnd)||editor.value}`,'文件修改'))));
    page.append(browsing,associated,history,editing);
    renderCollections();run(load)();
  }
  async function research() {
    const marker = revision, { request, notice } = pageActions(marker);
    const saved = await request('bookmarks.list'); if (marker !== revision) return;
    if (Array.isArray(saved.items)) { state.bookmarks = saved.items; save(); }
    const q = input('查找以前的讨论、决定与成果'), results = el('div', 'workbench-list'), url = input('参考网页地址 https://'), title = input('标题'), note = textArea('', '摘录或注释'), bookmarks = el('div', 'workbench-list');
    function render() { showList(bookmarks, state.bookmarks, (n, item, i) => { n.append(el('strong', '', item.title || item.url), el('p', '', item.note)); n.append(actionRow(button('打开来源', () => options.onOpen?.(item.url)), button('引用', () => quote(`${item.title}\n${item.url}\n${item.note}`, '参考资料')), destructiveButton('删除', run(async () => { if (item.id) { const r = await request('bookmarks.delete', { id: item.id }); state.bookmarks = r.items; } else state.bookmarks.splice(i, 1); save(); render(); })))); }); }
    async function search() { const r = await request('search.conversations', { query: q.value, limit: 20 }); showList(results, resultRows(r), (n, item) => { const text = item.excerpt || item.snippet || item.text || item.content || ''; n.append(el('strong', '', item.title || item.conversationTitle || '历史讨论'), el('p', '', text)); const actions = actionRow(button('引用内容', () => quote(text, item.title || '历史讨论'))); if (item.conversationId || item.id) actions.append(button('打开原会话', () => options.onNavigate?.(item.conversationId || item.id))); n.append(actions); }); }
    const discussions = card('历史讨论', '查找当前范围中的讨论、决定与成果。', 'research');
    discussions.append(row(q, primary(button('搜索历史', run(search)))),results);
    const sources = card('来源与注释', '保留参考地址、摘录与注释，方便继续使用。', 'notes');
    sources.append(row(title,url),note,row(primary(button('收藏来源', run(async () => { const address = new URL(url.value); if (!['http:', 'https:'].includes(address.protocol)) throw new Error('请输入 HTTP 或 HTTPS 地址'); const r = await request('bookmarks.save', { item: { title: title.value.trim(), url: address.href, note: note.value, savedAt: new Date().toISOString() } }); state.bookmarks = r.items; save(); render(); }))),
      button('导出参考资料', () => exportText('参考资料.md', state.bookmarks.map((b, i) => `## ${i + 1}. ${b.title || b.url}\n\n${b.url}\n\n${b.note || ''}`).join('\n\n'), 'text/markdown'))),bookmarks);
    page.append(discussions,sources);
    q.onkeydown = e => { if (e.key === 'Enter') run(search)(); }; render();
  }
  async function templates() {
    const marker = revision, { request, notice } = pageActions(marker);
    const persisted = await request('templates.list'); if (marker !== revision) return;
    if (Array.isArray(persisted.items)) { state.templates = persisted.items.length || state.customTemplatesUsed ? persisted.items : defaults; save(); }
    const list = el('div', 'workbench-list'), name = input('模板名称'), body = textArea('', '用 {{参数名}} 表示每次需要填写的内容'), parameters = el('div'), preview = textArea('', '填入参数后生成提示词'); let editingItem = null, values = {}, templateRevision = 0;
    const parameterInputs = new Map(); parameters.className = 'workbench-template-parameters';
    function prepare(item, index, focusEditor = false) { templateRevision++; editingItem = index >= 0 ? item : null; name.value = item.name; body.value = item.body; values = {}; parameterInputs.clear(); parameters.replaceChildren(); syncParameters(); if(focusEditor) { name.focus({preventScroll:true}); name.scrollIntoView({block:'center',behavior:matchMedia('(prefers-reduced-motion: reduce)').matches?'instant':'smooth'}); } }
    function syncParameters() {
      const keys = [...new Set(body.value.match(/\{\{[^{}]+\}\}/g) || [])];
      for (const [key, control] of parameterInputs) if (!keys.includes(key)) { control.parentElement.parentElement.remove(); parameterInputs.delete(key); delete values[key]; }
      for (const [index, key] of keys.entries()) {
        if (!parameterInputs.has(key)) { const control = input(key.slice(2, -2)); control.oninput = () => { values[key] = control.value; fill(); }; parameterInputs.set(key, control); }
        const control = parameterInputs.get(key), parameterRow = control.parentElement?.parentElement || row(field(key.slice(2, -2), control));
        if(parameters.children[index] !== parameterRow) parameters.insertBefore(parameterRow, parameters.children[index] || null);
      }
      fill();
    }
    function fill() { preview.value = body.value.replace(/\{\{[^{}]+\}\}/g, key => values[key] || key); }
    function render() { showList(list, state.templates, (n, item, i) => { n.append(el('strong', '', item.name), el('p', '', item.body), actionRow(button('使用／编辑', () => prepare(item, i, true)), destructiveButton('删除', run(async () => { if (item.id) { const r = await request('templates.delete', { id: item.id }); state.templates = r.items; } else { const index = state.templates.indexOf(item); if (index >= 0) state.templates.splice(index, 1); } if (editingItem === item || (item.id && editingItem?.id === item.id)) editingItem = null; state.customTemplatesUsed = true; save(); render(); })))); }); }
    const library = card('工作模板', '复用常用要求，按实际任务填写参数。', 'templates'); library.append(list);
    const templateEditor = card('模板编辑', '使用 {{参数名}} 标记需要填写的内容。', 'notes');
    body.oninput = syncParameters; templateEditor.append(name, body, row(button('新建模板', () => prepare({ name: '', body: '' }, -1, true)), primary(button('保存模板', run(async () => { if (!name.value.trim() || !body.value.trim()) throw new Error('填写模板名称与内容'); const item = { name: name.value.trim(), body: body.value }, prepared = templateRevision; if (editingItem?.id) item.id = editingItem.id; const r = await request('templates.save', { item }); state.templates = r.items; state.customTemplatesUsed = true; save(); render(); if(prepared===templateRevision) editingItem = state.templates.find(t => t.id === r.id) || null; notice('模板已保存'); })))), parameters, preview,
      button('放入输入框', () => quote(preview.value, name.value || '工作模板'))); page.append(library,templateEditor); render(); if (state.templates.length) prepare(state.templates[0], 0);
  }
  function batch() {
    const { notice } = pageActions(revision);
    const picker = el('input'); picker.type = 'file'; picker.multiple = true; const list = el('div', 'workbench-list'), progress = el('progress', 'workbench-progress'); progress.max = 1; progress.value = 0;
    const prefix = input('输出名前缀', '整理_'), extension = select([['original', '保留格式'], ['png', '图片 → PNG'], ['jpeg', '图片 → JPEG'], ['webp', '图片 → WebP'], ['txt', '文本 → UTF-8 TXT'], ['md', '文本 → Markdown']], 'original', () => {});
    extension.setAttribute('aria-label', '输出格式');
    let files = [], maxWidth = 1920;
    picker.onchange = () => { files = [...picker.files].slice(0, 500); showList(list, files, (n, f, i) => n.append(el('strong', '', `${i + 1}. ${f.name}`), el('p', '', `${(f.size / 1024).toFixed(1)} KiB`))); };
    const processing = card('批量处理', '选择输入文件，输出打成一个 ZIP；原文件保留。支持图片缩放转换、文本编码转换和批量命名。', 'batch');
    processing.append(picker,
      row(prefix, extension, field('图片最长边', number(1920, 64, 8192, n => maxWidth = n))), list, progress,
      primary(button('生成批量处理 ZIP', run(async () => {
        if (!files.length) throw new Error('先选择文件'); if (!window.JSZip) throw new Error('ZIP 组件尚未加载');
        if (files.reduce((n, f) => n + f.size, 0) > 48 * 1024 * 1024) throw new Error('本次批量输入最多 48 MiB，请分组处理');
        const zip = new window.JSZip(), report = []; let index = 0;
        for (const f of files) {
          let bytes = await f.arrayBuffer(), ext = f.name.split('.').pop(), result = '原件'; const target = extension.value;
          if (['png', 'jpeg', 'webp'].includes(target)) {
            if (!f.type.startsWith('image/')) { report.push(`${f.name}\t跳过：不是图片`); continue; }
            const bitmap = await createImageBitmap(f); const ratio = Math.min(1, maxWidth / Math.max(bitmap.width, bitmap.height)), canvas = document.createElement('canvas');
            canvas.width = Math.max(1, Math.round(bitmap.width * ratio)); canvas.height = Math.max(1, Math.round(bitmap.height * ratio)); const ctx = canvas.getContext('2d'); if (target === 'jpeg') { ctx.fillStyle = '#fff'; ctx.fillRect(0, 0, canvas.width, canvas.height); } ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height); bitmap.close();
            bytes = await new Promise((resolve, reject) => canvas.toBlob(b => b ? b.arrayBuffer().then(resolve, reject) : reject(new Error('图片转换失败')), 'image/' + target, .92)); ext = target === 'jpeg' ? 'jpg' : target; result = `${canvas.width}×${canvas.height}`;
          } else if (target === 'txt' || target === 'md') {
            let text; try { text = new TextDecoder('utf-8', { fatal: true }).decode(bytes); } catch { text = new TextDecoder('gb18030').decode(bytes); }
            bytes = new TextEncoder().encode(text); ext = target; result = 'UTF-8';
          }
          const safe = prefix.value.replace(/[<>:"/\\|?*\x00-\x1f]/g, '_') + String(++index).padStart(3, '0') + '_' + f.name.replace(/\.[^.]+$/, '').replace(/[<>:"/\\|?*\x00-\x1f]/g, '_') + '.' + ext;
          zip.file(safe, bytes); report.push(`${f.name}\t${safe}\t${result}`); progress.value = index / files.length;
        }
        zip.file('处理清单.tsv', report.join('\n')); const content = await zip.generateAsync({ type: 'base64', compression: 'STORE' }); options.onExport?.({ filename: '批量处理.zip', mimeType: 'application/zip', encoding: 'base64', content }); notice(`已整理 ${index} 个文件，请选择保存位置。`);
      })))); page.append(processing);
  }
  function connect() {
    const { request, notice } = pageActions(revision);
    const grid = el('div', 'workbench-grid'), voice = card('语音与朗读', '语音输入与文字朗读，按需启动。', 'connect'), transcript = textArea('', '识别文字或输入需要朗读的内容');
    const Recognition = window.SpeechRecognition || window.webkitSpeechRecognition;
    voice.append(transcript, actionRow(button('开始语音输入', () => {
      if (!Recognition) { notice('当前 WebView 未提供语音识别，可用 Windows 的 Win+H 语音输入。'); return; }
      speech?.stop?.(); speech = new Recognition(); speech.lang = 'zh-CN'; speech.continuous = true; speech.interimResults = false;
      speech.onresult = e => { for (let i = e.resultIndex; i < e.results.length; i++) if (e.results[i].isFinal) transcript.value += e.results[i][0].transcript; };
      speech.onerror = e => notice('语音识别：' + e.error, true); speech.start();
    }), button('停止语音', () => speech?.stop?.()), button('放入输入框', () => quote(transcript.value, '语音输入')),
      button('朗读', () => { if (!window.speechSynthesis) { notice('当前环境没有语音合成'); return; } speechSynthesis.cancel(); const utterance = new SpeechSynthesisUtterance(transcript.value); utterance.lang = 'zh-CN'; speechSynthesis.speak(utterance); }), button('停止朗读', () => window.speechSynthesis?.cancel())));
    const appearance = card('阅读与操作', '调整工作区文字与通知方式。', 'templates'); let size = Number(readStore('kynxa.workbench.fontsize', 15));
    appearance.append(row(field('阅读字号', number(size, 12, 24, n => { root.style.setProperty('--workbench-font-size', n + 'px'); writeStore('kynxa.workbench.fontsize', n); }))), actionRow(button('设置', () => action('settings')),
      button('启用完成通知', () => action('notifications')), button('关闭完成通知', () => action('notifications-off'))));
    const remote = card('手机连接', '连接后可在手机查看进度、发送消息和接收成果。公网访问需可达地址或中继。', 'connect');
    remote.append(actionRow(button('启动手机连接', () => action('remote-start')), button('关闭连接', () => action('remote-stop')), button('刷新连接状态', run(async () => { const r = await request('remote.status'); renderRemote(remote, r); }))));
    grid.append(voice, appearance, remote); page.append(grid);
    function renderRemote(parent, r) { parent.querySelectorAll('.remote-status').forEach(n => n.remove()); const n = el('div', 'remote-status');
      const url = r.inviteUrl || r.url; if (url) { n.append(el('p', '', url)); if (window.qrcode) { const qr = window.qrcode(0, 'M'); qr.addData(url); qr.make(); const view = el('div', 'workbench-qr'); view.innerHTML = qr.createSvgTag({ scalable: true, margin: 3 }); n.append(view); } }
      for (const d of r.pending || []) n.append(row(el('span', '', d.name || d.id), button('允许此设备', run(async () => { await request('remote.approve', { id: d.id }); notice('设备已允许'); })), button('拒绝', run(() => request('remote.reject', { id: d.id })))));
      for (const d of r.devices || []) n.append(row(el('span', '', d.name || d.id), button('授权当前会话', run(async () => { await request('remote.grantConversation', { deviceId: d.id }); notice('已授权当前会话'); })), button('撤销连接', run(() => request('remote.revoke', { id: d.id })))));
      if (!url) n.append(el('p', '', r.notice || '连接服务未启动')); parent.append(n); }
  }
  function diagnostics() {
    const marker=revision, { request }=pageActions(marker), view=el('div','workbench-metrics'), detail=el('div','workbench-list'), updated=el('p','workbench-subtitle');
    const labels={taskId:'任务标识',title:'名称',state:'状态',enabled:'已启用',ready:'已就绪',transport:'连接方式',toolCount:'工具数',code:'状态代码',name:'名称',id:'标识',updatedAtMs:'更新时间',stopReason:'暂停原因',scope:'统计范围',modelRequests:'模型请求',inputTokens:'输入 Token',outputTokens:'输出 Token',knownTokenRequests:'用量完整请求',unknownTokenRequests:'用量待确认',pendingRequests:'待结算请求',cacheReadTokens:'缓存读取 Token',cacheInputTokens:'有缓存读数的输入 Token',knownCacheRequests:'有缓存读数的请求',cacheHitRate:'缓存命中率',cacheCreationTokens:'缓存写入 Token',uncachedInputTokens:'未命中输入 Token',knownCacheCreationRequests:'有写入读数的请求',knownUncachedRequests:'有未命中读数的请求',firstVisibleTextMs:'平均首段文字',firstVisibleTextMsTotal:'首段文字累计等待',knownFirstVisibleTextRequests:'有首段文字读数的请求'};
    const states={running:'进行中',queued:'排队中',paused:'已暂停',completed:'已完成',partial:'尚未完成',failed:'未完成',cancelled:'已取消',ready:'已就绪',connected:'已连接',disconnected:'未连接',disabled:'已停用','waiting-network':'等待网络','waiting-resource':'等待资源',pending:'等待中',unknown:'待确认',conversation:'当前聊天',project:'当前工作'};
    function display(key,value) {
      if(value===null || value===undefined) return '未报告';
      if(typeof value==='boolean') return value?'是':'否';
      if(key==='cacheHitRate' && Number.isFinite(value)) return (value*100).toFixed(1)+'%';
      if(key==='updatedAtMs' && Number.isFinite(value)) return value>0?new Date(value).toLocaleString():'未记录';
      if(key.includes('TextMs') && Number.isFinite(value)) return value>=1000?(value/1000).toFixed(2)+' 秒':Math.round(value)+' 毫秒';
      return states[value] || (typeof value==='number'?value.toLocaleString():String(value));
    }
    function facts(parent,item) {
      if(!item || typeof item!=='object') {parent.append(el('p','',String(item??'未报告')));return;}
      const grid=el('dl','workbench-facts');
      for(const [key,value] of Object.entries(item)) if(value===null || typeof value!=='object') {
        const label=el('dt','',labels[key] || key), text=el('dd','',display(key,value)); label.title=key;text.title=String(value??'');
        if(key==='state') text.classList.add('workbench-state'); grid.append(label,text);
      }
      if(grid.children.length) parent.append(grid); else parent.append(el('p','preview-muted','暂无可显示的摘要。'));
    }
    async function refresh() {
      const r=await request('diagnostics');if(marker!==revision)return;view.replaceChildren();detail.replaceChildren();
      const usage=r.usage || {}, metrics=[['模型请求',usage.modelRequests??'—'],['输入 Token',usage.inputTokens??'—'],['缓存命中',Number.isFinite(usage.cacheHitRate)?(usage.cacheHitRate*100).toFixed(1)+'%':'未报告'],['平均首段文字',Number.isFinite(usage.firstVisibleTextMs)?display('firstVisibleTextMs',usage.firstVisibleTextMs):'未记录']];
      for(const [label,value] of metrics) {const c=el('div','workbench-metric');c.append(el('b','',String(value)),el('span','',label));view.append(c);}
      updated.textContent='当前聊天与工作范围 · 更新于 '+new Date().toLocaleTimeString()+' · 缺失读数显示为未报告';
      for(const key of ['tasks','models','cache','resources','extensions','runtime','usage']) if(r[key]) {
        const c=card(({tasks:'任务',models:'模型',cache:'缓存',resources:'资源',extensions:'扩展',runtime:'运行环境',usage:'调用与缓存记录'})[key]);
        const value=r[key]; if(Array.isArray(value)) {
          if(!value.length)c.append(el('p','preview-muted','当前范围暂无记录。'));
          for(const item of value.slice(0,30)) {const entry=el('div','workbench-diagnostic-entry');facts(entry,item);c.append(entry);}
          if(value.length>30)c.append(el('p','preview-muted',`显示前 30 / ${value.length} 条`));
        } else facts(c,value);detail.append(c);
      }
    }
    page.append(row(button('刷新状态',run(refresh)),button('本地模型与设置',()=>action('settings')),button('工具与技能',()=>action('extensions'))),updated,view,detail);run(refresh)();
  }
  navigate(tabs.some(([id]) => id === activePage) ? activePage : 'home');
  return { getState: () => ({ workbench: { page: activePage } }), setActive(active) { if (!active) { choices.close(); speech?.stop?.(); window.speechSynthesis?.cancel(); } }, dispose() { disposed = true; revision++; choices.dispose(); resizeObserver.disconnect(); speech?.stop?.(); window.speechSynthesis?.cancel(); feedback.dispose(); } };
}
