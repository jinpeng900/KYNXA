use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::protocol::{array, quote, string};

fn signature(serialized: &str) -> String {
    format!("{:x}", Sha256::digest(serialized.as_bytes()))
}

/// Exact profile identity is based on pinned assets and projection semantics; fallback cannot relabel vectors.
/// 精确模型身份绑定固定资产和投影语义；回退不能给另一空间的向量重新标记。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "models.registry.register" => {
            let manifest: Vec<String> = array(&args["manifestEntriesJson"])
                .iter()
                .map(|value| string(value).into())
                .collect();
            let assets: Vec<Value> = manifest
                .iter()
                .map(|asset| {
                    serde_json::from_str(asset).map_err(|_| "RETRIEVAL_INVALID_MODEL_MANIFEST")
                })
                .collect::<Result<_, _>>()?;
            let runtime_paths = [
                "config.json",
                "tokenizer_config.json",
                "special_tokens_map.json",
                "tokenizer.json",
                "onnx/model_quantized.onnx",
            ];
            let model_signature = signature(&format!(
                "[{}]",
                manifest
                    .iter()
                    .zip(&assets)
                    .filter(|(_, asset)| runtime_paths.contains(&string(&asset["path"])))
                    .map(|(asset, _)| asset.clone())
                    .collect::<Vec<_>>()
                    .join(",")
            ));
            let asset_signature = signature(&format!("[{}]", manifest.join(",")));
            let profile = &args["profile"];
            let kind = string(&args["kind"]);
            let implementation = string(&args["implementationId"]);
            let projection_json = string(&args["inputProjectionJson"]);
            let projection: Value = serde_json::from_str(projection_json)
                .map_err(|_| "RETRIEVAL_INVALID_MODEL_MANIFEST")?;
            let mut result = json!({"kind": kind, "implementationId": implementation, "local": true, "network": false,
                "languages": ["zh", "en", "multilingual"], "tokenizerId": "xlm-roberta", "inputProjection": projection,
                "inputProjectionVersion": projection["version"], "modelAssetSignature": model_signature, "assetSignature": asset_signature});
            if kind == "embedding" {
                let identity = format!(
                    "{{\"implementationId\":{},\"modelId\":{},\"modelVersion\":{},\"dimensions\":{},\"inputProjection\":{},\"modelAssetSignature\":{}}}",
                    quote(implementation),
                    profile["modelId"],
                    profile["modelVersion"],
                    profile["dimensions"],
                    projection_json,
                    quote(&model_signature)
                );
                result["embeddingSpaceId"] = json!(signature(&identity));
            }
            Ok(result)
        }
        "models.registry.list" => Ok(json!(
                array(&args["profiles"])
                    .iter()
                    .enumerate()
                    .filter(|(_, profile)| args.get("kind").is_none()
                        || profile["kind"] == args["kind"])
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>()
            )),
        "models.registry.resolve" => {
            let index = array(&args["profiles"]).iter().position(|profile| {
                profile["kind"] == args["kind"] && profile["id"] == args["profileId"]
            });
            Ok(json!({"index": index}))
        }
        "models.registry.metadata" => {
            let profile = &args["profile"];
            let mut result = json!({"profileId": profile["id"], "modelId": profile["modelId"], "modelVersion": profile["modelVersion"],
                "implementationId": profile["implementationId"], "inputProjectionVersion": profile["inputProjectionVersion"],
                "modelAssetSignature": profile["modelAssetSignature"], "assetSignature": profile["assetSignature"]});
            if profile["kind"] == "embedding" {
                result["dimensions"] = profile["dimensions"].clone();
                result["embeddingSpaceId"] = profile["embeddingSpaceId"].clone();
            }
            Ok(result)
        }
        "models.registry.unavailable" => Ok(
            json!({"profileId": args["profileId"].as_str(), "kind": args["kind"], "state": "unavailable",
            "loaded": false, "supported": false, "local": true, "network": false, "errorCode": "RETRIEVAL_MODEL_PROFILE_UNSUPPORTED"}),
        ),
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}
