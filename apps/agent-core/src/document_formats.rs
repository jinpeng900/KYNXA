//! Shared document-format contracts keep scanning, parsing and persisted identity consistent.
//! 共享格式合同统一扫描、解析和持久身份；扩展名只选择解析器，不证明内容合法。

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParserFamily {
    Pdf,
    Word,
    Office,
    Image,
    Archive,
    LegacyOffice,
}

#[derive(Clone, Copy, Debug)]
pub struct DocumentFormat {
    pub name: &'static str,
    pub family: ParserFamily,
    pub version: &'static str,
}

/// Return a canonical parser identity, including compound archive extensions.
/// 返回规范化解析身份，复合压缩后缀优先于最后一个扩展名。
pub fn detect(path: &Path) -> Option<DocumentFormat> {
    let filename = path.file_name()?.to_str()?.to_ascii_lowercase();
    for (suffix, name) in [
        (".tar.gz", "tar.gz"),
        (".tar.bz2", "tar.bz2"),
        (".tar.xz", "tar.xz"),
        (".tar.zst", "tar.zst"),
        (".tar.lz", "tar.lz"),
        (".tar.lzma", "tar.lzma"),
    ] {
        if filename.ends_with(suffix) {
            return known(name);
        }
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    known(&extension)
}

pub fn known(extension: &str) -> Option<DocumentFormat> {
    use ParserFamily::*;
    let (name, family, version) = match extension {
        "pdf" => ("pdf", Pdf, "rust-lopdf-text-v1"),
        "docx" | "docm" | "dotx" | "dotm" => ("docx", Word, "rust-docx-text-v1"),
        "png" => ("png", Image, "windows-image-ocr-text-v1"),
        "jpg" | "jpeg" => ("jpeg", Image, "windows-image-ocr-text-v1"),
        "bmp" => ("bmp", Image, "windows-image-ocr-text-v1"),
        "gif" => ("gif", Image, "windows-image-ocr-text-v1"),
        "tif" | "tiff" => ("tiff", Image, "windows-image-ocr-text-v1"),
        "webp" => ("webp", Image, "windows-image-ocr-text-v1"),
        "pptx" | "pptm" | "ppsx" | "ppsm" | "potx" | "potm" => {
            ("pptx", Office, "rust-office-text-v1")
        }
        "xlsx" | "xlsm" | "xltx" | "xltm" | "xlam" => ("xlsx", Office, "rust-office-text-v1"),
        "xls" | "xlt" => ("xls", Office, "rust-office-text-v1"),
        "xlsb" => ("xlsb", Office, "rust-office-text-v1"),
        "ods" | "ots" => ("ods", Office, "rust-office-text-v1"),
        "odt" | "ott" => ("odt", Office, "rust-office-text-v1"),
        "odp" | "otp" => ("odp", Office, "rust-office-text-v1"),
        "fodt" => ("fodt", Office, "rust-office-text-v1"),
        "fodp" => ("fodp", Office, "rust-office-text-v1"),
        "fods" => ("fods", Office, "rust-office-text-v1"),
        "epub" => ("epub", Office, "rust-office-text-v1"),
        "rtf" => ("rtf", Office, "rust-office-text-v1"),
        "html" | "htm" | "xhtml" => ("html", Office, "rust-office-text-v1"),
        "eml" => ("eml", Office, "rust-office-text-v1"),
        "mhtml" | "mht" => ("mhtml", Office, "rust-office-text-v1"),
        "mbox" => ("mbox", Office, "rust-office-text-v1"),
        "fb2" => ("fb2", Office, "rust-office-text-v1"),
        "svg" => ("svg", Office, "rust-office-text-v1"),
        "zip" | "cbz" | "jar" | "war" => ("zip", Archive, "rust-archive-members-v1"),
        "tar" => ("tar", Archive, "rust-archive-members-v1"),
        "gz" | "gzip" => ("gzip", Archive, "rust-archive-members-v1"),
        "tgz" | "tar.gz" => ("tar.gz", Archive, "rust-archive-members-v1"),
        "bz2" => ("bzip2", Archive, "rust-archive-members-v1"),
        "tbz" | "tbz2" | "tar.bz2" => ("tar.bz2", Archive, "rust-archive-members-v1"),
        "xz" => ("xz", Archive, "rust-archive-members-v1"),
        "txz" | "tar.xz" => ("tar.xz", Archive, "rust-archive-members-v1"),
        "zst" | "zstd" => ("zstd", Archive, "rust-archive-members-v1"),
        "tzst" | "tar.zst" => ("tar.zst", Archive, "rust-archive-members-v1"),
        "lz" | "lzip" => ("lzip", Archive, "rust-archive-members-v1"),
        "tar.lz" => ("tar.lz", Archive, "rust-archive-members-v1"),
        "lzma" => ("lzma", Archive, "rust-archive-members-v1"),
        "tar.lzma" => ("tar.lzma", Archive, "rust-archive-members-v1"),
        "7z" | "cb7" => ("7z", Archive, "toolhost-sharpcompress-members-v1"),
        "rar" | "cbr" => ("rar", Archive, "toolhost-sharpcompress-members-v1"),
        "ace" => ("ace", Archive, "toolhost-sharpcompress-members-v1"),
        "arc" => ("arc", Archive, "toolhost-sharpcompress-members-v1"),
        "arj" => ("arj", Archive, "toolhost-sharpcompress-members-v1"),
        "z" | "compress" => ("compress", Archive, "toolhost-sharpcompress-members-v1"),
        "doc" | "dot" => ("doc", LegacyOffice, "rust-msdoc-text-v1"),
        "ppt" | "pps" | "pot" => ("ppt", LegacyOffice, "rust-msppt-text-v1"),
        _ => return None,
    };
    Some(DocumentFormat {
        name,
        family,
        version,
    })
}

pub fn extracted_format(format: &str) -> bool {
    format == "image"
        || known(format).is_some_and(|entry| {
            matches!(
                entry.family,
                ParserFamily::Pdf
                    | ParserFamily::Word
                    | ParserFamily::Office
                    | ParserFamily::LegacyOffice
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_and_alias_formats_share_the_actual_parser() {
        assert_eq!(detect(Path::new("book.TAR.GZ")).unwrap().name, "tar.gz");
        assert_eq!(detect(Path::new("deck.PPTM")).unwrap().name, "pptx");
        assert_eq!(
            detect(Path::new("table.xlsb")).unwrap().family,
            ParserFamily::Office
        );
        assert_eq!(
            detect(Path::new("legacy.doc")).unwrap().family,
            ParserFamily::LegacyOffice
        );
        assert!(!extracted_format("zip"));
        assert!(extracted_format("doc"));
        assert!(extracted_format("ppt"));
        assert_eq!(
            detect(Path::new("template.DOT")).unwrap().version,
            "rust-msdoc-text-v1"
        );
        assert_eq!(
            detect(Path::new("show.PPS")).unwrap().version,
            "rust-msppt-text-v1"
        );
    }
}
