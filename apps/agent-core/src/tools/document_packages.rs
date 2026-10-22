//! Editable Office packages are generated from bounded structured input, without external runtimes.
//! 从有界结构化输入生成可编辑 Office 文档，不启动外部运行时。

use serde_json::Value;
use std::io::{Cursor, Write};

const XML_HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>";
const REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const OFFICE_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const CONTENT_NS: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_PART_XML_BYTES: usize = 64 * 1024 * 1024;
const MAX_PACKAGE_XML_BYTES: usize = 128 * 1024 * 1024;
const MAX_BLOCKS: usize = 20_000;
const MAX_SLIDES: usize = 300;
const MAX_SHEETS: usize = 64;
const MAX_ROWS: usize = 100_000;
const MAX_CELLS: usize = 1_000_000;
const MAX_COLUMNS: usize = 16_384;

#[derive(Default)]
struct InputBudget {
    text_bytes: usize,
    cells: usize,
}

impl InputBudget {
    fn text<'a>(&mut self, value: &'a str) -> Result<&'a str, String> {
        self.text_bytes = self.text_bytes.saturating_add(value.len());
        if self.text_bytes > MAX_TEXT_BYTES {
            return Err("文档文字总量超过 16 MiB，请拆分生成。".into());
        }
        Ok(value)
    }

    fn cells(&mut self, count: usize) -> Result<(), String> {
        self.cells = self.cells.saturating_add(count);
        if self.cells > MAX_CELLS {
            return Err("文档单元格总数超过 100 万，请拆分生成。".into());
        }
        Ok(())
    }
}

struct Package {
    archive: zip::ZipWriter<Cursor<Vec<u8>>>,
    xml_bytes: usize,
}

impl Package {
    fn new(main: &str, title: &str) -> Result<Self, String> {
        let mut package = Self {
            archive: zip::ZipWriter::new(Cursor::new(Vec::new())),
            xml_bytes: 0,
        };
        package.add("_rels/.rels", &format!(
            "{XML_HEADER}<Relationships xmlns=\"{REL_NS}\"><Relationship Id=\"rId1\" Type=\"{OFFICE_REL}/officeDocument\" Target=\"{main}\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/><Relationship Id=\"rId3\" Type=\"{OFFICE_REL}/extended-properties\" Target=\"docProps/app.xml\"/></Relationships>"
        ))?;
        package.add("docProps/core.xml", &format!(
            "{XML_HEADER}<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>{}</dc:title><dc:creator>KYNXA</dc:creator><cp:lastModifiedBy>KYNXA</cp:lastModifiedBy></cp:coreProperties>", escaped(title)
        ))?;
        package.add("docProps/app.xml", &format!(
            "{XML_HEADER}<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\"><Application>KYNXA</Application><AppVersion>{}</AppVersion></Properties>", env!("CARGO_PKG_VERSION")
        ))?;
        Ok(package)
    }

    fn add(&mut self, name: &str, xml: &str) -> Result<(), String> {
        if xml.len() > MAX_PART_XML_BYTES {
            return Err("单个文档组件超过 64 MiB，请拆分生成。".into());
        }
        self.xml_bytes = self.xml_bytes.saturating_add(xml.len());
        if self.xml_bytes > MAX_PACKAGE_XML_BYTES {
            return Err("生成的文档结构超过 128 MiB，请拆分生成。".into());
        }
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        self.archive.start_file(name, options).map_err(|error| error.to_string())?;
        self.archive.write_all(xml.as_bytes()).map_err(|error| error.to_string())
    }

    fn finish(self) -> Result<Vec<u8>, String> {
        self.archive.finish().map(|cursor| cursor.into_inner()).map_err(|error| error.to_string())
    }
}

fn append_xml(target: &mut String, fragment: &str) -> Result<(), String> {
    if target.len().saturating_add(fragment.len()) > MAX_PART_XML_BYTES - 4096 {
        return Err("单个文档组件接近 64 MiB，请拆分生成。".into());
    }
    target.push_str(fragment);
    Ok(())
}

// Only XML 1.0 characters survive, and every textual/attribute value uses the same escaping rule.
// 统一过滤 XML 1.0 非法字符并转义正文与属性，避免模型文字破坏包结构。
fn escaped(text: &str) -> String {
    let mut xml = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => xml.push_str("&amp;"),
            '<' => xml.push_str("&lt;"),
            '>' => xml.push_str("&gt;"),
            '"' => xml.push_str("&quot;"),
            '\'' => xml.push_str("&apos;"),
            '\t' | '\n' | '\r' => xml.push(character),
            '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}' => xml.push(character),
            _ => xml.push('\u{FFFD}'),
        }
    }
    xml
}

fn optional_text<'a>(object: &'a Value, name: &str) -> Result<&'a str, String> {
    match object.get(name) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(text)) => Ok(text),
        _ => Err(format!("{name} 必须是文字。")),
    }
}

fn scalar_text(value: &Value) -> Result<String, String> {
    match value {
        Value::Null => Ok(String::new()),
        Value::String(text) => Ok(text.clone()),
        Value::Bool(_) | Value::Number(_) => Ok(value.to_string()),
        _ => Err("表格单元格必须是文字、数字、布尔值或 null。".into()),
    }
}

