//! Native, offline ingestion checks use original bilingual fixtures and isolated formal storage.
//! 原生离线导入以自有中英文夹具和独立正式存储验证，不访问用户资料或生成模型。

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires KYNXA_DOCUMENT_BUNDLE_ROOT and KYNXA_DOCUMENT_TOOLHOST pointing to prepared native payloads"]
async fn native_document_imports_index_vectors_and_reject_changed_original_images() {
    use futures_util::FutureExt;
    use kynxa_agent_core_native::{
        gateway,
        gateway_contract::{GatewayConfig, new_id},
        models::gateway_inference,
        orchestration::gateway_runtime::GatewayRuntime,
        resources::gateway as resources,
        retrieval::{gateway_embedding, gateway_tools},
    };
    use serde_json::{Value, json};
    use std::{
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    async fn request(
        client: &reqwest::Client,
        url: &str,
        method: reqwest::Method,
        path: &str,
        body: Value,
    ) -> Value {
        let mut request = client.request(method, format!("{url}{path}"));
        if !body.is_null() {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let value: Value = response.json().await.unwrap();
        assert!(status.is_success(), "{path}: {status}, {value}");
        value
    }
    async fn completed_job(
        client: &reqwest::Client,
        url: &str,
        id: &str,
        expect_partial: bool,
    ) -> Value {
        let started = Instant::now();
        loop {
            let value = request(
                client,
                url,
                reqwest::Method::GET,
                &format!("/api/retrieval/index/jobs/{id}"),
                Value::Null,
            )
            .await;
            let job = &value;
            if ["completed", "partial", "failed", "cancelled"]
                .contains(&job["status"].as_str().unwrap())
            {
                assert_eq!(
                    job["status"],
                    if expect_partial {
                        "partial"
                    } else {
                        "completed"
                    },
                    "{job}"
                );
                assert_eq!(job["semantic"]["state"], "complete", "{job}");
                assert!(
                    job["semantic"]["vectorChunks"].as_u64().unwrap() >= 5,
                    "{job}"
                );
                return job.clone();
            }
            assert!(started.elapsed() < Duration::from_secs(120), "{value}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let bundle = PathBuf::from(std::env::var_os("KYNXA_DOCUMENT_BUNDLE_ROOT").unwrap());
    let toolhost = PathBuf::from(std::env::var_os("KYNXA_DOCUMENT_TOOLHOST").unwrap());
    let root = std::env::temp_dir().join(format!("kynxa-document-ingestion-{}", new_id()));
    let corpus = root.join("corpus");
    for directory in ["corpus", "Data", "Models", "Extensions"] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/documents");
    for name in [
        "account-text.pdf",
        "account-scan.pdf",
        "account-guide.docx",
        "account-guide.md",
        "account-guide.png",
    ] {
        std::fs::copy(fixtures.join(name), corpus.join(name)).unwrap();
    }
    let configuration = GatewayConfig {
        data_home: root.join("Models"),
        data_root: root.join("Data"),
        extension_root: root.join("Extensions"),
        application_root: bundle.clone(),
        port: 0,
        toolhost_exe: toolhost,
        vector_extension: bundle.join("runtime/native/vec0.dll"),
        inference_library: bundle.join("runtime/native/onnxruntime.dll"),
    };
    gateway_inference::initialize_runtime(&configuration.inference_library).unwrap();
    gateway_embedding::install_runtime(tokio::runtime::Handle::current());
    let resource = resources::ResourceService::start(
        &bundle.join("runtime/resource/kynxa-resource-service.exe"),
    )
    .await
    .unwrap();
    resources::install(resource.clone());
    let runtime = GatewayRuntime::new(configuration.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = gateway::router(runtime.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();
    // Even a failing assertion must shut down owned workers before the Tokio runtime exits.
    // 断言失败也要先清理自有后台作业，再退出 Tokio 运行时，避免测试留下阻塞线程。
    let outcome = std::panic::AssertUnwindSafe(async {
        let settings = request(
            &client,
            &url,
            reqwest::Method::GET,
            "/api/retrieval/settings",
            Value::Null,
        )
        .await;
        request(
            &client,
            &url,
            reqwest::Method::PATCH,
            "/api/retrieval/settings",
            json!({"expectedRevision":settings["revision"],"patch":{"local":{
            "enabled":true,"semantic":"auto","embeddingDevicePolicy":"cpu","rerankProfileId":null}}}),
        )
        .await;
        let catalog = request(
            &client,
            &url,
            reqwest::Method::GET,
            "/api/conversations/catalog?includeMessages=false",
            Value::Null,
        )
        .await;
        let project = new_id();
        let conversation = new_id();
        request(&client, &url, reqwest::Method::PUT, "/api/conversations/catalog", json!({"Revision":catalog["Revision"],"Chats":[],"Projects":[{
            "Id":project,"Name":"Offline document work","FolderPath":corpus,"Chats":[{"Id":conversation,"Title":"Document fixture","Messages":[{
                "Id":new_id(),"Role":"user","Content":"Inspect authorized account guides","CreatedAt":"2026-10-13T00:00:00Z"}]}]}]})).await;
        let imported = request(
            &client,
            &url,
            reqwest::Method::POST,
            "/api/retrieval/sources/import",
            json!({"path":corpus,"scope":"user"}),
        )
        .await;
        assert_eq!(imported["importedCount"], 5, "{imported}");
        assert_eq!(imported["sourceCoverage"]["complete"], true);
        let import_job = completed_job(&client, &url, imported["jobId"].as_str().unwrap(), false).await;
        let sources = request(
            &client,
            &url,
            reqwest::Method::GET,
            "/api/retrieval/sources",
            Value::Null,
        )
        .await;
        assert_eq!(sources["sources"].as_array().unwrap().len(), 5);
        let query = |text: &str| json!({"conversationId":conversation,"query":text,"limit":40,"maximumTokens":8192});
        let english = request(
            &client,
            &url,
            reqwest::Method::POST,
            "/api/retrieval/search",
            query("How to reset an account password?"),
        )
        .await;
        let chinese = request(
            &client,
            &url,
            reqwest::Method::POST,
            "/api/retrieval/search",
            query("如何重置账户密码？"),
        )
        .await;
        assert!(
            !english["items"].as_array().unwrap().is_empty(),
            "{english}"
        );
        assert!(
            !chinese["items"].as_array().unwrap().is_empty(),
            "{chinese}"
        );
        assert!(chinese["semanticDiagnostic"].is_null(), "{chinese}");
        let first_title = english["items"][0]["title"].as_str().unwrap();
        assert!(first_title.contains("account"));
        let reindexed = request(
            &client,
            &url,
            reqwest::Method::POST,
            "/api/retrieval/index/rebuild",
            json!({"projectId":project}),
        )
        .await;
        let work_job = completed_job(&client, &url, reindexed["jobId"].as_str().unwrap(), false).await;
        let work_search = request(
            &client,
            &url,
            reqwest::Method::POST,
            "/api/retrieval/search",
            query("account-guide.png 账户密码重置说明"),
        )
        .await;
        assert!(!work_search["items"].as_array().unwrap().is_empty());
        let indexed = tokio::task::spawn_blocking({let config=configuration.clone(); let scope=format!("project:{project}"); move ||
            kynxa_agent_core_native::storage::sqlite_queries::apply("storage.index.listSources", &json!({"root":config.data_root,
                "path":config.data_root.join("Index/retrieval.sqlite"),"scopes":[scope],"afterSourceId":null})).unwrap()}).await.unwrap();
        let mut image = indexed["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["sourceType"] == "work-file" && item["title"] == "account-guide.png")
            .unwrap()
            .clone();
        for field in ["locator", "sourceRevision"] {
            image[field] = serde_json::from_str(image[field].as_str().unwrap()).unwrap();
        }
        assert_eq!(image["locator"]["extraction"]["format"], "image");
        assert_eq!(image["locator"]["extraction"]["image"]["frameCount"], 1);
        let reference = kynxa_agent_core_native::retrieval::gateway_index::reference(&image, None);
        let context = json!({"conversationId":conversation,"requestId":new_id()});
        let read = tokio::task::spawn_blocking({
            let config = configuration.clone();
            let context = context.clone();
            let reference = reference.clone();
            move || {
                gateway_tools::handle_tool(
                    "knowledge.read",
                    &json!({"sourceRef":reference,"limit":4000}),
                    &context,
                    &config,
                )
                .unwrap()
                .unwrap()
            }
        })
        .await
        .unwrap();
        assert!(
            read.to_string().to_lowercase().contains("password"),
            "{read}"
        );
        // An ancillary PNG byte change keeps the same visible pixels; OCR equality must not preserve an old citation.
        // 修改 PNG 附加字节但保留相同像素；识别文字相同也不能继续采信旧引用。
        let image_path = corpus.join("account-guide.png");
        let mut image_bytes = std::fs::read(&image_path).unwrap();
        image_bytes.extend_from_slice(b"original-byte-version-changed");
        std::fs::write(&image_path, image_bytes).unwrap();
        let stale = tokio::task::spawn_blocking({
            let config = configuration.clone();
            let context = context.clone();
            move || {
                gateway_tools::handle_tool(
                    "knowledge.read",
                    &json!({"sourceRef":reference,"limit":4000}),
                    &context,
                    &config,
                )
                .unwrap()
                .unwrap_err()
                .code()
                .to_string()
            }
        })
        .await
        .unwrap();
        assert_eq!(stale, "STALE_RETRIEVAL_SOURCE");
        // Imported knowledge remains an authorized snapshot after its original file changes.
        // 导入资料保持授权快照语义；原件改变不会撤销仍存在的资料快照。
        let snapshot = english["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["title"] == "account-guide.png")
            .unwrap()["sourceRef"]
            .clone();
        let snapshot_read = tokio::task::spawn_blocking({
            let config = configuration.clone();
            let context = context.clone();
            move || {
                gateway_tools::handle_tool(
                    "knowledge.read",
                    &json!({"sourceRef":snapshot,"limit":4000}),
                    &context,
                    &config,
                )
                .unwrap()
                .unwrap()
            }
        })
        .await
        .unwrap();
        assert!(
            snapshot_read
                .to_string()
                .to_lowercase()
                .contains("password")
        );
        // A bad raster is recorded without stopping healthy documents or their vectors.
        // 单个错误栅格保留失败原因，不阻塞健康文档的词法索引或向量发布。
        std::fs::write(corpus.join("broken-image.png"), b"not a PNG file").unwrap();
        let failed = request(
            &client,
            &url,
            reqwest::Method::POST,
            "/api/retrieval/index/rebuild",
            json!({"projectId":project}),
        )
        .await;
        let partial = completed_job(&client, &url, failed["jobId"].as_str().unwrap(), true).await;
        let failure = &partial["coverage"]["failures"][0];
        assert_eq!(failure["errorCode"], "OCR_INVALID_REQUEST", "{failure}");
        assert_eq!(failure["documentCoverage"]["format"], "image");
        assert_eq!(
            failure["documentCoverage"]["rawContentHash"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        let report = json!({"passed":true,"scope":"native HTTP import and mounted source checks with real OCR and E5; not generation accuracy",
            "isolatedData":root,"importJob":import_job,"workJob":work_job,"partialJob":partial,
            "englishItems":english["items"].as_array().unwrap().len(),"chineseItems":chinese["items"].as_array().unwrap().len(),
            "rawByteChangeRejected":true,"importSnapshotRetained":true});
        if let Some(path) = std::env::var_os("KYNXA_DOCUMENT_RESULT_PATH") {
            std::fs::write(path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
        }
        println!("{report}");
    }).catch_unwind().await;
    runtime.shutdown().await;
    server.abort();
    let _ = server.await;
    resource.close().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// Legacy formats use the same formal import, live-source references and actual E5 worker.
/// 旧格式走同一正式导入、活动来源引用与真实 E5 worker，不另建测试专用向量发布路径。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires KYNXA_DOCUMENT_BUNDLE_ROOT and KYNXA_DOCUMENT_TOOLHOST pointing to prepared native payloads"]
async fn legacy_office_imports_persist_parts_generate_vectors_and_reject_stale_live_sources() {
    use futures_util::FutureExt;
    use kynxa_agent_core_native::{
        gateway,
        gateway_contract::{GatewayConfig, new_id},
        models::gateway_inference,
        orchestration::gateway_runtime::GatewayRuntime,
        resources::gateway as resources,
        retrieval::{gateway_embedding, gateway_tools},
    };
    use serde_json::{Value, json};
    use std::{
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    async fn request(
        client: &reqwest::Client,
        url: &str,
        method: reqwest::Method,
        path: &str,
        body: Value,
    ) -> Value {
        let mut request = client.request(method, format!("{url}{path}"));
        if !body.is_null() {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let result: Value = response.json().await.unwrap();
        assert!(status.is_success(), "{path}: {status}, {result}");
        result
    }
    async fn complete(client: &reqwest::Client, url: &str, id: &str) -> Value {
        let started = Instant::now();
        loop {
            let job = request(
                client,
                url,
                reqwest::Method::GET,
                &format!("/api/retrieval/index/jobs/{id}"),
                Value::Null,
            )
            .await;
            if ["completed", "partial", "failed", "cancelled"]
                .contains(&job["status"].as_str().unwrap())
            {
                assert_eq!(job["status"], "completed", "{job}");
                assert_eq!(job["semantic"]["state"], "complete", "{job}");
                assert!(
                    job["semantic"]["vectorChunks"].as_u64().unwrap() >= 2,
                    "{job}"
                );
                return job;
            }
            assert!(started.elapsed() < Duration::from_secs(120), "{job}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let bundle = PathBuf::from(std::env::var_os("KYNXA_DOCUMENT_BUNDLE_ROOT").unwrap());
    let root = std::env::temp_dir().join(format!("kynxa-legacy-documents-{}", new_id()));
    for directory in ["corpus", "Data", "Models", "Extensions"] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    let corpus = root.join("corpus");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::copy(
        fixtures.join("legacy-word/bilingual-stories.doc"),
        corpus.join("bilingual-stories.doc"),
    )
    .unwrap();
    std::fs::copy(
        fixtures.join("legacy-powerpoint/54880_chinese.ppt"),
        corpus.join("bilingual-slides.ppt"),
    )
    .unwrap();
    let configuration = GatewayConfig {
        data_home: root.join("Models"),
        data_root: root.join("Data"),
        extension_root: root.join("Extensions"),
        application_root: bundle.clone(),
        port: 0,
        toolhost_exe: PathBuf::from(std::env::var_os("KYNXA_DOCUMENT_TOOLHOST").unwrap()),
        vector_extension: bundle.join("runtime/native/vec0.dll"),
        inference_library: bundle.join("runtime/native/onnxruntime.dll"),
    };
    gateway_inference::initialize_runtime(&configuration.inference_library).unwrap();
    gateway_embedding::install_runtime(tokio::runtime::Handle::current());
    let resource = resources::ResourceService::start(
        &bundle.join("runtime/resource/kynxa-resource-service.exe"),
    )
    .await
    .unwrap();
    resources::install(resource.clone());
    let runtime = GatewayRuntime::new(configuration.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = gateway::router(runtime.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();
    let outcome = std::panic::AssertUnwindSafe(async {
        let settings = request(&client, &url, reqwest::Method::GET, "/api/retrieval/settings", Value::Null).await;
        request(&client, &url, reqwest::Method::PATCH, "/api/retrieval/settings", json!({"expectedRevision":settings["revision"],
            "patch":{"local":{"enabled":true,"semantic":"auto","embeddingDevicePolicy":"cpu","rerankProfileId":null}}})).await;
        let catalog = request(&client, &url, reqwest::Method::GET, "/api/conversations/catalog?includeMessages=false", Value::Null).await;
        let project = new_id(); let conversation = new_id();
        request(&client, &url, reqwest::Method::PUT, "/api/conversations/catalog", json!({"Revision":catalog["Revision"],"Chats":[],"Projects":[{
            "Id":project,"Name":"Legacy Office work","FolderPath":corpus,"Chats":[{"Id":conversation,"Title":"Legacy sources","Messages":[{
                "Id":new_id(),"Role":"user","Content":"Inspect authorized legacy documents","CreatedAt":"2026-10-13T00:00:00Z"}]}]}]})).await;
        let imported = request(&client, &url, reqwest::Method::POST, "/api/retrieval/sources/import", json!({"path":corpus,"scope":"user"})).await;
        assert_eq!(imported["importedCount"], 2, "{imported}");
        assert_eq!(imported["sourceCoverage"]["complete"], true);
        let import_job = complete(&client, &url, imported["jobId"].as_str().unwrap()).await;
        let sources = request(&client, &url, reqwest::Method::GET, "/api/retrieval/sources", Value::Null).await;
        assert_eq!(sources["sources"].as_array().unwrap().len(), 2);
        for query in ["Report 2026 measured mass 12.5 kg", "中文正文 力学与量子"] {
            let results = request(&client, &url, reqwest::Method::POST, "/api/retrieval/search", json!({"conversationId":conversation,"query":query,"limit":40,"maximumTokens":8192})).await;
            assert!(results["items"].as_array().unwrap().iter().any(|item| item["title"] == "bilingual-stories.doc"), "{results}");
            assert!(results["semanticDiagnostic"].is_null(), "{results}");
        }
        let reindexed = request(&client, &url, reqwest::Method::POST, "/api/retrieval/index/rebuild", json!({"projectId":project})).await;
        let work_job = complete(&client, &url, reindexed["jobId"].as_str().unwrap()).await;
        let indexed = tokio::task::spawn_blocking({ let config=configuration.clone(); let scope=format!("project:{project}"); move ||
            kynxa_agent_core_native::storage::sqlite_queries::apply("storage.index.listSources", &json!({"root":config.data_root,
                "path":config.data_root.join("Index/retrieval.sqlite"),"scopes":[scope],"afterSourceId":null})).unwrap() }).await.unwrap();
        let context = json!({"conversationId":conversation,"requestId":new_id()});
        let mut document_reference = String::new();
        for (name, format, version) in [("bilingual-stories.doc","doc","rust-msdoc-text-v1"),("bilingual-slides.ppt","ppt","rust-msppt-text-v1")] {
            let mut source = indexed["rows"].as_array().unwrap().iter().find(|item| item["sourceType"]=="work-file" && item["title"]==name).unwrap().clone();
            for field in ["locator","sourceRevision"] { source[field] = serde_json::from_str(source[field].as_str().unwrap()).unwrap(); }
            assert_eq!(source["locator"]["extraction"]["format"], format);
            assert_eq!(source["locator"]["extraction"]["version"], version);
            assert!(!source["locator"]["extraction"]["parts"].as_array().unwrap().is_empty());
            assert_eq!(source["locator"]["extraction"]["rawContentHash"].as_str().unwrap().len(), 64);
            let reference = kynxa_agent_core_native::retrieval::gateway_index::reference(&source, None);
            let read = tokio::task::spawn_blocking({ let config=configuration.clone(); let context=context.clone(); let reference=reference.clone(); move ||
                gateway_tools::handle_tool("knowledge.read", &json!({"sourceRef":reference,"limit":6000}), &context, &config).unwrap().unwrap() }).await.unwrap();
            if format == "doc" { assert!(read.to_string().contains("力学与量子")); document_reference = reference; }
            else {
                assert!(read.to_string().contains("Single byte"), "{read}");
                assert!(read.to_string().contains("表"), "{read}");
            }
        }
        // Change an unreferenced byte: identical extracted prose must not validate the old live citation.
        // 改变未引用字节：提取正文相同，也不能继续核验旧活动来源引用。
        let path = corpus.join("bilingual-stories.doc");
        let mut bytes = std::fs::read(&path).unwrap(); let last=bytes.len()-1; bytes[last]^=1;
        std::fs::write(path, bytes).unwrap();
        let stale = tokio::task::spawn_blocking({ let config=configuration.clone(); let context=context.clone(); move ||
            gateway_tools::handle_tool("knowledge.read", &json!({"sourceRef":document_reference,"limit":6000}), &context, &config).unwrap().unwrap_err().code().to_string() }).await.unwrap();
        assert_eq!(stale, "STALE_RETRIEVAL_SOURCE");
        let report=json!({"passed":true,"scope":"legacy DOC/PPT native formal ingestion, real E5 and live citation identity; not generation accuracy",
            "isolatedData":root,"importJob":import_job,"workJob":work_job,"formats":["doc","ppt"],"persistedParts":true,"changedOriginalRejected":true});
        if let Some(path)=std::env::var_os("KYNXA_LEGACY_DOCUMENT_RESULT_PATH") { std::fs::write(path,serde_json::to_string_pretty(&report).unwrap()).unwrap(); }
        println!("{report}");
    }).catch_unwind().await;
    runtime.shutdown().await;
    server.abort();
    let _ = server.await;
    resource.close().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
