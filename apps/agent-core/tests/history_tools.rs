//! Exercise model-facing history tools against formal temporary storage, without providers or credentials.
//! 用正式临时存储验证模型历史工具，不启动供应商、不读取日常数据或使用真实凭据。

use kynxa_agent_core_native::{
    gateway_contract::{GatewayConfig, new_id},
    retrieval::gateway_tools,
    storage::{ContractError, gateway as storage_gateway},
};
use serde_json::{Value, json};
use std::{collections::HashSet, path::PathBuf};

struct Fixture {
    root: PathBuf,
    config: GatewayConfig,
    project: String,
    other_project: String,
    current: String,
    sibling: String,
    foreign: String,
    archived: String,
    standalone: String,
    current_user: String,
    sibling_user: String,
    foreign_user: String,
    archived_user: String,
    standalone_user: String,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kynxa-history-tools-{}", new_id()));
        for directory in ["Models", "Data", "Extensions", "work", "other-work"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        let config = GatewayConfig {
            data_home: root.join("Models"),
            data_root: root.join("Data"),
            extension_root: root.join("Extensions"),
            application_root: root.join("Application"),
            port: 0,
            toolhost_exe: root.join("unused-toolhost.exe"),
            vector_extension: root.join("unused-vec0.dll"),
            inference_library: root.join("unused-onnxruntime.dll"),
        };
        let fixture = Self {
            root,
            config,
            project: new_id(),
            other_project: new_id(),
            current: new_id(),
            sibling: new_id(),
            foreign: new_id(),
            archived: new_id(),
            standalone: new_id(),
            current_user: new_id(),
            sibling_user: new_id(),
            foreign_user: new_id(),
            archived_user: new_id(),
            standalone_user: new_id(),
        };
        for (chat, message, content) in [
            (
                &fixture.current,
                &fixture.current_user,
                "曾问过物理学的惯性问题。CURRENT_USER",
            ),
            (
                &fixture.sibling,
                &fixture.sibling_user,
                "曾问过物理学的波动问题。SIBLING_ONLY",
            ),
            (
                &fixture.foreign,
                &fixture.foreign_user,
                "物理学 FOREIGN_PRIVATE",
            ),
            (
                &fixture.archived,
                &fixture.archived_user,
                "物理学 ARCHIVED_PRIVATE",
            ),
            (
                &fixture.standalone,
                &fixture.standalone_user,
                "物理学 STANDALONE_ONLY",
            ),
        ] {
            storage_gateway::ensure_conversation(&fixture.config.data_root, chat, "历史工具夹具")
                .unwrap();
            fixture.write_message(chat, message, "user", content);
        }
        fixture.write_message(
            &fixture.current,
            &new_id(),
            "assistant",
            "物理学 ASSISTANT_ONLY，不是用户曾提出的问题。",
        );

        // Move existing saved chats through the same CAS catalog operation used by the desktop.
        // 使用桌面相同的目录 CAS 操作移动已保存聊天，不直接构造另一份权限或历史状态。
        fixture.change_catalog(|catalog| {
            let mut existing = std::mem::take(catalog["Chats"].as_array_mut().unwrap());
            let mut take_chat = |id: &str| {
                let position = existing.iter().position(|chat| chat["Id"] == id).unwrap();
                existing.remove(position)
            };
            let current = take_chat(&fixture.current);
            let sibling = take_chat(&fixture.sibling);
            let mut archived = take_chat(&fixture.archived);
            archived["IsArchived"] = json!(true);
            let foreign = take_chat(&fixture.foreign);
            catalog["Projects"] = json!([
                {"Id":fixture.project,"Name":"当前工作","FolderPath":fixture.root.join("work"),
                    "IsFolderlessWorkspace":false,"IsArchived":false,"IsPinned":false,
                    "Chats":[current,sibling,archived]},
                {"Id":fixture.other_project,"Name":"其他工作","FolderPath":fixture.root.join("other-work"),
                    "IsFolderlessWorkspace":false,"IsArchived":false,"IsPinned":false,"Chats":[foreign]}
            ]);
            catalog["Chats"] = json!(existing);
        });
        fixture
    }

    fn change_catalog(&self, change: impl FnOnce(&mut Value)) {
        let mut catalog = storage_gateway::load_catalog(&self.config.data_root).unwrap();
        change(&mut catalog);
        let (status, _) = storage_gateway::handle(
            "PUT",
            "/api/conversations/catalog",
            &[("includeMessages".into(), "false".into())],
            &catalog,
            &self.config.data_root,
        )
        .expect("formal catalog route exists")
        .unwrap();
        assert_eq!(status, 200);
    }