fn table_rows(value: &Value, max_rows: usize, budget: &mut InputBudget) -> Result<Vec<Vec<String>>, String> {
    let rows = value.as_array().ok_or("rows 必须是二维数组。")?;
    if rows.is_empty() || rows.len() > max_rows {
        return Err(format!("表格须包含 1 至 {max_rows} 行。"));
    }
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let cells = row.as_array().ok_or("表格的每一行必须是数组。")?;
        if cells.is_empty() || cells.len() > 256 {
            return Err("文档表格每行须包含 1 至 256 列。".into());
        }
        budget.cells(cells.len())?;
        let mut values = Vec::with_capacity(cells.len());
        for value in cells {
            let text = scalar_text(value)?;
            budget.text(&text)?;
            values.push(text);
        }
        result.push(values);
    }
    Ok(result)
}

fn content_types(overrides: &[(String, &str)]) -> String {
    let mut xml = format!("{XML_HEADER}<Types xmlns=\"{CONTENT_NS}\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/><Override PartName=\"/docProps/app.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.extended-properties+xml\"/>");
    for (name, content_type) in overrides {
        xml.push_str(&format!("<Override PartName=\"/{name}\" ContentType=\"{content_type}\"/>"));
    }
    xml.push_str("</Types>");
    xml
}

fn word_run(text: &str, bold: bool) -> String {
    let mut xml = String::from("<w:r>");
    if bold {
        xml.push_str("<w:rPr><w:b/></w:rPr>");
    }
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            xml.push_str("<w:br/>");
        }
        xml.push_str(&format!("<w:t xml:space=\"preserve\">{}</w:t>", escaped(line.trim_end_matches('\r'))));
    }
    xml.push_str("</w:r>");
    xml
}

fn word_paragraph(text: &str, style: &str) -> String {
    format!("<w:p><w:pPr><w:pStyle w:val=\"{style}\"/></w:pPr>{}</w:p>", word_run(text, false))
}

fn word_table(rows: &[Vec<String>]) -> Result<String, String> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(1);
    let width = 9000 / columns;
    let mut xml = String::from("<w:tbl><w:tblPr><w:tblW w:w=\"9000\" w:type=\"dxa\"/><w:tblBorders>");
    for border in ["top", "left", "bottom", "right", "insideH", "insideV"] {
        xml.push_str(&format!("<w:{border} w:val=\"single\" w:sz=\"4\" w:color=\"D6DDE5\"/>"));
    }
    xml.push_str("</w:tblBorders><w:tblCellMar><w:top w:w=\"90\" w:type=\"dxa\"/><w:left w:w=\"110\" w:type=\"dxa\"/><w:bottom w:w=\"90\" w:type=\"dxa\"/><w:right w:w=\"110\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr><w:tblGrid>");
    for _ in 0..columns {
        xml.push_str(&format!("<w:gridCol w:w=\"{width}\"/>"));
    }
    xml.push_str("</w:tblGrid>");
    for (row_index, row) in rows.iter().enumerate() {
        xml.push_str("<w:tr>");
        if row_index == 0 {
            xml.push_str("<w:trPr><w:tblHeader/></w:trPr>");
        }
        for column in 0..columns {
            append_xml(&mut xml, &format!("<w:tc><w:tcPr><w:tcW w:w=\"{width}\" w:type=\"dxa\"/>{}</w:tcPr><w:p>{}</w:p></w:tc>",
                if row_index == 0 { "<w:shd w:fill=\"EAF0F6\"/>" } else { "" },
                word_run(row.get(column).map(String::as_str).unwrap_or(""), row_index == 0)))?;
        }
        xml.push_str("</w:tr>");
    }
    xml.push_str("</w:tbl><w:p/>");
    Ok(xml)
}

fn markdown_cells(line: &str) -> Vec<String> {
    line.trim().trim_matches('|').split('|').map(|cell| cell.trim().to_owned()).collect()
}

fn markdown_separator(line: &str) -> bool {
    line.contains('|') && markdown_cells(line).iter().all(|cell| {
        let rule = cell.trim_matches(':');
        rule.len() >= 3 && rule.chars().all(|character| character == '-')
    })
}

fn markdown_body(markdown: &str, budget: &mut InputBudget) -> Result<String, String> {
    budget.text(markdown)?;
    let lines: Vec<_> = markdown.lines().collect();
    if lines.len() > MAX_BLOCKS {
        return Err("Markdown 行数超过 2 万行，请拆分生成。".into());
    }
    let mut xml = String::new();
    let mut index = 0;
    let mut fenced = false;
    while index < lines.len() {
        let line = lines[index];
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fenced = !fenced;
            index += 1;
            continue;
        }
        if fenced {
            append_xml(&mut xml, &word_paragraph(line, "Code"))?;
        } else if index + 1 < lines.len() && line.contains('|') && markdown_separator(lines[index + 1]) {
            let mut rows = vec![markdown_cells(line)];
            index += 2;
            while index < lines.len() && lines[index].contains('|') && !lines[index].trim().is_empty() {
                rows.push(markdown_cells(lines[index]));
                index += 1;
            }
            if rows.iter().any(|row| row.len() > 256) {
                return Err("文档表格每行不得超过 256 列。".into());
            }
            budget.cells(rows.iter().map(Vec::len).sum())?;
            append_xml(&mut xml, &word_table(&rows)?)?;
            continue;
        } else if !line.trim().is_empty() {
            let heading_level = line.chars().take_while(|character| *character == '#').count();
            if (1..=6).contains(&heading_level) && line.as_bytes().get(heading_level) == Some(&b' ') {
                append_xml(&mut xml, &word_paragraph(line[heading_level + 1..].trim(), &format!("Heading{heading_level}")))?;
            } else if let Some(bullet) = line.trim_start().strip_prefix("- ").or_else(|| line.trim_start().strip_prefix("* ")) {
                append_xml(&mut xml, &word_paragraph(&format!("• {bullet}"), "Normal"))?;
            } else {
                append_xml(&mut xml, &word_paragraph(line, "Normal"))?;
            }
        } else {
            xml.push_str("<w:p/>");
        }
        index += 1;
    }
    Ok(xml)
}

