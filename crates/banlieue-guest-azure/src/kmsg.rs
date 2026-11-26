// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Logging to the kernel log.
//!
//! The spike lost every line written to `/dev/ttyS0` once `agetty` started
//! (its `vhangup` silences handles opened earlier); `/dev/kmsg` survived and
//! the console prints it (ADR-0163 finding 6). Each record is one `write`,
//! prefixed with a syslog priority and the program name.

use std::fs::OpenOptions;
use std::io::{self, Write};

/// The kernel log device.
pub const KMSG_PATH: &str = "/dev/kmsg";

/// Prefix on every record, so the boot log is greppable.
pub const LOG_PREFIX: &str = "banlieue-azure-guest";

/// syslog priority of an informational record.
const PRIORITY_INFO: u8 = 6;
/// syslog priority of an error record.
const PRIORITY_ERR: u8 = 3;

/// The kernel caps a record near 1 KiB; longer messages are cut here, on a
/// character boundary.
const MAX_MESSAGE_BYTES: usize = 900;

/// How bad a record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Progress.
    Info,
    /// A failure.
    Error,
}

impl Level {
    fn priority(self) -> u8 {
        match self {
            Self::Info => PRIORITY_INFO,
            Self::Error => PRIORITY_ERR,
        }
    }
}

/// One kernel-log record: `<priority>banlieue-azure-guest: message\n`, the
/// message truncated and stripped of control characters so one call is one
/// line.
#[must_use]
pub fn format_record(level: Level, message: &str) -> String {
    let mut clean: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if clean.len() > MAX_MESSAGE_BYTES {
        let mut end = MAX_MESSAGE_BYTES;
        while !clean.is_char_boundary(end) {
            end -= 1;
        }
        clean.truncate(end);
    }
    format!("<{}>{LOG_PREFIX}: {clean}\n", level.priority())
}

/// Where records go: `/dev/kmsg` in a guest, any writer in a test.
pub struct Logger {
    sink: Box<dyn Write>,
}

impl Logger {
    /// A logger writing to `/dev/kmsg`, or to stderr when that cannot be
    /// opened (not root, or not Linux).
    #[must_use]
    pub fn kernel() -> Self {
        match OpenOptions::new().write(true).open(KMSG_PATH) {
            Ok(f) => Self { sink: Box::new(f) },
            Err(_) => Self {
                sink: Box::new(io::stderr()),
            },
        }
    }

    /// A logger writing to `sink`.
    #[must_use]
    pub fn to(sink: Box<dyn Write>) -> Self {
        Self { sink }
    }

    /// Write one record. A failed write is ignored: logging must never stop
    /// the helper from doing its job.
    pub fn log(&mut self, level: Level, message: &str) {
        let record = format_record(level, message);
        let _ = self.sink.write_all(record.as_bytes());
        let _ = self.sink.flush();
    }

    /// [`Logger::log`] at [`Level::Info`].
    pub fn info(&mut self, message: &str) {
        self.log(Level::Info, message);
    }

    /// [`Logger::log`] at [`Level::Error`].
    pub fn error(&mut self, message: &str) {
        self.log(Level::Error, message);
    }
}

#[cfg(test)]
#[path = "kmsg_tests.rs"]
mod kmsg_tests;
