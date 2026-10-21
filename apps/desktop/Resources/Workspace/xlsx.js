/* Spreadsheet previews read package values and cached formula results; they never calculate or load links.
 * 表格预览只读取文件值与公式缓存，不执行公式、不加载文档外链；大表只创建可见区域的单元格。 */
(() => {
  'use strict';

  const REL_NS = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships';
  const MAX_EXPANDED_BYTES = 128 * 1024 * 1024;
  const MAX_XML_BYTES = 64 * 1024 * 1024;
  const MAX_ROWS = 1048576, MAX_COLUMNS = 16384;
  const MAX_VISIBLE_CELLS = 131072;
  const ROW_HEADER_WIDTH = 46, COLUMN_HEADER_HEIGHT = 27;
  const builtinFormats = { 0: 'General', 1: '0', 2: '0.00', 3: '#,##0', 4: '#,##0.00',
    9: '0%', 10: '0.00%', 11: '0.00E+00', 12: '# ?/?', 13: '# ??/??', 14: 'mm-dd-yy',
    15: 'd-mmm-yy', 16: 'd-mmm', 17: 'mmm-yy', 18: 'h:mm AM/PM', 19: 'h:mm:ss AM/PM',
    20: 'h:mm', 21: 'h:mm:ss', 22: 'm/d/yy h:mm', 27: 'yyyy/m/d', 28: 'yyyy/m/d',
    29: 'yyyy/m/d', 30: 'm/d/yy', 31: 'yyyy年m月d日', 32: 'h时mm分', 33: 'h时mm分ss秒',
    34: 'yyyy年m月', 35: 'm月d日', 36: 'yyyy年m月', 45: 'mm:ss', 46: '[h]:mm:ss', 47: 'mm:ss.0',
    48: '##0.0E+0', 49: '@' };
  const indexedColors = ['000000', 'FFFFFF', 'FF0000', '00FF00', '0000FF', 'FFFF00', 'FF00FF', '00FFFF',
    '000000', 'FFFFFF', 'FF0000', '00FF00', '0000FF', 'FFFF00', 'FF00FF', '00FFFF',
    '800000', '008000', '000080', '808000', '800080', '008080', 'C0C0C0', '808080',
    '9999FF', '993366', 'FFFFCC', 'CCFFFF', '660066', 'FF8080', '0066CC', 'CCCCFF',
    '000080', 'FF00FF', 'FFFF00', '00FFFF', '800080', '800000', '008080', '0000FF',
    '00CCFF', 'CCFFFF', 'CCFFCC', 'FFFF99', '99CCFF', 'FF99CC', 'CC99FF', 'FFCC99',
    '3366FF', '33CCCC', '99CC00', 'FFCC00', 'FF9900', 'FF6600', '666699', '969696',
    '003366', '339966', '003300', '333300', '993300', '993366', '333399', '333333'];
  const children = (node, name) => [...(node?.children || [])].filter(item => !name || item.localName === name);
  const child = (node, name) => children(node, name)[0] || null;
  const descendants = (node, name) => node?.getElementsByTagNameNS('*', name) || [];
  const attr = (node, name, fallback = '') => node?.getAttribute(name) ?? fallback;
  const numeric = (node, name, fallback = 0) => {
    const text = attr(node, name, ''); const value = text === '' ? NaN : Number(text);
    return Number.isFinite(value) ? value : fallback;
  };
  const truth = value => value === '1' || value === 'true';
  const clamp = (value, low, high) => Math.min(high, Math.max(low, value));
  const pause = () => new Promise(resolve => setTimeout(resolve, 0));

  function failure(message, code = 'XLSX_INVALID') { const error = new Error(message); error.code = code; return error; }
  const unsupported = message => failure(message, 'FEATURE_UNSUPPORTED');
  function checkSignal(signal) { if (signal?.aborted) throw new DOMException('XLSX preview cancelled', 'AbortError'); }

  function partPath(base, target) {
    let decoded;
    try { decoded = decodeURIComponent(target); } catch { throw failure('Invalid XLSX relationship path'); }
    if (!decoded || /[\\\u0000?#]/.test(decoded) || /^[a-z][a-z\d+.-]*:/i.test(decoded))
      throw failure('XLSX relationships must refer to local package parts');
    const result = decoded.startsWith('/') ? [] : base.split('/').slice(0, -1);
    for (const segment of decoded.split('/')) {
      if (!segment || segment === '.') continue;
      if (segment === '..') { if (!result.length) throw failure('XLSX relationship leaves its package'); result.pop(); }
      else result.push(segment);
    }
    return result.join('/');
  }

  async function openPackage(bytes, signal) {
    if (!window.JSZip) throw failure('XLSX ZIP reader unavailable');
    checkSignal(signal);
    const zip = await window.JSZip.loadAsync(bytes); checkSignal(signal);
    const xmlCache = new Map(), relationshipCache = new Map();
    let expandedBytes = 0, readBytes = 0, closed = false, count = 0;
    for (const [name, entry] of Object.entries(zip.files)) {
      if (++count > 20000) throw unsupported('XLSX package contains too many parts');
      for (const path of [name, entry.unsafeOriginalName || name]) {
        if (path.startsWith('/') || path.includes('\\') || path.includes('\0') || path.split('/').includes('..'))
          throw failure('Unsafe XLSX package path');
      }
      if ((Number(entry.unixPermissions) & 0o170000) === 0o120000) throw failure('XLSX package links are not supported');
      expandedBytes += entry._data?.uncompressedSize || 0;
      if (expandedBytes > MAX_EXPANDED_BYTES) throw unsupported('XLSX expanded package exceeds 128 MiB');
      if (!entry.dir && (/^xl\/(charts|pivotTables|pivotCache|embeddings|activeX)\//i.test(name) || /\/vbaProject\.bin$/i.test(name)))
        throw unsupported(`XLSX part requires Office page rendering: ${name}`);
    }
    async function xml(path, optional = false, cache = true) {
      if (cache && xmlCache.has(path)) return xmlCache.get(path);
      const pending = (async () => {
        checkSignal(signal); if (closed) throw new DOMException('XLSX preview disposed', 'AbortError');
        const entry = zip.file(path);
        if (!entry) { if (optional) return null; throw failure(`XLSX part is missing: ${path}`); }
        if ((entry._data?.uncompressedSize || 0) > MAX_XML_BYTES) throw unsupported('XLSX XML part exceeds 64 MiB');
        const data = await entry.async('uint8array'); checkSignal(signal);
        readBytes += data.byteLength;
        if (data.byteLength > MAX_XML_BYTES || readBytes > MAX_EXPANDED_BYTES) throw unsupported('XLSX XML exceeds preview capacity');
        if (closed) throw new DOMException('XLSX preview disposed', 'AbortError');
        const source = new TextDecoder('utf-8', { fatal: true }).decode(data);
        if (/<!DOCTYPE|<!ENTITY/i.test(source)) throw failure('XLSX XML document type declarations are not allowed');
        const document = new DOMParser().parseFromString(source, 'application/xml');
        if (document.getElementsByTagName('parsererror').length) throw failure(`Invalid XLSX XML: ${path}`);
        return document.documentElement;
      })();
      if (cache) xmlCache.set(path, pending);
      return pending;
    }
    async function relationships(path) {
      if (relationshipCache.has(path)) return relationshipCache.get(path);
      const slash = path.lastIndexOf('/');
      const name = path ? `${path.slice(0, slash + 1)}_rels/${path.slice(slash + 1)}.rels` : '_rels/.rels';
      const pending = (async () => {
        const root = await xml(name, true), result = new Map();
        for (const node of children(root, 'Relationship')) {
          const external = attr(node, 'TargetMode').toLowerCase() === 'external';
          result.set(attr(node, 'Id'), { type: attr(node, 'Type'), external,
            path: external ? null : partPath(path, attr(node, 'Target')) });
        }
        return result;
      })();
      relationshipCache.set(path, pending); return pending;
    }
    return { xml, relationships, close() { closed = true; xmlCache.clear(); relationshipCache.clear(); } };
  }

  function richText(node) {
    let value = '';
    for (const item of node?.children || []) {
      if (item.localName === 't') value += item.textContent;
      else if (item.localName === 'r') value += child(item, 't')?.textContent || '';
    }
    return value;
  }

  function columnName(index) {
    let result = '';
    for (let number = index + 1; number > 0; number = Math.floor((number - 1) / 26))
      result = String.fromCharCode(65 + (number - 1) % 26) + result;
    return result;
  }

  function address(text) {
    const match = /^\$?([A-Z]{1,3})\$?([1-9]\d{0,6})$/i.exec(text || '');
    if (!match) return null;
    let column = 0;
    for (const letter of match[1].toUpperCase()) column = column * 26 + letter.charCodeAt(0) - 64;
    const row = Number(match[2]) - 1; column--;
    return column < MAX_COLUMNS && row < MAX_ROWS ? { row, column } : null;
  }

  function cellRange(text) {
    const parts = String(text || '').split(':'), first = address(parts[0]), last = address(parts[1] || parts[0]);
    if (!first || !last || first.row > last.row || first.column > last.column) throw failure('Invalid XLSX cell range');
    return { r1: first.row, c1: first.column, r2: last.row, c2: last.column };
  }

  function color(node, theme, fallback = '') {
    if (!node) return fallback;
    let value = attr(node, 'rgb');
    if (/^[a-f\d]{8}$/i.test(value)) value = value.slice(2);
    if (!/^[a-f\d]{6}$/i.test(value)) {
      const themeIndex = numeric(node, 'theme', -1), index = numeric(node, 'indexed', -1);
      value = themeIndex >= 0 ? theme[themeIndex] : indexedColors[index];
    }
    if (!/^[a-f\d]{6}$/i.test(value || '')) return fallback;
    const tint = clamp(numeric(node, 'tint', 0), -1, 1);
    return '#' + [0, 2, 4].map(offset => {
      const channel = parseInt(value.slice(offset, offset + 2), 16);
      return Math.round(tint < 0 ? channel * (1 + tint) : channel + (255 - channel) * tint).toString(16).padStart(2, '0');
    }).join('');
  }

  async function readStyles(packageReader, relations) {
    const related = suffix => [...relations.values()].find(item => item.type.endsWith('/' + suffix) && !item.external)?.path;
    const [root, themeRoot] = await Promise.all([
      related('styles') ? packageReader.xml(related('styles')) : null,
      related('theme') ? packageReader.xml(related('theme')) : null,
    ]);
    const scheme = descendants(themeRoot, 'clrScheme')[0];
    const theme = ['lt1', 'dk1', 'lt2', 'dk2', 'accent1', 'accent2', 'accent3', 'accent4', 'accent5', 'accent6', 'hlink', 'folHlink']
      .map(name => { const item = child(scheme, name)?.firstElementChild; return attr(item, 'lastClr', attr(item, 'val')); });
    const formats = new Map(Object.entries(builtinFormats).map(([id, code]) => [Number(id), code]));
    for (const node of children(child(root, 'numFmts'), 'numFmt')) formats.set(numeric(node, 'numFmtId'), attr(node, 'formatCode'));
    const fontFlag = (node, name) => {
      const item = child(node, name); return !!item && !['0', 'false', 'off', 'none'].includes(attr(item, 'val').toLowerCase());
    };
    const fonts = children(child(root, 'fonts'), 'font').map(node => ({
      name: attr(child(node, 'name'), 'val'), size: clamp(numeric(child(node, 'sz'), 'val', 11), 1, 409),
      bold: fontFlag(node, 'b'), italic: fontFlag(node, 'i'),
      underline: fontFlag(node, 'u'), strike: fontFlag(node, 'strike'),
      color: color(child(node, 'color'), theme, '#262626'),
    }));
    const fills = children(child(root, 'fills'), 'fill').map(node => {
      const pattern = child(node, 'patternFill');
      return attr(pattern, 'patternType') === 'solid' ? color(child(pattern, 'fgColor'), theme) : '';
    });
    const borderStyle = { thin: '1px solid', medium: '2px solid', thick: '3px solid', hair: '1px dotted',
      dotted: '1px dotted', dashed: '1px dashed', dashDot: '1px dashed', dashDotDot: '1px dashed',
      mediumDashed: '2px dashed', mediumDashDot: '2px dashed', mediumDashDotDot: '2px dashed', double: '3px double', slantDashDot: '1px dashed' };
    const borders = children(child(root, 'borders'), 'border').map(node => Object.fromEntries(['left', 'right', 'top', 'bottom'].map(side => {
      const item = child(node, side), style = borderStyle[attr(item, 'style')];
      return [side, style ? `${style} ${color(child(item, 'color'), theme, '#555555')}` : ''];
    })));
    const styles = children(child(root, 'cellXfs'), 'xf').map(node => {
      const alignment = child(node, 'alignment');
      return { font: fonts[numeric(node, 'fontId')] || {}, fill: fills[numeric(node, 'fillId')] || '',
        border: borders[numeric(node, 'borderId')] || {}, format: formats.get(numeric(node, 'numFmtId')) || 'General',
        horizontal: attr(alignment, 'horizontal'), vertical: attr(alignment, 'vertical', 'bottom'),
        wrap: truth(attr(alignment, 'wrapText')), indent: clamp(numeric(alignment, 'indent'), 0, 250),
      };
    });
    return styles.length ? styles : [{ font: {}, border: {}, format: 'General' }];
  }

  function splitSections(code) {
    const sections = ['']; let quoted = false, escaped = false;
    for (const character of code) {
      if (!escaped && character === '"') quoted = !quoted;
      if (!quoted && !escaped && character === ';') sections.push('');
      else sections[sections.length - 1] += character;
      if (!escaped && character === '\\') escaped = true; else escaped = false;
    }
    return sections;
  }

  function formatLiteral(code) {
    return (code.match(/"[^"]*"|\\.|\[[^\]]*\]|_.|\*.|./g) || []).map(token => {
      if (token.startsWith('"')) return token.slice(1, -1);
      if (token.startsWith('\\')) return token.slice(1);
      if (token.startsWith('[$')) return /^\[\$([^\]-]*)/.exec(token)?.[1] || '';
      if (/^(\[|_|\*)/.test(token) || /^[0#?@.,%]$/.test(token)) return '';
      return token;
    }).join('');
  }

  function dateText(serial, code, date1904) {
    const days = Math.floor(serial), milliseconds = Math.round((serial - days) * 86400000);
    const time = new Date((date1904 ? Date.UTC(1904, 0, 1) : Date.UTC(1899, 11, 31))
      + (date1904 ? days : days - (days >= 60 ? 1 : 0)) * 86400000 + milliseconds);
    if (!Number.isFinite(time.getTime())) return String(serial);
    const fakeLeapDay = !date1904 && days === 60;
    const parts = { y: fakeLeapDay ? 1900 : time.getUTCFullYear(), m: fakeLeapDay ? 2 : time.getUTCMonth() + 1,
      d: fakeLeapDay ? 29 : time.getUTCDate(), h: time.getUTCHours(), s: time.getUTCSeconds(), minute: time.getUTCMinutes() };
    const tokens = code.match(/"[^"]*"|\\.|\[[hms]+\]|AM\/PM|A\/P|y+|m+|d+|h+|s+|\[[^\]]*\]|_.|\*.|./gi) || [];
    const hasAmPm = tokens.some(token => /^(AM\/PM|A\/P)$/i.test(token));
    const unit = token => /^[ymdhs]+$/i.test(token) ? token[0].toLowerCase() : null;
    const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
    const weekdays = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
    return tokens.map((token, index) => {
      if (token.startsWith('"')) return token.slice(1, -1);
      if (token.startsWith('\\')) return token.slice(1);
      if (/^(AM\/PM|A\/P)$/i.test(token)) return parts.h < 12 ? (token.length === 3 ? 'A' : 'AM') : (token.length === 3 ? 'P' : 'PM');
      if (/^\[[hms]+\]$/i.test(token)) {
        const multiplier = token[1].toLowerCase() === 'h' ? 24 : token[1].toLowerCase() === 'm' ? 1440 : 86400;
        return String(Math.floor(Math.abs(serial) * multiplier));
      }
      if (/^(\[|_|\*)/.test(token)) return '';
      const kind = unit(token), padded = number => String(number).padStart(token.length === 1 ? 1 : 2, '0');
      if (kind === 'y') return token.length === 2 ? String(parts.y).slice(-2) : String(parts.y);
      if (kind === 'd') return token.length >= 3 ? weekdays[time.getUTCDay()].slice(0, token.length === 3 ? 3 : undefined) : padded(parts.d);
      if (kind === 'h') return padded(hasAmPm ? parts.h % 12 || 12 : parts.h);
      if (kind === 's') return padded(parts.s);
      if (kind === 'm') {
        const before = tokens.slice(0, index).reverse().map(unit).find(Boolean), after = tokens.slice(index + 1).map(unit).find(Boolean);
        if (token.length <= 2 && (before === 'h' || after === 's')) return padded(parts.minute);
        if (token.length === 3) return months[parts.m - 1];
        if (token.length >= 4) return new Date(Date.UTC(2000, parts.m - 1, 1)).toLocaleString('en-US', { month: token.length === 5 ? 'narrow' : 'long', timeZone: 'UTC' });
        return padded(parts.m);
      }
      return token;
    }).join('');
  }

  function numberText(value, code, date1904) {
    if (!Number.isFinite(value)) return String(value);
    if (!code || /^(General|@)$/i.test(code)) return String(value);
    const sections = splitSections(code);
    const selected = value < 0 && sections[1] !== undefined ? sections[1] : value === 0 && sections[2] !== undefined ? sections[2] : sections[0];
    if (selected === '') return '';
    const pattern = selected.replace(/"[^"]*"|\\.|_.|\*./g, '').replace(/\[(?![hms]+\])[^\]]*\]/gi, '');
    if (/[ymdhs]/i.test(pattern)) return dateText(value, selected, date1904);
    const placeholders = selected.replace(/"[^"]*"|\\.|_.|\*.|\[[^\]]*\]/g, token => ' '.repeat(token.length));
    const first = placeholders.search(/[0#?]/);
    if (first < 0) return formatLiteral(selected);
    const last = Math.max(placeholders.lastIndexOf('0'), placeholders.lastIndexOf('#'), placeholders.lastIndexOf('?'));
    if (/[0#?]\s*\/\s*[0#?]/.test(pattern)) return String(value);
    const numericPattern = pattern.match(/[0#?][0#?,.]*([Ee][+-]?[0#?]+)?/)?.[0] || '0';
    const exponent = /[Ee]([+-]?)(0+)/.exec(numericPattern);
    const decimal = /\.([0#?]+)/.exec(numericPattern)?.[1] || '';
    const percentCount = (pattern.match(/%/g) || []).length;
    let number = value < 0 && sections[1] !== undefined ? Math.abs(value) : value;
    number *= 100 ** percentCount;
    const trailingCommas = /[0#?](,+)(?:\.|$)/.exec(numericPattern)?.[1].length || 0;
    number /= 1000 ** trailingCommas;
    let text;
    if (exponent) {
      const [mantissa, power] = number.toExponential(Math.min(decimal.length, 20)).split('e');
      const exponentValue = Number(power);
      text = `${mantissa}E${exponentValue < 0 ? '-' : exponent[1] === '+' ? '+' : ''}${String(Math.abs(exponentValue)).padStart(exponent[2].length, '0')}`;
    } else {
      const minimum = Math.min((decimal.match(/0/g) || []).length, 20), maximum = Math.min(decimal.length, 20);
      text = number.toLocaleString('en-US', { useGrouping: /[0#?],[0#?]/.test(numericPattern), minimumFractionDigits: minimum, maximumFractionDigits: maximum });
    }
    return formatLiteral(selected.slice(0, first)) + text + formatLiteral(selected.slice(last + 1)) + '%'.repeat(percentCount);
  }

  // Sparse size overrides and cumulative differences avoid a million-entry array for mostly empty sheets.
  // 稀疏尺寸覆写及前缀差值避免为空白大表分配百万项数组，定位只依赖二分查找。
  class Axis {
    constructor(count, defaultSize, overrides) {
      this.count = count; this.defaultSize = defaultSize;
      this.indices = [...overrides.keys()].filter(index => index >= 0 && index < count).sort((a, b) => a - b);
      this.sizes = this.indices.map(index => overrides.get(index)); this.differences = [0];
      for (const size of this.sizes) this.differences.push(this.differences[this.differences.length - 1] + size - defaultSize);
    }
    before(index) { let low = 0, high = this.indices.length; while (low < high) { const middle = (low + high) >>> 1; if (this.indices[middle] < index) low = middle + 1; else high = middle; } return low; }
    position(index) { return index * this.defaultSize + this.differences[this.before(index)]; }
    size(index) { const slot = this.before(index); return this.indices[slot] === index ? this.sizes[slot] : this.defaultSize; }
    at(pixel) { let low = 0, high = this.count; while (low < high) { const middle = (low + high) >>> 1; if (this.position(middle + 1) <= pixel) low = middle + 1; else high = middle; } return Math.min(low, this.count - 1); }
    visible(start, end, limit = Infinity) {
      const result = []; let index = this.at(Math.max(0, start));
      while (index < this.count && this.position(index) <= end && result.length < limit) {
        if (this.size(index) > 0) { result.push(index); index++; }
        else index = Math.max(index + 1, this.at(this.position(index) + .01));
      }
      return result;
    }
  }

  function mergeTree(ranges) {
    if (!ranges.length) return null;
    const middle = ranges.length >>> 1, value = ranges[middle];
    const left = mergeTree(ranges.slice(0, middle)), right = mergeTree(ranges.slice(middle + 1));
    return { value, left, right, maxRow: Math.max(value.r2, left?.maxRow ?? -1, right?.maxRow ?? -1) };
  }

  function visibleMerges(tree, firstRow, lastRow, firstColumn, lastColumn, result = []) {
    if (!tree || tree.maxRow < firstRow) return result;
    visibleMerges(tree.left, firstRow, lastRow, firstColumn, lastColumn, result);
    const range = tree.value;
    if (range.r1 <= lastRow && range.r2 >= firstRow && range.c1 <= lastColumn && range.c2 >= firstColumn) result.push(range);
    if (range.r1 <= lastRow) visibleMerges(tree.right, firstRow, lastRow, firstColumn, lastColumn, result);
    return result;
  }

  async function readWorkbook(packageReader, signal, selectedSheet = 0) {
    const rootRelations = await packageReader.relationships('');
    const workbookPath = [...rootRelations.values()].find(item => item.type.endsWith('/officeDocument') && !item.external)?.path || 'xl/workbook.xml';
    const [workbook, relations] = await Promise.all([packageReader.xml(workbookPath), packageReader.relationships(workbookPath)]);
    if (workbook.localName !== 'workbook') throw failure('XLSX workbook part is invalid');
    const sheets = [];
    for (const item of children(child(workbook, 'sheets'), 'sheet')) {
      if (['hidden', 'veryHidden'].includes(attr(item, 'state'))) continue;
      const id = item.getAttributeNS(REL_NS, 'id') || attr(item, 'r:id');
      const relationship = relations.get(id);
      if (!relationship || relationship.external) throw failure('XLSX worksheet relationship is invalid');
      if (!relationship.type.endsWith('/worksheet')) throw unsupported('This workbook contains a non-worksheet sheet');
      sheets.push({ name: attr(item, 'name', `Sheet ${sheets.length + 1}`), path: relationship.path });
    }
    if (!sheets.length) throw failure('XLSX workbook has no visible worksheets');
    const first = sheets[clamp(Math.trunc(selectedSheet) || 0, 0, sheets.length - 1)];
    // Begin the selected worksheet alongside shared strings/styles, without decoding every string first.
    // 选中表原件与共享字符串、样式并行读取；共享字符串只在实际单元格需要时解码一次。
    first.source = packageReader.xml(first.path, false, false);
    first.source.catch(() => {});
    const stringPath = [...relations.values()].find(item => item.type.endsWith('/sharedStrings') && !item.external)?.path;
    const result = { sheets, styles: [], date1904: truth(attr(child(workbook, 'workbookPr'), 'date1904')) };
    let stringNodes = null;
    const strings = new Map();
    result.stringAt = index => {
      if (!Number.isInteger(index) || index < 0 || stringNodes?.[index]?.localName !== 'si')
        throw failure('XLSX shared string reference is invalid');
      if (!strings.has(index)) strings.set(index, richText(stringNodes[index]));
      return strings.get(index);
    };
    result.ready = Promise.all([stringPath ? packageReader.xml(stringPath) : null, readStyles(packageReader, relations)])
      .then(([stringsRoot, styles]) => { checkSignal(signal); stringNodes = stringsRoot?.children || []; result.styles = styles; });
    result.ready.catch(() => {});
    checkSignal(signal);
    return result;
  }

  async function readSheet(packageReader, specification, workbook, signal) {
    const root = await (specification.source || packageReader.xml(specification.path, false, false));
    specification.source = null;
    if (root.localName !== 'worksheet') throw failure('XLSX worksheet part is invalid');
    if (descendants(root, 'oleObject').length || descendants(root, 'control').length) throw unsupported('XLSX embedded objects require Office rendering');
    const drawingNodes = descendants(root, 'drawing');
    if (drawingNodes.length) {
      const relations = await packageReader.relationships(specification.path);
      for (const node of drawingNodes) {
        const id = node.getAttributeNS(REL_NS, 'id') || attr(node, 'r:id'), relation = relations.get(id);
        if (!relation || relation.external) throw unsupported('XLSX drawing relationship is not a local supported part');
        const drawing = await packageReader.xml(relation.path);
        if (children(drawing).length) throw unsupported('XLSX drawing objects require Office page rendering');
      }
    }
    const sheetFormat = child(root, 'sheetFormatPr');
    const defaultHeight = Math.max(8, numeric(sheetFormat, 'defaultRowHeight', 15) * 4 / 3);
    const defaultWidth = Math.max(16, numeric(sheetFormat, 'defaultColWidth', 8.43) * 7 + 5);
    const rowSizes = new Map(), columnSizes = new Map(), rowStyles = new Map(), columnStyles = new Map(), cells = new Map();
    let maxRow = 0, maxColumn = 0;
    const dimension = child(root, 'dimension');
    if (dimension) { const range = cellRange(attr(dimension, 'ref')); maxRow = range.r2; maxColumn = range.c2; }
    for (const node of children(child(root, 'cols'), 'col')) {
      const first = clamp(Math.trunc(numeric(node, 'min', 1)) - 1, 0, MAX_COLUMNS - 1);
      const last = clamp(Math.trunc(numeric(node, 'max', first + 1)) - 1, first, MAX_COLUMNS - 1);
      const width = truth(attr(node, 'hidden')) ? 0 : Math.max(16, numeric(node, 'width', (defaultWidth - 5) / 7) * 7 + 5);
      for (let index = first; index <= last; index++) {
        columnSizes.set(index, width);
        if (attr(node, 'style') !== '') columnStyles.set(index, numeric(node, 'style'));
      }
    }
    const entries = [], rowEntries = new Map();
    let nextRow = 0, sliceStarted = performance.now();
    for (const row of child(root, 'sheetData')?.children || []) {
      if (row.localName !== 'row') continue;
      const rowIndex = clamp(Math.trunc(numeric(row, 'r', nextRow + 1)) - 1, 0, MAX_ROWS - 1); nextRow = rowIndex + 1;
      maxRow = Math.max(maxRow, rowIndex);
      if (truth(attr(row, 'hidden'))) rowSizes.set(rowIndex, 0);
      else if (attr(row, 'ht') !== '') rowSizes.set(rowIndex, Math.max(8, numeric(row, 'ht', defaultHeight * .75) * 4 / 3));
      if (attr(row, 's') !== '') rowStyles.set(rowIndex, numeric(row, 's'));
      let lastCell = row.lastElementChild;
      while (lastCell && lastCell.localName !== 'c') lastCell = lastCell.previousElementSibling;
      const lastAddress = lastCell && address(attr(lastCell, 'r'));
      if (lastAddress) maxColumn = Math.max(maxColumn, lastAddress.column);
      else {
        let nextColumn = 0;
        for (const cell of row.children) if (cell.localName === 'c') {
          const position = attr(cell, 'r') ? address(attr(cell, 'r')) : { column: nextColumn };
          if (!position) throw failure('XLSX cell address is invalid');
          nextColumn = position.column + 1; maxColumn = Math.max(maxColumn, position.column);
        }
      }
      if (rowEntries.has(rowIndex)) throw failure('XLSX worksheet contains a duplicate row');
      const entry = { row: rowIndex, node: row, cursor: row.firstElementChild, nextColumn: 0, complete: false };
      entries.push(entry); rowEntries.set(rowIndex, entry);
      if (performance.now() - sliceStarted > 8) { checkSignal(signal); await pause(); sliceStarted = performance.now(); }
    }
    const merges = children(child(root, 'mergeCells'), 'mergeCell').map(node => cellRange(attr(node, 'ref')));
    for (const range of merges) { maxRow = Math.max(maxRow, range.r2); maxColumn = Math.max(maxColumn, range.c2); }
    await workbook.ready; checkSignal(signal);
    let remaining = entries.length, backgroundIndex = 0;
    const result = { cells, missingCached: 0, rowStyles, columnStyles,
      rows: new Axis(Math.min(MAX_ROWS, Math.max(maxRow + 1, 100)), defaultHeight, rowSizes),
      columns: new Axis(Math.min(MAX_COLUMNS, Math.max(maxColumn + 1, 26)), defaultWidth, columnSizes),
      merges: mergeTree(merges.sort((a, b) => a.r1 - b.r1 || a.c1 - b.c1)) };

    function decodeRow(entry, deadline = Infinity) {
      if (!entry || entry.complete) return;
      let checked = 0;
      while (entry.cursor) {
        const node = entry.cursor;
        entry.cursor = node.nextElementSibling;
        if (node.localName !== 'c') continue;
        const position = attr(node, 'r') ? address(attr(node, 'r')) : { row: entry.row, column: entry.nextColumn };
        if (!position || position.column >= MAX_COLUMNS) throw failure('XLSX cell address is invalid');
        entry.nextColumn = position.column + 1;
        const type = attr(node, 't', 'n'), formulaNode = child(node, 'f'), valueNode = child(node, 'v');
        const raw = valueNode?.textContent ?? '', hasCached = !!valueNode && raw !== '';
        let value = raw, valueType = type;
        const formula = formulaNode ? (formulaNode.textContent || `共享公式 ${attr(formulaNode, 'si')}`) : null;
        if (formulaNode && !hasCached) { value = '=' + formula; valueType = 'str'; result.missingCached++; }
        else if (type === 's') value = workbook.stringAt(Number(raw));
        else if (type === 'inlineStr') value = richText(child(node, 'is'));
        else if (type === 'b') value = truth(raw) ? 'TRUE' : 'FALSE';
        else if (type === 'n' && raw !== '') { value = Number(raw); if (!Number.isFinite(value)) value = raw; }
        const styleIndex = numeric(node, 's', rowStyles.get(position.row) ?? columnStyles.get(position.column) ?? 0);
        const style = workbook.styles[styleIndex] || workbook.styles[0];
        cells.set(`${position.row}:${position.column}`, { value, type: valueType, numeric: typeof value === 'number', formula, style });
        if (++checked % 32 === 0 && performance.now() >= deadline) return;
      }
      entry.complete = true; remaining--;
      // Completed rows no longer retain their XML subtree; cached values remain the source for later views.
      // 已解析行释放 XML 子树，之后滚动和切表只读缓存值，不再解压或重复解码。
      entry.node.remove(); entry.node = null;
    }
    result.ensureRows = (rows, deadline = Infinity) => {
      checkSignal(signal);
      for (let index = 0; index < rows.length; index++) {
        const row = rows[index];
        const entry = rowEntries.get(row); decodeRow(entry, deadline);
        if (entry && !entry.complete) return false;
        if (performance.now() >= deadline) return index === rows.length - 1;
      }
      return true;
    };
    result.hasPending = () => remaining > 0;
    result.advance = deadline => {
      checkSignal(signal);
      while (backgroundIndex < entries.length && performance.now() < deadline) {
        const entry = entries[backgroundIndex]; decodeRow(entry, deadline);
        if (entry.complete) backgroundIndex++; else break;
      }
    };
    return result;
  }

  function installStyles(document) {
    if (document.getElementById('kynxa-xlsx-styles')) return;
    const style = document.createElement('style'); style.id = 'kynxa-xlsx-styles';
    style.textContent = `
      .xlsx-view{display:flex;flex-direction:column;width:100%;height:calc(100vh - 24px);min-height:260px;overflow:hidden;border:1px solid var(--appearance-border,#ddd);border-radius:8px;background:var(--appearance-main,#fff);color:var(--appearance-text,#262626);font:13px/1.4 var(--ui-font,'Segoe UI','Microsoft YaHei',sans-serif)}
      .xlsx-toolbar{display:flex;align-items:center;gap:9px;flex:0 0 auto;min-height:36px;padding:5px 9px;border-bottom:1px solid var(--appearance-border,#ddd);background:var(--appearance-sidebar,#f8f7f5)}
      .xlsx-title{max-width:35%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-weight:600}.xlsx-notice{font-size:11px;color:var(--appearance-secondary,#666);flex:1;min-width:0;line-height:1.3}
      .xlsx-view select,.xlsx-view button{font:inherit;color:inherit;border:1px solid var(--appearance-border,#ddd);border-radius:6px;background:var(--appearance-main,#fff)}
      .xlsx-view select{height:26px;padding:1px 4px}.xlsx-view :focus-visible{outline:1px solid var(--appearance-secondary,#666);outline-offset:1px}
      .xlsx-scroll{position:relative;overflow:auto;flex:1 1 0;min-width:0;min-height:0;overscroll-behavior:contain;background:#fff;scrollbar-color:#aaa7a1 transparent;scrollbar-width:thin}
      .xlsx-grid{position:relative;isolation:isolate;overflow:hidden;user-select:text;background:#fff;color:#262626}
      .xlsx-cell,.xlsx-header{position:absolute;box-sizing:border-box;overflow:hidden;white-space:nowrap;overflow-wrap:normal}
      .xlsx-cell{display:flex;align-items:flex-end;border-right:1px solid #e4e3e0;border-bottom:1px solid #e4e3e0;background:#fff;line-height:1.25;padding:1px 4px;z-index:1}
      .xlsx-value{display:block;width:100%;min-width:0;overflow:hidden;text-overflow:clip}.xlsx-header{z-index:3;display:flex;align-items:center;justify-content:center;user-select:none;background:#f2f1ee;border-right:1px solid #d9d7d1;border-bottom:1px solid #d9d7d1;font:12px/1.2 var(--ui-font,'Segoe UI',sans-serif);color:#65615b}
      .xlsx-corner{z-index:4}.xlsx-tabs{display:flex;gap:4px;flex:0 0 auto;overflow-x:auto;padding:4px 7px;background:var(--appearance-sidebar,#f8f7f5);border-top:1px solid var(--appearance-border,#ddd);scrollbar-width:thin}
      .xlsx-tabs button{flex:0 0 auto;max-width:220px;overflow:hidden;text-overflow:ellipsis;padding:4px 13px;cursor:pointer;border-color:transparent;background:transparent}
      .xlsx-tabs button[aria-selected=true]{border-color:var(--appearance-border,#d2cec7);background:var(--appearance-main,#fff);font-weight:600}.xlsx-tabs button:hover{background:var(--appearance-soft,#f1eeea)}
      @media(max-width:440px){.xlsx-title{display:none}.xlsx-toolbar{gap:5px}.xlsx-notice{font-size:10px}}
    `;
    document.head.append(style);
  }

  async function render(bytes, options = {}) {
    const { container, signal: externalSignal } = options;
    if (!container?.ownerDocument) throw failure('XLSX preview container is missing');
    const cancellation = new AbortController(), signal = cancellation.signal;
    const document = container.ownerDocument, initial = options.state || {};
    let disposed = false, packageReader, workbook, root, scroll, grid, note, observer;
    let currentSheet = -1, sheet, showRevision = 0, frame = 0, paintFrame = 0, viewportRevision = 0;
    let viewport = null, viewportPending = false, zoom = clamp(Number(initial.zoom) || 1, .25, 3);
    const sheetCache = new Map(), nodes = new Map(), positions = {}, tabButtons = [];
    const preparedSheets = new Map(), pendingSheets = new Set();
    let requestedSheet = -1, worker = null, initializing = true, backgroundPending = false;
    let idle = Promise.resolve(), resolveIdle = null;
    const cores = Math.max(1, Number(navigator.hardwareConcurrency) || 4);
    const memory = Number(navigator.deviceMemory) || 8;
    const sliceMs = Math.min(memory < 4 ? 8 : 12, Math.max(6, 6 + Math.floor(Math.log2(cores)) * 2));
    for (const [index, saved] of Object.entries(initial.sheets || {})) {
      if (/^\d+$/.test(index)) positions[index] = { x: Math.max(0, Number(saved?.x) || 0), y: Math.max(0, Number(saved?.y) || 0) };
    }
    function alive() { checkSignal(signal); if (disposed) throw new DOMException('XLSX preview disposed', 'AbortError'); }
    function savePosition() { if (currentSheet >= 0 && scroll) positions[currentSheet] = { x: scroll.scrollLeft, y: scroll.scrollTop }; }
    function getState() { savePosition(); return { sheet: Math.max(0, currentSheet), zoom, sheets: Object.fromEntries(Object.entries(positions).map(([index, position]) => [index, { ...position }])) }; }
    function emit() { if (!disposed && typeof options.onState === 'function') options.onState(getState()); }
    function updateBackground() {
      const pending = !disposed && (initializing || pendingSheets.size > 0 || viewportPending);
      if (pending === backgroundPending) return;
      backgroundPending = pending;
      if (pending) idle = new Promise(resolve => { resolveIdle = resolve; });
      else { resolveIdle?.(); resolveIdle = null; }
      if (typeof options.onBackground === 'function') options.onBackground(pending);
    }
    function updateNote() {
      if (!note || !sheet) return;
      if (viewportPending) { note.textContent = '正在显示当前区域；可继续滚动、缩放或切换工作表。'; return; }
      note.textContent = !sheet.hasPending() && sheet.missingCached
        ? `${sheet.missingCached} 个公式没有缓存，显示公式原文；不执行计算。`
        : '公式显示文件内缓存值；没有缓存时显示公式原文，不执行计算。';
    }
    function completed(index, parsed) {
      if (parsed.hasPending()) return;
      pendingSheets.delete(index); updateBackground();
      if (index === currentSheet) updateNote();
    }
    function prepareInBackground() {
      if (worker || disposed) return;
      worker = (async () => {
        // First paint and input get an event-loop turn before any remaining rows are decoded.
        // 首屏绘制和输入先获得事件循环时段，其余行按硬件适配的短批次继续解码。
        await pause();
        while (!disposed) {
          const selected = pendingSheets.has(requestedSheet) && preparedSheets.has(requestedSheet) ? requestedSheet
            : [...pendingSheets].find(index => preparedSheets.has(index));
          if (selected === undefined) break;
          const parsed = preparedSheets.get(selected);
          try {
            const budget = viewportPending || navigator.scheduling?.isInputPending?.() ? Math.min(5, sliceMs) : sliceMs;
            parsed.advance(performance.now() + budget); completed(selected, parsed);
          } catch (error) {
            pendingSheets.delete(selected); preparedSheets.delete(selected); updateBackground(); report(error); break;
          }
          if (pendingSheets.size) await pause();
        }
      })().catch(report).finally(() => {
        worker = null;
        if (!disposed && [...pendingSheets].some(index => preparedSheets.has(index))) prepareInBackground();
      });
    }
    function report(error) {
      if (disposed || error?.name === 'AbortError') return;
      root?.dispatchEvent(new CustomEvent('xlsx-error', { detail: error, bubbles: true }));
      if (typeof options.onError === 'function') options.onError(error);
    }
    function dispose() {
      if (disposed) return;
      savePosition(); disposed = true; showRevision++;
      cancellation.abort(); externalSignal?.removeEventListener('abort', dispose);
      if (frame) cancelAnimationFrame(frame);
      invalidateViewport(); viewportPending = false;
      observer?.disconnect(); signal?.removeEventListener('abort', dispose); packageReader?.close();
      sheetCache.clear(); preparedSheets.clear(); pendingSheets.clear(); nodes.clear(); root?.remove(); updateBackground();
    }
    function dimensions() {
      grid.style.width = `${ROW_HEADER_WIDTH + sheet.columns.position(sheet.columns.count) * zoom}px`;
      grid.style.height = `${COLUMN_HEADER_HEIGHT + sheet.rows.position(sheet.rows.count) * zoom}px`;
    }
    function element(key, className, role) {
      if (nodes.has(key)) return nodes.get(key);
      const node = document.createElement('div'); node.className = className; node.setAttribute('role', role);
      grid.append(node); nodes.set(key, node); return node;
    }
    function place(node, left, top, width, height) {
      Object.assign(node.style, { left: `${left}px`, top: `${top}px`, width: `${Math.max(0, width)}px`, height: `${Math.max(0, height)}px` });
    }
    function cellNode(row, column, merge) {
      const key = `cell:${row}:${column}`;
      const node = element(key, 'xlsx-cell', 'gridcell');
      const cell = sheet.cells.get(`${row}:${column}`);
      const style = cell?.style || workbook.styles[sheet.rowStyles.get(row) ?? sheet.columnStyles.get(column) ?? 0] || workbook.styles[0];
      if (!node.firstChild) {
        // Value formatting is cached on first visibility, not repeated for every cell in a large worksheet.
        // 数值格式只在单元格首次可见时计算并缓存，不为整张大表预先执行日期及数字格式化。
        if (cell && cell.text === undefined) cell.text = cell.numeric ? numberText(cell.value, style.format, workbook.date1904) : String(cell.value ?? '');
        const label = document.createElement('span'); label.className = 'xlsx-value'; label.textContent = cell?.text || ''; node.append(label);
        node.dataset.address = `${columnName(column)}${row + 1}`;
        node.setAttribute('aria-rowindex', String(row + 1)); node.setAttribute('aria-colindex', String(column + 1));
        if (cell?.formula !== null && cell?.formula !== undefined) { node.dataset.formula = cell.formula; node.title = '=' + cell.formula; }
      }
      const font = style.font || {}, label = node.firstChild;
      const horizontal = ({ centerContinuous: 'center', distributed: 'justify', fill: 'left' })[style.horizontal] || style.horizontal;
      Object.assign(node.style, { fontSize: `${(font.size || 11) * 4 / 3 * zoom}px`, fontWeight: font.bold ? '700' : '400',
        fontStyle: font.italic ? 'italic' : 'normal', color: font.color || '#262626', background: style.fill || '#ffffff',
        alignItems: style.vertical === 'top' ? 'flex-start' : style.vertical === 'center' ? 'center' : 'flex-end',
        textAlign: ['left', 'right', 'center', 'justify'].includes(horizontal) ? horizontal : cell?.numeric ? 'right' : cell?.type === 'b' ? 'center' : 'left',
        padding: `${zoom}px ${4 * zoom}px`, zIndex: merge ? '2' : '1' });
      if (font.name && /^[\p{L}\p{N} _.-]{1,80}$/u.test(font.name)) node.style.fontFamily = `"${font.name}",var(--ui-font,sans-serif)`;
      label.style.whiteSpace = style.wrap ? 'pre-wrap' : 'pre';
      label.style.textIndent = `${(style.indent || 0) * 8 * zoom}px`;
      label.style.textDecoration = [font.underline ? 'underline' : '', font.strike ? 'line-through' : ''].filter(Boolean).join(' ') || 'none';
      for (const side of ['Left', 'Right', 'Top', 'Bottom']) {
        const border = style.border?.[side.toLowerCase()];
        node.style['border' + side] = border || (side === 'Bottom' || side === 'Right' ? '1px solid #e4e3e0' : '0');
      }
      const lastRow = merge ? merge.r2 + 1 : row + 1, lastColumn = merge ? merge.c2 + 1 : column + 1;
      place(node, ROW_HEADER_WIDTH + sheet.columns.position(column) * zoom, COLUMN_HEADER_HEIGHT + sheet.rows.position(row) * zoom,
        (sheet.columns.position(lastColumn) - sheet.columns.position(column)) * zoom,
        (sheet.rows.position(lastRow) - sheet.rows.position(row)) * zoom);
    }
    function invalidateViewport() {
      viewportRevision++; viewport = null;
      if (paintFrame) cancelAnimationFrame(paintFrame);
      paintFrame = 0;
    }
    function finishViewport() {
      viewport = null; viewportPending = false; grid.setAttribute('aria-busy', 'false');
      completed(currentSheet, sheet); updateBackground(); updateNote();
    }
    function paintViewport(current) {
      if (disposed || viewport !== current || current.revision !== viewportRevision) return;
      const deadline = performance.now() + Math.min(navigator.scheduling?.isInputPending?.() ? 4 : 8, sliceMs);
      let steps = 0;
      // Complete the full visible range over short frames; a cell-count budget must never remove columns.
      // 单元格数量只限制每帧工作量，可见区域分帧完整补齐，不能用预算直接裁掉右侧列。
      while (steps++ < 512 && performance.now() < deadline) {
        if (current.phase === 'prune') {
          const next = current.previous.next();
          if (next.done) { current.phase = 'columns'; continue; }
          const [key, node] = next.value, parts = key.split(':');
          const keep = key === 'corner' || (parts[0] === 'column' ? current.columnSet.has(Number(parts[1]))
            : parts[0] === 'row' ? current.rowSet.has(Number(parts[1]))
              : current.mergeKeys.has(key) || current.rowSet.has(Number(parts[1])) && current.columnSet.has(Number(parts[2])));
          if (!keep) { node.remove(); nodes.delete(key); }
          continue;
        }
        if (current.phase === 'columns') {
          if (current.headerIndex === current.columns.length) { current.headerIndex = 0; current.phase = 'rows'; continue; }
          const column = current.columns[current.headerIndex++];
          const node = element('column:' + column, 'xlsx-header xlsx-column-header', 'columnheader'); node.textContent = columnName(column);
          place(node, ROW_HEADER_WIDTH + sheet.columns.position(column) * zoom, current.top, sheet.columns.size(column) * zoom, COLUMN_HEADER_HEIGHT);
          continue;
        }
        if (current.phase === 'rows') {
          if (current.headerIndex === current.rows.length) {
            place(element('corner', 'xlsx-header xlsx-corner', 'presentation'), current.left, current.top, ROW_HEADER_WIDTH, COLUMN_HEADER_HEIGHT);
            current.phase = 'cells'; continue;
          }
          const row = current.rows[current.headerIndex++];
          const node = element('row:' + row, 'xlsx-header xlsx-row-header', 'rowheader'); node.textContent = String(row + 1);
          place(node, current.left, COLUMN_HEADER_HEIGHT + sheet.rows.position(row) * zoom, ROW_HEADER_WIDTH, sheet.rows.size(row) * zoom);
          continue;
        }
        if (current.phase === 'cells') {
          if (current.rowIndex === current.rows.length) { current.phase = 'merges'; continue; }
          const row = current.rows[current.rowIndex];
          if (!current.rowReady) {
            // Decode each row before exposing its values, yielding if an unusually wide row needs more time.
            // 每行先解析再展示，超宽行可跨帧解码，不能将尚未解析的数据伪装成空白。
            if (!sheet.ensureRows([row], deadline)) break;
            current.rowRanges = current.merges.filter(range => range.r1 <= row && range.r2 >= row).sort((a, b) => a.c1 - b.c1);
            current.rangeIndex = 0; current.rowReady = true;
          }
          if (current.columnIndex === current.columns.length) {
            current.rowIndex++; current.columnIndex = 0; current.rowReady = false; continue;
          }
          const column = current.columns[current.columnIndex++];
          while (current.rangeIndex < current.rowRanges.length && current.rowRanges[current.rangeIndex].c2 < column) current.rangeIndex++;
          const range = current.rowRanges[current.rangeIndex];
          if (!range || range.c1 > column) cellNode(row, column, null);
          continue;
        }
        if (current.mergeIndex < current.merges.length) {
          const merge = current.merges[current.mergeIndex];
          if (!sheet.ensureRows([merge.r1], deadline)) break;
          cellNode(merge.r1, merge.c1, merge); current.mergeIndex++;
          continue;
        }
        finishViewport(); return;
      }
      paintFrame = requestAnimationFrame(() => {
        paintFrame = 0;
        try { paintViewport(current); }
        catch (error) { invalidateViewport(); finishViewport(); report(error); }
      });
    }
    function renderViewport() {
      if (disposed || !sheet) return;
      invalidateViewport();
      const width = scroll.clientWidth || 760, height = scroll.clientHeight || 440;
      const rows = sheet.rows.visible(scroll.scrollTop / zoom, (scroll.scrollTop + height - COLUMN_HEADER_HEIGHT) / zoom, MAX_VISIBLE_CELLS + 1);
      const columns = sheet.columns.visible(scroll.scrollLeft / zoom, (scroll.scrollLeft + width - ROW_HEADER_WIDTH) / zoom, MAX_VISIBLE_CELLS + 1);
      // Only an exceptionally dense viewport falls back explicitly; ordinary wide/zoomed sheets stay complete.
      // 仅当前视口超过十三万个格子时明确转入既有转换回退，常见大窗口及低缩放表格不再静默缺列。
      if (rows.length * columns.length > MAX_VISIBLE_CELLS) {
        finishViewport(); throw unsupported('XLSX visible area exceeds 131072 cells; Office page rendering is required');
      }
      const merges = rows.length && columns.length ? visibleMerges(sheet.merges, rows[0], rows[rows.length - 1], columns[0], columns[columns.length - 1]) : [];
      const current = { revision: viewportRevision, top: scroll.scrollTop, left: scroll.scrollLeft, rows, columns, merges,
        rowSet: new Set(rows), columnSet: new Set(columns), mergeKeys: new Set(merges.map(range => `cell:${range.r1}:${range.c1}`)),
        previous: nodes.entries(), phase: 'prune', headerIndex: 0, rowIndex: 0, columnIndex: 0, mergeIndex: 0,
        rowReady: false };
      viewport = current; viewportPending = true; grid.setAttribute('aria-busy', 'true'); updateBackground(); updateNote();
      paintViewport(current);
    }
    function schedule() {
      invalidateViewport();
      if (frame || disposed) return;
      frame = requestAnimationFrame(() => { frame = 0; try { renderViewport(); emit(); } catch (error) { report(error); } });
    }
    async function showSheet(index) {
      alive(); const revision = ++showRevision, selected = clamp(Math.trunc(index), 0, workbook.sheets.length - 1);
      requestedSheet = selected;
      if (!sheetCache.has(selected)) {
        pendingSheets.add(selected); updateBackground();
        const pending = readSheet(packageReader, workbook.sheets[selected], workbook, signal);
        sheetCache.set(selected, pending);
        pending.then(parsed => {
          if (disposed) return;
          preparedSheets.set(selected, parsed); completed(selected, parsed); prepareInBackground();
        }).catch(() => { pendingSheets.delete(selected); updateBackground(); });
      }
      const parsed = await sheetCache.get(selected); alive(); if (revision !== showRevision) return;
      savePosition(); currentSheet = selected; sheet = parsed;
      for (const button of tabButtons) button.setAttribute('aria-selected', String(Number(button.dataset.sheetIndex) === selected));
      grid.replaceChildren(); nodes.clear(); dimensions();
      grid.setAttribute('aria-rowcount', String(sheet.rows.count)); grid.setAttribute('aria-colcount', String(sheet.columns.count));
      const saved = positions[selected] || { x: 0, y: 0 }; scroll.scrollLeft = saved.x; scroll.scrollTop = saved.y;
      updateNote(); renderViewport(); emit(); prepareInBackground();
    }
    externalSignal?.addEventListener('abort', dispose, { once: true });
    try {
      if (externalSignal?.aborted) dispose();
      updateBackground(); alive(); packageReader = await openPackage(bytes, signal);
      workbook = await readWorkbook(packageReader, signal, Number(initial.sheet) || 0); alive();
      installStyles(document); root = document.createElement('div'); root.className = 'xlsx-view'; root.setAttribute('aria-label', options.title || 'Excel 表格预览');
      const toolbar = document.createElement('div'); toolbar.className = 'xlsx-toolbar';
      const title = document.createElement('span'); title.className = 'xlsx-title'; title.textContent = options.title || 'Excel'; toolbar.append(title);
      note = document.createElement('span'); note.className = 'xlsx-notice'; toolbar.append(note);
      const zoomSelect = document.createElement('select'); zoomSelect.dataset.action = 'zoom'; zoomSelect.setAttribute('aria-label', '表格缩放');
      const levels = [...new Set([.5, .75, 1, 1.25, 1.5, 2, zoom])].sort((a, b) => a - b);
      for (const level of levels) { const option = document.createElement('option'); option.value = String(level); option.textContent = `${Math.round(level * 100)}%`; zoomSelect.append(option); }
      zoomSelect.value = String(zoom);
      zoomSelect.addEventListener('change', () => {
        savePosition(); const next = Number(zoomSelect.value), ratio = next / zoom;
        for (const saved of Object.values(positions)) { saved.x *= ratio; saved.y *= ratio; }
        zoom = next; dimensions(); const saved = positions[currentSheet];
        if (saved) { scroll.scrollLeft = saved.x; scroll.scrollTop = saved.y; }
        renderViewport(); emit();
      });
      toolbar.append(zoomSelect); root.append(toolbar);
      scroll = document.createElement('div'); scroll.className = 'xlsx-scroll'; scroll.tabIndex = 0; scroll.setAttribute('aria-label', '表格滚动区域');
      grid = document.createElement('div'); grid.className = 'xlsx-grid'; grid.setAttribute('role', 'grid'); grid.setAttribute('aria-readonly', 'true');
      scroll.append(grid); scroll.addEventListener('scroll', schedule, { passive: true }); root.append(scroll);
      const tabs = document.createElement('div'); tabs.className = 'xlsx-tabs'; tabs.setAttribute('role', 'tablist'); tabs.setAttribute('aria-label', '工作表');
      workbook.sheets.forEach((specification, index) => {
        const button = document.createElement('button'); button.type = 'button'; button.dataset.action = 'sheet'; button.dataset.sheetIndex = String(index);
        button.textContent = specification.name; button.title = specification.name; button.setAttribute('role', 'tab'); button.setAttribute('aria-selected', 'false');
        button.addEventListener('click', () => { showSheet(index).catch(report); }); tabs.append(button); tabButtons.push(button);
      });
      root.append(tabs); container.append(root); signal?.addEventListener('abort', dispose, { once: true });
      observer = new ResizeObserver(schedule); observer.observe(scroll);
      await showSheet(Number(initial.sheet) || 0); alive();
      initializing = false; updateBackground();
      return { dispose, getState, get settled() { return idle; } };
    } catch (error) {
      dispose();
      if (error?.name === 'AbortError' || error?.code) throw error;
      throw failure(error?.message || 'XLSX preview failed');
    }
  }

  window.kynxaXlsx = Object.freeze({ render });
})();
