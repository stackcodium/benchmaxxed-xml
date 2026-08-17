use super::*;

impl<'a> Parser<'a> {
    pub(super) fn consume_compact_trusted_end_tag_xml10(
        &mut self,
        element_name: &str,
    ) -> XmlResult<()> {
        let name_start = self.index;
        let name_end = name_start + element_name.len();

        if end_tag_name_matches(self.bytes, name_start, element_name) {
            match self.bytes.get(name_end).copied() {
                Some(b'>') => {
                    self.index = name_end + 1;
                    return Ok(());
                }
                Some(byte) if is_space(byte) => {
                    self.index = skip_xml_whitespace_bytes(self.bytes, name_end);
                    self.expect_byte(b'>', ">")?;
                    return Ok(());
                }
                _ => {}
            }
        }

        let end_name = self.parse_name_slice()?;
        self.index = skip_xml_whitespace_bytes(self.bytes, self.index);
        self.expect_byte(b'>', ">")?;
        self.reject_mismatched_end_tag(element_name, end_name)
    }

    #[inline(always)]
    pub(super) fn consume_end_tag_matching(&mut self, element_name: &str) -> XmlResult<()> {
        let name_start = self.index;
        let name_end = name_start + element_name.len();

        if end_tag_name_matches(self.bytes, name_start, element_name) {
            if self.bytes.get(name_end).copied() == Some(b'>') {
                self.index = name_end + 1;
                return Ok(());
            }
            if self.starts_xml_whitespace_at(name_end) {
                self.index = name_end;
                self.skip_whitespace();
                self.expect_byte(b'>', ">")?;
                return Ok(());
            }
        }

        let end_name = self.parse_name_slice()?;
        self.skip_whitespace();
        self.expect_byte(b'>', ">")?;
        self.reject_mismatched_end_tag(element_name, end_name)
    }

    fn reject_mismatched_end_tag(&self, element_name: &str, end_name: &str) -> XmlResult<()> {
        if end_name != element_name {
            return Err(self.error_at(
                XmlErrorKind::MismatchedEndTag {
                    expected: element_name.to_owned(),
                    found: end_name.to_owned(),
                },
                self.index,
            ));
        }
        Ok(())
    }
}

#[inline(always)]
fn end_tag_name_matches(bytes: &[u8], start: usize, expected: &str) -> bool {
    let expected = expected.as_bytes();
    bytes
        .get(start..)
        .is_some_and(|remaining| remaining.starts_with(expected))
}
