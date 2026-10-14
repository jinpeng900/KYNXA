//! Small offline checks exercise the installed E5 and BGE models through native inference.
//! 少量离线样本通过原生推理验证随包 E5 和 BGE，不访问用户数据或下载模型。

use kynxa_agent_core_native::models::gateway_inference::{self, EmbedKind, InferenceOptions};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

fn verified_assets(root: &Path, manifest_json: &str) -> Vec<Value> {
    let manifest: Value = serde_json::from_str(manifest_json).unwrap();
    manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|asset| {
            let path = root.join(asset["path"].as_str().unwrap());
            let mut file = File::open(&path).unwrap();
            assert_eq!(
                file.metadata().unwrap().len(),
                asset["bytes"].as_u64().unwrap()
            );
            let mut digest = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let bytes = file.read(&mut buffer).unwrap();
                if bytes == 0 {
                    break;
                }
                digest.update(&buffer[..bytes]);
            }
            let actual = format!("{:x}", digest.finalize());
            assert_eq!(actual, asset["sha256"].as_str().unwrap());
            json!({"path":asset["path"],"bytes":asset["bytes"],"sha256":actual,"verified":true})
        })
        .collect()
}

fn vector_norm(vector: &[f32]) -> f64 {
    assert_eq!(vector.len(), 384);
    assert!(vector.iter().all(|value| value.is_finite()));
    let norm = vector
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    assert!((norm - 1.0).abs() < 1e-5, "E5 output norm: {norm}");
    norm
}

fn cosine(left: &[f32], right: &[f32]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| f64::from(*left) * f64::from(*right))
        .sum()
}

/// Explicit bundle selection keeps this check offline and independent from the user's configuration.
/// 显式选择已准备载荷，确保检查离线且独立于用户配置；CPU 验收不宣称 GPU 已通过。
#[test]
#[ignore = "requires KYNXA_LOCAL_RETRIEVAL_BUNDLE_ROOT pointing to the prepared Windows x64 payload"]
fn installed_e5_bge_cpu_support_chinese_and_english() {
    let started = Instant::now();
    let bundle = std::env::var_os("KYNXA_LOCAL_RETRIEVAL_BUNDLE_ROOT")
        .map(std::path::PathBuf::from)
        .expect("Select a prepared payload with KYNXA_LOCAL_RETRIEVAL_BUNDLE_ROOT");
    let embedding_root = bundle.join("runtime/embedding/builtin-multilingual");
    let reranker_root = bundle.join("runtime/rerank/builtin-multilingual-reranker");
    let embedding_assets = verified_assets(
        &embedding_root,
        include_str!("../../desktop/Build/native-assets/embedding.json"),
    );
    let reranker_assets = verified_assets(
        &reranker_root,
        include_str!("../../desktop/Build/native-assets/reranker.json"),
    );
    let runtime_manifest: Value = serde_json::from_str(include_str!(
        "../../desktop/Build/native-assets/native-runtime.json"
    ))
    .unwrap();
    let mut runtime_assets = Vec::new();
    for package in runtime_manifest["packages"].as_array().unwrap() {
        runtime_assets.extend(verified_assets(&bundle, &package.to_string()));
    }
    gateway_inference::initialize_runtime(&bundle.join("runtime/native/onnxruntime.dll")).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let options = InferenceOptions::default();
    let samples = [
        (
            "en",
            "How can I reset my account password?",
            [
                "Reset your account password by choosing Forgot password on the sign-in page and following the recovery email.",
                "Tomatoes grow best in a sunny garden with regular watering.",
            ],
        ),
        (
            "zh",
            "如何重置账户密码？",
            [
                "忘记账户密码时，在登录页面点击忘记密码，按照收到的邮件完成密码重置。",
                "西红柿适合种在阳光充足的菜园里，并且需要定期浇水。",
            ],
        ),
    ];
    let mut reports = Vec::new();
    for (language, query, passages) in samples {
        let sample_started = Instant::now();
        let query_batch = gateway_inference::embed_with_options(
            &embedding_root,
            &[query.to_string()],
            EmbedKind::Query,
            &options,
            cancel.clone(),
        )
        .unwrap();
        let texts = passages.map(str::to_string);
        let passage_batch = gateway_inference::embed_with_options(
            &embedding_root,
            &texts,
            EmbedKind::Passage,
            &options,
            cancel.clone(),
        )
        .unwrap();
        assert_eq!(query_batch.dimensions, 384);
        assert_eq!(query_batch.profile_id, "builtin-multilingual");
        assert_eq!(query_batch.space_id, passage_batch.space_id);
        assert_eq!(query_batch.inference_status["device"], "cpu");
        assert_eq!(query_batch.inference_status["gpuValidated"], false);
        let norms: Vec<f64> = query_batch
            .vectors
            .iter()
            .chain(&passage_batch.vectors)
            .map(|vector| vector_norm(vector))
            .collect();
        let similarities: Vec<f64> = passage_batch
            .vectors
            .iter()
            .map(|passage| cosine(&query_batch.vectors[0], passage))
            .collect();
        assert!(
            similarities[0] > similarities[1],
            "E5 {language}: {similarities:?}"
        );
        let ranked = gateway_inference::rerank_blocking(
            &reranker_root,
            query,
            &texts,
            &options,
            cancel.clone(),
        )
        .unwrap();
        assert_eq!(ranked.scores.len(), 2);
        assert!(
            ranked
                .scores
                .iter()
                .all(|score| score.is_finite() && (0.0..=1.0).contains(score))
        );
        assert!(
            ranked.scores[0] > ranked.scores[1],
            "BGE {language}: {:?}",
            ranked.scores
        );
        assert_eq!(ranked.truncated_inputs_count, 0);
        assert_eq!(ranked.profile_id, "builtin-multilingual-reranker");
        assert_eq!(ranked.inference_status["device"], "cpu");
        assert_eq!(ranked.inference_status["gpuValidated"], false);
        reports.push(json!({
            "language":language,"query":query,"passages":passages,"dimensions":384,
            "vectorNorms":norms,"embeddingCosines":similarities,"rerankScores":ranked.scores,
            "truncatedInputsCount":ranked.truncated_inputs_count,"embeddingProfileId":query_batch.profile_id,
            "embeddingModelVersion":query_batch.model_version,"embeddingSpaceId":query_batch.space_id,
            "rerankProfileId":ranked.profile_id,"rerankModelVersion":ranked.model_version,
            "embeddingBackend":query_batch.inference_status,"rerankBackend":ranked.inference_status,
            "elapsedMs":sample_started.elapsed().as_secs_f64()*1000.0
        }));
    }
    assert!(gateway_inference::retire_idle(&embedding_root, "cpu").unwrap());
    assert!(gateway_inference::retire_idle(&reranker_root, "cpu").unwrap());
    let report = json!({
        "schemaVersion":1,"createdAt":chrono::Utc::now().to_rfc3339(),"passed":true,
        "scope":"fixed Chinese and English CPU inference; GPU and HTTP resource admission are separate checks",
        "queryPrefix":"query: ","documentPrefix":"passage: ","pooling":"mean","normalization":"L2",
        "runtimeVersion":runtime_manifest["onnxRuntimeVersion"],"runtimeAssets":runtime_assets,
        "embeddingAssets":embedding_assets,"rerankerAssets":reranker_assets,"samples":reports,
        "elapsedMs":started.elapsed().as_secs_f64()*1000.0
    });
    if let Some(output) = std::env::var_os("KYNXA_LOCAL_RETRIEVAL_REPORT") {
        std::fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
