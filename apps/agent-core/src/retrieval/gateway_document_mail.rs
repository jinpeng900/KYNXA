//! MIME extraction publishes readable bodies and names without executing or fetching attachments.
//! MIME 提取保存可读正文与附件名称，不执行附件，也不自动抓取邮件链接。

use super::gateway_office_documents::{DocumentText, cancelled, failure, html_text};
use crate::storage::ContractError;
use mail_parser::{Message, MessageParser, MimeHeaders, PartType};
use serde_json::Value;
use std::sync::{Arc, atomic::AtomicBool};

fn message_text(
    message: &Message<'_>,
    output: &mut DocumentText,
    cancel: &Arc<AtomicBool>,
    depth: usize,
) -> Result<(), ContractError> {
    if depth > 16 || message.parts.len() > 4096 {
        return Err(failure("DOCUMENT_STRUCTURE_LIMIT"));
    }
    if let Some(subject) = message.subject() {
        output.append(&format!("Subject: {subject}\n"))?;
    }
    if let Some(addresses) = message.from() {
        for address in addresses.iter() {
            if let Some(name) = &address.name {
                output.append(name)?;
                output.append(" ")?;
            }
            if let Some(address) = &address.address {
                output.append(&format!("<{address}>\n"))?;
            }
        }
    }
    if let Some(date) = message.date() {
        output.append(&format!("Date: {}\n", date.to_rfc3339()))?;
    }
    let body_parts: Vec<_> = if message.text_body.is_empty() {
        message.html_bodies().collect()
    } else {
        message.text_bodies().collect()
    };
    for (index, part) in body_parts.into_iter().enumerate() {
        cancelled(cancel)?;
        if part.is_encoding_problem {
            return Err(failure("DOCUMENT_ENCODING_UNSUPPORTED"));
        }
        let name = format!("body {}", index + 1);
        let start = output.start_part("mail", &name)?;
        match &part.body {
            PartType::Text(text) => output.append(text)?,
            PartType::Html(text) => html_text(text.as_bytes(), output, cancel)?,
            _ => return Err(failure("DOCUMENT_PARSE_FAILED")),
        }
        output.end_part("mail", &name, start);
    }
    for part in message.attachments() {
        cancelled(cancel)?;
        if let PartType::Message(message) = &part.body {
            output.append("\n[Attached message]\n")?;
            message_text(message, output, cancel, depth + 1)?;
            continue;
        }
        if let Some(disposition) = part.content_disposition() {
            if let Some(name) = disposition.attribute("filename") {
                output.append(&format!(
                    "\n[Attachment filename: {name}; binary content not indexed]\n"
                ))?;
            }
        }
    }
    Ok(())
}

/// An MBOX envelope is recognized only at physical line boundaries; escaped body lines stay bodies.
/// MBOX 信封仅在真实行首识别，转义的正文 From 行不当成新邮件。
fn mailbox_ranges(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut starts = Vec::new();
    let mut position = 0usize;
    while position < bytes.len() {
        let end = bytes[position..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| position + offset + 1)
            .unwrap_or(bytes.len());
        let line = &bytes[position..end];
        if line.starts_with(b"From ") && line.iter().filter(|byte| **byte == b' ').count() >= 4 {
            starts.push((position, end));
        }
        position = end;
    }
    starts
        .iter()
        .enumerate()
        .map(|(index, (_, start))| {
            (
                *start,
                starts
                    .get(index + 1)
                    .map(|next| next.0)
                    .unwrap_or(bytes.len()),
            )
        })
        .collect()
}

pub(crate) fn decode(
    format: &str,
    bytes: &[u8],
    limits: &Value,
    cancel: &Arc<AtomicBool>,
    digest: &str,
) -> Result<Value, ContractError> {
    cancelled(cancel)?;
    let parser = MessageParser::default();
    let mut output = DocumentText::new(limits);
    let ranges = if format == "mbox" {
        let ranges = mailbox_ranges(bytes);
        if ranges.is_empty() || ranges.len() > 4096 {
            return Err(failure("DOCUMENT_PARSE_FAILED"));
        }
        ranges
    } else {
        vec![(0, bytes.len())]
    };
    for (index, (start, end)) in ranges.into_iter().enumerate() {
        cancelled(cancel)?;
        let message = parser
            .parse(&bytes[start..end])
            .ok_or_else(|| failure("DOCUMENT_PARSE_FAILED"))?;
        if message.text_body.is_empty() && message.html_body.is_empty() {
            return Err(failure("DOCUMENT_TEXT_UNAVAILABLE"));
        }
        let name = format!("message {}", index + 1);
        let start = output.start_part("message", &name)?;
        message_text(&message, &mut output, cancel, 0)?;
        output.end_part("message", &name, start);
    }
    output.finish(
        format,
        "rust-office-text-v1",
        digest,
        &["binary-attachments", "embedded-images", "external-links"],
    )
}