fn docx(args: &Value) -> Result<Vec<u8>, String> {
    let mut budget = InputBudget::default();
    let title = budget.text(optional_text(args, "title")?)?;
    let mut body = String::new();
    if !title.is_empty() {
        body.push_str(&word_paragraph(title, "Title"));
    }
    let markdown = optional_text(args, "content")?;
    if !markdown.is_empty() {
        append_xml(&mut body, &markdown_body(markdown, &mut budget)?)?;
    }
    if let Some(blocks) = args.get("blocks") {
        let blocks = blocks.as_array().ok_or("blocks 必须是数组。")?;
        if blocks.len() > MAX_BLOCKS {
            return Err("文档段落数量超过 2 万，请拆分生成。".into());
        }
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("heading") => {
                    let level = block.get("level").and_then(Value::as_u64).unwrap_or(1);
                    if !(1..=6).contains(&level) {
                        return Err("标题 level 必须是 1 至 6。".into());
                    }
                    append_xml(&mut body, &word_paragraph(budget.text(optional_text(block, "text")?)?, &format!("Heading{level}")))?;
                }
                Some("paragraph") => append_xml(&mut body, &word_paragraph(budget.text(optional_text(block, "text")?)?, "Normal"))?,
                Some("table") => append_xml(&mut body, &word_table(&table_rows(&block["rows"], MAX_ROWS, &mut budget)?)?)?,
                _ => return Err("blocks.type 仅支持 heading、paragraph、table。".into()),
            }
        }
    }
    if body.is_empty() {
        return Err("DOCX 需要 title、content 或非空 blocks。".into());
    }
    let mut package = Package::new("word/document.xml", title)?;
    package.add("[Content_Types].xml", &content_types(&[
        ("word/document.xml".into(), "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"),
        ("word/styles.xml".into(), "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"),
    ]))?;
    package.add("word/_rels/document.xml.rels", &format!("{XML_HEADER}<Relationships xmlns=\"{REL_NS}\"><Relationship Id=\"rId1\" Type=\"{OFFICE_REL}/styles\" Target=\"styles.xml\"/></Relationships>"))?;
    let mut styles = format!("{XML_HEADER}<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Aptos\" w:hAnsi=\"Aptos\" w:eastAsia=\"Microsoft YaHei\"/><w:sz w:val=\"22\"/><w:lang w:val=\"en-US\" w:eastAsia=\"zh-CN\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"140\" w:line=\"320\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style><w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:pPr><w:spacing w:before=\"200\" w:after=\"320\"/></w:pPr><w:rPr><w:b/><w:color w:val=\"233B53\"/><w:sz w:val=\"40\"/></w:rPr></w:style>");
    for level in 1..=6 {
        let size = [32, 28, 26, 24, 23, 22][level - 1];
        styles.push_str(&format!("<w:style w:type=\"paragraph\" w:styleId=\"Heading{level}\"><w:name w:val=\"heading {level}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"260\" w:after=\"120\"/><w:outlineLvl w:val=\"{}\"/></w:pPr><w:rPr><w:b/><w:color w:val=\"233B53\"/><w:sz w:val=\"{size}\"/></w:rPr></w:style>", level - 1));
    }
    styles.push_str("<w:style w:type=\"paragraph\" w:styleId=\"Code\"><w:name w:val=\"Code\"/><w:basedOn w:val=\"Normal\"/><w:pPr><w:shd w:fill=\"F3F5F7\"/><w:spacing w:after=\"0\"/></w:pPr><w:rPr><w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\" w:eastAsia=\"Microsoft YaHei\"/><w:sz w:val=\"19\"/></w:rPr></w:style></w:styles>");
    package.add("word/styles.xml", &styles)?;
    package.add("word/document.xml", &format!("{XML_HEADER}<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{body}<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1134\" w:right=\"1440\" w:bottom=\"1134\" w:left=\"1440\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/></w:sectPr></w:body></w:document>"))?;
    package.finish()
}

const DRAWING_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const PRESENTATION_NS: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const GROUP_SHAPE: &str = "<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>";
const COLOR_MAP: &str = "<p:clrMap bg1=\"lt1\" tx1=\"dk1\" bg2=\"lt2\" tx2=\"dk2\" accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" hlink=\"hlink\" folHlink=\"folHlink\"/>";

fn drawing_paragraph(text: &str, size: u32, bold: bool, bullet: bool) -> String {
    format!("<a:p><a:pPr marL=\"{}\" indent=\"{}\"><a:lnSpc><a:spcPct val=\"115000\"/></a:lnSpc><a:spcAft><a:spcPts val=\"800\"/></a:spcAft>{}</a:pPr><a:r><a:rPr lang=\"zh-CN\" altLang=\"en-US\" sz=\"{size}\" b=\"{}\"><a:solidFill><a:srgbClr val=\"233B53\"/></a:solidFill><a:latin typeface=\"Aptos\"/><a:ea typeface=\"Microsoft YaHei\"/></a:rPr><a:t>{}</a:t></a:r><a:endParaRPr lang=\"zh-CN\" sz=\"{size}\"/></a:p>",
        if bullet { 285750 } else { 0 }, if bullet { -228600 } else { 0 },
        if bullet { "<a:buChar char=\"•\"/>" } else { "<a:buNone/>" },
        if bold { 1 } else { 0 }, escaped(text))
}

