use super::*;

impl<'a> Parser<'a> {
    pub(super) fn parse_attribute_value(&mut self) -> XmlResult<String> {
        let quote = match self.peek() {
            Some(b'"') => b'"',
            Some(b'\'') => b'\'',
            _ => return Err(self.error(XmlErrorKind::Expected("attribute quote"))),
        };
        self.index += 1;

        let mut value: Option<String> = None;
        let mut segment_start = self.index;

        while let Some(delimiter) = self.find_attribute_value_delimiter(quote) {
            self.index = delimiter.index;
            if delimiter.byte == quote {
                let segment = self.normalize_attribute_value_known(
                    &self.input[segment_start..self.index],
                    delimiter.needs_normalization,
                );
                self.index += 1;
                let value = if let Some(mut value) = value {
                    value.push_str(&segment);
                    value
                } else {
                    segment
                };
                return Ok(self.apply_attribute_whitespace(value));
            }

            match delimiter.byte {
                b'<' => return Err(self.error(XmlErrorKind::InvalidAttributeValue)),
                b'&' => {
                    if !self.config.validate_references {
                        return self.parse_trusted_attribute_value_tail(quote, segment_start);
                    }
                    let value = value.get_or_insert_with(|| String::with_capacity(32));
                    value.push_str(&self.normalize_attribute_value_known(
                        &self.input[segment_start..self.index],
                        delimiter.needs_normalization,
                    ));
                    let resolved = self.parse_reference()?;
                    value.push_str(&resolved);
                    segment_start = self.index;
                }
                _ => unreachable!("attribute value delimiter must be quote, '<', or '&'"),
            }
        }

        Err(self.error(XmlErrorKind::UnexpectedEof))
    }

    #[inline(always)]
    pub(super) fn skip_attribute_value(&mut self) -> XmlResult<(usize, usize)> {
        let quote = match self.peek() {
            Some(b'"') => b'"',
            Some(b'\'') => b'\'',
            _ => return Err(self.error(XmlErrorKind::Expected("attribute quote"))),
        };
        self.index += 1;
        let value_start = self.index;

        if !self.config.validate_references {
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
            return Err(self.error(XmlErrorKind::UnexpectedEof));
        }

        while let Some(delimiter) = self.find_attribute_value_delimiter(quote) {
            self.index = delimiter.index;
            match delimiter.byte {
                byte if byte == quote => {
                    let value_len = self.index - value_start;
                    self.index += 1;
                    return Ok((value_start, value_len));
                }
                b'<' => return Err(self.error(XmlErrorKind::InvalidAttributeValue)),
                b'&' => {
                    if self.config.validate_references {
                        self.skip_reference()?;
                    } else {
                        self.index += 1;
                    }
                }
                _ => unreachable!("attribute value delimiter must be quote, '<', or '&'"),
            }
        }

        Err(self.error(XmlErrorKind::UnexpectedEof))
    }

    #[cold]
    #[inline(never)]
    fn parse_trusted_attribute_value_tail(
        &mut self,
        quote: u8,
        value_start: usize,
    ) -> XmlResult<String> {
        if let Some((index, byte)) = find_byte2(self.bytes, self.index + 1, quote, b'<') {
            self.index = index;
            match byte {
                byte if byte == quote => {
                    let value = normalize_attribute_value(
                        &self.input[value_start..self.index],
                        self.version,
                    );
                    self.index += 1;
                    return Ok(self.apply_attribute_whitespace(value));
                }
                b'<' => return Err(self.error(XmlErrorKind::InvalidAttributeValue)),
                _ => unreachable!("attribute value delimiter must be quote or '<'"),
            }
        }
        Err(self.error(XmlErrorKind::UnexpectedEof))
    }

    #[inline(always)]
    pub(super) fn skip_text(&mut self) -> XmlResult<(usize, usize)> {
        let value_start = self.index;
        if !self.config.validate_references {
            return self.skip_text_trusted_references();
        }

        while let Some(delimiter) = self.find_text_delimiter() {
            self.index = delimiter.index;
            match delimiter.byte {
                b'<' => return Ok((value_start, self.index - value_start)),
                b'&' => {
                    if self.config.validate_references {
                        self.skip_reference()?;
                    } else {
                        self.index += 1;
                    }
                }
                b'>' => {
                    return Err(self.error_at(XmlErrorKind::UnexpectedToken, delimiter.index - 2));
                }
                _ => unreachable!("text delimiter must be '<', '&', or a CDATA close marker"),
            }
        }

        Err(self.error(XmlErrorKind::UnexpectedEof))
    }

