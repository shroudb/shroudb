//! shroudb-codegen — client SDK generator for the ShrouDB protocol.
//!
//! Reads protocol.toml and produces ready-to-publish client packages.
//!
//! Usage:
//!   shroudb-codegen --spec protocol.toml --lang python --output generated/python
//!   shroudb-codegen --spec protocol.toml --lang typescript --output generated/typescript
//!   shroudb-codegen --spec protocol.toml --lang all --output generated/

use clap::Parser;
use shroudb_codegen_core::cli::{CodegenCli, run};

#[derive(Parser)]
#[command(
    name = "shroudb-codegen",
    about = "Generate typed client libraries from the ShrouDB protocol spec",
    long_about = "Reads protocol.toml and produces ready-to-publish client \
                  packages in the target language."
)]
struct Cli {
    #[command(flatten)]
    inner: CodegenCli,
}

fn main() {
    let cli = Cli::parse();
    run(&cli.inner, shroudb_codegen_core::wire::generate);
}