fn slide_textbox(id: u32, name: &str, y: u32, height: u32, paragraphs: &str) -> String {
    format!("<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"{name}\"/><p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"548640\" y=\"{y}\"/><a:ext cx=\"11094720\" cy=\"{height}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:noFill/><a:ln><a:noFill/></a:ln></p:spPr><p:txBody><a:bodyPr wrap=\"square\" lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\" anchor=\"t\"><a:normAutofit/></a:bodyPr><a:lstStyle/>{paragraphs}</p:txBody></p:sp>")
}

fn slide_table(rows: &[Vec<String>], with_body: bool) -> String {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(1);
    let (y, height) = if with_body { (3_566_160, 2_743_200) } else { (1_463_040, 4_846_320) };
    let width = 11_094_720 / columns;
    let row_height = height / rows.len();
    let font_size = if rows.len() > 15 || columns > 8 { 1200 } else { 1600 };
    let mut xml = format!("<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id=\"4\" name=\"Table\"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x=\"548640\" y=\"{y}\"/><a:ext cx=\"11094720\" cy=\"{height}\"/></p:xfrm><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/table\"><a:tbl><a:tblPr firstRow=\"1\" bandRow=\"1\"><a:tableStyleId>{{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}}</a:tableStyleId></a:tblPr><a:tblGrid>");
    for _ in 0..columns {
        xml.push_str(&format!("<a:gridCol w=\"{width}\"/>"));
    }
    xml.push_str("</a:tblGrid>");
    for (row_index, row) in rows.iter().enumerate() {
        xml.push_str(&format!("<a:tr h=\"{row_height}\">"));
        for column in 0..columns {
            let value = row.get(column).map(String::as_str).unwrap_or("");
            let paragraphs = value.split('\n').map(|line| drawing_paragraph(line, font_size, row_index == 0, false)).collect::<String>();
            xml.push_str(&format!("<a:tc><a:txBody><a:bodyPr><a:normAutofit/></a:bodyPr><a:lstStyle/>{paragraphs}</a:txBody><a:tcPr marL=\"91440\" marR=\"91440\" marT=\"45720\" marB=\"45720\"/></a:tc>"));
        }
        xml.push_str("</a:tr>");
    }
    xml.push_str("</a:tbl></a:graphicData></a:graphic></p:graphicFrame>");
    xml
}

fn presentation_theme() -> String {
    let mut xml = format!("{XML_HEADER}<a:theme xmlns:a=\"{DRAWING_NS}\" name=\"KYNXA\"><a:themeElements><a:clrScheme name=\"KYNXA\">");
    for (name, color) in [("dk1", "233B53"), ("lt1", "FFFFFF"), ("dk2", "334155"), ("lt2", "F3F5F7"), ("accent1", "3B6B8F"), ("accent2", "A67C52"), ("accent3", "5B8C78"), ("accent4", "8A7098"), ("accent5", "668CA3"), ("accent6", "B88B62"), ("hlink", "0563C1"), ("folHlink", "954F72")] {
        xml.push_str(&format!("<a:{name}><a:srgbClr val=\"{color}\"/></a:{name}>"));
    }
    xml.push_str("</a:clrScheme><a:fontScheme name=\"KYNXA\">");
    for kind in ["majorFont", "minorFont"] {
        xml.push_str(&format!("<a:{kind}><a:latin typeface=\"Aptos\"/><a:ea typeface=\"Microsoft YaHei\"/><a:cs typeface=\"Arial\"/><a:font script=\"Hans\" typeface=\"Microsoft YaHei\"/></a:{kind}>"));
    }
    xml.push_str("</a:fontScheme><a:fmtScheme name=\"KYNXA\"><a:fillStyleLst>");
    for _ in 0..3 {
        xml.push_str("<a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>");
    }
    xml.push_str("</a:fillStyleLst><a:lnStyleLst>");
    for width in [9525, 25400, 38100] {
        xml.push_str(&format!("<a:ln w=\"{width}\" cap=\"flat\" cmpd=\"sng\" algn=\"ctr\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:prstDash val=\"solid\"/><a:miter lim=\"800000\"/></a:ln>"));
    }
    xml.push_str("</a:lnStyleLst><a:effectStyleLst>");
    for _ in 0..3 {
        xml.push_str("<a:effectStyle><a:effectLst/></a:effectStyle>");
    }
    xml.push_str("</a:effectStyleLst><a:bgFillStyleLst>");
    for _ in 0..3 {
        xml.push_str("<a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>");
    }
    xml.push_str("</a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>");
    xml
}

