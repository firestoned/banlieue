// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `xml/escape.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    // ------------------------------------------------------------------
    // The five predefined entities
    // ------------------------------------------------------------------

    #[test]
    fn passes_through_text_needing_no_escaping() {
        assert_eq!(esc("db-prod-01").unwrap(), "db-prod-01");
        assert_eq!(esc("").unwrap(), "");
        assert_eq!(
            esc("/var/lib/libvirt/images/x.qcow2").unwrap(),
            "/var/lib/libvirt/images/x.qcow2"
        );
    }

    #[test]
    fn escapes_all_five_predefined_entities() {
        assert_eq!(esc("&").unwrap(), "&amp;");
        assert_eq!(esc("<").unwrap(), "&lt;");
        assert_eq!(esc(">").unwrap(), "&gt;");
        assert_eq!(esc("\"").unwrap(), "&quot;");
        assert_eq!(esc("'").unwrap(), "&apos;");
    }

    /// `&` must be escaped first, or `&lt;` becomes `&amp;lt;`. Pinning the
    /// composite case catches an ordering regression that single-character
    /// tests cannot.
    #[test]
    fn escapes_ampersand_without_double_escaping_the_rest() {
        assert_eq!(esc("a&<b").unwrap(), "a&amp;&lt;b");
        assert_eq!(esc("&amp;").unwrap(), "&amp;amp;");
    }

    /// One escaper for both attribute values and element text. Quotes matter
    /// only in attributes and angle brackets only in text, but a single
    /// function that handles all five removes the "which one do I call here"
    /// decision — and that decision is where injection bugs come from.
    #[test]
    fn output_is_safe_in_both_attribute_and_text_position() {
        let hostile = r#"br0"/><disk type='file'><source file='/etc/shadow'/></disk><x a=""#;
        let escaped = esc(hostile).unwrap();
        assert!(!escaped.contains('<'), "{escaped}");
        assert!(!escaped.contains('>'), "{escaped}");
        assert!(!escaped.contains('"'), "{escaped}");
        assert!(!escaped.contains('\''), "{escaped}");
    }

    /// The whole point: a bridge name carrying markup cannot close the
    /// attribute it sits in and add a device of the attacker's choosing.
    #[test]
    fn a_hostile_value_cannot_break_out_of_an_attribute() {
        let bridge = esc(r#"br0'/><disk/><interface a='"#).unwrap();
        let xml = format!("<source bridge='{bridge}'/>");
        // Exactly one element: the one we wrote.
        assert_eq!(xml.matches('<').count(), 1, "{xml}");
        assert!(!xml.contains("<disk"), "{xml}");
    }

    // ------------------------------------------------------------------
    // Characters that cannot be escaped at all
    // ------------------------------------------------------------------

    /// XML 1.0 has no representation for most control characters — not even
    /// as a numeric reference. Escaping cannot help, so these are rejected
    /// rather than silently dropped: a domain name containing a NUL is a bug
    /// upstream, and quietly renaming the domain would be worse than failing.
    #[test]
    fn rejects_characters_xml_cannot_represent() {
        for bad in ['\0', '\u{1}', '\u{8}', '\u{b}', '\u{c}', '\u{e}', '\u{1f}'] {
            let input = format!("name{bad}suffix");
            assert!(
                esc(&input).is_err(),
                "U+{:04X} should be rejected",
                bad as u32
            );
        }
    }

    /// Tab, newline and carriage return are the three control characters XML
    /// 1.0 does allow, and cloud-config payloads are full of them.
    #[test]
    fn allows_the_three_legal_control_characters() {
        assert_eq!(esc("a\tb").unwrap(), "a\tb");
        assert_eq!(esc("a\nb").unwrap(), "a\nb");
        assert_eq!(esc("a\rb").unwrap(), "a\rb");
    }

    #[test]
    fn reports_where_the_illegal_character_was() {
        let err = esc("ok\u{0}bad").unwrap_err();
        let XmlError::IllegalChar {
            position,
            codepoint,
        } = err;
        assert_eq!(position, 2);
        assert_eq!(codepoint, 0);
    }

    /// Non-ASCII is fine and must survive byte-for-byte: libvirt speaks
    /// UTF-8, and a pool path can legitimately contain it.
    #[test]
    fn preserves_multibyte_utf8() {
        assert_eq!(esc("café-01").unwrap(), "café-01");
        assert_eq!(esc("données/disque.qcow2").unwrap(), "données/disque.qcow2");
    }

    /// Position is a character index, not a byte offset — otherwise the
    /// number in the error points into the middle of a multibyte character.
    #[test]
    fn position_is_a_character_index_not_a_byte_offset() {
        let err = esc("café\u{0}").unwrap_err();
        let XmlError::IllegalChar { position, .. } = err;
        assert_eq!(position, 4, "é is two bytes but one character");
    }
}
