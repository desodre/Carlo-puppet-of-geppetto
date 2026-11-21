//! Streaming writer for the hierarchy document (docs/hierarchy.md). Output is
//! appended to one buffer under a byte budget: exceeding it is a typed
//! truncation and the caller discards the buffer, so a partial document never
//! leaves this process.

use super::Error;

pub(super) const HEADER: &str = "<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>";

pub(super) struct Writer {
    out: Vec<u8>,
    budget: usize,
    sanitized: u64,
}

impl Writer {
    pub(super) fn new(budget: usize) -> Result<Self, Error> {
        let mut writer = Self {
            out: Vec::new(),
            budget,
            sanitized: 0,
        };
        writer.raw(HEADER)?;
        Ok(writer)
    }

    fn raw(&mut self, text: &str) -> Result<(), Error> {
        if text.len() > self.budget - self.out.len() {
            return Err(Error::Truncated { limit: "xml_bytes" });
        }
        self.out.extend_from_slice(text.as_bytes());
        Ok(())
    }

    /// `<name`; attributes follow, then [`Self::close_start`] or [`Self::empty`].
    pub(super) fn open(&mut self, name: &str) -> Result<(), Error> {
        self.raw("<")?;
        self.raw(name)
    }

    /// One attribute. `name` is a fixed identifier; `value` is escaped. Characters
    /// XML 1.0 cannot carry become U+FFFD and are counted. TAB, LF and CR become
    /// character references so attribute-value normalisation keeps them.
    pub(super) fn attribute(&mut self, name: &str, value: &str) -> Result<(), Error> {
        self.raw(" ")?;
        self.raw(name)?;
        self.raw("=\"")?;
        let mut plain = 0;
        for (index, character) in value.char_indices() {
            let replacement = match character {
                '&' => "&amp;",
                '<' => "&lt;",
                '>' => "&gt;",
                '"' => "&quot;",
                '\'' => "&apos;",
                '\t' => "&#9;",
                '\n' => "&#10;",
                '\r' => "&#13;",
                '\u{0}'..='\u{8}'
                | '\u{b}'
                | '\u{c}'
                | '\u{e}'..='\u{1f}'
                | '\u{fffe}'
                | '\u{ffff}' => {
                    self.sanitized += 1;
                    "\u{fffd}"
                }
                _ => continue,
            };
            self.raw(&value[plain..index])?;
            self.raw(replacement)?;
            plain = index + character.len_utf8();
        }
        self.raw(&value[plain..])?;
        self.raw("\"")
    }

    pub(super) fn close_start(&mut self) -> Result<(), Error> {
        self.raw(">")
    }

    pub(super) fn empty(&mut self) -> Result<(), Error> {
        self.raw(" />")
    }

    pub(super) fn end(&mut self, name: &str) -> Result<(), Error> {
        self.raw("</")?;
        self.raw(name)?;
        self.raw(">")
    }

    /// The complete document and the number of replaced characters.
    pub(super) fn finish(self) -> (Vec<u8>, u64) {
        (self.out, self.sanitized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attribute(value: &str) -> (String, u64) {
        let mut writer = Writer::new(1 << 20).unwrap();
        writer.open("n").unwrap();
        writer.attribute("a", value).unwrap();
        writer.empty().unwrap();
        let (bytes, sanitized) = writer.finish();
        let text = String::from_utf8(bytes).unwrap();
        let body = text
            .strip_prefix(HEADER)
            .and_then(|rest| rest.strip_prefix("<n a=\""))
            .and_then(|rest| rest.strip_suffix("\" />"))
            .unwrap()
            .to_owned();
        (body, sanitized)
    }

    #[test]
    fn escaping_table() {
        for (input, expected) in [
            ("", ""),
            ("plain", "plain"),
            ("&", "&amp;"),
            ("<", "&lt;"),
            (">", "&gt;"),
            ("\"", "&quot;"),
            ("'", "&apos;"),
            ("a&b<c>d\"e'f", "a&amp;b&lt;c&gt;d&quot;e&apos;f"),
            ("&amp;", "&amp;amp;"),
            ("]]>", "]]&gt;"),
            ("a\tb\nc\rd", "a&#9;b&#10;c&#13;d"),
            ("\r\n", "&#13;&#10;"),
        ] {
            assert_eq!(attribute(input), (expected.to_owned(), 0), "{input:?}");
        }
    }

    #[test]
    fn characters_xml_cannot_carry_are_replaced_and_counted() {
        let mut every = String::new();
        for code in (0..0x20_u32).chain([0xfffe, 0xffff]) {
            every.push(char::from_u32(code).unwrap());
        }
        let (body, sanitized) = attribute(&every);
        // 32 C0 controls minus TAB, LF and CR, plus the two noncharacters.
        assert_eq!(sanitized, 31);
        assert_eq!(body.matches('\u{fffd}').count(), 31);
        assert_eq!(body.replace('\u{fffd}', ""), "&#9;&#10;&#13;");
        assert_eq!(attribute("a\0b"), ("a\u{fffd}b".to_owned(), 1));
        // Already-present replacement characters are data, not sanitisation.
        assert_eq!(attribute("\u{fffd}"), ("\u{fffd}".to_owned(), 0));
    }

    #[test]
    fn unicode_emoji_and_astral_characters_are_preserved_as_utf8() {
        for text in [
            "ação",
            "終了",
            "🪵",
            "👩‍👩‍👧‍👦",
            "\u{10ffff}",
            "\u{7f}\u{85}\u{a0}",
            "\u{fffd}\u{e000}\u{d7ff}",
            "العربية",
        ] {
            assert_eq!(attribute(text), (text.to_owned(), 0));
        }
    }

    #[test]
    fn byte_budget_is_exact_and_exceeding_it_is_a_typed_truncation() {
        let document = |budget: usize| -> Result<Vec<u8>, Error> {
            let mut writer = Writer::new(budget)?;
            writer.open("hierarchy")?;
            writer.attribute("rotation", "unknown")?;
            writer.close_start()?;
            writer.open("node")?;
            writer.attribute("text", "a&🪵")?;
            writer.empty()?;
            writer.end("hierarchy")?;
            Ok(writer.finish().0)
        };
        let full = document(usize::MAX).unwrap();
        assert_eq!(
            String::from_utf8(full.clone()).unwrap(),
            format!(
                "{HEADER}<hierarchy rotation=\"unknown\"><node text=\"a&amp;🪵\" /></hierarchy>"
            )
        );
        assert_eq!(document(full.len()).unwrap(), full);
        for budget in 0..full.len() {
            assert_eq!(
                document(budget).unwrap_err(),
                Error::Truncated { limit: "xml_bytes" },
                "budget {budget}"
            );
        }
    }
}
