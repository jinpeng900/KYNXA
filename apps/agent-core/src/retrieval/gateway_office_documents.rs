//! Offline structured document decoders share bounded packages and stable source identities.
//! 离线结构化文档解码共用有界容器与稳定来源身份，不执行宏、脚本或外部资源。

use crate::storage::{ContractError, gateway};
use quick_xml::{Reader, events::Event};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::{Cursor, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) fn failure(code: &'static str) -> ContractError {
    gateway::error("文档解析未完成，原文件已保留。", code, 422)
}

pub(crate) fn cancelled(cancel: &Arc<AtomicBool>) -> Result<(), ContractError> {
    if cancel.load(Ordering::Acquire) {
        Err(failure("ABORT_ERR"))
    } else {
        crate::storage::sqlite_store::cancelled()
    }
}

/// Text offsets use UTF-16, matching source references rather than UTF-8 byte positions.
/// 正文位置使用 UTF-16，与来源引用一致，不将 UTF-8 字节数当成字符偏移。
pub(crate) struct DocumentText {
    pub(crate) text: String,
    pub(crate) parts: Vec<Value>,
    maximum_bytes: usize,
    length_utf16: usize,
}

impl DocumentText {
    pub(crate) fn new(limits: &Value) -> Self {
        Self {
            text: String::new(),
            parts: Vec::new(),
            maximum_bytes: limits["maximumDocumentOutputBytes"]
                .as_u64()
                .unwrap_or(2 * 1024 * 1024)
                .clamp(1, 64 * 1024 * 1024) as usize,
            length_utf16: 0,
        }
    }

    pub(crate) fn append(&mut self, text: &str) -> Result<(), ContractError> {
        if self.text.len().saturating_add(text.len()) > self.maximum_bytes {
            return Err(failure("DOCUMENT_OUTPUT_LIMIT"));
        }
        self.length_utf16 += text.encode_utf16().count();
        self.text.push_str(text);
        Ok(())
    }

    pub(crate) fn start_part(&mut self, kind: &str, name: &str) -> Result<usize, ContractError> {
        if self.parts.len() >= 4096 || kind.len() > 512 || name.encode_utf16().count() > 2048 {
            return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
        }
        let start = self.length_utf16;
        self.append(&format!("\n[{kind}: {name}]\n"))?;
        Ok(start)
    }

    pub(crate) fn end_part(&mut self, kind: &str, name: &str, start: usize) {
        self.parts.push(json!({"kind":kind,"name":name,
            "startOffset":start,"endOffset":self.length_utf16}));
    }

    pub(crate) fn finish(
        self,
        format: &str,
        version: &str,
        digest: &str,
        omitted: &[&str],
    ) -> Result<Value, ContractError> {
        if self.text.trim().is_empty() {
            return Err(failure("DOCUMENT_TEXT_UNAVAILABLE"));
        }
        Ok(
            json!({"text":self.text,"extraction":{"format":format,"version":version,
            "rawContentHash":digest,"parts":self.parts,
            "capabilities":["text","structure"],"omittedContent":omitted},"documentComplete":true}),
        )
    }
}

/// All ZIP members are checked before a parser allocates from their declared sizes.
/// 在解析器按声明大小分配内存前核验所有 ZIP 成员；不解压到磁盘或跟随容器路径。
pub(crate) struct DocumentPackage<'a> {
    archive: zip::ZipArchive<Cursor<&'a [u8]>>,
    pub(crate) names: Vec<String>,
    maximum_entry_bytes: u64,
}

impl<'a> DocumentPackage<'a> {
    pub(crate) fn open(
        bytes: &'a [u8],
        limits: &Value,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Self, ContractError> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
        let maximum_entries = limits["maximumArchiveEntries"]
            .as_u64()
            .unwrap_or(8192)
            .clamp(1, 65536) as usize;
        let maximum_expanded = limits["maximumArchiveExpandedBytes"]
            .as_u64()
            .unwrap_or(256 * 1024 * 1024)
            .clamp(1, 1024 * 1024 * 1024);
        let maximum_entry_bytes = limits["maximumArchiveEntryBytes"]
            .as_u64()
            .unwrap_or(64 * 1024 * 1024)
            .clamp(1, maximum_expanded);
        if archive.len() > maximum_entries {
            return Err(failure("DOCUMENT_ARCHIVE_LIMIT"));
        }
        let mut expanded = 0u64;
        let mut names = Vec::new();
        let mut unique = HashSet::new();
        for index in 0..archive.len() {
            cancelled(cancel)?;
            let entry = archive
                .by_index(index)
                .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
            expanded = expanded
                .checked_add(entry.size())
                .ok_or_else(|| failure("DOCUMENT_ARCHIVE_LIMIT"))?;
            if entry.enclosed_name().is_none()
                || entry.name().contains('\\')
                || entry.size() > maximum_entry_bytes
                || expanded > maximum_expanded
                || entry
                    .unix_mode()
                    .is_some_and(|mode| mode & 0o170000 == 0o120000)
                || !unique.insert(entry.name().to_string())
            {
                return Err(failure("DOCUMENT_ARCHIVE_LIMIT"));
            }
            names.push(entry.name().to_string());
        }
        Ok(Self {
            archive,
            names,
            maximum_entry_bytes,
        })
    }

