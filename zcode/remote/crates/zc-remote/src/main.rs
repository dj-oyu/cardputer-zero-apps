//! `zc-remote`: CLI と serve（docs/stage3-design.md §7）。中身は `zc_remote::cli`。

use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(zc_remote::cli::main(std::env::args_os().collect()))
}