    pub(super) fn skip_text_no_range(&mut self) -> XmlResult<()> {
        if !self.config.validate_references {
            return self.skip_text_trusted_references_no_range();
        }

        while let Some(delimiter) = self.find_text_delimiter() {
            self.index = delimiter.index;
            match delimiter.byte {
                b'<' => return Ok(()),
                b'&' => {
                    self.skip_reference()?;
                }
                b'>' => {
                    return Err(self.error_at(XmlErrorKind::UnexpectedToken, delimiter.index - 2));
                }
                _ => unreachable!("text delimiter must be '<', '&', or a CDATA close marker"),
            }
        }

        Err(self.error(XmlErrorKind::UnexpectedEof))
    }

    #[inline(always)]
    pub(super) fn skip_text_trusted_references(&mut self) -> XmlResult<(usize, usize)> {
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

    pub(super) fn skip_text_trusted_references_no_range(&mut self) -> XmlResult<()> {
        while let Some((index, byte)) = find_byte2(self.bytes, self.index, b'<', b'>') {
            self.index = index;
            if byte == b'<' {
                return Ok(());
            }
            if index >= 2 && self.bytes[index - 2] == b']' && self.bytes[index - 1] == b']' {
                return Err(self.error_at(XmlErrorKind::UnexpectedToken, index - 2));
            }
            self.index += 1;
        }

        Err(self.error(XmlErrorKind::UnexpectedEof))
    }

    pub(super) fn skip_whitespace_text(&mut self) -> XmlResult<bool> {
        let start = self.index;
        self.index = self.skip_xml_whitespace_at(self.index);
        if self.index > start && self.peek() == Some(b'<') {
            return Ok(true);
        }

        self.index = start;
        Ok(false)
    }

    pub(super) fn find_attribute_value_delimiter(&self, quote: u8) -> Option<SegmentDelimiter> {
        if self.version == XmlVersion::Xml11 {
            return self.find_xml11_attribute_value_delimiter(quote);
        }

        let mut index = self.index;
        let mut needs_normalization = false;
        while let Some((candidate, byte)) = find_byte4(self.bytes, index, b'<', b'&', b'\r', quote)
        {
            index = candidate;
            if byte != b'\r' {
                return Some(SegmentDelimiter {
                    byte,
                    index,
                    needs_normalization,
                });
            }
            needs_normalization = true;
            index = candidate + 1;
        }
        None
    }

    pub(super) fn find_text_delimiter(&self) -> Option<SegmentDelimiter> {
        if self.version == XmlVersion::Xml11 {
            return self.find_xml11_text_delimiter();
        }

        let mut index = self.index;
        let mut needs_normalization = false;
        while let Some((candidate, byte)) = find_byte4(self.bytes, index, b'<', b'&', b'>', b'\r') {
            index = candidate;
            if byte == b'\r' {
                needs_normalization = true;
                index += 1;
                continue;
            }
            if byte != b'>' {
                return Some(SegmentDelimiter {
                    byte,
                    index,
                    needs_normalization,
                });
            }
            if index >= 2 && self.bytes[index - 2] == b']' && self.bytes[index - 1] == b']' {
                return Some(SegmentDelimiter {
                    byte,
                    index,
                    needs_normalization,
                });
            }
            index += 1;
        }
        None
    }

    pub(super) fn find_xml11_attribute_value_delimiter(
        &self,
        quote: u8,
    ) -> Option<SegmentDelimiter> {
        let mut index = self.index;
        let mut needs_normalization = false;
        while let Some(byte) = self.bytes.get(index).copied() {
            if byte == quote || byte == b'<' || byte == b'&' {
                return Some(SegmentDelimiter {
                    byte,
                    index,
                    needs_normalization,
                });
            }
            match byte {
                b'\t' | b'\n' | b'\r' => needs_normalization = true,
                0xc2 if self.bytes.get(index + 1) == Some(&0x85) => {
                    needs_normalization = true;
                    index += 1;
                }
                0xe2 if self.bytes.get(index + 1) == Some(&0x80)
                    && self.bytes.get(index + 2) == Some(&0xa8) =>
                {
                    needs_normalization = true;
                    index += 2;
                }
                _ => {}
            }
            index += 1;
        }
        None
    }

    pub(super) fn find_xml11_text_delimiter(&self) -> Option<SegmentDelimiter> {
        let mut index = self.index;
        let mut needs_normalization = false;
        while let Some(byte) = self.bytes.get(index).copied() {
            match byte {
                b'<' | b'&' => {
                    return Some(SegmentDelimiter {
                        byte,
                        index,
                        needs_normalization,
                    });
                }
                b'>' => {
                    if index >= 2 && self.bytes[index - 2] == b']' && self.bytes[index - 1] == b']'
                    {
                        return Some(SegmentDelimiter {
                            byte,
                            index,
                            needs_normalization,
                        });
                    }
                }
                b'\r' => needs_normalization = true,
                0xc2 if self.bytes.get(index + 1) == Some(&0x85) => {
                    needs_normalization = true;
                    index += 1;
                }
                0xe2 if self.bytes.get(index + 1) == Some(&0x80)
                    && self.bytes.get(index + 2) == Some(&0xa8) =>
                {
                    needs_normalization = true;
                    index += 2;
                }
                _ => {}
            }
            index += 1;
        }
        None
    }

    pub(super) fn parse_comment(&mut self) -> XmlResult<String> {
        self.expect("<!--")?;
        let start = self.index;
        let end = self
            .input
            .get(self.index..)
            .and_then(|tail| tail.find("-->"))
            .map(|offset| self.index + offset)
            .ok_or_else(|| self.error(XmlErrorKind::UnexpectedEof))?;

        let comment = &self.input[start..end];
        if comment.contains("--") || comment.ends_with('-') {
            return Err(self.error_at(XmlErrorKind::InvalidComment, start));
        }

        self.index = end + 3;
        Ok(normalize_newlines(comment, self.version))
    }

    #[inline(always)]
    pub(super) fn skip_comment(&mut self) -> XmlResult<()> {
        self.expect("<!--")?;
        self.skip_comment_body()
    }

    #[inline(always)]
    pub(super) fn skip_comment_opened(&mut self) -> XmlResult<()> {
        debug_assert!(self.starts_with("<!--"));
        self.index += 4;
        self.skip_comment_body()
    }

    #[inline(always)]
    fn skip_comment_body(&mut self) -> XmlResult<()> {
        let start = self.index;
        while self.index + 2 < self.bytes.len() {
            if self.bytes[self.index] == b'-' && self.bytes[self.index + 1] == b'-' {
                if self.bytes[self.index + 2] == b'>' {
                    self.index += 3;
                    return Ok(());
                }
                return Err(self.error_at(XmlErrorKind::InvalidComment, start));
            }
            self.index += 1;
        }
        Err(self.error_at(XmlErrorKind::UnexpectedEof, start))
    }

    pub(super) fn skip_cdata(&mut self) -> XmlResult<()> {
        self.expect("<![CDATA[")?;
        let end = self
            .input
            .get(self.index..)
            .and_then(|tail| tail.find("]]>"))
            .map(|offset| self.index + offset)
            .ok_or_else(|| self.error(XmlErrorKind::UnexpectedEof))?;

        self.index = end + 3;
        Ok(())
    }

    #[cfg_attr(debug_assertions, allow(dead_code))]
    pub(super) fn skip_cdata_count(&mut self) -> XmlResult<bool> {
        Ok(self.skip_cdata_range()?.is_some())
    }

    #[inline(always)]
    pub(super) fn skip_cdata_range(&mut self) -> XmlResult<Option<(usize, usize)>> {
        self.expect("<![CDATA[")?;
        self.skip_cdata_range_body()
    }

    #[inline(always)]
    pub(super) fn skip_cdata_range_opened(&mut self) -> XmlResult<Option<(usize, usize)>> {
        debug_assert!(self.starts_with("<![CDATA["));
        self.index += 9;
        self.skip_cdata_range_body()
    }

    #[inline(always)]
    fn skip_cdata_range_body(&mut self) -> XmlResult<Option<(usize, usize)>> {
        let start = self.index;
        while self.index + 2 < self.bytes.len() {
            if self.bytes[self.index] == b']'
                && self.bytes[self.index + 1] == b']'
                && self.bytes[self.index + 2] == b'>'
            {
                let len = self.index - start;
                self.index += 3;
                return Ok((len != 0).then_some((start, len)));
            }
            self.index += 1;
        }
        Err(self.error_at(XmlErrorKind::UnexpectedEof, start))
    }

    pub(super) fn parse_xml_declaration(&mut self) -> XmlResult<XmlProcessingInstruction> {
        let pi = self.parse_processing_instruction_with_target(TargetMode::XmlDeclaration)?;
        if !is_xml_target(&pi.target) {
            return Err(self.error(XmlErrorKind::Expected("XML declaration")));
        }
        let Some(version) = parse_xml_declaration_version(&pi.data) else {
            return Err(self.error(XmlErrorKind::InvalidXmlDeclaration));
        };
        self.version = version;
        Ok(pi)
    }

    pub(super) fn parse_processing_instruction(&mut self) -> XmlResult<XmlProcessingInstruction> {
        self.parse_processing_instruction_with_target(TargetMode::ProcessingInstruction)
    }

    pub(super) fn skip_processing_instruction(&mut self) -> XmlResult<()> {
        self.expect("<?")?;
        let target = self.parse_name_slice()?;
        if is_xml_target(target) {
            return Err(self.error(XmlErrorKind::InvalidProcessingInstructionTarget));
        }

        if self.starts_with("?>") {
            self.index += 2;
            return Ok(());
        }

        self.require_space()?;
        let end = self
            .input
            .get(self.index..)
            .and_then(|tail| tail.find("?>"))
            .map(|offset| self.index + offset)
            .ok_or_else(|| self.error(XmlErrorKind::UnexpectedEof))?;
        self.index = end + 2;
        Ok(())
    }

    #[inline(always)]
    pub(super) fn skip_processing_instruction_target(&mut self) -> XmlResult<(usize, usize)> {
        self.expect("<?")?;
        self.skip_processing_instruction_target_body()
    }

    #[inline(always)]
    pub(super) fn skip_processing_instruction_target_opened(
        &mut self,
    ) -> XmlResult<(usize, usize)> {
        debug_assert!(self.starts_with("<?"));
        self.index += 2;
        self.skip_processing_instruction_target_body()
    }

    #[inline(always)]
    fn skip_processing_instruction_target_body(&mut self) -> XmlResult<(usize, usize)> {
        let target_start = self.index;
        let target = self.parse_name_slice()?;
        let target_len = target.len();
        if is_xml_target(target) {
            return Err(self.error(XmlErrorKind::InvalidProcessingInstructionTarget));
        }

        if self.starts_with("?>") {
            self.index += 2;
            return Ok((target_start, target_len));
        }

        self.require_space()?;
        let data_start = self.index;
        while self.index + 1 < self.bytes.len() {
            if self.bytes[self.index] == b'?' && self.bytes[self.index + 1] == b'>' {
                self.index += 2;
                return Ok((target_start, target_len));
            }
            self.index += 1;
        }
        Err(self.error_at(XmlErrorKind::UnexpectedEof, data_start))
    }

    pub(super) fn parse_processing_instruction_with_target(
        &mut self,
        mode: TargetMode,
    ) -> XmlResult<XmlProcessingInstruction> {
        self.expect("<?")?;
        let target = self.parse_name()?;
        if mode == TargetMode::ProcessingInstruction && is_xml_target(&target) {
            return Err(self.error(XmlErrorKind::InvalidProcessingInstructionTarget));
        }

        let data = if self.starts_with("?>") {
            String::new()
        } else {
            self.require_space()?;
            let start = self.index;
            let end = self
                .input
                .get(self.index..)
                .and_then(|tail| tail.find("?>"))
                .map(|offset| self.index + offset)
                .ok_or_else(|| self.error(XmlErrorKind::UnexpectedEof))?;
            self.index = end;
            normalize_newlines(&self.input[start..end], self.version)
        };

        self.expect("?>")?;
        Ok(XmlProcessingInstruction { target, data })
    }

    pub(super) fn skip_doctype(&mut self) -> XmlResult<()> {
        self.parse_doctype_impl(false).map(|_| ())
    }

    pub(super) fn parse_doctype(&mut self) -> XmlResult<XmlDoctype> {
        self.parse_doctype_impl(true)
            .map(|doctype| doctype.expect("preserving doctype produces metadata"))
    }

    fn parse_doctype_impl(&mut self, preserve: bool) -> XmlResult<Option<XmlDoctype>> {
        self.expect("<!DOCTYPE")?;
        self.require_space()?;
        let name = {
            let parsed = self.parse_name_slice()?;
            preserve.then(|| parsed.to_owned())
        };
        self.skip_whitespace();

        let mut public_id = None;
        let mut system_id = None;

        if self.consume_doctype_keyword("SYSTEM") {
            self.require_space()?;
            let start = self.index;
            self.skip_quoted_literal()?;
            if preserve {
                system_id = Some(self.input[start + 1..self.index - 1].to_owned());
            }
            self.skip_whitespace();
        } else if self.consume_doctype_keyword("PUBLIC") {
            self.require_space()?;
            let public_start = self.index;
            self.skip_pubid_literal()?;
            if preserve {
                public_id = Some(self.input[public_start + 1..self.index - 1].to_owned());
            }
            self.require_space()?;
            let system_start = self.index;
            self.skip_quoted_literal()?;
            if preserve {
                system_id = Some(self.input[system_start + 1..self.index - 1].to_owned());
            }
            self.skip_whitespace();
        }

        if !self.starts_with("[") && !self.starts_with(">") {
            return Err(self.error(XmlErrorKind::InvalidDocumentStructure));
        }

        let mut quote = None;
        let mut bracket_depth = 0usize;
        let mut subset_start = None;
        let mut internal_subset = None;

        while let Some(byte) = self.peek() {
            if let Some(active_quote) = quote {
                self.index += self.char_width_at_index();
                if byte == active_quote {
                    quote = None;
                }
                continue;
            }

            match byte {
                b'"' | b'\'' => {
                    quote = Some(byte);
                    self.index += 1;
                }
                _ if bracket_depth > 0 && self.starts_with("<!--") => {
                    let end = self
                        .input
                        .get(self.index + 4..)
                        .and_then(|tail| tail.find("-->"))
                        .map(|offset| self.index + 4 + offset + 3)
                        .ok_or_else(|| self.error(XmlErrorKind::UnexpectedEof))?;
                    self.index = end;
                }
                _ if bracket_depth > 0 && self.starts_with("<?") => {
                    let end = self
                        .input
                        .get(self.index + 2..)
                        .and_then(|tail| tail.find("?>"))
                        .map(|offset| self.index + 2 + offset + 2)
                        .ok_or_else(|| self.error(XmlErrorKind::UnexpectedEof))?;
                    self.index = end;
                }
                b'[' => {
                    if bracket_depth == 0 {
                        subset_start = Some(self.index + 1);
                    }
                    bracket_depth += 1;
                    self.index += 1;
                }
                b']' if bracket_depth > 0 => {
                    bracket_depth -= 1;
                    if bracket_depth == 0 {
                        if let Some(start) = subset_start.take() {
                            self.general_entities = parse_internal_subset_entities(
                                &self.input[start..self.index],
                                start,
                                self.version == XmlVersion::Xml11,
                            )?;
                            if preserve {
                                internal_subset = Some(self.input[start..self.index].to_owned());
                            }
                        }
                    }
                    self.index += 1;
                }
                b'>' if bracket_depth == 0 => {
                    self.index += 1;
                    return Ok(name.map(|name| XmlDoctype {
                        name,
                        public_id,
                        system_id,
                        internal_subset,
                    }));
                }
                _ => self.index += self.char_width_at_index(),
            }
        }

        Err(self.error(XmlErrorKind::UnexpectedEof))
    }

    pub(super) fn consume_doctype_keyword(&mut self, keyword: &'static str) -> bool {
        if !self.starts_with(keyword) {
            return false;
        }
        if self
            .input
            .get(self.index + keyword.len()..)
            .and_then(|tail| tail.chars().next())
            .is_some_and(is_name_char)
        {
            return false;
        }
        self.index += keyword.len();
        true
    }

    pub(super) fn skip_quoted_literal(&mut self) -> XmlResult<()> {
        let quote = match self.peek() {
            Some(b'"') => b'"',
            Some(b'\'') => b'\'',
            _ => return Err(self.error(XmlErrorKind::Expected("quoted literal"))),
        };
        self.index += 1;

        while let Some(byte) = self.peek() {
            self.index += self.char_width_at_index();
            if byte == quote {
                return Ok(());
            }
        }

        Err(self.error(XmlErrorKind::UnexpectedEof))
    }

    pub(super) fn skip_pubid_literal(&mut self) -> XmlResult<()> {
        let quote = match self.peek() {
            Some(b'"') => b'"',
            Some(b'\'') => b'\'',
            _ => return Err(self.error(XmlErrorKind::Expected("quoted literal"))),
        };
        self.index += 1;

        while let Some(byte) = self.peek() {
            if byte == quote {
                self.index += 1;
                return Ok(());
            }

            let ch = self.peek_char()?;
            if !is_pubid_char(ch) {
                return Err(self.error(XmlErrorKind::InvalidDocumentStructure));
            }
            self.index += ch.len_utf8();
        }

        Err(self.error(XmlErrorKind::UnexpectedEof))
    }

    pub(super) fn parse_reference(&mut self) -> XmlResult<String> {
        self.expect("&")?;
        if self.consume("#x") {
            return self.parse_char_reference(16);
        }
        if self.consume("#") {
            return self.parse_char_reference(10);
        }

        let name = self.parse_name_slice()?;
        self.expect(";")?;
        match name {
            "amp" => Ok("&".to_owned()),
            "lt" => Ok("<".to_owned()),
            "gt" => Ok(">".to_owned()),
            "apos" => Ok("'".to_owned()),
            "quot" => Ok("\"".to_owned()),
            _ => self.resolve_runtime_entity(name),
        }
    }

    pub(super) fn parse_char_reference(&mut self, radix: u32) -> XmlResult<String> {
        let start = self.index;
        while let Some(byte) = self.peek() {
            let valid = match radix {
                16 => byte.is_ascii_hexdigit(),
                _ => byte.is_ascii_digit(),
            };
            if !valid {
                break;
            }
            self.index += 1;
        }

        if self.index == start {
            return Err(self.error(XmlErrorKind::InvalidCharacterReference));
        }

        let digits = &self.input[start..self.index];
        self.expect(";")?;
        let value = u32::from_str_radix(digits, radix)
            .map_err(|_| self.error_at(XmlErrorKind::InvalidCharacterReference, start))?;
        let ch = char::from_u32(value)
            .filter(|ch| self.is_xml_char(*ch))
            .ok_or_else(|| self.error_at(XmlErrorKind::InvalidCharacterReference, start))?;

        Ok(ch.to_string())
    }

    pub(super) fn skip_reference(&mut self) -> XmlResult<()> {
        self.expect("&")?;
        if self.consume("#x") {
            return self.skip_char_reference(16);
        }
        if self.consume("#") {
            return self.skip_char_reference(10);
        }

        let name = self.parse_name_slice()?;
        self.expect(";")?;
        match name {
            "amp" | "lt" | "gt" | "apos" | "quot" => Ok(()),
            _ => self.resolve_runtime_entity(name).map(|_| ()),
        }
    }

    pub(super) fn skip_char_reference(&mut self, radix: u32) -> XmlResult<()> {
        let start = self.index;
        while let Some(byte) = self.peek() {
            let valid = match radix {
                16 => byte.is_ascii_hexdigit(),
                _ => byte.is_ascii_digit(),
            };
            if !valid {
                break;
            }
            self.index += 1;
        }

        if self.index == start {
            return Err(self.error(XmlErrorKind::InvalidCharacterReference));
        }

        let digits = &self.input[start..self.index];
        self.expect(";")?;
        let value = u32::from_str_radix(digits, radix)
            .map_err(|_| self.error_at(XmlErrorKind::InvalidCharacterReference, start))?;
        let ch = char::from_u32(value)
            .filter(|ch| self.is_xml_char(*ch))
            .ok_or_else(|| self.error_at(XmlErrorKind::InvalidCharacterReference, start))?;
        let _ = ch;

        Ok(())
    }

    pub(super) fn parse_name(&mut self) -> XmlResult<String> {
        self.parse_name_slice().map(str::to_owned)
    }

    #[inline(always)]
    pub(super) fn parse_name_slice(&mut self) -> XmlResult<&'a str> {
        let start = self.index;
        let Some(first) = self.bytes.get(self.index).copied() else {
            return Err(self.error(XmlErrorKind::UnexpectedEof));
        };

        if first.is_ascii() {
            if !is_ascii_name_start(first) {
                return Err(self.error(XmlErrorKind::InvalidName));
            }

            self.index += 1;
            while let Some(byte) = self.bytes.get(self.index).copied() {
                if byte.is_ascii() {
                    if !is_ascii_name_char(byte) {
                        break;
                    }
                    self.index += 1;
                } else {
                    self.consume_non_ascii_name_chars();
                    break;
                }
            }

            return Ok(&self.input[start..self.index]);
        }

        let first = self.peek_char()?;
        if !is_name_start_char(first) {
            return Err(self.error(XmlErrorKind::InvalidName));
        }
        self.index += first.len_utf8();
        self.consume_non_ascii_name_chars();

        Ok(&self.input[start..self.index])
    }

