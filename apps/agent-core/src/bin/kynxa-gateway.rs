//! Native production entry point; desktop startup never invokes Node.
//! 原生正式入口；桌面启动不会调用 Node。

#[tokio::main]
async fn main() {
    let arguments = std::env::args().collect::<Vec<_>>();
    if let Some(index) = arguments
        .iter()
        .position(|value| value == "--initialize-storage")
    {
        let result = arguments
            .get(index + 1)
            .ok_or_else(|| {
                kynxa_agent_core_native::gateway_contract::GatewayError::new(
                    "INVALID_DATA_DIRECTORY",
                    "缺少目标数据目录。",
                    400,
                )
            })
            .and_then(|target| kynxa_agent_core_native::gateway::initialize_storage(target));
        match result {
            Ok(_) => {
                println!("{{\"initialized\":true}}");
                return;
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }
    let result = match kynxa_agent_core_native::gateway_contract::GatewayConfig::from_environment()
    {
        Ok(config) => kynxa_agent_core_native::gateway::serve(config).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