fn pptx(args: &Value) -> Result<Vec<u8>, String> {
    let slides = args.get("slides").and_then(Value::as_array).ok_or("PPTX 需要 slides 数组。")?;
    if slides.is_empty() || slides.len() > MAX_SLIDES {
        return Err("演示文稿须包含 1 至 300 张幻灯片。".into());
    }
    let mut budget = InputBudget::default();
    let title = budget.text(optional_text(args, "title")?)?;
    let mut package = Package::new("ppt/presentation.xml", title)?;
    let mut overrides = vec![
        ("ppt/presentation.xml".into(), "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"),
        ("ppt/slideMasters/slideMaster1.xml".into(), "application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"),
        ("ppt/slideLayouts/slideLayout1.xml".into(), "application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"),
        ("ppt/theme/theme1.xml".into(), "application/vnd.openxmlformats-officedocument.theme+xml"),
        ("ppt/tableStyles.xml".into(), "application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml"),
    ];
    let mut slide_ids = String::new();
    let mut relationships = format!("{XML_HEADER}<Relationships xmlns=\"{REL_NS}\"><Relationship Id=\"rId1\" Type=\"{OFFICE_REL}/slideMaster\" Target=\"slideMasters/slideMaster1.xml\"/>");
    for (index, slide) in slides.iter().enumerate() {
        let number = index + 1;
        let slide_title = budget.text(optional_text(slide, "title")?)?;
        let body = budget.text(optional_text(slide, "body")?)?;
        let mut paragraphs = body.lines().map(|line| drawing_paragraph(line, 2200, false, false)).collect::<String>();
        if let Some(bullets) = slide.get("bullets") {
            let bullets = bullets.as_array().ok_or("slides.bullets 必须是文字数组。")?;
            if bullets.len() > 200 {
                return Err("单张幻灯片要点不得超过 200 条，请拆分幻灯片。".into());
            }
            for bullet in bullets {
                let text = budget.text(bullet.as_str().ok_or("slides.bullets 每一项必须是文字。")?)?;
                paragraphs.push_str(&drawing_paragraph(text, 2200, false, true));
            }
        }
        if slide_title.len() + body.len() + paragraphs.len() > 256 * 1024 {
            return Err("单张幻灯片文字过多，请拆分幻灯片。".into());
        }
        let table = slide.get("rows").filter(|rows| !rows.is_null()).map(|rows| table_rows(rows, 50, &mut budget)).transpose()?;
        if table.as_ref().is_some_and(|rows| rows.iter().any(|row| row.len() > 32)) {
            return Err("幻灯片表格最多 32 列，请拆分表格。".into());
        }
        let with_body = !paragraphs.is_empty();
        let mut shapes = slide_textbox(2, "Title", 365_760, 822_960, &drawing_paragraph(slide_title, 3000, true, false));
        if with_body {
            shapes.push_str(&slide_textbox(3, "Body", 1_463_040, if table.is_some() { 1_920_240 } else { 4_846_320 }, &paragraphs));
        }
        if let Some(rows) = table {
            shapes.push_str(&slide_table(&rows, with_body));
        }
        package.add(&format!("ppt/slides/slide{number}.xml"), &format!("{XML_HEADER}<p:sld xmlns:a=\"{DRAWING_NS}\" xmlns:r=\"{OFFICE_REL}\" xmlns:p=\"{PRESENTATION_NS}\"><p:cSld><p:bg><p:bgPr><a:solidFill><a:srgbClr val=\"FFFFFF\"/></a:solidFill><a:effectLst/></p:bgPr></p:bg><p:spTree>{GROUP_SHAPE}{shapes}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"))?;
        package.add(&format!("ppt/slides/_rels/slide{number}.xml.rels"), &format!("{XML_HEADER}<Relationships xmlns=\"{REL_NS}\"><Relationship Id=\"rId1\" Type=\"{OFFICE_REL}/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/></Relationships>"))?;
        overrides.push((format!("ppt/slides/slide{number}.xml"), "application/vnd.openxmlformats-officedocument.presentationml.slide+xml"));
        slide_ids.push_str(&format!("<p:sldId id=\"{}\" r:id=\"rId{}\"/>", index + 256, index + 2));
        relationships.push_str(&format!("<Relationship Id=\"rId{}\" Type=\"{OFFICE_REL}/slide\" Target=\"slides/slide{number}.xml\"/>", index + 2));
    }
    relationships.push_str(&format!("<Relationship Id=\"rId{}\" Type=\"{OFFICE_REL}/tableStyles\" Target=\"tableStyles.xml\"/></Relationships>", slides.len() + 2));
    package.add("[Content_Types].xml", &content_types(&overrides))?;
    package.add("ppt/presentation.xml", &format!("{XML_HEADER}<p:presentation xmlns:a=\"{DRAWING_NS}\" xmlns:r=\"{OFFICE_REL}\" xmlns:p=\"{PRESENTATION_NS}\"><p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rId1\"/></p:sldMasterIdLst><p:sldIdLst>{slide_ids}</p:sldIdLst><p:sldSz cx=\"12192000\" cy=\"6858000\" type=\"screen16x9\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/></p:presentation>"))?;
    package.add("ppt/_rels/presentation.xml.rels", &relationships)?;
    package.add("ppt/slideMasters/slideMaster1.xml", &format!("{XML_HEADER}<p:sldMaster xmlns:a=\"{DRAWING_NS}\" xmlns:r=\"{OFFICE_REL}\" xmlns:p=\"{PRESENTATION_NS}\"><p:cSld name=\"KYNXA\"><p:spTree>{GROUP_SHAPE}</p:spTree></p:cSld>{COLOR_MAP}<p:sldLayoutIdLst><p:sldLayoutId id=\"2147483649\" r:id=\"rId1\"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle/><p:bodyStyle/><p:otherStyle/></p:txStyles></p:sldMaster>"))?;
    package.add("ppt/slideMasters/_rels/slideMaster1.xml.rels", &format!("{XML_HEADER}<Relationships xmlns=\"{REL_NS}\"><Relationship Id=\"rId1\" Type=\"{OFFICE_REL}/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/><Relationship Id=\"rId2\" Type=\"{OFFICE_REL}/theme\" Target=\"../theme/theme1.xml\"/></Relationships>"))?;
    package.add("ppt/slideLayouts/slideLayout1.xml", &format!("{XML_HEADER}<p:sldLayout xmlns:a=\"{DRAWING_NS}\" xmlns:r=\"{OFFICE_REL}\" xmlns:p=\"{PRESENTATION_NS}\" type=\"blank\" preserve=\"1\"><p:cSld name=\"Blank\"><p:spTree>{GROUP_SHAPE}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"))?;
    package.add("ppt/slideLayouts/_rels/slideLayout1.xml.rels", &format!("{XML_HEADER}<Relationships xmlns=\"{REL_NS}\"><Relationship Id=\"rId1\" Type=\"{OFFICE_REL}/slideMaster\" Target=\"../slideMasters/slideMaster1.xml\"/></Relationships>"))?;
    package.add("ppt/theme/theme1.xml", &presentation_theme())?;
    package.add("ppt/tableStyles.xml", &format!("{XML_HEADER}<a:tblStyleLst xmlns:a=\"{DRAWING_NS}\" def=\"{{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}}\"/>"))?;
    package.finish()
}