    pub(super) fn consume_non_ascii_name_chars(&mut self) {
        while self.index < self.bytes.len() {
            let Some(ch) = self.input[self.index..].chars().next() else {
                break;
            };
            if !is_name_char(ch) {
                break;
            }
            self.index += ch.len_utf8();
        }
    }

    pub(super) fn reject_invalid_chars(&self) -> XmlResult<(bool, bool)> {
        if self.version == XmlVersion::Xml11 {
            return self.reject_invalid_xml11_chars();
        }

        let mut index = 0;
        let mut compact_lexemes_are_borrowable = true;
        let mut inspect_attribute_values = false;
        while index + 64 <= self.bytes.len() {
            let mut invalid = 0u64;
            let mut markers = 0u64;
            for offset in [0usize, 8, 16, 24, 32, 40, 48, 56] {
                let (word_invalid, word_markers) =
                    analyze_fast_ascii_xml_word(&self.bytes[index + offset..index + offset + 8]);
                invalid |= word_invalid;
                markers |= word_markers;
            }
            if invalid != 0 {
                break;
            }
            compact_lexemes_are_borrowable &= markers == 0;
            index += 64;
        }
        while index < self.bytes.len() {
            if index + 8 <= self.bytes.len() {
                if let Some(marker) = analyze_fast_ascii_xml_chunk(&self.bytes[index..index + 8]) {
                    compact_lexemes_are_borrowable &= !marker;
                    index += 8;
                    continue;
                }
            }

            let byte = self.bytes[index];
            if byte < 0x80 {
                if byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r') {
                    return Err(self.error_at(XmlErrorKind::InvalidCharacter, index));
                }
                if matches!(byte, b'&' | b'\r') {
                    compact_lexemes_are_borrowable = false;
                }
                inspect_attribute_values |= matches!(byte, b'\t' | b'\n');
                index += 1;
            } else {
                let ch = self.input[index..].chars().next().unwrap();
                if !is_xml_char(ch) {
                    return Err(self.error_at(XmlErrorKind::InvalidCharacter, index));
                }
                index += ch.len_utf8();
            }
        }
        Ok((compact_lexemes_are_borrowable, inspect_attribute_values))
    }

    pub(super) fn reject_invalid_xml11_chars(&self) -> XmlResult<(bool, bool)> {
        let mut index = 0;
        let mut compact_lexemes_are_borrowable = true;
        let mut inspect_attribute_values = false;
        while index + 32 <= self.bytes.len() {
            let first = &self.bytes[index..index + 8];
            let second = &self.bytes[index + 8..index + 16];
            let third = &self.bytes[index + 16..index + 24];
            let fourth = &self.bytes[index + 24..index + 32];
            if !is_fast_valid_ascii_xml11_chunk(first)
                || !is_fast_valid_ascii_xml11_chunk(second)
                || !is_fast_valid_ascii_xml11_chunk(third)
                || !is_fast_valid_ascii_xml11_chunk(fourth)
            {
                break;
            }
            compact_lexemes_are_borrowable &= !(fast_chunk_has_compact_decode_marker(first)
                || fast_chunk_has_compact_decode_marker(second)
                || fast_chunk_has_compact_decode_marker(third)
                || fast_chunk_has_compact_decode_marker(fourth));
            index += 32;
        }
        while index < self.bytes.len() {
            if index + 8 <= self.bytes.len()
                && is_fast_valid_ascii_xml11_chunk(&self.bytes[index..index + 8])
            {
                compact_lexemes_are_borrowable &=
                    !fast_chunk_has_compact_decode_marker(&self.bytes[index..index + 8]);
                index += 8;
                continue;
            }

            let byte = self.bytes[index];
            if byte < 0x80 {
                if !is_xml11_literal_char(byte as char) {
                    return Err(self.error_at(XmlErrorKind::InvalidCharacter, index));
                }
                if matches!(byte, b'&' | b'\r') {
                    compact_lexemes_are_borrowable = false;
                }
                inspect_attribute_values |= matches!(byte, b'\t' | b'\n');
                index += 1;
            } else {
                let ch = self.input[index..].chars().next().unwrap();
                if !is_xml11_literal_char(ch) {
                    return Err(self.error_at(XmlErrorKind::InvalidCharacter, index));
                }
                if matches!(ch, '\u{85}' | '\u{2028}') {
                    compact_lexemes_are_borrowable = false;
                }
                index += ch.len_utf8();
            }
        }
        Ok((compact_lexemes_are_borrowable, inspect_attribute_values))
    }

    #[inline(always)]
    pub(super) fn require_space(&mut self) -> XmlResult<()> {
        if !self.starts_xml_whitespace_at(self.index) {
            return Err(self.error(XmlErrorKind::Expected("whitespace")));
        }
        self.skip_whitespace();
        Ok(())
    }

    #[inline(always)]
    pub(super) fn skip_whitespace(&mut self) -> bool {
        let start = self.index;
        self.index = self.skip_xml_whitespace_at(self.index);
        self.index != start
    }

    #[inline(always)]
    pub(super) fn skip_xml_whitespace_at(&self, index: usize) -> usize {
        match self.version {
            XmlVersion::Xml10 => skip_xml_whitespace_bytes(self.bytes, index),
            XmlVersion::Xml11 => skip_xml11_whitespace_bytes(self.bytes, index),
        }
    }

    pub(super) fn starts_xml_whitespace_at(&self, index: usize) -> bool {
        match self.version {
            XmlVersion::Xml10 => matches!(self.bytes.get(index), Some(byte) if is_space(*byte)),
            XmlVersion::Xml11 => is_xml11_space_at(self.bytes, index),
        }
    }

    pub(super) fn normalize_attribute_value_known(
        &self,
        value: &str,
        needs_normalization: bool,
    ) -> String {
        match self.version {
            XmlVersion::Xml10 => normalize_xml10_attribute_value_known(value, needs_normalization),
            XmlVersion::Xml11 => normalize_xml11_attribute_value_known(value, needs_normalization),
        }
    }

    fn apply_attribute_whitespace(&self, mut value: String) -> String {
        if self.config.attribute_whitespace == XmlAttributeWhitespacePolicy::NormalizeAndCollapse {
            collapse_xml_whitespace(&mut value);
        }
        value
    }

    pub(super) fn is_xml_char(&self, ch: char) -> bool {
        match self.version {
            XmlVersion::Xml10 => is_xml_char(ch),
            XmlVersion::Xml11 => is_xml11_char(ch),
        }
    }

    pub(super) fn skip_bom(&mut self) {
        if self.starts_bytes(b"\xef\xbb\xbf") {
            self.index = 3;
        }
    }

    pub(super) fn expect(&mut self, token: &'static str) -> XmlResult<()> {
        if self.consume(token) {
            Ok(())
        } else {
            Err(self.error(XmlErrorKind::Expected(token)))
        }
    }

    pub(super) fn expect_byte(&mut self, byte: u8, expected: &'static str) -> XmlResult<()> {
        if self.consume_byte(byte) {
            Ok(())
        } else {
            Err(self.error(XmlErrorKind::Expected(expected)))
        }
    }

    pub(super) fn consume(&mut self, token: &str) -> bool {
        if self.starts_with(token) {
            self.index += token.len();
            true
        } else {
            false
        }
    }

    pub(super) fn consume_byte(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    pub(super) fn consume_empty_element_end(&mut self) -> bool {
        if self.starts_empty_element_end() {
            self.index += 2;
            true
        } else {
            false
        }
    }

    pub(super) fn starts_empty_element_end(&self) -> bool {
        self.bytes.get(self.index).copied() == Some(b'/')
            && self.bytes.get(self.index + 1).copied() == Some(b'>')
    }

    pub(super) fn starts_with(&self, token: &str) -> bool {
        self.starts_bytes(token.as_bytes())
    }

    pub(super) fn starts_bytes(&self, token: &[u8]) -> bool {
        self.bytes.get(self.index..self.index + token.len()) == Some(token)
    }

    pub(super) fn starts_xml_declaration(&self) -> bool {
        self.starts_bytes(b"<?xml")
            && matches!(
                self.bytes.get(self.index + 5).copied(),
                Some(b' ' | b'\t' | b'\r' | b'\n')
            )
    }

    pub(super) fn char_width_at_index(&self) -> usize {
        self.input[self.index..]
            .chars()
            .next()
            .map_or(1, char::len_utf8)
    }

    pub(super) fn peek_char(&self) -> XmlResult<char> {
        self.input[self.index..]
            .chars()
            .next()
            .ok_or_else(|| self.error(XmlErrorKind::UnexpectedEof))
    }

    pub(super) fn is_eof(&self) -> bool {
        self.index >= self.bytes.len()
    }

    pub(super) fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    pub(super) fn error(&self, kind: XmlErrorKind) -> XmlError {
        self.error_at(kind, self.index)
    }

    pub(super) fn error_at(&self, kind: XmlErrorKind, byte: usize) -> XmlError {
        XmlError::new(kind, byte)
    }
}
