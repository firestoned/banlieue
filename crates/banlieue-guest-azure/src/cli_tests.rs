// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `cli.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_two_subcommands_parse() {
        assert_eq!(parse(&args(&["customdata"])), Ok(Command::CustomData));
        assert_eq!(parse(&args(&["ready"])), Ok(Command::Ready));
        assert_eq!(parse(&args(&["--help"])), Ok(Command::Help));
        assert_eq!(parse(&args(&["-V"])), Ok(Command::Version));
    }

    #[test]
    fn nothing_else_parses_not_even_an_address() {
        assert_eq!(parse(&args(&[])), Err(USAGE));
        assert_eq!(parse(&args(&["Ready"])), Err(USAGE));
        assert_eq!(
            parse(&args(&["ready", "--address", "127.0.0.1:80"])),
            Err(USAGE)
        );
        assert_eq!(parse(&args(&["customdata", "/dev/sr0"])), Err(USAGE));
    }

    #[test]
    fn exit_codes_are_distinct() {
        assert_ne!(EXIT_OK, EXIT_FAILED);
        assert_ne!(EXIT_FAILED, EXIT_USAGE);
    }
}
