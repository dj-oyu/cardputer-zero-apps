//! `zc-buildd`: ビルド係の受付（docs/stage3-design.md §4）。
//!
//! ```text
//! zc-buildd [--listen 0.0.0.0:7411] [--data /var/lib/zc-buildd]
//! ```

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("{}", zc_buildd::SERVER_NAME);
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("使い方: zc-buildd [--listen 0.0.0.0:7411] [--data /var/lib/zc-buildd]");
        println!("環境変数: ZC_BUILDD_LISTEN  ZC_BUILDD_DATA  ZC_BUILDD_MAX_RUNNING  ZC_BUILDD_JOB_TIMEOUT_S");
        println!("口: GET /v1/ws（WebSocket、zc-build.v1）  GET /healthz  GET /v1/status");
        return ExitCode::SUCCESS;
    }
    let cfg = match zc_buildd::Config::from_env(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("zc-buildd: {e}");
            return ExitCode::from(2);
        }
    };
    match zc_buildd::server::run(cfg) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("zc-buildd: {e}");
            ExitCode::FAILURE
        }
    }
}
