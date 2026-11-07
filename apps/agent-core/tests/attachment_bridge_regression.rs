use std::{fs, io::Cursor, path::PathBuf, sync::Arc};

use base64::{Engine, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use kynxa_agent_core_native::{
    storage::{gateway as storage, message_attachments},
    tools::gateway::{self, GatewayToolContext, ToolRuntime},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

struct Fixture {
    root: PathBuf,
    context: GatewayToolContext,
    cancellation: watch::Sender<bool>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kynxa-attachment-bridge-{}", uuid::Uuid::new_v4()));
        for directory in ["work", "data", "extensions", "official/Skills"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        let (cancellation, receiver) = watch::channel(false);
        let context = GatewayToolContext {
            data_home: root.join("data"),
            extension_root: root.join("extensions"),
            workdir: root.join("work"),
            toolhost_exe: root.join("ToolHost.exe"),
            permission_mode: "smart".into(),
            conversation_id: uuid::Uuid::new_v4().to_string(),
            project_id: None,
            authorized_roots: vec![root.join("work")],
            cancellation: receiver,
            runtime: Arc::new(ToolRuntime::default()),
            config: json!({"officialToolsRoot":root.join("official"),"mcpServers":[]}),
            approved: false,
            events: None,
            external_execute: None,
        };
        storage::ensure_conversation(&context.data_home, &context.conversation_id, "Attachment bridge fixture").unwrap();
        Self { root, context, cancellation }
    }

    fn save(&self, name: &str, bytes: &[u8]) -> Value {
        let message = uuid::Uuid::new_v4().to_string();
        let attachment = uuid::Uuid::new_v4().to_string();
        storage::upsert_message(&self.context.data_home, &self.context.conversation_id, &json!({
            "Id":message,"Role":"user","Content":"Read or export the original attachment.",
            "Attachments":[{"Id":attachment,"Name":name,"SizeBytes":bytes.len()}]
        })).unwrap();
        let saved = message_attachments::save(&self.context.data_home, &self.context.conversation_id,
            &message, &attachment, name, bytes).unwrap();
        json!({"messageId":message,"attachmentId":attachment,"expectedSha256":saved["sha256"]})
    }

    fn assert_workspace_empty(&self) {
        assert_eq!(fs::read_dir(&self.context.workdir).unwrap().count(), 0);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Remove only this explicitly named fixture directly below the canonical temporary directory.
        let target = self.root.canonicalize().unwrap();
        let temporary = std::env::temp_dir().canonicalize().unwrap();
        assert_eq!(target.parent(), Some(temporary.as_path()));
        assert!(target.file_name().unwrap().to_string_lossy().starts_with("kynxa-attachment-bridge-"));
        fs::remove_dir_all(target).unwrap();
    }
}

fn png() -> Vec<u8> {
    let image = DynamicImage::ImageRgb8(RgbImage::from_fn(80, 40, |x, _| {
        if x < 40 { Rgb([255, 0, 0]) } else { Rgb([0, 0, 255]) }
    }));
    let mut encoded = Cursor::new(Vec::new());
    image.write_to(&mut encoded, ImageFormat::Png).unwrap();
    encoded.into_inner()
}

#[tokio::test]
async fn cached_image_without_ocr_text_still_has_a_readable_exportable_original() {
    let fixture = Fixture::new();
    let bytes = png();
    let identity = fixture.save("no-text.png", &bytes);
    let cached = json!({"parserVersion":"message-attachment-v1","sourceVersion":identity["expectedSha256"],
        "text":"","complete":false,"error":"No recognized text","errorCode":"DOCUMENT_TEXT_UNAVAILABLE"});
    message_attachments::save_extraction(&fixture.context.data_home, &fixture.context.conversation_id,
        identity["messageId"].as_str().unwrap(), identity["attachmentId"].as_str().unwrap(),
        identity["expectedSha256"].as_str().unwrap(), &cached).unwrap();
    let extracted = gateway::execute("attachment.read", identity.clone(), &fixture.context).await.unwrap();
    assert_eq!(extracted["textState"], "empty");
    assert_eq!(extracted["originalAvailable"], true);
    assert_eq!(extracted["sourceVersion"], identity["expectedSha256"]);
    assert!(extracted.get("error").is_none() && extracted.get("errorCode").is_none());
    let listed = message_attachments::list(&fixture.context.data_home, &fixture.context.conversation_id,
        identity["messageId"].as_str().unwrap()).unwrap();
    assert_eq!(listed["attachments"][0]["status"], "ready");
    assert!(listed["attachments"][0]["error"].is_null());
    let image = gateway::execute("image.read", identity.clone(), &fixture.context).await.unwrap();
    assert_eq!(STANDARD.decode(image["content"][0]["data"].as_str().unwrap()).unwrap(), bytes);
    let mut destination = identity;
    destination["path"] = json!("original.png");
    gateway::execute("attachment.export", destination, &fixture.context).await.unwrap();
    assert_eq!(fs::read(fixture.context.workdir.join("original.png")).unwrap(), bytes);
}

#[test]
fn original_exports_do_not_invent_quality_checks_or_progress_when_reused() {
    use kynxa_agent_core_native::orchestration::progress::RunProgress;
    let mut progress = RunProgress::default();
    let call = json!({"id":"copy","name":"attachment.export","arguments":{"path":"original.pdf"}});
    let body = json!({"path":"original.pdf","bytes":3,"sha256":"a".repeat(64),"created":true,"executed":true,"reused":false});
    progress.apply("progress.observeOutcome", &json!({"call":call,"result":{"status":"completed","content":body}})).unwrap();
    let state = progress.apply("progress.state", &json!({})).unwrap();
    assert_eq!(state["mutationRevision"], 1);
    assert_eq!(state["codeMutationRevision"], 0);
    assert!(state["requiredChecks"].as_array().unwrap().is_empty());
    let mut reused = body;
    reused["executed"] = json!(false);
    reused["reused"] = json!(true);
    reused["created"] = json!(false);
    progress.apply("progress.observeOutcome", &json!({"call":call,"result":{"status":"completed","content":reused}})).unwrap();
    assert_eq!(progress.apply("progress.state", &json!({})).unwrap()["mutationRevision"], 1);
    let mut code_call = call;
    code_call["arguments"]["path"] = json!("script.py");
    progress.apply("progress.observeOutcome", &json!({"call":code_call,"result":{"status":"completed","content":{
        "path":"script.py","bytes":3,"sha256":"b".repeat(64),"created":true,"executed":true,"reused":false,"codeTarget":true}}})).unwrap();
    assert_eq!(progress.apply("progress.state", &json!({})).unwrap()["codeMutationRevision"], 2);
}

#[tokio::test]
async fn original_attachment_pixels_and_export_preserve_the_same_bytes_and_hash() {
    let fixture = Fixture::new();
    let bytes = png();
    let identity = fixture.save("参考原图.png", &bytes);
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let viewed = gateway::execute("image.read", identity.clone(), &fixture.context).await.unwrap();
    let sent = STANDARD.decode(viewed["content"][0]["data"].as_str().unwrap()).unwrap();
    assert_eq!(sent, bytes);
    assert_eq!(viewed["structuredContent"]["sha256"], hash);
    assert_eq!(viewed["structuredContent"]["sourceVersion"], hash);
    let decoded = image::load_from_memory(&sent).unwrap().to_rgb8();
    assert_eq!(decoded.dimensions(), (80, 40));
    assert_eq!(decoded.get_pixel(10, 10), &Rgb([255, 0, 0]));
    assert_eq!(decoded.get_pixel(60, 10), &Rgb([0, 0, 255]));
    for field in ["path", "contentUrl", "previewStatus", "imagePreview"] {
        assert!(viewed.get(field).is_none());
        assert!(viewed["structuredContent"].get(field).is_none());
    }
    fixture.assert_workspace_empty();
    let mut args = identity.clone();
    args["path"] = json!("exported.png");
    let exported = gateway::execute("attachment.export", args.clone(), &fixture.context).await.unwrap();
    assert_eq!(fs::read(fixture.context.workdir.join("exported.png")).unwrap(), bytes);
    assert_eq!(exported["sha256"], hash);
    assert_eq!(exported["sourceVersion"], hash);
    assert_eq!(exported["sizeBytes"], bytes.len());
    assert_eq!(exported["name"], "参考原图.png");
    assert_eq!(exported["sourceId"], viewed["structuredContent"]["sourceId"]);
    assert_eq!(exported["executed"], true);
    assert_eq!(exported["reused"], false);
    let original_modified = fs::metadata(fixture.context.workdir.join("exported.png")).unwrap().modified().unwrap();
    let reused = gateway::execute("attachment.export", args, &fixture.context).await.unwrap();
    assert_eq!(reused["executed"], false);
    assert_eq!(reused["reused"], true);
    assert_eq!(fs::metadata(fixture.context.workdir.join("exported.png")).unwrap().modified().unwrap(), original_modified);
    assert_eq!(fs::read_dir(&fixture.context.workdir).unwrap().count(), 1);
}

#[tokio::test]
async fn export_conflict_never_overwrites_even_a_same_length_different_file() {
    let fixture = Fixture::new();
    let bytes = png();
    let mut args = fixture.save("reference.png", &bytes);
    args["path"] = json!("occupied.png");
    let existing = vec![42_u8; bytes.len()];
    fs::write(fixture.context.workdir.join("occupied.png"), &existing).unwrap();
    let error = gateway::execute("attachment.export", args, &fixture.context).await.unwrap_err();
    assert_eq!(error.code, "TOOL_FILE_CONFLICT");
    assert_eq!(fs::read(fixture.context.workdir.join("occupied.png")).unwrap(), existing);
    assert_eq!(fs::read_dir(&fixture.context.workdir).unwrap().count(), 1);
}

#[tokio::test]
async fn attachment_identity_and_source_hash_are_checked_before_any_export() {
    let fixture = Fixture::new();
    let mut args = fixture.save("reference.png", &png());
    args["path"] = json!("must-not-exist.png");
    let mut other = fixture.context.clone();
    other.conversation_id = uuid::Uuid::new_v4().to_string();
    storage::ensure_conversation(&other.data_home, &other.conversation_id, "Other fixture conversation").unwrap();
    assert_eq!(gateway::execute("attachment.export", args.clone(), &other).await.unwrap_err().code, "MESSAGE_NOT_FOUND");
    args["expectedSha256"] = json!("0".repeat(64));
    assert_eq!(gateway::execute("attachment.export", args.clone(), &fixture.context).await.unwrap_err().code, "ATTACHMENT_VERSION_CHANGED");
    args.as_object_mut().unwrap().remove("path");
    assert_eq!(gateway::execute("image.read", args, &fixture.context).await.unwrap_err().code, "ATTACHMENT_VERSION_CHANGED");
    fixture.assert_workspace_empty();
}

#[tokio::test]
async fn cancellation_and_ask_approval_leave_the_workspace_untouched() {
    let fixture = Fixture::new();
    let mut args = fixture.save("reference.png", &png());
    args["path"] = json!("approved.png");
    let mut ask = fixture.context.clone();
    ask.permission_mode = "ask".into();
    let approval = gateway::execute("attachment.export", args.clone(), &ask).await.unwrap();
    assert_eq!(approval["requiresApproval"], true);
    assert_eq!(approval["invocation"]["name"], "attachment.export");
    assert!(approval.get("sourceVersion").is_none());
    assert!(approval.get("executed").is_none());
    fixture.assert_workspace_empty();
    fixture.cancellation.send(true).unwrap();
    assert_eq!(gateway::execute("attachment.export", args, &fixture.context).await.unwrap_err().code, "TOOL_CANCELLED");
    fixture.assert_workspace_empty();
}

#[tokio::test]
async fn nonimage_attachments_export_their_original_bytes_without_text_or_image_decoding() {
    let fixture = Fixture::new();
    for (name, bytes) in [
        ("original.pdf", b"%PDF-original\0\xff".as_slice()),
        ("original.zip", b"PK\x03\x04\0binary\xfe".as_slice()),
        ("original.txt", b"original\r\ntext\0unchanged".as_slice()),
    ] {
        let mut args = fixture.save(name, bytes);
        args["path"] = json!(name);
        let result = gateway::execute("attachment.export", args, &fixture.context).await.unwrap();
        assert_eq!(fs::read(fixture.context.workdir.join(name)).unwrap(), bytes);
        assert_eq!(result["sha256"], format!("{:x}", Sha256::digest(bytes)));
        assert_eq!(result["executed"], true);
    }
    assert_eq!(fs::read_dir(&fixture.context.workdir).unwrap().count(), 3);
}

#[tokio::test]
async fn full_mode_uses_existing_outside_path_permissions_and_protects_formal_app_data() {
    let fixture = Fixture::new();
    let bytes = png();
    let mut args = fixture.save("reference.png", &bytes);
    let outside = fixture.root.join("outside.png");
    args["path"] = json!(outside);
    let mut full = fixture.context.clone();
    full.permission_mode = "full".into();
    let result = gateway::execute("attachment.export", args.clone(), &full).await.unwrap();
    assert_eq!(result["executed"], true);
    assert_eq!(fs::read(&outside).unwrap(), bytes);
    let protected = full.data_home.join("must-not-exist.png");
    args["path"] = json!(protected);
    assert_eq!(gateway::execute("attachment.export", args, &full).await.unwrap_err().code, "PROTECTED_APP_DATA");
    assert!(!protected.exists());
}
