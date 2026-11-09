use std::env;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("stdo {VERSION}");
        }
        Some("run") => {
            println!("⚡ stdo v{VERSION} — Executing workflow...");
        }
        Some("serve") => {
            println!(
                "⚡ stdo v{VERSION} — Embedded Studio UI starting on http://127.0.0.1:8080 (100% Offline)"
            );
        }
        _ => {
            println!("⚡ stdo v{VERSION} — Standard I/O Orchestrator");
            println!("Zero-network Unix DAG workflow engine with an embedded Studio UI.\n");
            println!("USAGE:");
            println!("  stdo run [FILE]      Execute a workflow DAG headlessly");
            println!(
                "  stdo serve [FILE]    Launch the embedded offline Studio UI on 127.0.0.1:8080"
            );
            println!("  stdo --version       Print version information");
        }
    }
}
