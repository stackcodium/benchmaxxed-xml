use super::*;

impl<'a> Parser<'a> {
    pub(super) fn is_compact_trusted_xml10(&self) -> bool {
        self.version == XmlVersion::Xml10
            && !self.config.preserve_comments
            && !self.config.preserve_processing_instructions
            && !self.config.preserve_cdata_nodes
            && self.config.preserve_text_nodes
            && self.config.effective_text_whitespace()
                == XmlTextWhitespacePolicy::DiscardWhitespaceOnly
            && self.config.attribute_whitespace == XmlAttributeWhitespacePolicy::Normalize
            && !self.config.validate_characters
            && !self.config.validate_references
            && !self.config.validate_duplicate_attributes
    }

    pub(super) fn count_compact_trusted_element_xml10(
        &mut self,
        stats: &mut XmlTreeStats,
        depth: usize,
    ) -> XmlResult<()> {
        self.expect_byte(b'<', "<")?;
        let name = self.parse_name_slice()?;
        let attributes = self.count_compact_trusted_attributes_xml10()?;

        stats.elements += 1;
        stats.attributes += attributes;
        stats.nodes += 1;

        if self.consume_empty_element_end() {
            return Ok(());
        }

        self.expect_byte(b'>', ">")?;
        self.count_compact_trusted_content_xml10(name, stats, depth)
    }

    fn count_compact_trusted_content_xml10(
        &mut self,
        element_name: &str,
        stats: &mut XmlTreeStats,
        depth: usize,
    ) -> XmlResult<()> {
        loop {
            if self.is_eof() {
                return Err(self.error(XmlErrorKind::UnexpectedEof));
            }

            if self.peek() == Some(b'<') {
                match self.bytes.get(self.index + 1).copied() {
                    Some(b'/') => {
                        self.index += 2;
                        self.consume_compact_trusted_end_tag_xml10(element_name)?;
                        return Ok(());
                    }
                    Some(b'!') if self.starts_with("<!--") => self.skip_comment()?,
                    Some(b'!') if self.starts_with("<![CDATA[") => {
                        if self.skip_cdata_count()? {
                            stats.nodes += 1;
                        }
                    }
                    Some(b'!') => return Err(self.error(XmlErrorKind::UnexpectedToken)),
                    Some(b'?') => self.skip_processing_instruction()?,
                    Some(_) => {
                        if depth == MAX_DOM_DEPTH {
                            return Err(self.error(XmlErrorKind::DepthLimitExceeded));
                        }
                        self.count_compact_trusted_element_xml10(stats, depth + 1)?;
                    }
                    None => return Err(self.error(XmlErrorKind::UnexpectedEof)),
                }
            } else {
                let start = self.index;
                self.index = skip_xml_whitespace_bytes(self.bytes, self.index);
                if self.index > start && self.peek() == Some(b'<') {
                    continue;
                }
                self.index = start;
                self.skip_compact_trusted_text_xml10()?;
                stats.nodes += 1;
            }
        }
    }

    fn count_compact_trusted_attributes_xml10(&mut self) -> XmlResult<usize> {
        let mut needs_space = false;
        let mut count = 0usize;

        loop {
            let before_space = self.index;
            self.index = skip_xml_whitespace_bytes(self.bytes, self.index);
            let had_space = self.index != before_space;

            if self.peek() == Some(b'>') || self.starts_empty_element_end() {
                return Ok(count);
            }
            if needs_space && !had_space {
                return Err(self.error(XmlErrorKind::Expected("whitespace")));
            }

            self.parse_name_slice()?;
            self.index = skip_xml_whitespace_bytes(self.bytes, self.index);
            self.expect_byte(b'=', "=")?;
            self.index = skip_xml_whitespace_bytes(self.bytes, self.index);
            self.skip_compact_trusted_attribute_value_xml10()?;
            count += 1;
            needs_space = true;
        }
    }

    pub(super) fn skip_compact_trusted_attribute_value_xml10(
        &mut self,
    ) -> XmlResult<(usize, usize)> {
        let quote = match self.peek() {
            Some(b'"') => b'"',
            Some(b'\'') => b'\'',
            _ => return Err(self.error(XmlErrorKind::Expected("attribute quote"))),
        };
        self.index += 1;
        let value_start = self.index;

        if let Some((index, byte)) = find_byte2(self.bytes, self.index, quote, b'<') {
            self.index = index;
            match byte {
                byte if byte == quote => {
                    let value_len = self.index - value_start;
                    self.index += 1;
                    return Ok((value_start, value_len));
                }
                b'<' => return Err(self.error(XmlErrorKind::InvalidAttributeValue)),
                _ => unreachable!("attribute value delimiter must be quote or '<'"),
            }
        }

        Err(self.error(XmlErrorKind::UnexpectedEof))
    }

    pub(super) fn skip_compact_trusted_text_xml10(&mut self) -> XmlResult<(usize, usize)> {
        let value_start = self.index;
        while let Some((index, byte)) = find_byte2(self.bytes, self.index, b'<', b'>') {
            self.index = index;
            if byte == b'<' {
                return Ok((value_start, self.index - value_start));
            }
            if index >= 2 && self.bytes[index - 2] == b']' && self.bytes[index - 1] == b']' {
                return Err(self.error_at(XmlErrorKind::UnexpectedToken, index - 2));
            }
            self.index += 1;
        }

        Err(self.error(XmlErrorKind::UnexpectedEof))
    }
}