fn column_name(mut column: usize) -> String {
    let mut letters = Vec::new();
    column += 1;
    while column > 0 {
        letters.push((b'A' + ((column - 1) % 26) as u8) as char);
        column = (column - 1) / 26;
    }
    letters.into_iter().rev().collect()
}

fn spreadsheet_text<'a>(text: &'a str, budget: &mut InputBudget) -> Result<&'a str, String> {
    if text.encode_utf16().count() > 32_767 {
        return Err("XLSX 单个文字单元格不得超过 32767 个 UTF-16 字符。".into());
    }
    budget.text(text)
}

fn spreadsheet_cell(value: &Value, address: &str, header: bool, budget: &mut InputBudget) -> Result<String, String> {
    let style = if header { " s=\"1\"" } else { "" };
    let prefix = format!("<c r=\"{address}\"{style}");
    match value {
        Value::Null => Ok(String::new()),
        Value::String(text) => Ok(format!("{prefix} t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>", escaped(spreadsheet_text(text, budget)?))),
        Value::Number(number) => Ok(format!("{prefix}><v>{number}</v></c>")),
        Value::Bool(value) => Ok(format!("{prefix} t=\"b\"><v>{}</v></c>", u8::from(*value))),
        Value::Object(object) => {
            let formula = object.get("formula").and_then(Value::as_str).ok_or("公式单元格需要 formula 文字。")?;
            let formula = formula.strip_prefix('=').unwrap_or(formula);
            if formula.trim().is_empty() || formula.encode_utf16().count() > 8192 {
                return Err("XLSX 公式须为非空文字，且不得超过 8192 个字符。".into());
            }
            budget.text(formula)?;
            let (kind, cached) = match object.get("cachedValue") {
                None | Some(Value::Null) => ("", String::new()),
                Some(Value::Number(number)) => ("", format!("<v>{number}</v>")),
                Some(Value::Bool(value)) => (" t=\"b\"", format!("<v>{}</v>", u8::from(*value))),
                Some(Value::String(text)) => (" t=\"str\"", format!("<v>{}</v>", escaped(spreadsheet_text(text, budget)?))),
                _ => return Err("cachedValue 必须是文字、数字、布尔值或 null。".into()),
            };
            // Cached values are supplied by the caller; writing a formula does not evaluate it.
            // 缓存值由调用方提供；写入公式本身不代表已经执行计算。
            Ok(format!("{prefix}{kind}><f>{}</f>{cached}</c>", escaped(formula)))
        }
        _ => Err("XLSX 单元格仅支持文字、数字、布尔值、null 或公式对象。".into()),
    }
}

fn workbook_styles() -> String {
    format!("{XML_HEADER}<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><fonts count=\"2\"><font><sz val=\"11\"/><color rgb=\"FF233B53\"/><name val=\"Microsoft YaHei\"/><family val=\"2\"/></font><font><b/><sz val=\"11\"/><color rgb=\"FFFFFFFF\"/><name val=\"Microsoft YaHei\"/><family val=\"2\"/></font></fonts><fills count=\"3\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill><fill><patternFill patternType=\"solid\"><fgColor rgb=\"FF3B6B8F\"/><bgColor indexed=\"64\"/></patternFill></fill></fills><borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs><cellXfs count=\"2\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyAlignment=\"1\"><alignment vertical=\"top\" wrapText=\"1\"/></xf><xf numFmtId=\"0\" fontId=\"1\" fillId=\"2\" borderId=\"0\" xfId=\"0\" applyFont=\"1\" applyFill=\"1\" applyAlignment=\"1\"><alignment vertical=\"center\" wrapText=\"1\"/></xf></cellXfs><cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles></styleSheet>")
}

