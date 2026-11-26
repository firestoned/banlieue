// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue-azure-guest`: the in-guest Azure helper (ADR-0171). See the
//! library crate for what each subcommand does.

use banlieue_guest_azure::cli::{self, EXIT_USAGE};
use banlieue_guest_azure::kmsg::Logger;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse(&args) {
        Ok(c) => c,
        Err(usage) => {
            eprint!("{usage}");
            std::process::exit(EXIT_USAGE);
        }
    };
    let mut log = Logger::kernel();
    std::process::exit(cli::execute(command, &mut log));
}
