//! Binary fixtures exercise source identity and live piece/story mapping end to end.
//! 二进制夹具端到端验证来源身份和活动片段/故事映射，不用 strings 模拟解析。

use super::*;
use std::{path::Path, sync::atomic::Ordering};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/legacy-word")
            .join(name),
    )
    .unwrap()
}
fn extract(name: &str, limits: Value) -> Result<Value, ContractError> {
    let bytes = fixture(name);
    let digest = crate::retrieval::gateway_index::hash_bytes(&bytes);
    decode(&bytes, &limits, &Arc::new(AtomicBool::new(false)), &digest)
}

#[test]
fn live_word_pieces_preserve_bilingual_stories_cached_fields_and_source_identity() {
    let result = extract("bilingual-stories.doc", json!({})).unwrap();
    let text = result["text"].as_str().unwrap();
    assert!(text.contains("Report 2026: mass\t 12.5 kg\t\n"));
    assert!(text.contains("中文正文：力学与量子；😀；官方网站"));
    assert!(!text.contains("HYPERLINK"));
    assert!(!text.contains("invalid.example"));
    assert!(!text.contains("DELETED OLD BODY"));
    assert!(!text.contains("历史正文"));
    assert!(text.find("Report 2026").unwrap() < text.find("中文正文").unwrap());
    for expected in [
        "Footnote:",
        "页眉：实验记录",
        "Comment:",
        "尾注：",
        "文本框：注意温度",
        "Header textbox:",
    ] {
        assert!(text.contains(expected), "missing {expected}");
    }
    assert_eq!(result["extraction"]["format"], "doc");
    assert_eq!(result["extraction"]["version"], "rust-msdoc-text-v1");
    assert_eq!(result["extraction"]["parts"].as_array().unwrap().len(), 7);
    assert_eq!(result["documentComplete"], true);
    let bytes = fixture("bilingual-stories.doc");
    assert_eq!(
        result["extraction"]["rawContentHash"],
        crate::retrieval::gateway_index::hash_bytes(&bytes)
    );
    crate::storage::retrieval::apply(
        "storage.retrieval.extraction",
        &json!({"value":result["extraction"],"textLength":text.encode_utf16().count()}),
    )
    .unwrap();
    let mut changed = result["extraction"].clone();
    changed["rawContentHash"] = json!("different-original-bytes");
    assert_eq!(
        crate::retrieval::gateway_documents::verify_extraction(&result["extraction"], &changed)
            .unwrap_err()
            .code(),
        "STALE_RETRIEVAL_SOURCE"
    );
}

#[test]
fn encrypted_old_damaged_or_cancelled_word_never_returns_a_prefix() {
    for (name, code) in [
        ("encrypted.doc", "DOCUMENT_ENCRYPTED_UNSUPPORTED"),
        ("word95.doc", "DOCUMENT_LEGACY_WORD_VERSION_UNSUPPORTED"),
        ("bad-piece-order.doc", "DOCUMENT_PARSE_FAILED"),
        ("bad-surrogate.doc", "DOCUMENT_TEXT_ENCODING_UNSUPPORTED"),
    ] {
        assert_eq!(extract(name, json!({})).unwrap_err().code(), code);
    }
    for (limits, code) in [
        (
            json!({"maximumDocumentInputBytes":32}),
            "DOCUMENT_INPUT_LIMIT",
        ),
        (
            json!({"maximumArchiveEntryBytes":32}),
            "DOCUMENT_ARCHIVE_LIMIT",
        ),
        (json!({"maximumArchiveEntries":1}), "DOCUMENT_ARCHIVE_LIMIT"),
        (
            json!({"maximumDocumentCharacters":4}),
            "DOCUMENT_STRUCTURE_LIMIT",
        ),
        (
            json!({"maximumDocumentPieces":1}),
            "DOCUMENT_STRUCTURE_LIMIT",
        ),
        (
            json!({"maximumDocumentOutputBytes":16}),
            "DOCUMENT_OUTPUT_LIMIT",
        ),
    ] {
        assert_eq!(
            extract("bilingual-stories.doc", limits).unwrap_err().code(),
            code
        );
    }
    let bytes = fixture("bilingual-stories.doc");
    let cancel = Arc::new(AtomicBool::new(true));
    assert_eq!(
        decode(&bytes, &json!({}), &cancel, "digest")
            .unwrap_err()
            .code(),
        "ABORT_ERR"
    );
    cancel.store(false, Ordering::Release);
    for length in [0, 8, 100, bytes.len() / 2] {
        assert!(decode(&bytes[..length], &json!({}), &cancel, "digest").is_err());
    }
}

#[test]
fn compressed_mapping_and_surrogates_follow_binary_contract_not_language_guessing() {
    assert_eq!(compressed_unit(0x80), 0x80);
    assert_eq!(compressed_unit(0x82), 0x201a);
    assert_eq!(compressed_unit(0x93), 0x201c);
    assert_eq!(compressed_unit(0xd6), 0xd6);
    let bytes = fixture("bilingual-stories.doc");
    let cancel = Arc::new(AtomicBool::new(false));
    let mut compound = CompoundDocument::open(&bytes, &json!({}), &cancel).unwrap();
    let word = compound.read("/WordDocument", &cancel).unwrap();
    let fib = read_fib(&word, &json!({})).unwrap();
    let table = compound.read("/1Table", &cancel).unwrap();
    let plc = piece_table(
        range(&table, fib.fc_clx, fib.lcb_clx).unwrap(),
        &json!({}),
        &cancel,
    )
    .unwrap();
    let mut invalid = plc.to_vec();
    invalid[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(read_characters(&word, &invalid, &fib, &cancel).is_err());
    let mut too_far = plc.to_vec();
    let descriptors = ((plc.len() - 4) / 12 + 1) * 4;
    too_far[descriptors + 2..descriptors + 6].copy_from_slice(&0x7fff_fffeu32.to_le_bytes());
    assert!(read_characters(&word, &too_far, &fib, &cancel).is_err());
    let mut header_as_text = plc.to_vec();
    header_as_text[descriptors + 2..descriptors + 6].copy_from_slice(&0x4000_0000u32.to_le_bytes());
    assert!(read_characters(&word, &header_as_text, &fib, &cancel).is_err());
    let mut output = DocumentText::new(&json!({}));
    let malformed_field = "\u{13} instruction without end"
        .encode_utf16()
        .collect::<Vec<_>>();
    assert!(append_story(&mut output, &malformed_field, &cancel).is_err());
}

#[test]
fn apache_poi_producer_files_match_independent_header_note_and_comment_oracles() {
    let unicode = extract("poi-HeaderFooterUnicode.doc", json!({})).unwrap();
    let text = unicode["text"].as_str().unwrap();
    assert!(text.contains("This is a simple header, with a € euro symbol in it."));
    assert!(text.contains("The footer, with Molière, has Unicode in it."));
    let notes = extract("poi-footnote.doc", json!({})).unwrap();
    let text = notes["text"].as_str().unwrap();
    assert!(text.contains("TestFootnote"));
    assert!(text.contains("TestEndnote"));
    assert!(text.contains("TestComment"));
    assert!(
        notes["extraction"]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["name"].as_str().unwrap().starts_with("comments"))
    );
}