fn xlsx(args: &Value) -> Result<Vec<u8>, String> {
    let sheets = args.get("sheets").and_then(Value::as_array).ok_or("XLSX 需要 sheets 数组。")?;
    if sheets.is_empty() || sheets.len() > MAX_SHEETS {
        return Err("工作簿须包含 1 至 64 个工作表。".into());
    }
    let mut budget = InputBudget::default();
    let mut names = std::collections::HashSet::new();
    let mut package = Package::new("xl/workbook.xml", budget.text(optional_text(args, "title")?)?)?;
    let mut overrides = vec![
        ("xl/workbook.xml".into(), "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"),
        ("xl/styles.xml".into(), "application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"),
    ];
    let mut sheet_entries = String::new();
    let mut relationships = format!("{XML_HEADER}<Relationships xmlns=\"{REL_NS}\">");
    for (index, sheet) in sheets.iter().enumerate() {
        let number = index + 1;
        let requested_name = optional_text(sheet, "name")?;
        let name = if requested_name.is_empty() { format!("Sheet{number}") } else { requested_name.to_owned() };
        if name.trim().is_empty() || name.encode_utf16().count() > 31
            || name.contains(['[', ']', ':', '*', '?', '/', '\\'])
            || name.starts_with('\'') || name.ends_with('\'')
            || name.chars().any(char::is_control)
            || !names.insert(name.to_lowercase())
        {
            return Err("工作表名称须唯一、长度 1 至 31，且不能包含 []:*?/\\ 或首尾单引号。".into());
        }
        budget.text(&name)?;
        let rows = sheet.get("rows").and_then(Value::as_array).ok_or("sheets.rows 必须是二维数组。")?;
        if rows.len() > MAX_ROWS {
            return Err("单个工作表行数超过 10 万，请拆分工作表。".into());
        }
        let mut sheet_data = String::new();
        let mut columns = 0;
        let mut widths: Vec<usize> = Vec::new();
        for (row_index, row) in rows.iter().enumerate() {
            let cells = row.as_array().ok_or("工作表的每一行必须是数组。")?;
            if cells.len() > MAX_COLUMNS {
                return Err("XLSX 每行不得超过 16384 列。".into());
            }
            budget.cells(cells.len())?;
            columns = columns.max(cells.len());
            widths.resize(columns, 12);
            sheet_data.push_str(&format!("<row r=\"{}\"{}>", row_index + 1, if row_index == 0 { " ht=\"26\" customHeight=\"1\"" } else { "" }));
            for (column, value) in cells.iter().enumerate() {
                let address = format!("{}{}", column_name(column), row_index + 1);
                append_xml(&mut sheet_data, &spreadsheet_cell(value, &address, row_index == 0, &mut budget)?)?;
                if row_index < 1000 {
                    let width = match value {
                        Value::String(text) => text.lines().map(|line| line.chars().take(48).map(|character| if character.is_ascii() { 1 } else { 2 }).sum::<usize>()).max().unwrap_or(0),
                        _ => 12,
                    };
                    widths[column] = widths[column].max(width.saturating_add(2).min(48));
                }
            }
            sheet_data.push_str("</row>");
        }
        let last_cell = format!("{}{}", column_name(columns.saturating_sub(1)), rows.len().max(1));
        let mut column_xml = String::new();
        if !widths.is_empty() {
            column_xml.push_str("<cols>");
            for (column, width) in widths.iter().enumerate() {
                column_xml.push_str(&format!("<col min=\"{}\" max=\"{}\" width=\"{width}\" customWidth=\"1\"/>", column + 1, column + 1));
            }
            column_xml.push_str("</cols>");
        }
        package.add(&format!("xl/worksheets/sheet{number}.xml"), &format!("{XML_HEADER}<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><dimension ref=\"A1:{last_cell}\"/><sheetViews><sheetView workbookViewId=\"0\"><pane ySplit=\"1\" topLeftCell=\"A2\" activePane=\"bottomLeft\" state=\"frozen\"/><selection pane=\"bottomLeft\" activeCell=\"A2\" sqref=\"A2\"/></sheetView></sheetViews><sheetFormatPr defaultRowHeight=\"20\"/>{column_xml}<sheetData>{sheet_data}</sheetData><pageMargins left=\"0.5\" right=\"0.5\" top=\"0.75\" bottom=\"0.75\" header=\"0.3\" footer=\"0.3\"/></worksheet>"))?;
        sheet_entries.push_str(&format!("<sheet name=\"{}\" sheetId=\"{number}\" r:id=\"rId{number}\"/>", escaped(&name)));
        relationships.push_str(&format!("<Relationship Id=\"rId{number}\" Type=\"{OFFICE_REL}/worksheet\" Target=\"worksheets/sheet{number}.xml\"/>"));
        overrides.push((format!("xl/worksheets/sheet{number}.xml"), "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"));
    }
    relationships.push_str(&format!("<Relationship Id=\"rId{}\" Type=\"{OFFICE_REL}/styles\" Target=\"styles.xml\"/></Relationships>", sheets.len() + 1));
    package.add("[Content_Types].xml", &content_types(&overrides))?;
    package.add("xl/_rels/workbook.xml.rels", &relationships)?;
    package.add("xl/styles.xml", &workbook_styles())?;
    package.add("xl/workbook.xml", &format!("{XML_HEADER}<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"{OFFICE_REL}\"><bookViews><workbookView/></bookViews><sheets>{sheet_entries}</sheets><calcPr calcId=\"191029\" calcMode=\"auto\" fullCalcOnLoad=\"1\" forceFullCalc=\"1\"/></workbook>"))?;
    package.finish()
}

