// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `kmsg.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    /// A writer the test can read back.
    #[derive(Clone, Default)]
    struct Shared(Rc<RefCell<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn info_record_carries_priority_six_and_the_prefix() {
        assert_eq!(
            format_record(Level::Info, "ready reported"),
            "<6>banlieue-azure-guest: ready reported\n"
        );
    }

    #[test]
    fn error_record_carries_priority_three() {
        assert!(format_record(Level::Error, "x").starts_with("<3>"));
    }

    #[test]
    fn control_characters_cannot_split_a_record() {
        let record = format_record(Level::Info, "a\nb\rc\u{1b}d");
        assert_eq!(record, "<6>banlieue-azure-guest: a b c d\n");
        assert_eq!(record.matches('\n').count(), 1);
    }

    #[test]
    fn long_messages_are_truncated_on_a_char_boundary() {
        let long = "\u{e9}".repeat(2000);
        let record = format_record(Level::Info, &long);
        assert!(record.len() < 1024);
        assert!(record.ends_with('\n'));
    }

    #[test]
    fn logger_writes_one_record_per_call() {
        let sink = Shared::default();
        let mut log = Logger::to(Box::new(sink.clone()));
        log.info("one");
        log.error("two");
        let out = String::from_utf8(sink.0.borrow().clone()).expect("utf8");
        assert_eq!(
            out,
            "<6>banlieue-azure-guest: one\n<3>banlieue-azure-guest: two\n"
        );
    }
}