    fn write_message(&self, chat: &str, message_id: &str, role: &str, content: &str) {
        storage_gateway::upsert_message(
            &self.config.data_root,
            chat,
            &json!({"Id":message_id,"Role":role,"Content":content,
                "CreatedAt":"2026-10-12T00:00:00.000Z","Status":"completed"}),
        )
        .unwrap();
    }

    fn context(&self) -> Value {
        json!({"conversationId":self.current,"requestId":new_id(),
            "projectId":self.project,"expectedProjectId":self.project,
            "expectedFolderPath":self.root.join("work"),"permissionMode":"smart"})
    }

    fn standalone_context(&self) -> Value {
        json!({"conversationId":self.standalone,"requestId":new_id(),
            "projectId":null,"expectedProjectId":null,"expectedFolderPath":null,"permissionMode":"smart"})
    }

    fn execute(&self, name: &str, input: Value, context: &Value) -> Result<Value, ContractError> {
        gateway_tools::handle_tool(name, &input, context, &self.config)
            .expect("history specialist route exists")
    }

    fn search(&self, input: Value, context: &Value) -> Value {
        self.execute("conversation.history.search", input, context)
            .unwrap()
    }

    fn sibling_row(&self) -> Value {
        self.search(
            json!({"scope":"work","role":"user","query":"SIBLING_ONLY"}),
            &self.context(),
        )["messages"][0]
            .clone()
    }