/// Produces actual OOXML bytes; authorization and atomic publication remain with the tool caller.
/// 返回真实 OOXML 字节；文件授权与原子发布仍由上层工具负责。
pub(super) fn generate(args: &Value, format: &str) -> Result<Vec<u8>, String> {
    if !args.is_object() {
        return Err("文档参数必须是 JSON 对象。".into());
    }
    match format {
        "docx" => docx(args),
        "pptx" => pptx(args),
        "xlsx" => xlsx(args),
        _ => Err("此生成器仅支持 docx、pptx、xlsx。".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::{Data, Reader, Xlsx};
    use serde_json::json;
    use std::io::Read;

    fn part(bytes: &[u8], name: &str) -> String {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut xml = String::new();
        archive.by_name(name).unwrap().read_to_string(&mut xml).unwrap();
        xml
    }

    fn assert_xml_parts(bytes: &[u8]) {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            assert!(entry.enclosed_name().is_some());
            let mut xml = String::new();
            entry.read_to_string(&mut xml).unwrap();
            let mut reader = quick_xml::Reader::from_str(&xml);
            loop {
                match reader.read_event().unwrap() {
                    quick_xml::events::Event::Eof => break,
                    quick_xml::events::Event::DocType(_) => panic!("generated package must not contain a DTD"),
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn word_and_presentation_packages_keep_editable_text_and_complete_relationships() {
        let word = generate(&json!({
            "title":"中文 & English <报告>",
            "content":"# 第一章\n正文\u{1}内容\n\n| 名称 | 数值 |\n| --- | --- |\n| 样本 | 42 |",
            "blocks":[{"type":"heading","level":2,"text":"结构化内容"},{"type":"paragraph","text":"第一行\n第二行"}]
        }), "docx").unwrap();
        assert_xml_parts(&word);
        let document = part(&word, "word/document.xml");
        assert!(document.contains("中文 &amp; English &lt;报告&gt;"));
        assert!(document.contains("正文\u{FFFD}内容"));
        assert!(document.contains("<w:tbl>"));
        assert!(document.contains("<w:br/>"));
        assert!(part(&word, "word/styles.xml").contains("Microsoft YaHei"));
        let slides = generate(&json!({"title":"课件","slides":[
            {"title":"第一讲","body":"原理 & 实验","bullets":["中文要点","English"],"rows":[["项目","结果"],["样本",true]]},
            {"title":"总结","bullets":["下一步"]}
        ]}), "pptx").unwrap();
        assert_xml_parts(&slides);
        assert!(part(&slides, "ppt/slides/slide1.xml").contains("<a:tbl>"));
        assert!(part(&slides, "ppt/slides/slide2.xml").contains("下一步"));
        assert!(part(&slides, "ppt/slideMasters/_rels/slideMaster1.xml.rels").contains("../theme/theme1.xml"));
        assert!(part(&slides, "ppt/slideLayouts/_rels/slideLayout1.xml.rels").contains("../slideMasters/slideMaster1.xml"));
        assert!(part(&slides, "ppt/theme/theme1.xml").contains("<a:fmtScheme"));
    }

    #[test]
    fn workbook_roundtrips_cell_types_and_formula_caches_without_calculating() {
        let bytes = generate(&json!({"sheets":[{"name":"中文数据","rows":[
            ["项目","数量","通过","备注","金额","文本公式","布尔公式"],
            ["样本 & <A>",3,true,null,{"formula":"=B2*2","cachedValue":6},{"formula":"\"结果\"","cachedValue":"结果"},{"formula":"B2>0","cachedValue":true}],
            ["=this remains text",1.25,false,"第二行\n后续",{"formula":"SUM(B2:B3)"}]
        ]}]}), "xlsx").unwrap();
        assert_xml_parts(&bytes);
        let mut workbook: Xlsx<_> = Xlsx::new(Cursor::new(bytes.clone())).unwrap();
        let range = workbook.worksheet_range("中文数据").unwrap();
        assert_eq!(range.get_value((1, 0)), Some(&Data::String("样本 & <A>".into())));
        assert_eq!(range.get_value((1, 1)), Some(&Data::Float(3.0)));
        assert_eq!(range.get_value((1, 2)), Some(&Data::Bool(true)));
        assert_eq!(range.get_value((1, 4)), Some(&Data::Float(6.0)));
        assert_eq!(range.get_value((1, 5)), Some(&Data::String("结果".into())));
        assert_eq!(range.get_value((1, 6)), Some(&Data::Bool(true)));
        assert_eq!(range.get_value((2, 0)), Some(&Data::String("=this remains text".into())));
        let formulas = workbook.worksheet_formula("中文数据").unwrap();
        assert_eq!(formulas.get_value((1, 4)).map(String::as_str), Some("B2*2"));
        assert!(part(&bytes, "xl/workbook.xml").contains("fullCalcOnLoad=\"1\""));
        assert!(!part(&bytes, "xl/worksheets/sheet1.xml").contains("<f>SUM(B2:B3)</f><v>"));
    }

    #[test]
    fn invalid_structures_fail_before_publishing() {
        assert!(generate(&json!({"sheets":[{"name":"数据","rows":[]},{"name":"数据","rows":[]}]}), "xlsx").is_err());
        assert!(generate(&json!({"sheets":[{"name":"bad/name","rows":[]}]}), "xlsx").is_err());
        assert!(generate(&json!({"sheets":[{"rows":[["a".repeat(32768)]]}]}), "xlsx").is_err());
        assert!(generate(&json!({"slides":[]}), "pptx").is_err());
        assert!(generate(&json!({"blocks":[{"type":"unknown"}]}), "docx").is_err());
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(16_383), "XFD");
    }
}
