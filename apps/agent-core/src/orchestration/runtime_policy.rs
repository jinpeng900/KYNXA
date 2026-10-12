use serde_json::{Value, json};

/// Public failures are based on observed categories, not guessed provider or resource causes.
/// 公开失败依据实际分类，不猜测供应商或资源故障原因。
pub fn calculate(operation: &str, args: &Value) -> Result<Value, String> {
    match operation {
        "orchestration.completedContext" => {
            let mut user = None;
            let mut turns = Vec::new();
            for (index, message) in args["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if message["id"] == args["beforeUserId"] && args.get("beforeUserId").is_some() {
                    break;
                }
                if message["role"] == "user" {
                    user = Some(index);
                } else if message["role"] == "assistant"
                    && message["status"] == "completed"
                    && message["hasContent"] == true
                {
                    if let Some(user) = user.take() {
                        turns.push(json!([user, index]));
                    }
                }
            }
            Ok(json!(turns))
        }
        "orchestration.safeFailure" => {
            if args["inputBudgetError"] == true {
                return Ok(
                    json!({"originalMessage":true,"phase":"interrupted","code":args["code"],"statusCode":args["statusCode"]}),
                );
            }
            let name = args["name"].as_str().unwrap_or("");
            if name == "TimeoutError" {
                return Ok(
                    json!({"message":"模型响应超时，已保留生成内容和完成的工具记录。","phase":"interrupted","code":"MODEL_RESPONSE_TIMEOUT"}),
                );
            }
            if name == "AbortError" {
                return Ok(
                    json!({"message":"连续执行已停止，已保留生成内容和完成的工具记录。","phase":"interrupted","code":"MODEL_CANCELLED"}),
                );
            }
            let code = args["code"].as_str().unwrap_or("");
            if ["RESOURCE_", "INFERENCE_RESOURCE_", "STRUCTURE_RESOURCE_"]
                .iter()
                .any(|prefix| code.starts_with(prefix))
            {
                let message = if args["details"]["actionStarted"] == false
                    && args["details"]["workload"] == "model-transport"
                {
                    "当前这一步尚未发送模型请求：执行资源不足或等待超时。对话和已完成结果已保留，可释放内存后重试。"
                } else if code == "RESOURCE_WAIT_TIMEOUT" {
                    "等待执行资源超时，已有结果已保留。"
                } else {
                    "当前执行资源不足或暂时不可用，已有结果已保留。"
                };
                return Ok(
                    json!({"message":message,"code":code,"statusCode":args.get("statusCode").filter(|v|!v.is_null()).cloned().unwrap_or(json!(503)),"resource":true}),
                );
            }
            Ok(json!({"message":"模型调用失败，请检查服务与数据存储位置。"}))
        }
        "orchestration.evidenceWarning" => Ok(json!(if regex::Regex::new(r"\p{Han}")
            .unwrap()
            .is_match(args["message"].as_str().unwrap_or(""))
        {
            "\n\n部分来源在回答前发生变化或无法再次核验，相关结论仍待回读确认。"
        } else {
            "\n\nSome sources changed or could not be rechecked before this answer; affected conclusions still need current-source verification."
        })),
        "orchestration.httpFailure" => {
            let status = args["status"].as_u64().unwrap_or(0);
            let hint = match status {
                401 => "请检查 API Key",
                403 => "当前密钥没有访问权限",
                402 => "请检查账号余额",
                404 => "请检查服务地址与模型 ID",
                429 => "请求频繁或额度不足，请稍后重试",
                _ => "",
            };
            Ok(json!(format!(
                "模型服务返回 HTTP {status}{}。",
                if hint.is_empty() {
                    String::new()
                } else {
                    format!("，{hint}")
                }
            )))
        }
        _ => Err("AGENT_CORE_UNKNOWN_OPERATION".into()),
    }
}
