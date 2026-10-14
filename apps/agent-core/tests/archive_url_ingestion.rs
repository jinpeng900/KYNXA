//! Real E5 ingestion of Office members and explicit public links, with isolated formal data.
//! 用独立正式数据验证 Office 压缩包成员、明确公共链接与真实 E5 向量，不使用模拟向量。

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires prepared native bundle, ToolHost and network access to example.com"]
async fn native_archive_url_snapshots_use_real_vectors_and_preserve_scope_deletion() {
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
        io::Write,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    async fn http(
        client: &reqwest::Client,
        base: &str,
        method: reqwest::Method,
        path: &str,
        body: Value,
    ) -> Value {
        let mut request = client.request(method, format!("{base}{path}"));
        if !body.is_null() {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let value: Value = response.json().await.unwrap();
        assert!(status.is_success(), "{path}: {status} {value}");
        value
    }
    async fn indexed(client: &reqwest::Client, base: &str, id: &str) -> Value {
        let started = Instant::now();
        loop {
            let job = http(
                client,
                base,
                reqwest::Method::GET,
                &format!("/api/retrieval/index/jobs/{id}"),
                Value::Null,
            )
            .await;
            if matches!(
                job["status"].as_str(),
                Some("completed" | "partial" | "failed" | "cancelled")
            ) {
                assert!(
                    matches!(job["status"].as_str(), Some("completed" | "partial")),
                    "{job}"
                );
                assert_eq!(job["semantic"]["state"], "complete", "{job}");
                assert!(
                    job["semantic"]["vectorChunks"].as_u64().unwrap() > 0,
                    "{job}"
                );
                return job;
            }
            assert!(started.elapsed() < Duration::from_secs(180), "{job}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let bundle = PathBuf::from(std::env::var_os("KYNXA_DOCUMENT_BUNDLE_ROOT").unwrap());
    let toolhost = PathBuf::from(std::env::var_os("KYNXA_DOCUMENT_TOOLHOST").unwrap());
    let root = std::env::temp_dir().join(format!("kynxa-archive-url-{}", new_id()));
    for name in ["Data", "Models", "Extensions", "work", "other"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extended-documents");
    let archive = root.join("documents.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
    for name in ["guide.pptx", "guide.xlsx", "guide.epub"] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&std::fs::read(fixtures.join(name)).unwrap())
            .unwrap();
    }
    zip.start_file("资料/账户.md", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all("# 账户密码重置\narchive_unique_account_20261013\n打开账户设置，确认邮件地址后重置密码。\nAccount password reset requires a verified email address.".as_bytes()).unwrap();
    zip.finish().unwrap();
    std::fs::copy(&archive, root.join("work/documents.zip")).unwrap();
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
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = gateway::router(runtime.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(300))
        .build()
        .unwrap();
    let outcome=std::panic::AssertUnwindSafe(async {
        let settings=http(&client,&base,reqwest::Method::GET,"/api/retrieval/settings",Value::Null).await;
        http(&client,&base,reqwest::Method::PATCH,"/api/retrieval/settings",json!({"expectedRevision":settings["revision"],
            "patch":{"local":{"enabled":true,"semantic":"auto","embeddingDevicePolicy":"cpu","rerankProfileId":null}}})).await;
        let catalog=http(&client,&base,reqwest::Method::GET,"/api/conversations/catalog?includeMessages=false",Value::Null).await;
        let project=new_id();let conversation=new_id();let other_project=new_id();let other_chat=new_id();
        http(&client,&base,reqwest::Method::PUT,"/api/conversations/catalog",json!({"Revision":catalog["Revision"],"Chats":[],"Projects":[
            {"Id":project,"Name":"Archive URL fixture","FolderPath":root.join("work"),"Chats":[{"Id":conversation,"Title":"Read sources","Messages":[{"Id":new_id(),"Role":"user","Content":"核验双语资料","CreatedAt":"2026-10-13T00:00:00Z"}]}]},
            {"Id":other_project,"Name":"Isolated work","FolderPath":root.join("other"),"Chats":[{"Id":other_chat,"Title":"Other scope","Messages":[{"Id":new_id(),"Role":"user","Content":"No shared work sources","CreatedAt":"2026-10-13T00:00:00Z"}]}]}]})).await;
        let imported=http(&client,&base,reqwest::Method::POST,"/api/retrieval/sources/import",json!({"path":archive,"scope":"user"})).await;
        assert!(imported["importedCount"].as_u64().unwrap()>=4,"{imported}");
        assert_eq!(imported["sourceCoverage"]["complete"],true,"{imported}");
        let archive_job=indexed(&client,&base,imported["jobId"].as_str().unwrap()).await;
        let query=|chat:&str,text:&str|json!({"conversationId":chat,"query":text,"limit":40,"maximumTokens":8192});
        let english=http(&client,&base,reqwest::Method::POST,"/api/retrieval/search",query(&conversation,"Account password reset verified email address")).await;
        let chinese=http(&client,&base,reqwest::Method::POST,"/api/retrieval/search",query(&conversation,"如何重置账户密码，怎样确认邮件地址？")).await;
        assert!(!english["items"].as_array().unwrap().is_empty(),"{english}");
        assert!(!chinese["items"].as_array().unwrap().is_empty(),"{chinese}");
        assert!(chinese["semanticDiagnostic"].is_null(),"{chinese}");
        let sources=http(&client,&base,reqwest::Method::GET,"/api/retrieval/sources",Value::Null).await;
        assert!(sources["sources"].as_array().unwrap().iter().any(|source|source["path"].as_str().unwrap().contains("guide.pptx")));
        let context=json!({"conversationId":conversation,"requestId":new_id()});
        let snapshot=chinese["items"].as_array().unwrap().iter().find(|item|item["title"].as_str().unwrap_or("").contains("账户.md")).unwrap()["sourceRef"].clone();
        let read_snapshot=|reference:Value| {let config=configuration.clone();let context=context.clone();tokio::task::spawn_blocking(move||
            gateway_tools::handle_tool("knowledge.read",&json!({"sourceRef":reference,"limit":4000}),&context,&config).unwrap())};
        assert!(read_snapshot(snapshot.clone()).await.unwrap().unwrap().to_string().contains("账户"));
        std::fs::write(&archive,b"original archive changed").unwrap();
        assert!(read_snapshot(snapshot.clone()).await.unwrap().is_ok(),"Imported snapshot must not reread changed original bytes.");
        let work_job=http(&client,&base,reqwest::Method::POST,"/api/retrieval/index/rebuild",json!({"projectId":project})).await;
        indexed(&client,&base,work_job["jobId"].as_str().unwrap()).await;
        let live=tokio::task::spawn_blocking({let config=configuration.clone();let project=project.clone();move||
            kynxa_agent_core_native::storage::sqlite_queries::apply("storage.index.listSources",&json!({"root":config.data_root,"path":config.data_root.join("Index/retrieval.sqlite"),"scopes":[format!("project:{project}")],"afterSourceId":null})).unwrap()}).await.unwrap();
        let mut mounted=live["rows"].as_array().unwrap().iter().find(|source|source["title"].as_str().unwrap_or("").contains("账户.md")).unwrap().clone();
        for key in ["locator","sourceRevision"] {mounted[key]=serde_json::from_str(mounted[key].as_str().unwrap()).unwrap();}
        assert!(mounted["locator"]["archive"]["chain"].is_array());
        let current_ref=json!(kynxa_agent_core_native::retrieval::gateway_index::reference(&mounted,None));
        assert!(read_snapshot(current_ref.clone()).await.unwrap().is_ok());
        std::fs::write(root.join("work/documents.zip"),b"changed mounted archive").unwrap();
        assert_eq!(read_snapshot(current_ref).await.unwrap().unwrap_err().code(),"STALE_RETRIEVAL_SOURCE");

        let linked=http(&client,&base,reqwest::Method::POST,"/api/retrieval/sources/import",json!({"scope":"project","projectId":project,
            "urls":["https://example.com/","http://127.0.0.1/private"]})).await;
        assert_eq!(linked["importedCount"],1,"{linked}");
        assert_eq!(linked["sourceCoverage"]["failed"],1,"{linked}");
        assert_eq!(linked["sourceCoverage"]["failures"][0]["errorCode"],"WEB_URL_BLOCKED","{linked}");
        let url_job=indexed(&client,&base,linked["jobId"].as_str().unwrap()).await;
        let site=http(&client,&base,reqwest::Method::POST,"/api/retrieval/search",query(&conversation,"Example Domain documentation examples")).await;
        let url_source=site["items"].as_array().unwrap().iter().find(|item|item["title"].as_str().unwrap_or("").contains("example.com")).unwrap();
        let url_reference=url_source["sourceRef"].clone();
        let url_read=read_snapshot(url_reference.clone()).await.unwrap().unwrap();
        assert!(url_read.to_string().contains("Example Domain"));
        let other=http(&client,&base,reqwest::Method::POST,"/api/retrieval/search",query(&other_chat,"Example Domain documentation examples")).await;
        assert!(!other["items"].as_array().unwrap().iter().any(|item|item["sourceId"]==url_source["sourceId"]),"Project-only URL must not enter other work.");
        http(&client,&base,reqwest::Method::DELETE,&format!("/api/retrieval/sources/{}",linked["id"].as_str().unwrap()),json!({"expectedRevision":linked["revision"]})).await;
        assert!(read_snapshot(url_reference).await.unwrap().is_err(),"Deleted snapshots cannot be reread by stale source refs.");
        let after=http(&client,&base,reqwest::Method::POST,"/api/retrieval/search",query(&conversation,"Example Domain documentation examples")).await;
        assert!(!after["items"].as_array().unwrap().iter().any(|item|item["sourceId"]==url_source["sourceId"]));
        let report=json!({"passed":true,"scope":"native HTTP archive/Office/URL snapshots with real E5, not model-answer accuracy","isolatedData":root,
            "archiveJob":archive_job,"urlJob":url_job,"bilingualQueries":true,"privateAddressBlocked":true,"projectIsolation":true,
            "mountedChangedBytesRejected":true,"importSnapshotRetained":true,"deletedSourceRetired":true});
        if let Some(path)=std::env::var_os("KYNXA_ARCHIVE_URL_RESULT_PATH") {std::fs::write(path,serde_json::to_vec_pretty(&report).unwrap()).unwrap();}
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
