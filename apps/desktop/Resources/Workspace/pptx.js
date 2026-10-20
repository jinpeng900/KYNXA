/* Static OOXML slides keep their original coordinates inside a scriptless document frame.
 * 静态 OOXML 幻灯片在禁脚本文档框架中保留原始坐标，不按网页段落重新排版。 */
(() => {
  'use strict';

  const REL = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships';
  const SVG = 'http://www.w3.org/2000/svg';
  const EMU = 9525;
  const MAX_EXPANDED_BYTES = 128 * 1024 * 1024;
  const MAX_XML_BYTES = 64 * 1024 * 1024;
  const MAX_MEDIA_BYTES = 32 * 1024 * 1024;
  const defaultColors = { dk1: '#000000', lt1: '#ffffff', dk2: '#334155', lt2: '#f1f5f9',
    accent1: '#4472c4', accent2: '#ed7d31', accent3: '#a5a5a5', accent4: '#ffc000',
    accent5: '#5b9bd5', accent6: '#70ad47', hlink: '#0563c1', folHlink: '#954f72' };
  const defaultMap = { bg1: 'lt1', tx1: 'dk1', bg2: 'lt2', tx2: 'dk2' };

  function failure(message, code = 'PPTX_INVALID') {
    const error = new Error(message); error.code = code; return error;
  }

  const unsupported = message => failure(message, 'PPTX_UNSUPPORTED');
  const children = (node, name) => [...(node?.children || [])].filter(child => !name || child.localName === name);
  const child = (node, name) => children(node, name)[0] || null;
  const descendants = (node, name) => [...(node?.getElementsByTagNameNS('*', name) || [])];
  const attr = (node, name, fallback = '') => node?.getAttribute(name) ?? fallback;
  const number = (node, name, fallback = 0) => {
    const value = Number(attr(node, name, fallback)); return Number.isFinite(value) ? value : fallback;
  };
  const relId = (node, name = 'id') => node?.getAttributeNS(REL, name) || attr(node, `r:${name}`);
  const isTrue = value => value === '1' || value === 'true';
  const clamp = (value, low, high) => Math.min(high, Math.max(low, value));

  function checkSignal(signal) {
    if (signal?.aborted) throw new DOMException('PPTX preview cancelled', 'AbortError');
  }

  function partPath(base, target) {
    if (!target || /[\\\u0000?#]/.test(target) || /^[a-z][a-z\d+.-]*:/i.test(target))
      throw failure('PPTX relationship is not a local package part');
    let decoded;
    try { decoded = decodeURIComponent(target); } catch { throw failure('Invalid PPTX part name'); }
    if (/[\\\u0000?#]/.test(decoded) || /^[a-z][a-z\d+.-]*:/i.test(decoded))
      throw failure('PPTX relationship is not a local package part');
    const result = decoded.startsWith('/') ? [] : base.split('/').slice(0, -1);
    for (const segment of decoded.split('/')) {
      if (!segment || segment === '.') continue;
      if (segment === '..') {
        if (!result.length) throw failure('PPTX relationship leaves the package');
        result.pop();
      } else result.push(segment);
    }
    return result.join('/');
  }

  function relationshipsPath(path) {
    const slash = path.lastIndexOf('/');
    return `${path.slice(0, slash + 1)}_rels/${path.slice(slash + 1)}.rels`;
  }

  async function openPackage(bytes, signal) {
    if (!window.JSZip) throw failure('PPTX ZIP reader unavailable');
    if ((bytes.byteLength ?? bytes.size ?? 0) > 256 * 1024 * 1024) throw unsupported('PPTX exceeds direct preview size');
    checkSignal(signal);
    const zip = await window.JSZip.loadAsync(bytes);
    checkSignal(signal);
    let expanded = 0, entries = 0, xmlBytes = 0, closed = false;
    const xml = new Map(), relations = new Map(), imageUrls = new Map(), urls = new Set();
    for (const [name, entry] of Object.entries(zip.files)) {
      if (++entries > 8192) throw unsupported('PPTX has too many package parts');
      for (const path of [name, entry.unsafeOriginalName || name]) {
        if (path.startsWith('/') || path.includes('\\') || path.split('/').includes('..') || path.includes('\0'))
          throw failure('Unsafe PPTX package path');
      }
      const size = entry._data?.uncompressedSize || 0;
      expanded += size;
      if (expanded > MAX_EXPANDED_BYTES || (/\/media\//i.test(name) && size > MAX_MEDIA_BYTES))
        throw unsupported('PPTX expanded content exceeds direct preview capacity');
    }

    async function readXml(path, optional = false) {
      if (xml.has(path)) return xml.get(path);
      const promise = (async () => {
        checkSignal(signal);
        const entry = zip.file(path);
        if (!entry) {
          if (optional) return null;
          throw failure(`PPTX part is missing: ${path}`);
        }
        if ((entry._data?.uncompressedSize || 0) > MAX_XML_BYTES) throw unsupported('PPTX XML part is too large');
        const source = await entry.async('string');
        checkSignal(signal);
        if (closed) throw new DOMException('PPTX preview disposed', 'AbortError');
        xmlBytes += source.length * 2;
        if (xmlBytes > MAX_EXPANDED_BYTES * 2 || /<!DOCTYPE|<!ENTITY/i.test(source))
          throw failure('PPTX XML cannot contain document type declarations or oversized content');
        const document = new DOMParser().parseFromString(source, 'application/xml');
        if (document.getElementsByTagName('parsererror').length) throw failure(`Invalid PPTX XML: ${path}`);
        return document.documentElement;
      })();
      xml.set(path, promise); return promise;
    }

    async function readRelationships(path) {
      if (relations.has(path)) return relations.get(path);
      const promise = (async () => {
        const root = await readXml(relationshipsPath(path), true);
        const result = new Map();
        for (const item of children(root, 'Relationship')) {
          const external = attr(item, 'TargetMode').toLowerCase() === 'external';
          result.set(attr(item, 'Id'), { type: attr(item, 'Type'), external,
            path: external ? null : partPath(path, attr(item, 'Target')) });
        }
        return result;
      })();
      relations.set(path, promise); return promise;
    }

    async function image(path) {
      if (imageUrls.has(path)) return imageUrls.get(path);
      const promise = (async () => {
        checkSignal(signal);
        const extension = path.split('.').pop().toLowerCase();
        const mime = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif',
          webp: 'image/webp', bmp: 'image/bmp', tif: 'image/tiff', tiff: 'image/tiff' }[extension];
        if (!mime || extension === 'tif' || extension === 'tiff') throw unsupported(`PPTX image format requires PDF preview: ${extension}`);
        const entry = zip.file(path);
        if (!entry || (entry._data?.uncompressedSize || 0) > MAX_MEDIA_BYTES) throw unsupported('PPTX image unavailable or oversized');
        const data = await entry.async('uint8array');
        checkSignal(signal);
        if (closed) throw new DOMException('PPTX preview disposed', 'AbortError');
        if (data.byteLength > MAX_MEDIA_BYTES) throw unsupported('PPTX image exceeds direct preview capacity');
        const url = URL.createObjectURL(new Blob([data], { type: mime })); urls.add(url); return url;
      })();
      imageUrls.set(path, promise); return promise;
    }

    return { readXml, readRelationships, image, dispose() {
      closed = true;
      for (const url of urls) URL.revokeObjectURL(url);
      urls.clear(); imageUrls.clear(); xml.clear(); relations.clear();
    } };
  }

  function transformColor(hex, node) {
    if (!/^#[\da-f]{6}$/i.test(hex)) return hex;
    let channels = [1, 3, 5].map(offset => parseInt(hex.slice(offset, offset + 2), 16) / 255), alpha = 1;
    for (const modifier of children(node)) {
      const ratio = number(modifier, 'val', 100000) / 100000;
      if (modifier.localName === 'tint') channels = channels.map(value => value + (1 - value) * ratio);
      else if (modifier.localName === 'shade' || modifier.localName === 'lumMod') channels = channels.map(value => value * ratio);
      else if (modifier.localName === 'lumOff') channels = channels.map(value => value + ratio);
      else if (modifier.localName === 'alpha') alpha = ratio;
    }
    const rgb = channels.map(value => Math.round(clamp(value, 0, 1) * 255));
    return `rgba(${rgb.join(',')},${clamp(alpha, 0, 1)})`;
  }

  function color(node, theme, fallback = '#233b53') {
    if (!node) return fallback;
    const definition = ['srgbClr', 'schemeClr', 'sysClr', 'prstClr'].includes(node.localName)
      ? node : children(node).find(item => ['srgbClr', 'schemeClr', 'sysClr', 'prstClr'].includes(item.localName));
    if (!definition) return fallback;
    let value = attr(definition, 'val');
    if (definition.localName === 'srgbClr') value = /^[\da-f]{6}$/i.test(value) ? `#${value}` : fallback;
    else if (definition.localName === 'sysClr') value = `#${attr(definition, 'lastClr', '000000')}`;
    else if (definition.localName === 'schemeClr') value = value === 'phClr' ? fallback : theme.colors[theme.map[value] || value] || fallback;
    else value = ({ black: '#000000', white: '#ffffff', red: '#ff0000', green: '#008000', blue: '#0000ff',
      yellow: '#ffff00', gray: '#808080', ltGray: '#d3d3d3', dkGray: '#a9a9a9', cyan: '#00ffff', magenta: '#ff00ff' })[value] || fallback;
    return transformColor(value, definition);
  }

  function themeData(root, mapRoot) {
    const theme = { colors: { ...defaultColors }, map: { ...defaultMap },
      major: { latin: 'Aptos Display', ea: 'Microsoft YaHei' }, minor: { latin: 'Aptos', ea: 'Microsoft YaHei' },
      fills: [], backgroundFills: [] };
    const elements = child(root, 'themeElements');
    for (const definition of children(child(elements, 'clrScheme'))) {
      theme.colors[definition.localName] = color(definition, theme, theme.colors[definition.localName]);
    }
    for (const family of ['major', 'minor']) {
      const fonts = child(child(elements, 'fontScheme'), `${family}Font`);
      theme[family] = { latin: attr(child(fonts, 'latin'), 'typeface', theme[family].latin),
        ea: attr(child(fonts, 'ea'), 'typeface') || attr(children(fonts, 'font').find(item => attr(item, 'script') === 'Hans'), 'typeface', theme[family].ea) };
    }
    const mapping = child(mapRoot, 'clrMap');
    if (mapping) for (const item of mapping.attributes) theme.map[item.localName] = item.value;
    const format = child(elements, 'fmtScheme');
    theme.fills = children(child(format, 'fillStyleLst'));
    theme.backgroundFills = children(child(format, 'bgFillStyleLst'));
    return theme;
  }

  function fill(properties, theme, fallback = 'transparent') {
    if (child(properties, 'noFill')) return 'transparent';
    const solid = child(properties, 'solidFill');
    if (solid) return color(solid, theme, fallback);
    const gradient = child(properties, 'gradFill');
    if (gradient) {
      const stops = children(child(gradient, 'gsLst'), 'gs').map(stop =>
        `${color(stop, theme, fallback)} ${clamp(number(stop, 'pos') / 1000, 0, 100)}%`);
      if (stops.length) return `linear-gradient(${number(child(gradient, 'lin'), 'ang') / 60000 + 90}deg,${stops.join(',')})`;
    }
    return fallback;
  }

  function fontFamily(name, theme) {
    const aliases = { '+mj-lt': theme.major.latin, '+mj-ea': theme.major.ea,
      '+mn-lt': theme.minor.latin, '+mn-ea': theme.minor.ea };
    const face = aliases[name] || name || theme.minor.ea || 'Microsoft YaHei';
    return `${JSON.stringify(face)},"Microsoft YaHei","Segoe UI",sans-serif`;
  }

  function placeholder(shape) {
    const ph = descendants(shape, 'ph')[0];
    return ph ? { type: attr(ph, 'type', 'obj'), index: attr(ph, 'idx', '0') } : null;
  }

  function inheritedShape(shape, root) {
    const identity = placeholder(shape);
    if (!identity) return null;
    const shapes = children(child(child(root, 'cSld'), 'spTree'));
    return shapes.find(candidate => {
      const other = placeholder(candidate);
      return other && other.index === identity.index && other.type === identity.type;
    }) || shapes.find(candidate => placeholder(candidate)?.type === identity.type) || null;
  }

  function geometry(shape, inherited) {
    const transform = child(child(shape, shape.localName === 'graphicFrame' ? 'xfrm' : 'spPr'), 'xfrm')
      || (shape.localName === 'graphicFrame' ? child(shape, 'xfrm') : null)
      || child(child(inherited, 'spPr'), 'xfrm');
    if (!transform) return null;
    const offset = child(transform, 'off'), extent = child(transform, 'ext');
    return { x: number(offset, 'x') / EMU, y: number(offset, 'y') / EMU,
      width: Math.max(0, number(extent, 'cx')) / EMU, height: Math.max(0, number(extent, 'cy')) / EMU,
      rotation: number(transform, 'rot') / 60000, flipX: isTrue(attr(transform, 'flipH')),
      flipY: isTrue(attr(transform, 'flipV')), transform };
  }

  function position(element, bounds) {
    Object.assign(element.style, { position: 'absolute', left: `${bounds.x}px`, top: `${bounds.y}px`,
      width: `${bounds.width}px`, height: `${bounds.height}px`, transformOrigin: 'center center',
      transform: `rotate(${bounds.rotation}deg) scale(${bounds.flipX ? -1 : 1},${bounds.flipY ? -1 : 1})` });
  }

  function assertSupported(root) {
    for (const name of ['oleObj', 'control', 'relIds', 'custGeom']) {
      if (descendants(root, name).length) throw unsupported(`PPTX ${name} requires Office/PDF preview`);
    }
    for (const graphic of descendants(root, 'graphicData')) {
      if (!attr(graphic, 'uri').endsWith('/table')) throw unsupported('PPTX chart or SmartArt requires Office/PDF preview');
    }
    for (const transform of descendants(root, 'bodyPr')) {
      if (!['', 'horz'].includes(attr(transform, 'vert'))) throw unsupported('PPTX vertical text requires Office/PDF preview');
    }
    for (const warp of descendants(root, 'prstTxWarp')) {
      if (attr(warp, 'prst', 'textNoShape') !== 'textNoShape') throw unsupported('PPTX warped text requires Office/PDF preview');
    }
  }

  async function loadDeck(packageReader, signal) {
    const rootRelationships = await packageReader.readRelationships('');
    const main = [...rootRelationships.values()].find(item => item.type.endsWith('/officeDocument'))?.path || 'ppt/presentation.xml';
    const presentation = await packageReader.readXml(main);
    const relationships = await packageReader.readRelationships(main);
    const size = child(presentation, 'sldSz');
    const width = number(size, 'cx', 12192000) / EMU, height = number(size, 'cy', 6858000) / EMU;
    if (!(width > 0 && height > 0 && width < 20000 && height < 20000)) throw unsupported('PPTX slide size is unsupported');
    const slides = [], contexts = new Map(), loadedSlides = new Map();
    async function context(path) {
      if (contexts.has(path)) return contexts.get(path);
      const promise = (async () => {
        const [xml, rels] = await Promise.all([packageReader.readXml(path), packageReader.readRelationships(path)]);
        assertSupported(xml);
        return { xml, rels, path };
      })();
      contexts.set(path, promise); return promise;
    }
    for (const identity of children(child(presentation, 'sldIdLst'), 'sldId')) {
      checkSignal(signal);
      if (slides.length >= 1000) throw unsupported('PPTX has too many slides');
      const relation = relationships.get(relId(identity));
      if (!relation?.path) throw failure('PPTX slide relationship missing');
      slides.push({ path: relation.path });
    }
    if (!slides.length) throw failure('PPTX contains no slides');
    // Read only the requested slide before first paint; shared layouts and later slides keep their own promises.
    // 首屏前只解析当前页；共用布局和后续页缓存各自 Promise，翻页及后台准备不会重复解包。
    function loadSlide(index) {
      if (loadedSlides.has(index)) return loadedSlides.get(index);
      const pending = (async () => {
        checkSignal(signal);
        const slide = await context(slides[index].path);
        const layoutPath = [...slide.rels.values()].find(item => item.type.endsWith('/slideLayout'))?.path;
        const layout = layoutPath ? await context(layoutPath) : null;
        const masterPath = layout && [...layout.rels.values()].find(item => item.type.endsWith('/slideMaster'))?.path;
        const master = masterPath ? await context(masterPath) : null;
        const themePath = master && [...master.rels.values()].find(item => item.type.endsWith('/theme'))?.path;
        const theme = themeData(themePath ? await packageReader.readXml(themePath) : null, master?.xml);
        const override = child(child(slide.xml, 'clrMapOvr'), 'overrideClrMapping') || child(child(layout?.xml, 'clrMapOvr'), 'overrideClrMapping');
        if (override) for (const item of override.attributes) theme.map[item.localName] = item.value;
        return { ...slide, layout, master, theme };
      })();
      loadedSlides.set(index, pending); return pending;
    }
    return { width, height, slides, loadSlide };
  }

  function textProperties(run, defaults, theme) {
    const result = {};
    for (const properties of [...defaults, run].filter(Boolean)) {
      if (properties.hasAttribute('sz')) result.size = number(properties, 'sz', 1800) / 100 * 96 / 72;
      if (properties.hasAttribute('b')) result.bold = isTrue(attr(properties, 'b'));
      if (properties.hasAttribute('i')) result.italic = isTrue(attr(properties, 'i'));
      if (properties.hasAttribute('u')) result.underline = !['none', ''].includes(attr(properties, 'u'));
      if (properties.hasAttribute('strike')) result.strike = attr(properties, 'strike') !== 'noStrike';
      if (child(properties, 'solidFill')) result.color = color(child(properties, 'solidFill'), theme);
      if (child(properties, 'latin')) result.family = fontFamily(attr(child(properties, 'latin'), 'typeface'), theme);
      if (child(properties, 'ea') && attr(child(properties, 'ea'), 'typeface')) result.family = fontFamily(attr(child(properties, 'ea'), 'typeface'), theme);
      if (properties.hasAttribute('baseline')) result.baseline = number(properties, 'baseline') / 1000;
      if (properties.hasAttribute('spc')) result.spacing = number(properties, 'spc') / 100 * 96 / 72;
    }
    return result;
  }

  function applyTextProperties(element, properties) {
    if (properties.size) { element.style.fontSize = `${properties.size}px`; element.dataset.baseSize = properties.size; }
    if (properties.bold !== undefined) element.style.fontWeight = properties.bold ? '700' : '400';
    if (properties.italic !== undefined) element.style.fontStyle = properties.italic ? 'italic' : 'normal';
    const decorations = [];
    if (properties.underline) decorations.push('underline');
    if (properties.strike) decorations.push('line-through');
    if (decorations.length) element.style.textDecoration = decorations.join(' ');
    if (properties.color) element.style.color = properties.color;
    if (properties.family) element.style.fontFamily = properties.family;
    if (properties.baseline) element.style.verticalAlign = `${properties.baseline}%`;
    if (properties.spacing) element.style.letterSpacing = `${properties.spacing}px`;
  }

  function paragraphSpacing(parent, name) {
    const spacing = child(parent, name);
    if (child(spacing, 'spcPts')) return `${number(child(spacing, 'spcPts'), 'val') / 100 * 96 / 72}px`;
    if (child(spacing, 'spcPct')) return number(child(spacing, 'spcPct'), 'val') / 100000;
    return null;
  }

  function renderText(document, body, shape, inherited, slide, bounds) {
    if (!body) return null;
    const host = document.createElement('div'); host.className = 'pptx-textbox';
    const properties = child(body, 'bodyPr');
    const autofit = child(properties, 'normAutofit');
    const anchor = attr(properties, 'anchor', 't');
    Object.assign(host.style, { position: 'absolute', inset: '0', boxSizing: 'border-box', display: 'flex',
      flexDirection: 'column', justifyContent: ({ ctr: 'center', b: 'flex-end' })[anchor] || 'flex-start',
      padding: `${number(properties, 'tIns', 45720) / EMU}px ${number(properties, 'rIns', 91440) / EMU}px ${number(properties, 'bIns', 45720) / EMU}px ${number(properties, 'lIns', 91440) / EMU}px`,
      overflow: 'hidden', whiteSpace: attr(properties, 'wrap') === 'none' ? 'pre' : 'pre-wrap',
      fontFamily: fontFamily('', slide.theme), color: color(child(child(shape, 'style'), 'fontRef'), slide.theme) });
    const text = document.createElement('div'); text.className = 'pptx-text'; text.style.minHeight = '0'; host.append(text);
    if (autofit) { host.dataset.autofit = 'true'; host.dataset.fontScale = String(number(autofit, 'fontScale', 100000) / 100000); }
    const identity = placeholder(shape);
    const masterStyle = child(child(slide.master?.xml, 'txStyles'), identity?.type?.includes('Title') || identity?.type === 'title' ? 'titleStyle' : identity ? 'bodyStyle' : 'otherStyle');
    const inheritedBody = child(inherited, 'txBody');
    const counters = new Map();
    for (const paragraph of children(body, 'p')) {
      const paragraphProperties = child(paragraph, 'pPr');
      const level = clamp(number(paragraphProperties, 'lvl'), 0, 8);
      const levelName = `lvl${level + 1}pPr`;
      const defaults = [child(masterStyle, levelName), child(child(inheritedBody, 'lstStyle'), levelName),
        child(child(body, 'lstStyle'), levelName), paragraphProperties].filter(Boolean);
      const merged = name => [...defaults].reverse().find(item => item.hasAttribute(name));
      const paragraphNode = document.createElement('p'); paragraphNode.className = 'pptx-paragraph';
      paragraphNode.dataset.baseSize = '24';
      Object.assign(paragraphNode.style, { margin: '0', padding: '0', position: 'relative', fontSize: '24px',
        lineHeight: '1.15', minHeight: '1em', textAlign: ({ ctr: 'center', r: 'right', just: 'justify', dist: 'justify' })[attr(merged('algn'), 'algn')] || 'left',
        marginLeft: `${number(merged('marL'), 'marL') / EMU}px`, marginRight: `${number(merged('marR'), 'marR') / EMU}px`,
        textIndent: `${number(merged('indent'), 'indent') / EMU}px` });
      for (const definition of defaults) {
        const line = paragraphSpacing(definition, 'lnSpc'); if (line !== null) paragraphNode.style.lineHeight = String(line);
        const before = paragraphSpacing(definition, 'spcBef'); if (before !== null) paragraphNode.style.marginTop = typeof before === 'number' ? `${before}em` : before;
        const after = paragraphSpacing(definition, 'spcAft'); if (after !== null) paragraphNode.style.marginBottom = typeof after === 'number' ? `${after}em` : after;
      }
      const runDefaults = defaults.map(item => child(item, 'defRPr')).filter(Boolean);
      applyTextProperties(paragraphNode, textProperties(null, runDefaults, slide.theme));
      const bulletDefinition = [...defaults].reverse().find(item => children(item).some(node => ['buNone', 'buChar', 'buAutoNum'].includes(node.localName)));
      const bulletChar = child(bulletDefinition, 'buChar'), autoNumber = child(bulletDefinition, 'buAutoNum');
      if (bulletChar || autoNumber) {
        const bullet = document.createElement('span'); bullet.className = 'pptx-bullet';
        Object.assign(bullet.style, { display: 'inline-block', minWidth: '1.2em', textIndent: '0', paddingRight: '.25em' });
        if (bulletChar) bullet.textContent = attr(bulletChar, 'char', '•');
        else {
          const current = counters.get(level) ?? number(autoNumber, 'startAt', 1); counters.set(level, current + 1);
          const type = attr(autoNumber, 'type');
          const label = type.startsWith('alpha') ? String.fromCharCode((type.startsWith('alphaUc') ? 65 : 97) + (current - 1) % 26) : String(current);
          bullet.textContent = `${label}${type.includes('ParenR') ? ')' : '.'}`;
        }
        paragraphNode.append(bullet);
      }
      for (const run of children(paragraph)) {
        if (run.localName === 'br') { paragraphNode.append(document.createElement('br')); continue; }
        if (!['r', 'fld'].includes(run.localName)) continue;
        const span = document.createElement('span');
        span.textContent = child(run, 't')?.textContent || '';
        applyTextProperties(span, textProperties(child(run, 'rPr'), runDefaults, slide.theme));
        paragraphNode.append(span);
      }
      if (!paragraphNode.childNodes.length) paragraphNode.append(document.createElement('br'));
      text.append(paragraphNode);
    }
    return host;
  }

  function svgElement(document, name, attributes) {
    const element = document.createElementNS(SVG, name);
    for (const [key, value] of Object.entries(attributes || {})) element.setAttribute(key, String(value));
    return element;
  }

  function renderShapePaint(document, shape, properties, theme, bounds, inherited) {
    const requestedPreset = attr(child(properties, 'prstGeom'), 'prst', shape.localName === 'cxnSp' ? 'line' : 'rect');
    const preset = ({ flowChartProcess: 'rect', flowChartDecision: 'diamond', flowChartTerminator: 'roundRect' })[requestedPreset] || requestedPreset;
    const style = child(shape, 'style'), fillReference = child(style, 'fillRef');
    const defaultFill = fillReference ? color(fillReference, theme, 'transparent') : 'transparent';
    const background = fill(properties, theme, defaultFill);
    const line = child(properties, 'ln') || child(child(inherited, 'spPr'), 'ln');
    const lineReference = child(style, 'lnRef');
    const stroke = line ? fill(line, theme, '#233b53') : lineReference ? color(lineReference, theme) : 'none';
    const strokeWidth = line ? number(line, 'w', 9525) / EMU : lineReference ? 1 : 0;
    if (preset === 'rect' || preset === 'roundRect') {
      const rectangle = document.createElement('div'); rectangle.className = 'pptx-shape-paint';
      Object.assign(rectangle.style, { position: 'absolute', inset: '0', boxSizing: 'border-box', background,
        border: stroke !== 'transparent' && stroke !== 'none' && strokeWidth > 0 ? `${strokeWidth}px ${child(line, 'prstDash') && attr(child(line, 'prstDash'), 'val') !== 'solid' ? 'dashed' : 'solid'} ${stroke}` : 'none',
        borderRadius: preset === 'roundRect' ? `${Math.min(bounds.width, bounds.height) * .12}px` : '0' });
      return rectangle;
    }
    const svg = svgElement(document, 'svg', { viewBox: `0 0 ${Math.max(bounds.width, 1)} ${Math.max(bounds.height, 1)}`,
      preserveAspectRatio: 'none', width: Math.max(bounds.width, 1), height: Math.max(bounds.height, 1), 'aria-hidden': 'true' });
    Object.assign(svg.style, { position: 'absolute', inset: '0', overflow: 'visible' });
    const w = bounds.width, h = bounds.height;
    let primitive;
    if (preset === 'ellipse') primitive = svgElement(document, 'ellipse', { cx: w / 2, cy: h / 2, rx: w / 2, ry: h / 2 });
    else if (preset === 'line' || preset === 'straightConnector1') primitive = svgElement(document, 'line', { x1: 0, y1: 0, x2: w, y2: h });
    else if (/^bentConnector[2-5]$/.test(preset)) primitive = svgElement(document, 'polyline', {
      points: preset === 'bentConnector2' ? `0,0 ${w},0 ${w},${h}` : `0,0 ${w / 2},0 ${w / 2},${h} ${w},${h}` });
    else if (/^curvedConnector[2-5]$/.test(preset)) primitive = svgElement(document, 'path', {
      d: preset === 'curvedConnector2' ? `M0 0 Q${w} 0 ${w} ${h}` : `M0 0 C${w / 2} 0 ${w / 2} ${h} ${w} ${h}` });
    else {
      const polygons = { triangle: [[.5, 0], [1, 1], [0, 1]], rtTriangle: [[0, 0], [1, 1], [0, 1]],
        diamond: [[.5, 0], [1, .5], [.5, 1], [0, .5]], parallelogram: [[.2, 0], [1, 0], [.8, 1], [0, 1]],
        trapezoid: [[.2, 0], [.8, 0], [1, 1], [0, 1]], hexagon: [[.25, 0], [.75, 0], [1, .5], [.75, 1], [.25, 1], [0, .5]],
        pentagon: [[.5, 0], [1, .38], [.81, 1], [.19, 1], [0, .38]], chevron: [[0, 0], [.7, 0], [1, .5], [.7, 1], [0, 1], [.3, .5]],
        rightArrow: [[0, .25], [.65, .25], [.65, 0], [1, .5], [.65, 1], [.65, .75], [0, .75]],
        leftArrow: [[0, .5], [.35, 0], [.35, .25], [1, .25], [1, .75], [.35, .75], [.35, 1]],
        upArrow: [[.5, 0], [1, .35], [.75, .35], [.75, 1], [.25, 1], [.25, .35], [0, .35]],
        downArrow: [[.25, 0], [.75, 0], [.75, .65], [1, .65], [.5, 1], [0, .65], [.25, .65]] };
      const points = polygons[preset];
      if (!points) throw unsupported(`PPTX shape ${preset} requires Office/PDF preview`);
      primitive = svgElement(document, 'polygon', { points: points.map(([x, y]) => `${x * w},${y * h}`).join(' ') });
    }
    if (background.startsWith('linear-gradient')) throw unsupported('Gradient vector shape requires Office/PDF preview');
    primitive.setAttribute('fill', preset === 'line' || /Connector/.test(preset) ? 'none' : background);
    primitive.setAttribute('stroke', stroke); primitive.setAttribute('stroke-width', String(strokeWidth));
    if (child(line, 'prstDash') && attr(child(line, 'prstDash'), 'val') !== 'solid') primitive.setAttribute('stroke-dasharray', `${strokeWidth * 4} ${strokeWidth * 3}`);
    svg.append(primitive); return svg;
  }

  async function renderPicture(document, shape, properties, context, packageReader, bounds) {
    const fillRoot = child(shape, 'blipFill') || child(properties, 'blipFill');
    const blip = child(fillRoot, 'blip'), relation = context.rels.get(relId(blip, 'embed'));
    if (!relation?.path || relation.external) throw unsupported('Linked PPTX image requires Office/PDF preview');
    const image = document.createElement('img'); image.alt = attr(descendants(shape, 'cNvPr')[0], 'descr');
    image.draggable = false; image.src = await packageReader.image(relation.path);
    try { await image.decode(); } catch { throw unsupported('PPTX image cannot be decoded directly'); }
    const crop = child(fillRoot, 'srcRect');
    const left = clamp(number(crop, 'l') / 100000, 0, .99), right = clamp(number(crop, 'r') / 100000, 0, .99);
    const top = clamp(number(crop, 't') / 100000, 0, .99), bottom = clamp(number(crop, 'b') / 100000, 0, .99);
    const visibleWidth = Math.max(.01, 1 - left - right), visibleHeight = Math.max(.01, 1 - top - bottom);
    Object.assign(image.style, { position: 'absolute', width: `${100 / visibleWidth}%`, height: `${100 / visibleHeight}%`,
      left: `${-left / visibleWidth * 100}%`, top: `${-top / visibleHeight * 100}%`, maxWidth: 'none' });
    const wrapper = document.createElement('div'); wrapper.className = 'pptx-picture';
    Object.assign(wrapper.style, { position: 'absolute', inset: '0', overflow: 'hidden' }); wrapper.append(image);
    const radius = attr(child(properties, 'prstGeom'), 'prst');
    if (radius === 'ellipse') wrapper.style.borderRadius = '50%';
    else if (radius === 'roundRect') wrapper.style.borderRadius = `${Math.min(bounds.width, bounds.height) * .12}px`;
    return wrapper;
  }

  function renderTable(document, graphic, slide, bounds) {
    const tableRoot = descendants(graphic, 'tbl')[0];
    if (!tableRoot) throw unsupported('PPTX graphic is not a supported table');
    const columns = children(child(tableRoot, 'tblGrid'), 'gridCol');
    const rows = children(tableRoot, 'tr');
    if (!columns.length || !rows.length) throw failure('PPTX table has no rows or columns');
    const table = document.createElement('table'); table.className = 'pptx-table';
    Object.assign(table.style, { width: '100%', height: '100%', tableLayout: 'fixed', borderCollapse: 'collapse',
      borderSpacing: '0', position: 'absolute', inset: '0', color: color(null, slide.theme) });
    const group = document.createElement('colgroup');
    const totalWidth = columns.reduce((sum, item) => sum + number(item, 'w'), 0) || 1;
    for (const column of columns) { const col = document.createElement('col'); col.style.width = `${number(column, 'w') / totalWidth * 100}%`; group.append(col); }
    table.append(group);
    const body = document.createElement('tbody'); table.append(body);
    const firstRow = isTrue(attr(child(tableRoot, 'tblPr'), 'firstRow'));
    const bandRows = isTrue(attr(child(tableRoot, 'tblPr'), 'bandRow'));
    const totalHeight = rows.reduce((sum, row) => sum + number(row, 'h'), 0) || 1;
    for (const [index, row] of rows.entries()) {
      const tr = document.createElement('tr'); tr.style.height = `${number(row, 'h') / totalHeight * 100}%`;
      for (const cell of children(row, 'tc')) {
        if (isTrue(attr(cell, 'hMerge')) || isTrue(attr(cell, 'vMerge'))) continue;
        const td = document.createElement(index === 0 && firstRow ? 'th' : 'td');
        td.colSpan = clamp(number(cell, 'gridSpan', 1), 1, columns.length);
        td.rowSpan = clamp(number(cell, 'rowSpan', 1), 1, rows.length);
        const properties = child(cell, 'tcPr');
        const defaultFill = index === 0 && firstRow ? slide.theme.colors.accent1 : bandRows && index % 2 ? '#edf2f7' : 'transparent';
        Object.assign(td.style, { position: 'relative', padding: '0', verticalAlign: 'top', border: '1px solid #d6dde5',
          background: fill(properties, slide.theme, defaultFill), overflow: 'hidden', fontWeight: '400' });
        const cellBody = child(cell, 'txBody');
        const content = renderText(document, cellBody, cell, null, slide, bounds);
        if (content) {
          Object.assign(content.style, { position: 'relative', height: '100%', padding:
            `${number(properties, 'marT', 45720) / EMU}px ${number(properties, 'marR', 91440) / EMU}px ${number(properties, 'marB', 45720) / EMU}px ${number(properties, 'marL', 91440) / EMU}px` });
          if (index === 0 && firstRow && !descendants(cellBody, 'solidFill').length) content.style.color = '#ffffff';
          td.append(content);
        }
        tr.append(td);
      }
      body.append(tr);
    }
    return table;
  }

  async function renderTree(document, tree, parent, slide, context, packageReader, staticOnly = false) {
    for (const shape of children(tree)) {
      if (!['sp', 'pic', 'cxnSp', 'graphicFrame', 'grpSp'].includes(shape.localName)) continue;
      if (isTrue(attr(descendants(shape, 'cNvPr')[0], 'hidden'))) continue;
      if (staticOnly && placeholder(shape)) continue;
      if (shape.localName === 'grpSp') {
        const transform = child(child(shape, 'grpSpPr'), 'xfrm');
        const off = child(transform, 'off'), ext = child(transform, 'ext');
        const chOff = child(transform, 'chOff'), chExt = child(transform, 'chExt');
        if (!transform || !number(chExt, 'cx') || !number(chExt, 'cy')) continue;
        const bounds = { x: number(off, 'x') / EMU, y: number(off, 'y') / EMU,
          width: number(ext, 'cx') / EMU, height: number(ext, 'cy') / EMU,
          rotation: number(transform, 'rot') / 60000, flipX: isTrue(attr(transform, 'flipH')), flipY: isTrue(attr(transform, 'flipV')) };
        const group = document.createElement('div'); group.className = 'pptx-group'; position(group, bounds);
        const stage = document.createElement('div');
        const sx = number(ext, 'cx') / number(chExt, 'cx'), sy = number(ext, 'cy') / number(chExt, 'cy');
        Object.assign(stage.style, { position: 'absolute', left: `${-number(chOff, 'x') / EMU * sx}px`, top: `${-number(chOff, 'y') / EMU * sy}px`,
          transform: `scale(${sx},${sy})`, transformOrigin: '0 0' });
        group.append(stage); parent.append(group);
        await renderTree(document, shape, stage, slide, context, packageReader, staticOnly); continue;
      }
      const inherited = inheritedShape(shape, slide.layout?.xml) || inheritedShape(shape, slide.master?.xml);
      const bounds = geometry(shape, inherited);
      if (!bounds) continue;
      const host = document.createElement('div'); host.className = 'pptx-object'; position(host, bounds);
      host.setAttribute('aria-label', attr(descendants(shape, 'cNvPr')[0], 'name'));
      const properties = child(shape, 'spPr') || child(inherited, 'spPr');
      if (shape.localName === 'pic') host.append(await renderPicture(document, shape, properties, context, packageReader, bounds));
      else if (shape.localName === 'graphicFrame') host.append(renderTable(document, shape, slide, bounds));
      else {
        host.append(renderShapePaint(document, shape, properties, slide.theme, bounds, inherited));
        if (child(properties, 'blipFill')) host.append(await renderPicture(document, shape, properties, context, packageReader, bounds));
        const text = renderText(document, child(shape, 'txBody'), shape, inherited, slide, bounds);
        if (text) host.append(text);
      }
      parent.append(host);
    }
  }

  function slideBackground(slide) {
    for (const context of [slide, slide.layout, slide.master].filter(Boolean)) {
      const background = child(child(context.xml, 'cSld'), 'bg');
      if (!background) continue;
      if (child(background, 'bgPr')) return fill(child(background, 'bgPr'), slide.theme, '#ffffff');
      const reference = child(background, 'bgRef');
      if (reference) {
        const index = number(reference, 'idx');
        const source = index >= 1001 ? slide.theme.backgroundFills[index - 1001] : slide.theme.fills[index - 1];
        if (source?.localName === 'solidFill') return color(source, slide.theme, color(reference, slide.theme, '#ffffff'));
        return color(reference, slide.theme, '#ffffff');
      }
    }
    return '#ffffff';
  }

  async function renderBackgroundImage(document, element, slide, packageReader, bounds) {
    for (const context of [slide, slide.layout, slide.master].filter(Boolean)) {
      const background = child(child(context.xml, 'cSld'), 'bg');
      if (!background) continue;
      const properties = child(background, 'bgPr');
      if (child(properties, 'blipFill')) element.append(await renderPicture(document, properties, properties, context, packageReader, bounds));
      const reference = child(background, 'bgRef'), index = number(reference, 'idx');
      const source = index >= 1001 ? slide.theme.backgroundFills[index - 1001] : slide.theme.fills[index - 1];
      if (source?.localName === 'blipFill') throw unsupported('Theme background image requires Office/PDF preview');
      break;
    }
  }

  function fitText(root) {
    for (const host of root.querySelectorAll('.pptx-textbox[data-autofit]')) {
      const text = host.firstElementChild;
      const styles = host.ownerDocument.defaultView.getComputedStyle(host);
      const availableHeight = host.clientHeight - parseFloat(styles.paddingTop) - parseFloat(styles.paddingBottom);
      const availableWidth = host.clientWidth - parseFloat(styles.paddingLeft) - parseFloat(styles.paddingRight);
      let scale = Number(host.dataset.fontScale) || 1;
      for (let attempt = 0; attempt < 3; attempt += 1) {
        for (const element of [text, ...text.querySelectorAll('[data-base-size]')]) {
          if (element.dataset.baseSize) element.style.fontSize = `${Number(element.dataset.baseSize) * scale}px`;
        }
        const heightRatio = availableHeight > 0 ? availableHeight / Math.max(text.scrollHeight, 1) : 1;
        const widthRatio = availableWidth > 0 ? availableWidth / Math.max(text.scrollWidth, 1) : 1;
        const ratio = Math.min(heightRatio, widthRatio, 1);
        if (ratio > .98) break;
        scale *= Math.max(.1, ratio * .98);
      }
    }
  }

  async function render(bytes, options = {}) {
    const { container, title = 'PPTX', onState, signal } = options;
    if (!container || typeof container.append !== 'function') throw failure('PPTX container is required');
    const packageReader = await openPackage(bytes, signal);
    let disposed = false, frame = null, observer = null, root = null;
    let page = 0, requestedPage = 0, zoom = 'fit', currentShow = 0;
    const pending = new Map(), cache = new Map();
    let deck, stage, pageInput, countLabel, previous, next, zoomSelect;
    const initial = typeof options.state === 'string' ? (() => { try { return JSON.parse(options.state); } catch { return {}; } })() : options.state || {};
    const state = () => ({ page, zoom });
    const emit = () => { if (!disposed && typeof onState === 'function') onState(state()); };
    const alive = () => { checkSignal(signal); if (disposed) throw new DOMException('PPTX preview disposed', 'AbortError'); };
    const dispose = () => {
      if (disposed) return;
      disposed = true; currentShow += 1; observer?.disconnect();
      signal?.removeEventListener('abort', dispose); packageReader.dispose(); cache.clear(); pending.clear(); root?.remove();
    };
    signal?.addEventListener('abort', dispose, { once: true });

    function scaleSlide() {
      if (!frame?.contentDocument || !deck || disposed) return;
      const document = frame.contentDocument;
      const viewWidth = Math.max(1, frame.clientWidth - 24), viewHeight = Math.max(1, frame.clientHeight - 24);
      const scale = zoom === 'fit' ? Math.min(viewWidth / deck.width, viewHeight / deck.height) : zoom;
      const sheet = cache.get(page);
      stage.style.width = `${Math.max(viewWidth, deck.width * scale) + 24}px`;
      stage.style.height = `${Math.max(viewHeight, deck.height * scale) + 24}px`;
      for (const slide of cache.values()) {
        slide.style.transform = `scale(${scale})`;
        slide.style.left = `${Math.max(12, (viewWidth - deck.width * scale) / 2 + 12)}px`;
        slide.style.top = `${Math.max(12, (viewHeight - deck.height * scale) / 2 + 12)}px`;
      }
      if (sheet) fitText(sheet);
      document.body.style.minHeight = '100%';
    }

    async function buildSlide(index) {
      if (cache.has(index)) return cache.get(index);
      if (pending.has(index)) return pending.get(index);
      const promise = (async () => {
        alive();
        const slide = await deck.loadSlide(index); alive();
        const document = frame.contentDocument;
        const element = document.createElement('section'); element.className = 'pptx-slide';
        element.dataset.page = String(index); element.setAttribute('aria-label', `第 ${index + 1} 页`);
        Object.assign(element.style, { width: `${deck.width}px`, height: `${deck.height}px`, background: slideBackground(slide),
          position: 'absolute', transformOrigin: '0 0', overflow: 'hidden', display: 'none', boxShadow: '0 2px 12px #0002' });
        await renderBackgroundImage(document, element, slide, packageReader, { width: deck.width, height: deck.height });
        const tree = context => child(child(context?.xml, 'cSld'), 'spTree');
        if (attr(slide.xml, 'showMasterSp') !== '0' && attr(slide.layout?.xml, 'showMasterSp') !== '0')
          if (slide.master) await renderTree(document, tree(slide.master), element, slide, slide.master, packageReader, true);
        if (slide.layout) await renderTree(document, tree(slide.layout), element, slide, slide.layout, packageReader, true);
        await renderTree(document, tree(slide), element, slide, slide, packageReader);
        alive();
        stage.append(element); cache.set(index, element); return element;
      })();
      pending.set(index, promise);
      try { return await promise; } finally { pending.delete(index); }
    }

    async function show(index) {
      alive(); const revision = ++currentShow;
      const target = clamp(Math.trunc(index), 0, deck.slides.length - 1);
      requestedPage = target;
      const element = await buildSlide(target); alive();
      if (revision !== currentShow) return;
      for (const sheet of cache.values()) sheet.style.display = 'none';
      element.style.display = 'block'; page = target;
      pageInput.value = String(page + 1); countLabel.textContent = `/ ${deck.slides.length}`;
      previous.disabled = page === 0; next.disabled = page === deck.slides.length - 1;
      scaleSlide(); emit();
    }

    function report(error) {
      if (error?.name === 'AbortError' || disposed) return;
      const event = new CustomEvent('pptx-error', { detail: error, bubbles: true }); root.dispatchEvent(event);
      // Page failures stay visible to the trusted host, which can switch this file to PDF.
      // 后续页失败通知可信宿主，仅将当前文件切换为 PDF，不伪装为成功页面。
      if (typeof options.onError === 'function') options.onError(error);
    }

    function navigate(index) { show(index).catch(report); }
    function backgroundTurn() {
      return new Promise(resolve => {
        if (typeof window.requestIdleCallback === 'function') window.requestIdleCallback(resolve, { timeout: 200 });
        else setTimeout(resolve, 32);
      });
    }
    async function prepareRemaining() {
      options.onBackground?.(true);
      try {
        const remaining = new Set(deck.slides.map((_, index) => index)); remaining.delete(page);
        // Parse a single upcoming page per idle turn. User navigation uses the same cached promises immediately.
        // 每个空闲片段只解析一张后续页；用户翻页直接复用同一 Promise，不等待整份演示稿准备。
        while (remaining.size) {
          await backgroundTurn(); alive();
          if (pending.size) continue;
          const index = [...remaining].reduce((best, candidate) =>
            Math.abs(candidate - requestedPage) < Math.abs(best - requestedPage) ? candidate : best);
          remaining.delete(index);
          await deck.loadSlide(index); alive();
        }
        await frame.contentDocument.fonts.ready; alive(); scaleSlide();
      } catch (error) { report(error); }
      finally { if (!disposed) options.onBackground?.(false); }
    }
    function keyboard(event) {
      if (event.defaultPrevented || event.ctrlKey || event.altKey || event.metaKey
        || ['INPUT', 'TEXTAREA', 'SELECT'].includes(event.target?.tagName)) return;
      const keys = { ArrowLeft: requestedPage - 1, PageUp: requestedPage - 1, ArrowRight: requestedPage + 1, PageDown: requestedPage + 1,
        Home: 0, End: deck.slides.length - 1 };
      if (Object.hasOwn(keys, event.key)) { event.preventDefault(); navigate(keys[event.key]); }
    }

    try {
      deck = await loadDeck(packageReader, signal); alive();
      page = clamp(Math.trunc(Number(initial.page) || 0), 0, deck.slides.length - 1);
      zoom = initial.zoom === 'fit' || !Number.isFinite(Number(initial.zoom)) ? 'fit' : clamp(Number(initial.zoom), .25, 3);
      root = container.ownerDocument.createElement('div'); root.className = 'pptx-view';
      Object.assign(root.style, { display: 'flex', flexDirection: 'column', minHeight: '360px', height: 'calc(100vh - 24px)',
        maxHeight: 'calc(100vh - 24px)', overflow: 'hidden', background: '#f3f2f0', borderRadius: '8px', outlineColor: '#8c806e' });
      root.tabIndex = 0; root.addEventListener('keydown', keyboard);
      const toolbar = container.ownerDocument.createElement('div'); toolbar.className = 'pptx-toolbar';
      toolbar.setAttribute('aria-label', '幻灯片翻页与缩放');
      Object.assign(toolbar.style, { display: 'flex', alignItems: 'center', flexWrap: 'wrap', gap: '8px', padding: '7px 10px',
        flex: '0 0 auto', color: '#3f3f3f', font: '13px "Segoe UI","Microsoft YaHei",sans-serif', borderBottom: '1px solid #ddd9' });
      const button = (action, label, glyph) => {
        const element = container.ownerDocument.createElement('button'); element.type = 'button';
        element.dataset.action = action; element.title = label; element.setAttribute('aria-label', label); element.textContent = glyph;
        Object.assign(element.style, { border: '0', borderRadius: '6px', background: 'transparent', color: 'inherit',
          minWidth: '30px', height: '28px', cursor: 'pointer', fontSize: '18px' });
        element.addEventListener('focus', () => { element.style.outline = '1px solid #8c806e'; });
        element.addEventListener('blur', () => { element.style.outline = 'none'; });
        toolbar.append(element); return element;
      };
      previous = button('previous', '上一页（←）', '‹'); next = button('next', '下一页（→）', '›');
      previous.addEventListener('click', () => navigate(requestedPage - 1)); next.addEventListener('click', () => navigate(requestedPage + 1));
      pageInput = container.ownerDocument.createElement('input'); pageInput.type = 'number'; pageInput.dataset.action = 'page';
      pageInput.min = '1'; pageInput.max = String(deck.slides.length); pageInput.setAttribute('aria-label', '幻灯片页码');
      Object.assign(pageInput.style, { width: '48px', height: '26px', boxSizing: 'border-box', borderRadius: '6px',
        border: '1px solid #c9c5bf', color: 'inherit', background: '#fff8', textAlign: 'center', outlineColor: '#8c806e' });
      pageInput.addEventListener('change', () => navigate((Number(pageInput.value) || 1) - 1));
      pageInput.addEventListener('keydown', event => { if (event.key === 'Enter') { event.preventDefault(); navigate((Number(pageInput.value) || 1) - 1); } });
      countLabel = container.ownerDocument.createElement('span'); toolbar.append(pageInput, countLabel);
      const spacer = container.ownerDocument.createElement('span'); spacer.style.flex = '1'; toolbar.append(spacer);
      zoomSelect = container.ownerDocument.createElement('select'); zoomSelect.dataset.action = 'zoom'; zoomSelect.setAttribute('aria-label', '幻灯片缩放');
      Object.assign(zoomSelect.style, { border: '1px solid #c9c5bf', borderRadius: '6px', height: '28px',
        background: '#fff8', color: 'inherit', outlineColor: '#8c806e' });
      for (const [value, label] of [['fit', '适应窗口'], ['0.5', '50%'], ['0.75', '75%'], ['1', '100%'], ['1.25', '125%'], ['1.5', '150%'], ['2', '200%']]) {
        const option = container.ownerDocument.createElement('option'); option.value = value; option.textContent = label; zoomSelect.append(option);
      }
      if (![...zoomSelect.options].some(option => option.value === String(zoom))) {
        const option = container.ownerDocument.createElement('option'); option.value = String(zoom); option.textContent = `${Math.round(zoom * 100)}%`; zoomSelect.append(option);
      }
      zoomSelect.value = String(zoom); zoomSelect.addEventListener('change', () => { zoom = zoomSelect.value === 'fit' ? 'fit' : Number(zoomSelect.value); scaleSlide(); emit(); });
      toolbar.append(zoomSelect); root.append(toolbar);
      frame = container.ownerDocument.createElement('iframe'); frame.className = 'pptx-frame'; frame.title = title;
      frame.setAttribute('sandbox', 'allow-same-origin');
      Object.assign(frame.style, { display: 'block', border: '0', width: '100%', flex: '1 1 0', minHeight: '0', background: '#f3f2f0' });
      const loaded = new Promise(resolve => frame.addEventListener('load', resolve, { once: true }));
      frame.srcdoc = `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src blob:; font-src 'none'; connect-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"><style>html,body{margin:0;min-height:100%;background:#f3f2f0}body{overflow:auto;font-family:'Microsoft YaHei','Segoe UI',sans-serif}*{box-sizing:border-box}p{overflow-wrap:normal}img{display:block}section{isolation:isolate}.pptx-text{flex-shrink:0;width:100%}.pptx-paragraph{overflow-wrap:break-word}.pptx-object{outline:0}</style></head><body><main id="slides" style="position:relative" tabindex="0"></main></body></html>`;
      root.append(frame); container.append(root); await loaded; alive();
      stage = frame.contentDocument.getElementById('slides');
      frame.contentDocument.addEventListener('keydown', keyboard);
      frame.contentDocument.addEventListener('pointerdown', event => {
        if (event.target.closest('a,input,button,select,textarea')) return;
        stage.focus({ preventScroll: true });
      });
      observer = new ResizeObserver(scaleSlide); observer.observe(frame);
      await show(page); alive();
      const settled = prepareRemaining();
      return { dispose, getState: state, settled };
    } catch (error) {
      dispose();
      if (error?.name === 'AbortError' || error?.code) throw error;
      throw failure(error?.message || 'PPTX preview failed');
    }
  }

  window.kynxaPptx = Object.freeze({ render });
})();
