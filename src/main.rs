//! Launch one local MCP session with an operator-approved configuration.
use mas_mcp::{config::Config, gateway::Gateway, transport};
use std::path::Path;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("mas-mcp: {error}");
        std::process::exit(1);
    }
    // Tokio stdin can retain a blocking read after a malformed frame closes the session.
    // Accepted work and journals have already been drained and dropped by run().
    std::process::exit(0);
}

async fn run() -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Err("this MAS host deployment profile requires Linux".into());
    }
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--version"] {
        println!("mas-mcp {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args == ["--help"] || args.is_empty() {
        println!(
            "mas-mcp --config FILE\nLocal stdio MCP gateway for approved MAS host operations."
        );
        return Ok(());
    }
    if args.len() != 2 || args[0] != "--config" {
        return Err("usage: mas-mcp --config FILE".into());
    }
    let gateway = Gateway::open(Config::read(Path::new(&args[1]))?).await?;
    transport::serve(gateway).await
}
