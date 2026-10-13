//! Load the real packaged SQLite vector library using the default Windows layout.
//! 按默认 Windows 布局加载真实随包 SQLite 向量库，不用路径存在检查代替可用性。

#[cfg(windows)]
#[test]
fn default_native_vector_layout_loads_and_arbitrary_parent_is_rejected() {
    use kynxa_agent_core_native::storage::sqlite_vector_reads;
    use serde_json::json;
    use std::path::Path;
    let application = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("artifacts/runtime/native-desktop/win-x64");
    let extension = application.join("runtime/native/vec0.dll");
    assert!(
        extension.is_file(),
        "prepare the real native bundle before this deployment check"
    );
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let info =
        sqlite_vector_reads::extension_info(&connection, &json!({"extensionPath":extension}))
            .unwrap();
    assert!(!info["version"].as_str().unwrap().is_empty());
    assert_eq!(info["owner"], "rust-sqlite");
    let invalid = rusqlite::Connection::open_in_memory().unwrap();
    let error = sqlite_vector_reads::extension_info(
        &invalid,
        &json!({"extensionPath":application.join("untrusted/vec0.dll")}),
    )
    .unwrap_err();
    assert_eq!(error.code(), "RETRIEVAL_VECTOR_UNAVAILABLE");
}