    pub(crate) fn read(
        &mut self,
        name: &str,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Vec<u8>, ContractError> {
        cancelled(cancel)?;
        let mut entry = self
            .archive
            .by_name(name)
            .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
        let mut bytes = Vec::with_capacity((entry.size() as usize).min(65536));
        let mut buffer = [0u8; 65536];
        loop {
            cancelled(cancel)?;
            let count = entry
                .read(&mut buffer)
                .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
            if count == 0 {
                break;
            }
            if bytes.len().saturating_add(count) as u64 > self.maximum_entry_bytes {
                return Err(failure("DOCUMENT_ARCHIVE_LIMIT"));
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        Ok(bytes)
    }
}

pub(crate) fn attribute(
    event: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
) -> Result<Option<String>, ContractError> {
    for field in event.attributes() {
        let field = field.map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
        if field.key.local_name().as_ref() == name {
            let value =
                std::str::from_utf8(&field.value).map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
            return Ok(Some(
                quick_xml::escape::unescape(value)
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?
                    .into_owned(),
            ));
        }
    }
    Ok(None)
}

/// Namespace-independent XML extraction still rejects DTDs and keeps tables and paragraph breaks.
/// XML 不依赖前缀命名，但拒绝 DTD；保留表格列、段落和实体文字，不解析外部实体。
fn xml_text(
    bytes: &[u8],
    style: XmlTextStyle,
    output: &mut DocumentText,
    cancel: &Arc<AtomicBool>,
) -> Result<(), ContractError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Vec<u8>> = Vec::new();
    loop {
        cancelled(cancel)?;
        match reader
            .read_event()
            .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?
        {
            Event::Start(event) => {
                if stack.len() >= 256 {
                    return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
                }
                let name = event.local_name().as_ref().to_vec();
                if name == b"table" {
                    if let Some(name) = attribute(&event, b"name")? {
                        output.append(&format!("\n[Table: {name}]\n"))?;
                    }
                }
                if [b"table-cell".as_slice(), b"table-row".as_slice()].contains(&name.as_slice()) {
                    if let Some(repeats) = attribute(&event, b"number-columns-repeated")?
                        .or(attribute(&event, b"number-rows-repeated")?)
                    {
                        let repeats: u64 = repeats
                            .parse()
                            .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
                        if repeats > 100000 {
                            return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
                        }
                        if repeats > 1 {
                            output.append(&format!("[repeated {repeats} times] "))?;
                        }
                    }
                }
                if name == b"table-cell" {
                    open_document_cell_fields(&event, output)?;
                }
                stack.push(name);
            }
            Event::Text(event) => {
                if style.capture(&stack) {
                    let text = event
                        .decode()
                        .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
                    output.append(
                        &quick_xml::escape::unescape(&text)
                            .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?,
                    )?;
                }
            }
            Event::CData(event) if style.capture(&stack) => {
                output.append(
                    &event
                        .decode()
                        .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?,
                )?;
            }
            Event::GeneralRef(event) if style.capture(&stack) => {
                let name = event
                    .decode()
                    .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
                output.append(
                    &quick_xml::escape::unescape(&format!("&{name};"))
                        .map_err(|_| failure("DOCUMENT_XML_UNSUPPORTED"))?,
                )?;
            }
            Event::Empty(event) => match event.local_name().as_ref() {
                b"table-cell" => {
                    open_document_cell_fields(&event, output)?;
                    output.append("\t")?;
                }
                b"br" | b"line-break" | b"cr" => output.append("\n")?,
                b"tab" => output.append("\t")?,
                b"s" => {
                    let count = attribute(&event, b"c")?
                        .and_then(|count| count.parse::<usize>().ok())
                        .unwrap_or(1);
                    if count > 65536 {
                        return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
                    }
                    output.append(&" ".repeat(count))?;
                }
                _ => {}
            },
            Event::End(event) => {
                let name = event.local_name().as_ref().to_vec();
                if stack.pop().as_deref() != Some(name.as_slice()) {
                    return Err(failure("DOCUMENT_PARSE_FAILED"));
                }
                match name.as_slice() {
                    b"p" | b"h" | b"tr" | b"table-row" | b"section" | b"title" => {
                        output.append("\n")?
                    }
                    b"tc" | b"table-cell" => output.append("\t")?,
                    _ if matches!(style, XmlTextStyle::All) => output.append("\n")?,
                    _ => {}
                }
            }
            Event::DocType(_) => return Err(failure("DOCUMENT_XML_UNSUPPORTED")),
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    Ok(())
}

/// Store cached numeric/date/currency values even when an exporter omits rendered paragraph text.
/// 即使导出器未写显示段落，也保留缓存数值、日期和货币单位；公式作为原文而非计算指令。
fn open_document_cell_fields(
    event: &quick_xml::events::BytesStart<'_>,
    output: &mut DocumentText,
) -> Result<(), ContractError> {
    if let Some(formula) = attribute(event, b"formula")? {
        output.append(&format!("[formula={formula}] "))?;
    }
    for field in [
        b"value".as_slice(),
        b"date-value",
        b"time-value",
        b"boolean-value",
        b"string-value",
    ] {
        if let Some(value) = attribute(event, field)? {
            output.append(&format!(
                "[stored-{}={value}] ",
                String::from_utf8_lossy(field)
            ))?;
        }
    }
    if let Some(currency) = attribute(event, b"currency")? {
        output.append(&format!("[currency={currency}] "))?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum XmlTextStyle {
    Drawing,
    OpenDocument,
    All,
}
impl XmlTextStyle {
    fn capture(self, stack: &[Vec<u8>]) -> bool {
        if stack
            .iter()
            .any(|name| [b"script".as_slice(), b"style", b"binary"].contains(&name.as_slice()))
        {
            return false;
        }
        match self {
            Self::Drawing => stack.last().is_some_and(|name| name == b"t"),
            Self::OpenDocument => stack
                .iter()
                .any(|name| [b"p".as_slice(), b"h"].contains(&name.as_slice())),
            Self::All => true,
        }
    }
}

fn relationships(
    xml: &[u8],
    cancel: &Arc<AtomicBool>,
) -> Result<HashMap<String, String>, ContractError> {
    let mut reader = Reader::from_reader(xml);
    let mut result = HashMap::new();
    loop {
        cancelled(cancel)?;
        match reader
            .read_event()
            .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?
        {
            Event::Empty(event) | Event::Start(event)
                if event.local_name().as_ref() == b"Relationship" =>
            {
                if attribute(&event, b"TargetMode")?.as_deref() == Some("External") {
                    continue;
                }
                if let (Some(id), Some(target)) =
                    (attribute(&event, b"Id")?, attribute(&event, b"Target")?)
                {
                    if result.insert(id, target).is_some() {
                        return Err(failure("DOCUMENT_PARSE_FAILED"));
                    }
                }
            }
            Event::DocType(_) => return Err(failure("DOCUMENT_XML_UNSUPPORTED")),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(result)
}

/// Relative package relationships may move within the package but never escape its root.
/// 相对关系可在容器内部移动，不能逃出根目录、引用盘符或变成远程 URL。
fn package_target(base: &str, target: &str) -> Result<String, ContractError> {
    if target.contains('\\')
        || target.contains(':')
        || target.contains('\0')
        || target.contains('%')
        || target.contains('#')
        || target.contains('?')
    {
        return Err(failure("DOCUMENT_RELATIONSHIP_UNSUPPORTED"));
    }
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        base.rsplit_once('/')
            .map(|(folder, _)| folder.split('/').collect())
            .unwrap_or_default()
    };
    for part in target.trim_start_matches('/').split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(failure("DOCUMENT_RELATIONSHIP_UNSUPPORTED"));
                }
            }
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err(failure("DOCUMENT_RELATIONSHIP_UNSUPPORTED"));
    }
    Ok(parts.join("/"))
}

fn presentation(
    format: &str,
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    digest: &str,
) -> Result<Value, ContractError> {
    let mut package = DocumentPackage::open(bytes, limits, cancel)?;
    let xml = package.read("ppt/presentation.xml", cancel)?;
    let rels = relationships(
        &package.read("ppt/_rels/presentation.xml.rels", cancel)?,
        cancel,
    )?;
    let mut slides = Vec::new();
    let mut reader = Reader::from_reader(xml.as_slice());
    loop {
        cancelled(cancel)?;
        match reader
            .read_event()
            .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?
        {
            Event::Start(event) | Event::Empty(event)
                if event.local_name().as_ref() == b"sldId" =>
            {
                let id = event
                    .attributes()
                    .filter_map(Result::ok)
                    .find(|attr| attr.key.as_ref().ends_with(b":id"))
                    .map(|attr| String::from_utf8_lossy(&attr.value).into_owned())
                    .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
                let target = rels
                    .get(&id)
                    .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
                slides.push(package_target("ppt/presentation.xml", target)?);
            }
            Event::DocType(_) => return Err(failure("DOCUMENT_XML_UNSUPPORTED")),
            Event::Eof => break,
            _ => {}
        }
    }
    if slides.is_empty() {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    let mut output = DocumentText::new(limits);
    for (index, slide) in slides.into_iter().enumerate() {
        cancelled(cancel)?;
        let name = (index + 1).to_string();
        let start = output.start_part("slide", &name)?;
        xml_text(
            &package.read(&slide, cancel)?,
            XmlTextStyle::Drawing,
            &mut output,
            cancel,
        )?;
        if let Some((folder, file)) = slide.rsplit_once('/') {
            let relationship_file = format!("{folder}/_rels/{file}.rels");
            if package.names.contains(&relationship_file) {
                let relations = package.read(&relationship_file, cancel)?;
                let note_targets = relationships(&relations, cancel)?;
                let mut targets: Vec<_> = note_targets
                    .values()
                    .filter(|target| target.contains("notesSlides/"))
                    .collect();
                targets.sort();
                for target in targets {
                    output.append("\n[Speaker notes]\n")?;
                    xml_text(
                        &package.read(&package_target(&slide, target)?, cancel)?,
                        XmlTextStyle::Drawing,
                        &mut output,
                        cancel,
                    )?;
                }
            }
        }
        output.end_part("slide", &name, start);
    }
    output.finish(
        format,
        "rust-office-text-v1",
        digest,
        &[
            "embedded-images",
            "charts-as-images",
            "macros",
            "external-links",
        ],
    )
}

pub(crate) fn open_document(
    format: &str,
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    digest: &str,
) -> Result<Value, ContractError> {
    let xml = if format.starts_with("fod") {
        bytes.to_vec()
    } else {
        let mut package = DocumentPackage::open(bytes, limits, cancel)?;
        if package.names.contains(&"META-INF/manifest.xml".into()) {
            let manifest = package.read("META-INF/manifest.xml", cancel)?;
            if manifest
                .windows(b"encryption-data".len())
                .any(|part| part == b"encryption-data")
            {
                return Err(failure("DOCUMENT_ENCRYPTED"));
            }
        }
        package.read("content.xml", cancel)?
    };
    let mut output = DocumentText::new(limits);
    xml_text(&xml, XmlTextStyle::OpenDocument, &mut output, cancel)?;
    output.finish(
        format,
        "rust-office-text-v1",
        digest,
        &["embedded-images", "scripts", "external-links"],
    )
}

/// EPUB chapter order is taken from the spine, never guessed from ZIP member ordering.
/// EPUB 正文按 spine 阅读顺序组织，不用 ZIP 文件名顺序猜测章节。
fn epub(
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    digest: &str,
) -> Result<Value, ContractError> {
    let mut package = DocumentPackage::open(bytes, limits, cancel)?;
    if package.names.contains(&"META-INF/encryption.xml".into()) {
        return Err(failure("DOCUMENT_ENCRYPTED"));
    }
    let container = package.read("META-INF/container.xml", cancel)?;
    let mut reader = Reader::from_reader(container.as_slice());
    let mut root = None;
    loop {
        cancelled(cancel)?;
        match reader
            .read_event()
            .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?
        {
            Event::Start(event) | Event::Empty(event)
                if event.local_name().as_ref() == b"rootfile" =>
            {
                root = attribute(&event, b"full-path")?;
                if root.is_some() {
                    break;
                }
            }
            Event::DocType(_) => return Err(failure("DOCUMENT_XML_UNSUPPORTED")),
            Event::Eof => break,
            _ => {}
        }
    }
    let root = root.ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
    let document = package.read(&package_target("", &root)?, cancel)?;
    let mut reader = Reader::from_reader(document.as_slice());
    let mut items = HashMap::new();
    let mut order = Vec::new();
    loop {
        cancelled(cancel)?;
        match reader
            .read_event()
            .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?
        {
            Event::Start(event) | Event::Empty(event) => match event.local_name().as_ref() {
                b"item" => {
                    if let (Some(id), Some(href), Some(media)) = (
                        attribute(&event, b"id")?,
                        attribute(&event, b"href")?,
                        attribute(&event, b"media-type")?,
                    ) {
                        items.insert(id, (href, media));
                    }
                }
                b"itemref" => {
                    if let Some(id) = attribute(&event, b"idref")? {
                        order.push(id);
                    }
                }
                _ => {}
            },
            Event::DocType(_) => return Err(failure("DOCUMENT_XML_UNSUPPORTED")),
            Event::Eof => break,
            _ => {}
        }
    }
    if order.is_empty() {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    let mut output = DocumentText::new(limits);
    for id in order {
        cancelled(cancel)?;
        let (href, media) = items
            .get(&id)
            .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
        if !["application/xhtml+xml", "text/html"].contains(&media.as_str()) {
            return Err(failure("DOCUMENT_CONTENT_UNSUPPORTED"));
        }
        let path = package_target(&root, href)?;
        let start = output.start_part("chapter", &id)?;
        html_text(&package.read(&path, cancel)?, &mut output, cancel)?;
        output.end_part("chapter", &id, start);
    }
    output.finish(
        "epub",
        "rust-office-text-v1",
        digest,
        &[
            "embedded-images",
            "scripts",
            "audio-video",
            "external-links",
        ],
    )
}

/// Static HTML never runs scripts, loads images or follows network references.
/// 静态 HTML 只读正文，不运行脚本、加载图片或跟随网络引用。
pub(crate) fn html_text(
    bytes: &[u8],
    output: &mut DocumentText,
    cancel: &Arc<AtomicBool>,
) -> Result<(), ContractError> {
    html_text_with_content_type(bytes, output, cancel, None)
}

fn html_text_with_content_type(
    bytes: &[u8],
    output: &mut DocumentText,
    cancel: &Arc<AtomicBool>,
    content_type: Option<&str>,
) -> Result<(), ContractError> {
    cancelled(cancel)?;
    let declaration = String::from_utf8_lossy(&bytes[..bytes.len().min(8192)]);
    static HEADER_CHARSET: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static DOCUMENT_CHARSET: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let header_charset = HEADER_CHARSET.get_or_init(|| {
        regex::Regex::new(r#"(?i)(?:^|;)\s*charset\s*=\s*["']?\s*([a-z0-9_-]+)"#)
            .expect("constant MIME charset expression")
    });
    let document_charset = DOCUMENT_CHARSET.get_or_init(|| {
        regex::Regex::new(
            r#"(?is)(?:<meta\s[^>]*?charset|<\?xml\s[^>]*?encoding)\s*=\s*["']?\s*([a-z0-9_-]+)"#,
        )
        .expect("constant document charset expression")
    });
    let charset = content_type
        .and_then(|header| header_charset.captures(header))
        .or_else(|| document_charset.captures(&declaration))
        .and_then(|capture| capture.get(1).map(|label| label.as_str().to_string()));
    let encoding = if let Some((encoding, _)) = encoding_rs::Encoding::for_bom(bytes) {
        encoding
    } else if let Some(charset) = charset.as_deref() {
        encoding_rs::Encoding::for_label(charset.as_bytes())
            .ok_or_else(|| failure("DOCUMENT_ENCODING_UNSUPPORTED"))?
    } else {
        encoding_rs::UTF_8
    };
    let (text, _, errors) = encoding.decode(bytes);
    if errors {
        return Err(failure("DOCUMENT_ENCODING_UNSUPPORTED"));
    }
    let document = scraper::Html::parse_document(&text);
    for node in document.tree.nodes() {
        cancelled(cancel)?;
        if let scraper::node::Node::Text(text) = node.value() {
            if node.ancestors().any(|ancestor| {
                ancestor.value().as_element().is_some_and(|element| {
                    ["script", "style", "noscript", "template"].contains(&element.name())
                })
            }) {
                continue;
            }
            if !text.trim().is_empty() {
                output.append(text)?;
                output.append("\n")?;
            }
        }
    }
    Ok(())
}

/// Stream reviewed RTF tokens without allocating an unchecked document or losing the declared code page.
/// 逐个处理已审阅词法器的 RTF token，避免 unchecked 文档分配，并保留声明的文字编码。
fn rtf(
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    digest: &str,
) -> Result<Value, ContractError> {
    use rtf_grimoire::tokenizer::{Token, read_token};
    let mut remaining = bytes;
    if !remaining.starts_with(b"{\\rtf") {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    let mut output = DocumentText::new(limits);
    let mut stack = Vec::new();
    let mut ignored = false;
    let mut fallback_count = 1usize;
    let mut encoding: &'static encoding_rs::Encoding = encoding_rs::WINDOWS_1252;
    let mut skip = 0usize;
    let mut unicode = Vec::new();
    let mut encoded_bytes = Vec::new();
    while !remaining.is_empty() {
        cancelled(cancel)?;
        let (tail, token) = read_token(remaining).map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
        if tail.len() >= remaining.len() {
            return Err(failure("DOCUMENT_PARSE_FAILED"));
        }
        remaining = tail;
        match token {
            Token::StartGroup => {
                if stack.len() >= 256 {
                    return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
                }
                stack.push((ignored, fallback_count, encoding));
            }
            Token::EndGroup => {
                flush_encoded(&mut encoded_bytes, encoding, &mut output)?;
                (ignored, fallback_count, encoding) = stack
                    .pop()
                    .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
            }
            Token::ControlSymbol('*') => ignored = true,
            Token::ControlSymbol(symbol) if !ignored => {
                flush_unicode(&mut unicode, &mut output)?;
                flush_encoded(&mut encoded_bytes, encoding, &mut output)?;
                match symbol {
                    '~' => output.append("\u{a0}")?,
                    '-' => {}
                    '_' => output.append("‑")?,
                    '\\' | '{' | '}' => output.append(&symbol.to_string())?,
                    _ => {}
                }
            }
            Token::ControlWord { name, arg } => match name.as_str() {
                "fonttbl" | "colortbl" | "filetbl" | "stylesheet" | "pict" | "object" | "info"
                | "fldinst" | "datastore" => ignored = true,
                "ansicpg" => {
                    flush_encoded(&mut encoded_bytes, encoding, &mut output)?;
                    encoding = match arg {
                        Some(1252) => encoding_rs::WINDOWS_1252,
                        Some(936) => encoding_rs::GBK,
                        Some(950) => encoding_rs::BIG5,
                        Some(932) => encoding_rs::SHIFT_JIS,
                        Some(949) => encoding_rs::EUC_KR,
                        Some(65001) => encoding_rs::UTF_8,
                        Some(1250) => encoding_rs::WINDOWS_1250,
                        Some(1251) => encoding_rs::WINDOWS_1251,
                        _ => return Err(failure("DOCUMENT_ENCODING_UNSUPPORTED")),
                    };
                }
                "uc" => {
                    fallback_count = arg
                        .unwrap_or(1)
                        .try_into()
                        .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?;
                    if fallback_count > 16 {
                        return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
                    }
                }
                "u" if !ignored => {
                    flush_encoded(&mut encoded_bytes, encoding, &mut output)?;
                    let value = arg.ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
                    if !(-32768..=65535).contains(&value) {
                        return Err(failure("DOCUMENT_PARSE_FAILED"));
                    }
                    unicode.push(if value < 0 {
                        (value + 65536) as u16
                    } else {
                        value as u16
                    });
                    skip = fallback_count;
                }
                "'" if !ignored => {
                    if skip > 0 {
                        skip -= 1;
                    } else {
                        flush_unicode(&mut unicode, &mut output)?;
                        encoded_bytes.push(
                            arg.ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?
                                .try_into()
                                .map_err(|_| failure("DOCUMENT_PARSE_FAILED"))?,
                        );
                    }
                }
                "par" | "line" | "row" | "tab" | "cell" if !ignored => {
                    flush_unicode(&mut unicode, &mut output)?;
                    flush_encoded(&mut encoded_bytes, encoding, &mut output)?;
                    output.append(if matches!(name.as_str(), "tab" | "cell") {
                        "\t"
                    } else {
                        "\n"
                    })?;
                }
                "emdash" | "endash" | "bullet" | "lquote" | "rquote" | "ldblquote"
                | "rdblquote"
                    if !ignored =>
                {
                    flush_unicode(&mut unicode, &mut output)?;
                    flush_encoded(&mut encoded_bytes, encoding, &mut output)?;
                    output.append(match name.as_str() {
                        "emdash" => "—",
                        "endash" => "–",
                        "bullet" => "•",
                        "lquote" | "rquote" => "’",
                        _ => "\"",
                    })?;
                }
                _ => {}
            },
            Token::Text(text) if !ignored => {
                let skipped = skip.min(text.len());
                skip -= skipped;
                if text.len() > skipped {
                    flush_unicode(&mut unicode, &mut output)?;
                    encoded_bytes.extend_from_slice(&text[skipped..]);
                }
            }
            Token::ControlBin(_) if !ignored => {
                return Err(failure("DOCUMENT_CONTENT_UNSUPPORTED"));
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(failure("DOCUMENT_PARSE_FAILED"));
    }
    flush_unicode(&mut unicode, &mut output)?;
    flush_encoded(&mut encoded_bytes, encoding, &mut output)?;
    output.finish(
        "rtf",
        "rust-office-text-v1",
        digest,
        &["embedded-images", "objects"],
    )
}

fn flush_encoded(
    bytes: &mut Vec<u8>,
    encoding: &'static encoding_rs::Encoding,
    output: &mut DocumentText,
) -> Result<(), ContractError> {
    if !bytes.is_empty() {
        let (text, _, errors) = encoding.decode(bytes);
        if errors {
            return Err(failure("DOCUMENT_ENCODING_UNSUPPORTED"));
        }
        output.append(&text)?;
        bytes.clear();
    }
    Ok(())
}

fn flush_unicode(unicode: &mut Vec<u16>, output: &mut DocumentText) -> Result<(), ContractError> {
    if !unicode.is_empty() {
        let text =
            String::from_utf16(unicode).map_err(|_| failure("DOCUMENT_ENCODING_UNSUPPORTED"))?;
        output.append(&text)?;
        unicode.clear();
    }
    Ok(())
}

/// Parsers return complete text or a typed error; a limited prefix is never silently indexed.
/// 解析返回完成正文或类型化错误，不把额度截断的前缀静默发布到索引。
pub(crate) fn decode(
    format: &str,
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    digest: &str,
) -> Result<Value, ContractError> {
    cancelled(cancel)?;
    match format {
        "pptx" | "pptm" | "ppsx" | "ppsm" | "potx" | "potm" => {
            presentation(format, bytes, limits, cancel, digest)
        }
        "xlsx" | "xlsm" | "xlam" | "xltx" | "xltm" | "xls" | "xlsb" | "ods" => {
            super::gateway_spreadsheets::decode(format, bytes, limits, cancel, digest)
        }
        "odt" | "ott" | "odp" | "otp" | "fodt" | "fodp" | "fods" => {
            open_document(format, bytes, limits, cancel, digest)
        }
        "epub" => epub(bytes, limits, cancel, digest),
        "rtf" => rtf(bytes, limits, cancel, digest),
        "html" | "htm" | "xhtml" => {
            let mut output = DocumentText::new(limits);
            html_text_with_content_type(
                bytes,
                &mut output,
                cancel,
                limits["documentContentType"].as_str(),
            )?;
            output.finish(
                format,
                "rust-office-text-v1",
                digest,
                &["scripts", "embedded-images", "external-links"],
            )
        }
        "eml" | "mhtml" | "mht" | "mbox" => {
            super::gateway_document_mail::decode(format, bytes, limits, cancel, digest)
        }
        "fb2" | "svg" | "xml" => {
            let mut output = DocumentText::new(limits);
            xml_text(bytes, XmlTextStyle::All, &mut output, cancel)?;
            output.finish(
                format,
                "rust-office-text-v1",
                digest,
                &["embedded-images", "scripts", "external-links"],
            )
        }
        _ => Err(failure("DOCUMENT_FORMAT_UNSUPPORTED")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/extended-documents")
                .join(name),
        )
        .unwrap()
    }

    fn extract(format: &str, bytes: &[u8], limits: Value) -> Result<Value, ContractError> {
        decode(
            format,
            bytes,
            &limits,
            &Arc::new(AtomicBool::new(false)),
            &super::super::gateway_index::hash_bytes(bytes),
        )
    }

    #[test]
    fn office_mail_books_and_markup_keep_bilingual_text_and_durable_identity() {
        for (name, format, expected) in [
            ("guide.pptx", "pptx", "第一张 First 25 kg & balance"),
            ("guide.xlsx", "xlsx", "A2\tvalue=25.5"),
            ("guide.xlsb", "xlsb", "质量 kg"),
            ("guide.odt", "odt", "账户密码 reset password"),
            ("guide.odp", "odp", "质量 kg"),
            ("guide.ods", "ods", "25.5"),
            ("guide.fodt", "fodt", "账户密码 reset password"),
            ("guide.fodp", "fodp", "质量 kg"),
            ("guide.fods", "fods", "25.5"),
            ("guide.epub", "epub", "第一章 25 kg"),
            ("guide.rtf", "rtf", "账户"),
            ("guide-gbk.rtf", "rtf", "账户"),
            ("guide.html", "html", "质量 kg"),
            ("guide-gbk.html", "html", "账户密码"),
            ("guide.eml", "eml", "账户密码 reset password"),
            ("guide.mhtml", "mhtml", "账户 reset 25 kg"),
            ("guide.mbox", "mbox", "账户确认"),
            ("guide.fb2", "fb2", "Reset password 25 kg"),
            ("guide.svg", "svg", "账户 reset 25 kg"),
        ] {
            let bytes = fixture(name);
            let result = extract(format, &bytes, json!({}))
                .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            let text = result["text"].as_str().unwrap();
            assert!(text.contains(expected), "{name}: {text}");
            assert!(
                !text.contains("do_not_index_script")
                    && !text.contains("excluded_script")
                    && !text.contains("secret_script")
                    && !text.contains("secret_css")
                    && !text.contains("excluded_binary")
            );
            assert_eq!(result["extraction"]["format"], format);
            assert_eq!(
                result["extraction"]["rawContentHash"],
                super::super::gateway_index::hash_bytes(&bytes)
            );
            assert_eq!(result["extraction"]["version"], "rust-office-text-v1");
            crate::storage::retrieval::apply(
                "storage.retrieval.extraction",
                &json!({
                "value":result["extraction"],"textLength":text.encode_utf16().count()}),
            )
            .unwrap();
            assert_eq!(
                result,
                extract(format, &bytes, json!({})).unwrap(),
                "{name} must have stable ordering"
            );
        }
    }

    #[test]
    fn presentation_spine_sheet_coordinates_formulas_and_mbox_use_real_order() {
        let slide = extract("pptx", &fixture("guide.pptx"), json!({})).unwrap();
        let text = slide["text"].as_str().unwrap();
        assert!(text.find("第一张").unwrap() < text.find("Second").unwrap());
        assert!(text.contains("Speaker notes"));
        assert_eq!(slide["extraction"]["parts"].as_array().unwrap().len(), 2);
        let book = extract("epub", &fixture("guide.epub"), json!({})).unwrap();
        let text = book["text"].as_str().unwrap();
        assert!(text.find("First chapter").unwrap() < text.find("Second chapter").unwrap());
        let sheet = extract("xlsx", &fixture("guide.xlsx"), json!({})).unwrap();
        assert!(
            sheet["text"]
                .as_str()
                .unwrap()
                .contains("C2\tformula=A2*B2")
        );
        let mailbox = extract("mbox", &fixture("guide.mbox"), json!({})).unwrap();
        assert_eq!(
            mailbox["text"]
                .as_str()
                .unwrap()
                .matches("Subject: 账户确认")
                .count(),
            2
        );
    }

    fn zip_members(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn malformed_limits_entities_encryption_and_cancellation_never_publish_a_prefix() {
        let bytes = fixture("guide.xlsx");
        assert_eq!(
            extract("xlsx", &bytes, json!({"maximumDocumentOutputBytes":8}))
                .unwrap_err()
                .code(),
            "DOCUMENT_OUTPUT_LIMIT"
        );
        assert_eq!(
            extract("xlsx", &bytes, json!({"maximumArchiveEntries":1}))
                .unwrap_err()
                .code(),
            "DOCUMENT_ARCHIVE_LIMIT"
        );
        let xml =
            br#"<!DOCTYPE root [<!ENTITY leak SYSTEM "file:///private">]><root>&leak;</root>"#;
        assert_eq!(
            extract("xml", xml, json!({})).unwrap_err().code(),
            "DOCUMENT_XML_UNSUPPORTED"
        );
        assert_eq!(
            extract("rtf", br"{\rtf1\uc-1 Invalid}", json!({}))
                .unwrap_err()
                .code(),
            "DOCUMENT_PARSE_FAILED"
        );
        assert_eq!(
            extract("rtf", br"{\rtf1\u-10179?}", json!({}))
                .unwrap_err()
                .code(),
            "DOCUMENT_ENCODING_UNSUPPORTED"
        );
        let unsafe_package = zip_members(&[("../outside.txt", b"private")]);
        assert_eq!(
            extract("pptx", &unsafe_package, json!({}))
                .unwrap_err()
                .code(),
            "DOCUMENT_ARCHIVE_LIMIT"
        );
        let encrypted = zip_members(&[("META-INF/encryption.xml", b"encrypted")]);
        assert_eq!(
            extract("epub", &encrypted, json!({})).unwrap_err().code(),
            "DOCUMENT_ENCRYPTED"
        );
        let cancelled = Arc::new(AtomicBool::new(true));
        assert_eq!(
            decode(
                "xlsx",
                &bytes,
                &json!({}),
                &cancelled,
                &super::super::gateway_index::hash_bytes(&bytes)
            )
            .unwrap_err()
            .code(),
            "ABORT_ERR"
        );
    }

    fn legacy_workbook(maximum_row: u32) -> Vec<u8> {
        fn record(kind: u16, bytes: &[u8]) -> Vec<u8> {
            [
                kind.to_le_bytes().to_vec(),
                (bytes.len() as u16).to_le_bytes().to_vec(),
                bytes.to_vec(),
            ]
            .concat()
        }
        fn bof(kind: u16) -> Vec<u8> {
            record(
                0x0809,
                &[
                    0x0600u16.to_le_bytes(),
                    kind.to_le_bytes(),
                    [0u8; 2],
                    [0u8; 2],
                    [0u8; 2],
                    [0u8; 2],
                    [0u8; 2],
                    [0u8; 2],
                ]
                .concat(),
            )
        }
        let mut bounds = vec![0u8; 8];
        bounds[6] = 4;
        bounds.extend(b"Data");
        let prefix = [bof(0x0005), record(0x0042, &1200u16.to_le_bytes())].concat();
        let offset = (prefix.len() + 4 + bounds.len() + 4) as u32;
        bounds[..4].copy_from_slice(&offset.to_le_bytes());
        let dimensions = [
            0u32.to_le_bytes().to_vec(),
            maximum_row.to_le_bytes().to_vec(),
            0u16.to_le_bytes().to_vec(),
            2u16.to_le_bytes().to_vec(),
            [0u8; 2].to_vec(),
        ]
        .concat();
        let mut label = vec![0u8; 6];
        let text = "质量 kg".encode_utf16().collect::<Vec<_>>();
        label.extend((text.len() as u16).to_le_bytes());
        label.push(1);
        for code in text {
            label.extend(code.to_le_bytes());
        }
        let number = [
            1u16.to_le_bytes().to_vec(),
            [0u8; 4].to_vec(),
            25.5f64.to_le_bytes().to_vec(),
        ]
        .concat();
        let workbook = [
            prefix,
            record(0x0085, &bounds),
            record(0x000a, &[]),
            bof(0x0010),
            record(0x0200, &dimensions),
            record(0x0204, &label),
            record(0x0203, &number),
            record(0x000a, &[]),
        ]
        .concat();
        let mut compound = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        compound
            .create_stream("/Workbook")
            .unwrap()
            .write_all(&workbook)
            .unwrap();
        compound.into_inner().into_inner()
    }

    #[test]
    fn legacy_xls_text_is_real_and_huge_dense_dimension_is_rejected_before_allocation() {
        let result = extract("xls", &legacy_workbook(2), json!({})).unwrap();
        let text = result["text"].as_str().unwrap();
        assert!(text.contains("质量 kg") && text.contains("25.5"), "{text}");
        assert_eq!(
            extract(
                "xls",
                &legacy_workbook(65536),
                json!({"maximumSpreadsheetCells":1000})
            )
            .unwrap_err()
            .code(),
            "DOCUMENT_STRUCTURE_LIMIT"
        );
    }
}
