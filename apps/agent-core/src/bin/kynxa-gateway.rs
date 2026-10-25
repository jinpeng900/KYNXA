//! Native production entry point; desktop startup never invokes Node.
//! 原生正式入口；桌面启动不会调用 Node。

#[tokio::main]
async fn main() {
    let arguments = std::env::args().collect::<Vec<_>>();
    let desktop_shutdown = arguments.iter().any(|argument| argument == "--desktop-lifetime").then(|| {
        let (shutdown, requested) = tokio::sync::oneshot::channel();
        // Only the owning desktop opts into stdin lifetime; manual launch keeps its normal lifecycle.
        // 仅桌面自启动进程使用 stdin 生命周期；手动运行不因输入 EOF 自动退出。
        std::thread::spawn(move || {
            use std::io::BufRead;
            let input = std::io::stdin();
            for line in input.lock().lines() {
                match line {
                    Ok(line) if line.trim().eq_ignore_ascii_case("shutdown") => break,
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
            let _ = shutdown.send(());
        });
        requested
    });
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
        Ok(config) => kynxa_agent_core_native::gateway::serve_with_desktop_lifetime(config, desktop_shutdown).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
