// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The command line: one subcommand, no flags. Nothing the helper does is
//! configurable at run time (ADR-0171 Decision 4): paths, the WireServer
//! address and the retry policy are constants.

use crate::kmsg::Logger;
use crate::{customdata, ready};

/// Usage text.
pub const USAGE: &str = "usage: banlieue-azure-guest <customdata|ready>\n\
\n\
  customdata  copy customData from the Azure provisioning disk to\n\
              /oem/91_azure_customdata.yaml (0600)\n\
  ready       report ready to Azure's WireServer (installed boots only)\n\
\n\
Both do nothing and exit 0 when the machine is not on Azure.\n";

/// Exit code: success, or nothing to do.
pub const EXIT_OK: i32 = 0;
/// Exit code: the command ran and failed.
pub const EXIT_FAILED: i32 = 1;
/// Exit code: the command line was wrong.
pub const EXIT_USAGE: i32 = 2;

/// What the command line asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// `customdata`.
    CustomData,
    /// `ready`.
    Ready,
    /// `--help` / `-h`.
    Help,
    /// `--version` / `-V`.
    Version,
}

/// Parse the arguments after the program name.
///
/// # Errors
/// The usage text, for anything but exactly one known word.
pub fn parse(args: &[String]) -> Result<Command, &'static str> {
    let [arg] = args else {
        return Err(USAGE);
    };
    match arg.as_str() {
        "customdata" => Ok(Command::CustomData),
        "ready" => Ok(Command::Ready),
        "--help" | "-h" => Ok(Command::Help),
        "--version" | "-V" => Ok(Command::Version),
        _ => Err(USAGE),
    }
}

/// Run `command`, logging to `log`; returns the process exit code.
pub fn execute(command: Command, log: &mut Logger) -> i32 {
    match command {
        Command::Help => {
            print!("{USAGE}");
            EXIT_OK
        }
        Command::Version => {
            println!("banlieue-azure-guest {}", env!("CARGO_PKG_VERSION"));
            EXIT_OK
        }
        Command::CustomData => match customdata::run(log) {
            Ok(_) => EXIT_OK,
            Err(e) => {
                log.error(&format!("customdata: {e}"));
                EXIT_FAILED
            }
        },
        Command::Ready => match ready::run(log) {
            Ok(_) => EXIT_OK,
            Err(e) => {
                log.error(&format!("ready: {e}"));
                EXIT_FAILED
            }
        },
    }
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod cli_tests;