    fn read_input(row: &Value) -> Value {
        json!({"messageId":row["messageId"],"sourceConversationId":row["conversationId"],
            "sourceVersion":row["sourceVersion"],"offset":0,"limit":4096})
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Every fixture path originates from this unique temporary owner, never from application configuration.
        // 每个夹具路径来自独立临时所有者，不读取或清理用户应用配置中的路径。
        assert!(
            self.root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("kynxa-history-tools-")
        );
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn assert_error(result: Result<Value, ContractError>, code: &str, status: u16) {
    let error = result.expect_err("operation must be rejected");
    assert_eq!(error.code(), code);
    assert_eq!(error.status_code(), status);
}

#[test]
fn work_search_reads_user_evidence_with_source_identity_and_default_chat_stays_local() {
    let fixture = Fixture::new();
    let context = fixture.context();
    let result = fixture.search(
        json!({"scope":"work","role":"user","query":"物理学"}),
        &context,
    );
    let rows = result["messages"].as_array().unwrap();
    let sources = rows
        .iter()
        .map(|row| row["conversationId"].as_str().unwrap())
        .collect::<HashSet<_>>();
    assert_eq!(
        sources,
        HashSet::from([fixture.current.as_str(), fixture.sibling.as_str()])
    );
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row["role"], "user");
        let version = row["sourceVersion"].as_str().unwrap();
        assert_eq!(version.len(), 64);
        assert!(version.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let read = fixture
            .execute(
                "conversation.history.read",
                Fixture::read_input(row),
                &context,
            )
            .unwrap();
        assert_eq!(read["conversationId"], row["conversationId"]);
        assert_eq!(read["messageId"], row["messageId"]);
        assert!(read["text"].as_str().unwrap().contains("物理学"));
    }
    assert_eq!(
        fixture.search(json!({"query":"SIBLING_ONLY","role":"user"}), &context)["total"],
        0
    );
    assert_eq!(
        fixture.search(
            json!({"scope":"work","query":"ASSISTANT_ONLY","role":"user"}),
            &context
        )["total"],
        0
    );
    assert_eq!(
        fixture.search(
            json!({"scope":"work","query":"ASSISTANT_ONLY","role":"assistant"}),
            &context
        )["total"],
        1
    );
}

#[test]
fn standalone_null_scope_is_usable_and_cannot_expand_to_a_work() {
    let fixture = Fixture::new();
    let context = fixture.standalone_context();
    let result = fixture.search(json!({"query":"STANDALONE_ONLY","role":"user"}), &context);
    assert_eq!(result["total"], 1);
    assert_eq!(result["messages"][0]["messageId"], fixture.standalone_user);
    let read = fixture
        .execute(
            "conversation.history.read",
            Fixture::read_input(&result["messages"][0]),
            &context,
        )
        .unwrap();
    assert!(read["text"].as_str().unwrap().contains("STANDALONE_ONLY"));
    assert_error(
        fixture.execute(
            "conversation.history.search",
            json!({"scope":"work","query":"物理学"}),
            &context,
        ),
        "CONVERSATION_HISTORY_SCOPE_REQUIRED",
        403,
    );
}

#[test]
fn work_history_rejects_foreign_archived_and_unknown_sources() {
    let fixture = Fixture::new();
    let context = fixture.context();
    assert_error(
        fixture.execute(
            "conversation.history.read",
            json!({"sourceConversationId":fixture.foreign,"messageId":fixture.foreign_user}),
            &context,
        ),
        "CONVERSATION_HISTORY_SCOPE_FORBIDDEN",
        403,
    );
    assert_error(
        fixture.execute(
            "conversation.history.read",
            json!({"sourceConversationId":fixture.archived,"messageId":fixture.archived_user}),
            &context,
        ),
        "CONVERSATION_HISTORY_ARCHIVED",
        409,
    );
    assert_error(
        fixture.execute(
            "conversation.history.read",
            json!({"sourceConversationId":new_id(),"messageId":new_id()}),
            &context,
        ),
        "CONVERSATION_NOT_FOUND",
        404,
    );
    assert_error(
        fixture.execute(
            "conversation.history.read",
            json!({"messageId":new_id()}),
            &context,
        ),
        "CONVERSATION_HISTORY_MESSAGE_NOT_FOUND",
        404,
    );
}

#[test]
fn moved_current_chat_preserves_workspace_changed_error_for_search_and_read() {
    let fixture = Fixture::new();
    let old_context = fixture.context();
    let old_row =
        fixture.search(json!({"query":"CURRENT_USER"}), &old_context)["messages"][0].clone();
    fixture.change_catalog(|catalog| {
        let current_chats = catalog["Projects"][0]["Chats"].as_array_mut().unwrap();
        let position = current_chats
            .iter()
            .position(|chat| chat["Id"] == fixture.current)
            .unwrap();
        let moved = current_chats.remove(position);
        catalog["Projects"][1]["Chats"]
            .as_array_mut()
            .unwrap()
            .push(moved);
    });
    assert_error(
        fixture.execute(
            "conversation.history.search",
            json!({"query":"CURRENT_USER"}),
            &old_context,
        ),
        "WORKSPACE_CHANGED",
        409,
    );
    assert_error(
        fixture.execute(
            "conversation.history.read",
            Fixture::read_input(&old_row),
            &old_context,
        ),
        "WORKSPACE_CHANGED",
        409,
    );
}

#[test]
fn deleted_and_newly_archived_sibling_sources_cannot_reuse_old_references() {
    let fixture = Fixture::new();
    let old_row = fixture.sibling_row();
    fixture.change_catalog(|catalog| {
        let sibling = catalog["Projects"][0]["Chats"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|chat| chat["Id"] == fixture.sibling)
            .unwrap();
        sibling["IsArchived"] = json!(true);
    });
    assert_error(
        fixture.execute(
            "conversation.history.read",
            Fixture::read_input(&old_row),
            &fixture.context(),
        ),
        "CONVERSATION_HISTORY_ARCHIVED",
        409,
    );
    fixture.change_catalog(|catalog| {
        catalog["Projects"][0]["Chats"]
            .as_array_mut()
            .unwrap()
            .retain(|chat| chat["Id"] != fixture.sibling);
    });
    assert_error(
        fixture.execute(
            "conversation.history.read",
            Fixture::read_input(&old_row),
            &fixture.context(),
        ),
        "CONVERSATION_DELETED",
        410,
    );
}

#[test]
fn another_message_append_preserves_the_referenced_message_version() {
    let fixture = Fixture::new();
    let old_row = fixture.sibling_row();
    fixture.write_message(
        &fixture.sibling,
        &new_id(),
        "user",
        "新增公开用户内容 VERSION_CHANGED",
    );
    let read = fixture
        .execute(
            "conversation.history.read",
            Fixture::read_input(&old_row),
            &fixture.context(),
        )
        .unwrap();
    assert!(read["text"].as_str().unwrap().contains("SIBLING_ONLY"));
    assert_eq!(
        fixture.sibling_row()["sourceVersion"],
        old_row["sourceVersion"]
    );
}

#[test]
fn edited_target_rejects_old_message_version_and_exposes_new_search_evidence() {
    let fixture = Fixture::new();
    let old_row = fixture.sibling_row();
    fixture.write_message(
        &fixture.sibling,
        &fixture.sibling_user,
        "user",
        "已纠正物理学原文 SIBLING_ONLY TARGET_EDITED",
    );
    assert_error(
        fixture.execute(
            "conversation.history.read",
            Fixture::read_input(&old_row),
            &fixture.context(),
        ),
        "CONVERSATION_HISTORY_SOURCE_CHANGED",
        409,
    );
    let new_row = fixture.sibling_row();
    assert_eq!(new_row["messageId"], old_row["messageId"]);
    assert_ne!(new_row["sourceVersion"], old_row["sourceVersion"]);
    let read = fixture
        .execute(
            "conversation.history.read",
            Fixture::read_input(&new_row),
            &fixture.context(),
        )
        .unwrap();
    assert!(read["text"].as_str().unwrap().contains("TARGET_EDITED"));
}

#[test]
fn work_history_cursor_pages_do_not_drop_or_repeat_messages() {
    let fixture = Fixture::new();
    let mut expected = HashSet::new();
    for index in 0..7 {
        let chat = if index % 2 == 0 {
            &fixture.current
        } else {
            &fixture.sibling
        };
        let message = new_id();
        fixture.write_message(
            chat,
            &message,
            "user",
            &format!("页码物理检索 fixture {index}"),
        );
        expected.insert((chat.clone(), message));
    }
    let context = fixture.context();
    let mut input = json!({"scope":"work","role":"user","query":"页码物理检索","limit":2});
    let mut observed = HashSet::new();
    let mut pages = 0;
    loop {
        let result = fixture.search(input.clone(), &context);
        pages += 1;
        assert!(pages <= 4, "cursor must finish in bounded pages");
        for row in result["messages"].as_array().unwrap() {
            assert_eq!(row["role"], "user");
            let identity = (
                row["conversationId"].as_str().unwrap().to_owned(),
                row["messageId"].as_str().unwrap().to_owned(),
            );
            assert!(
                observed.insert(identity),
                "a cursor page must not replay a prior message"
            );
        }
        assert!(result["total"].as_u64().unwrap() >= observed.len() as u64);
        if result["hasMore"] == false {
            assert_eq!(result["total"], 7);
            assert_eq!(result["totalIsExact"], true);
            break;
        }
        assert_eq!(result["totalIsExact"], false);
        let cursor = result["nextCursor"]
            .as_str()
            .expect("continued page needs a cursor");
        assert!(!cursor.is_empty());
        input["cursor"] = json!(cursor);
    }
    assert_eq!(pages, 4);
    assert_eq!(observed, expected);
}

#[test]
fn history_cursor_binds_query_role_and_scope() {
    let fixture = Fixture::new();
    let context = fixture.context();
    let first = fixture.search(
        json!({"scope":"work","role":"user","query":"物理学","limit":1}),
        &context,
    );
    let cursor = first["nextCursor"]
        .as_str()
        .expect("two user hits require continuation");
    for changed in [
        json!({"scope":"work","role":"user","query":"不同查询","limit":1,"cursor":cursor}),
        json!({"scope":"work","role":"assistant","query":"物理学","limit":1,"cursor":cursor}),
        json!({"scope":"chat","role":"user","query":"物理学","limit":1,"cursor":cursor}),
    ] {
        assert_error(
            fixture.execute("conversation.history.search", changed, &context),
            "CONVERSATION_HISTORY_CURSOR_INVALID",
            400,
        );
    }
}

#[test]
fn history_cursor_rejects_changed_page_sources_instead_of_skipping_new_evidence() {
    let fixture = Fixture::new();
    let context = fixture.context();
    let first = fixture.search(
        json!({"scope":"work","role":"user","query":"物理学","limit":1}),
        &context,
    );
    let cursor = first["nextCursor"]
        .as_str()
        .expect("two user hits require continuation");
    fixture.write_message(
        first["messages"][0]["conversationId"].as_str().unwrap(),
        &new_id(),
        "user",
        "新增物理学分页内容 PAGE_SOURCE_CHANGED",
    );
    assert_error(
        fixture.execute(
            "conversation.history.search",
            json!({"scope":"work","role":"user","query":"物理学","limit":1,"cursor":cursor}),
            &context,
        ),
        "CONVERSATION_HISTORY_CHANGED",
        409,
    );
}
