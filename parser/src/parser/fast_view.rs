use super::*;

impl<'a> Parser<'a> {
    /// Attempts the common small-tree shape without instantiating the general attribute and
    /// mixed-markup machinery. The preceding strict character scan has already validated all
    /// literal characters and proved that references and newline normalization are unnecessary.
    /// Unsupported syntax returns `None` so the caller can restart with the general parser.
    pub(super) fn try_parse_simple_full_view_element_xml10(
        &mut self,
        builder: &mut XmlDocumentViewBuilder<'a>,
    ) -> XmlResult<Option<XmlViewNodeId>> {
        let mut open = [0u16; MAX_DOM_DEPTH];
        let mut depth = 0usize;
        let mut root = None;
        let mut element_count = 0usize;

        loop {
            if self.peek() != Some(b'<') {
                let value_start = self.index;
                while let Some(byte) = self.bytes.get(self.index).copied() {
                    if byte == b'<' {
                        break;
                    }
                    if byte == b'&' || byte == b'\r' {
                        return Ok(None);
                    }
                    if byte < 0x20 && !matches!(byte, b'\t' | b'\n') {
                        return Err(self.error_at(XmlErrorKind::InvalidCharacter, self.index));
                    }
                    if byte >= 0x80 {
                        let ch = self.input[self.index..].chars().next().unwrap();
                        if !is_xml_char(ch) {
                            return Err(self.error_at(XmlErrorKind::InvalidCharacter, self.index));
                        }
                        self.index += ch.len_utf8();
                        continue;
                    }
                    if byte == b']'
                        && self.bytes.get(self.index + 1..self.index + 3) == Some(b"]]>")
                    {
                        return Err(self.error_at(XmlErrorKind::UnexpectedToken, self.index));
                    }
                    self.index += 1;
                }
                if self.index == self.bytes.len() {
                    return Err(self.error(XmlErrorKind::UnexpectedEof));
                }
                builder.push_leaf_child(
                    XmlViewNodeId(open[depth - 1] as usize),
                    XmlNodeKind::Text,
                    fast_compact_range(value_start, self.index - value_start),
                );
                continue;
            }

            match self.bytes.get(self.index + 1).copied() {
                Some(b'/') => {
                    if depth == 0 {
                        return Err(self.error(XmlErrorKind::UnexpectedToken));
                    }
                    self.index += 2;
                    let closing_name_start = self.index;
                    let element = XmlViewNodeId(open[depth - 1] as usize);
                    let record = &builder.nodes[element.0];
                    let expected_start = record.name_start as usize;
                    let expected_end = expected_start + record.name_len as usize;
                    let expected_len = expected_end - expected_start;
                    let actual_end = self.index + expected_len;
                    if self.bytes.get(self.index..actual_end)
                        != self.bytes.get(expected_start..expected_end)
                    {
                        return self
                            .reject_simple_end_tag(&self.input[expected_start..expected_end]);
                    }
                    self.index = actual_end;
                    if self.starts_xml_whitespace_at(self.index) {
                        self.index = skip_xml_whitespace_bytes(self.bytes, self.index);
                    }
                    if self.peek() != Some(b'>') {
                        self.index = closing_name_start;
                        return self
                            .reject_simple_end_tag(&self.input[expected_start..expected_end]);
                    }
                    self.index += 1;
                    builder.close_node(element);
                    depth -= 1;
                    if depth == 0 {
                        finish_fast_view_stats(builder, element_count, 0);
                        return Ok(Some(root.expect("root node was initialized")));
                    }
                }
                Some(b'!' | b'?') | None => return Ok(None),
                Some(_) => {
                    if depth == MAX_DOM_DEPTH {
                        return Err(self.error(XmlErrorKind::DepthLimitExceeded));
                    }
                    self.index += 1;
                    let name_start = self.index;
                    let Some(first) = self.bytes.get(self.index).copied() else {
                        return Err(self.error(XmlErrorKind::UnexpectedEof));
                    };
                    if !is_ascii_name_start(first) {
                        return Ok(None);
                    }
                    self.index += 1;
                    while self.index < self.bytes.len()
                        && is_ascii_name_char(self.bytes[self.index])
                    {
                        self.index += 1;
                    }
                    let name_len = self.index - name_start;
                    if self.starts_xml_whitespace_at(self.index) {
                        self.index = skip_xml_whitespace_bytes(self.bytes, self.index);
                    }
                    let empty = if self.peek() == Some(b'>') {
                        self.index += 1;
                        false
                    } else if self.bytes.get(self.index..self.index + 2) == Some(b"/>") {
                        self.index += 2;
                        true
                    } else {
                        return Ok(None);
                    };

                    let node = builder.push_node(
                        XmlNodeKind::Element,
                        fast_compact_range(name_start, name_len),
                        (0, 0),
                    );
                    element_count += 1;
                    if depth == 0 {
                        root = Some(node);
                    } else {
                        builder.link_existing_child(XmlViewNodeId(open[depth - 1] as usize), node);
                    }

                    if empty {
                        builder.close_node(node);
                        if depth == 0 {
                            finish_fast_view_stats(builder, element_count, 0);
                            return Ok(Some(root.expect("root node was initialized")));
                        }
                    } else {
                        open[depth] = node.0 as u16;
                        depth += 1;
                    }
                }
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn reject_simple_end_tag<T>(&mut self, expected: &str) -> XmlResult<T> {
        self.consume_end_tag_matching(expected)?;
        unreachable!("the simple end-tag fast path only delegates mismatches")
    }

    pub(super) fn is_full_view_xml10(&self) -> bool {
        // Declaration and doctype preservation are handled before element parsing and cannot
        // change the element-tree builder. Keep the dense iterative path available when either
        // document-level node is intentionally omitted.
        self.version == XmlVersion::Xml10
            && self.config.preserve_comments
            && self.config.preserve_processing_instructions
            && self.config.preserve_cdata_nodes
            && self.config.preserve_text_nodes
            && self.config.text_whitespace == XmlTextWhitespacePolicy::Preserve
            && self.config.attribute_whitespace == XmlAttributeWhitespacePolicy::Normalize
            && self.config.entity_expansion
                == XmlEntityExpansionPolicy::ExpandInternal {
                    max_depth: 16,
                    max_expanded_bytes: 1024 * 1024,
                }
            && self.config.external_entities == XmlExternalEntityPolicy::Reject
            && self.general_entities.is_empty()
    }

    /// Iterative full-policy view construction. It retains every in-element node and uses the
    /// ordinary strict name, reference, duplicate-attribute, and delimiter validators. Keeping
    /// the open-element state in one fixed stack removes recursive parser frames without changing
    /// the public depth limit or falling back to trusted-input assumptions.
    pub(super) fn parse_full_view_element_xml10(
        &mut self,
        builder: &mut XmlDocumentViewBuilder<'a>,
    ) -> XmlResult<XmlViewNodeId> {
        let mut open = [0u32; MAX_DOM_DEPTH];
        #[cfg(debug_assertions)]
        let mut source_nodes = [None; MAX_DOM_DEPTH];
        let mut depth = 0usize;
        let mut root = None;
        let mut element_count = 0usize;
        let mut attribute_count_total = 0usize;

        loop {
            if depth == MAX_DOM_DEPTH {
                return Err(self.error(XmlErrorKind::DepthLimitExceeded));
            }
            #[cfg(debug_assertions)]
            let source_start = self.index;
            self.expect_byte(b'<', "<")?;
            let name_start = self.index;
            let name_len = self.parse_name_slice()?.len();
            #[cfg(debug_assertions)]
            let source_node = self.start_source_node(XmlNodeKind::Element, source_start);
            let attribute_start = builder.attributes.len();
            let attribute_count = if self.peek() == Some(b'>') || self.starts_empty_element_end() {
                0
            } else {
                self.parse_view_attributes(builder)?.1
            };

            let node = builder.push_node(
                XmlNodeKind::Element,
                fast_compact_range(name_start, name_len),
                fast_compact_range(attribute_start, attribute_count),
            );
            element_count += 1;
            attribute_count_total += attribute_count;

            if depth == 0 {
                root = Some(node);
            } else {
                builder.link_existing_child(XmlViewNodeId(open[depth - 1] as usize), node);
            }

            if self.consume_empty_element_end() {
                builder.close_node(node);
                #[cfg(debug_assertions)]
                self.finish_source_node(source_node, self.index);
                if depth == 0 {
                    finish_fast_view_stats(builder, element_count, attribute_count_total);
                    return Ok(root.expect("root node was initialized"));
                }
            } else {
                self.expect_byte(b'>', ">")?;
                open[depth] = fast_compact_usize(node.0);
                #[cfg(debug_assertions)]
                {
                    source_nodes[depth] = source_node;
                }
                depth += 1;
            }

            loop {
                if self.is_eof() {
                    return Err(self.error(XmlErrorKind::UnexpectedEof));
                }
                if self.peek() == Some(b'<') {
                    match self.bytes.get(self.index + 1).copied() {
                        Some(b'/') => {
                            if depth == 0 {
                                return Err(self.error(XmlErrorKind::UnexpectedToken));
                            }
                            self.index += 2;
                            let element = XmlViewNodeId(open[depth - 1] as usize);
                            let record = &builder.nodes[element.0];
                            let name_start = record.name_start as usize;
                            let name =
                                &self.input[name_start..name_start + record.name_len as usize];
                            self.consume_end_tag_matching(name)?;
                            builder.close_node(element);
                            #[cfg(debug_assertions)]
                            self.finish_source_node(source_nodes[depth - 1], self.index);
                            depth -= 1;
                            if depth == 0 {
                                finish_fast_view_stats(
                                    builder,
                                    element_count,
                                    attribute_count_total,
                                );
                                return Ok(root.expect("root node was initialized"));
                            }
                        }
                        Some(b'!') if self.starts_with("<!--") => {
                            let start = self.index;
                            self.skip_comment_opened()?;
                            #[cfg(debug_assertions)]
                            self.push_source_leaf(XmlNodeKind::Comment, start, self.index);
                            builder.push_leaf_child(
                                XmlViewNodeId(open[depth - 1] as usize),
                                XmlNodeKind::Comment,
                                fast_compact_range(start + 4, self.index - start - 7),
                            );
                        }
                        Some(b'!') if self.starts_with("<![CDATA[") => {
                            #[cfg(debug_assertions)]
                            let start = self.index;
                            let value = self.skip_cdata_range_opened()?.unwrap_or((0, 0));
                            #[cfg(debug_assertions)]
                            self.push_source_leaf(XmlNodeKind::Cdata, start, self.index);
                            builder.push_leaf_child(
                                XmlViewNodeId(open[depth - 1] as usize),
                                XmlNodeKind::Cdata,
                                fast_compact_range(value.0, value.1),
                            );
                        }
                        Some(b'!') => return Err(self.error(XmlErrorKind::UnexpectedToken)),
                        Some(b'?') => {
                            #[cfg(debug_assertions)]
                            let start = self.index;
                            let target = self.skip_processing_instruction_target_opened()?;
                            #[cfg(debug_assertions)]
                            self.push_source_leaf(
                                XmlNodeKind::ProcessingInstruction,
                                start,
                                self.index,
                            );
                            let data_start = self.skip_xml_whitespace_at(target.0 + target.1);
                            builder.push_leaf_child_with_secondary(
                                XmlViewNodeId(open[depth - 1] as usize),
                                XmlNodeKind::ProcessingInstruction,
                                fast_compact_range(target.0, target.1),
                                fast_compact_range(data_start, self.index - 2 - data_start),
                            );
                        }
                        Some(_) => break,
                        None => return Err(self.error(XmlErrorKind::UnexpectedEof)),
                    }
                } else {
                    let value = self.skip_text()?;
                    #[cfg(debug_assertions)]
                    self.push_source_leaf(XmlNodeKind::Text, value.0, value.0 + value.1);
                    builder.push_leaf_child(
                        XmlViewNodeId(open[depth - 1] as usize),
                        XmlNodeKind::Text,
                        fast_compact_range(value.0, value.1),
                    );
                }
            }
        }
    }

    pub(super) fn parse_compact_trusted_view_element_xml10(
        &mut self,
        builder: &mut XmlDocumentViewBuilder<'a>,
    ) -> XmlResult<XmlViewNodeId> {
        let mut open = [0u32; MAX_DOM_DEPTH];
        let mut depth = 0usize;
        let mut root = None;
        let mut element_count = 0usize;
        let mut attribute_count_total = 0usize;

        loop {
            if depth == MAX_DOM_DEPTH {
                return Err(self.error(XmlErrorKind::DepthLimitExceeded));
            }
            self.expect_byte(b'<', "<")?;
            let name_start = self.index;
            let name_len = self.parse_name_slice()?.len();
            let attribute_start = builder.attributes.len();
            let attribute_count = if self.peek() == Some(b'>') || self.starts_empty_element_end() {
                0
            } else {
                self.parse_compact_trusted_view_attributes_xml10(builder)?
            };

            let node = builder.push_node(
                XmlNodeKind::Element,
                fast_compact_range(name_start, name_len),
                fast_compact_range(attribute_start, attribute_count),
            );
            element_count += 1;
            attribute_count_total += attribute_count;

            if depth == 0 {
                root = Some(node);
            } else {
                builder.link_existing_child(XmlViewNodeId(open[depth - 1] as usize), node);
            }

            if self.consume_empty_element_end() {
                builder.close_node(node);
                if depth == 0 {
                    finish_fast_view_stats(builder, element_count, attribute_count_total);
                    return Ok(root.expect("root node was initialized"));
                }
            } else {
                self.expect_byte(b'>', ">")?;
                open[depth] = fast_compact_usize(node.0);
                depth += 1;
            }

            loop {
                if self.is_eof() {
                    return Err(self.error(XmlErrorKind::UnexpectedEof));
                }

                if self.peek() == Some(b'<') {
                    match self.bytes.get(self.index + 1).copied() {
                        Some(b'/') => {
                            if depth == 0 {
                                return Err(self.error(XmlErrorKind::UnexpectedToken));
                            }
                            self.index += 2;
                            let element = XmlViewNodeId(open[depth - 1] as usize);
                            let record = &builder.nodes[element.0];
                            let name_start = record.name_start as usize;
                            let element_name =
                                &self.input[name_start..name_start + record.name_len as usize];
                            self.consume_compact_trusted_end_tag_xml10(element_name)?;
                            builder.close_node(element);
                            depth -= 1;
                            if depth == 0 {
                                finish_fast_view_stats(
                                    builder,
                                    element_count,
                                    attribute_count_total,
                                );
                                return Ok(root.expect("root node was initialized"));
                            }
                        }
                        Some(b'!') if self.starts_with("<!--") => self.skip_comment()?,
                        Some(b'!') if self.starts_with("<![CDATA[") => {
                            if let Some(value) = self.skip_cdata_range()? {
                                builder.push_leaf_child(
                                    XmlViewNodeId(open[depth - 1] as usize),
                                    XmlNodeKind::Text,
                                    fast_compact_range(value.0, value.1),
                                );
                            }
                        }
                        Some(b'!') => return Err(self.error(XmlErrorKind::UnexpectedToken)),
                        Some(b'?') => self.skip_processing_instruction()?,
                        Some(_) => break,
                        None => return Err(self.error(XmlErrorKind::UnexpectedEof)),
                    }
                } else {
                    let start = self.index;
                    self.index = skip_xml_whitespace_bytes(self.bytes, self.index);
                    if self.index > start && self.peek() == Some(b'<') {
                        continue;
                    }
                    self.index = start;
                    let value = self.skip_compact_trusted_text_xml10()?;
                    builder.push_leaf_child(
                        XmlViewNodeId(open[depth - 1] as usize),
                        XmlNodeKind::Text,
                        fast_compact_range(value.0, value.1),
                    );
                }
            }
        }
    }

    fn parse_compact_trusted_view_attributes_xml10(
        &mut self,
        builder: &mut XmlDocumentViewBuilder<'a>,
    ) -> XmlResult<usize> {
        let start = builder.attributes.len();
        let mut needs_space = false;

        loop {
            let before_space = self.index;
            self.index = skip_xml_whitespace_bytes(self.bytes, self.index);
            let had_space = self.index != before_space;

            if self.peek() == Some(b'>') || self.starts_empty_element_end() {
                return Ok(builder.attributes.len() - start);
            }
            if needs_space && !had_space {
                return Err(self.error(XmlErrorKind::Expected("whitespace")));
            }

            let name_start = self.index;
            let name = self.parse_name_slice()?;
            let name_len = name.len();
            self.index = skip_xml_whitespace_bytes(self.bytes, self.index);
            self.expect_byte(b'=', "=")?;
            self.index = skip_xml_whitespace_bytes(self.bytes, self.index);
            let value = self.skip_compact_trusted_attribute_value_xml10()?;
            builder.note_attribute_value(value);
            let owner = builder.attribute_source_owner();
            builder.attributes.push(RawXmlAttribute::new(
                owner,
                fast_compact_usize(name_start),
                fast_compact_usize(name_len),
                fast_compact_usize(value.0),
                fast_compact_usize(value.1),
            ));
            needs_space = true;
        }
    }
}

#[inline(always)]
fn fast_compact_range(start: usize, len: usize) -> (u32, u32) {
    (fast_compact_usize(start), fast_compact_usize(len))
}

#[inline(always)]
fn finish_fast_view_stats(
    builder: &mut XmlDocumentViewBuilder<'_>,
    elements: usize,
    attributes: usize,
) {
    builder.stats.elements += elements;
    builder.stats.attributes += attributes;
    builder.stats.nodes += elements;
}

#[inline(always)]
fn fast_compact_usize(value: usize) -> u32 {
    debug_assert!(u32::try_from(value).is_ok());
    value as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_node_preservation_does_not_disable_full_view_fast_path() {
        for preserve_declaration in [false, true] {
            for preserve_doctype in [false, true] {
                let config = ParserConfig::preserve_all()
                    .preserve_declaration(preserve_declaration)
                    .preserve_doctype(preserve_doctype);
                assert!(Parser::new("<root/>", config).is_full_view_xml10());
            }
        }
    }

    #[test]
    fn element_policy_changes_still_disable_full_view_fast_path() {
        let config = ParserConfig::preserve_all().preserve_comments(false);
        assert!(!Parser::new("<root/>", config).is_full_view_xml10());
    }

    #[test]
    fn iterative_view_paths_reject_the_excessive_child_at_its_start() {
        let compact_trusted = ParserConfig::default()
            .preserve_comments(false)
            .preserve_processing_instructions(false)
            .preserve_cdata_nodes(false)
            .text_whitespace(XmlTextWhitespacePolicy::DiscardWhitespaceOnly)
            .validate_characters(false)
            .validate_references(false)
            .validate_duplicate_attributes(false);

        for config in [ParserConfig::preserve_all(), compact_trusted] {
            for leaf in ["<e/>", "<e></e>"] {
                let input = format!("{}{}{}", "<n>".repeat(128), leaf, "</n>".repeat(128));
                let error = parse_document_view_with_config(&input, config).unwrap_err();
                assert_eq!(error.kind, XmlErrorKind::DepthLimitExceeded);
                assert_eq!(error.byte, 384);
            }
        }
    }

    #[test]
    fn simple_full_compact_path_preserves_semantics_and_strict_fallbacks() {
        let simple = parse_compact_document("<root><a>plain λ</a><b/></root>".to_owned()).unwrap();
        assert_eq!(simple.tree_stats().elements, 3);
        assert_eq!(simple.tree_stats().nodes, 4);
        assert_eq!(
            simple.to_xml_string().unwrap(),
            "<root><a>plain λ</a><b/></root>"
        );

        for (input, expected) in [
            (
                "<root a='value'><a/></root>",
                "<root a=\"value\"><a/></root>",
            ),
            ("<root><a>A&amp;B</a></root>", "<root><a>A&amp;B</a></root>"),
            (
                "<root><!--note--><a/></root>",
                "<root><!--note--><a/></root>",
            ),
            (
                "<root><![CDATA[value]]></root>",
                "<root><![CDATA[value]]></root>",
            ),
        ] {
            let document = parse_compact_document(input.to_owned()).unwrap();
            assert_eq!(document.to_xml_string().unwrap(), expected);
        }

        let invalid = parse_compact_document("<root>\u{1}</root>".to_owned()).unwrap_err();
        assert_eq!(invalid.kind, XmlErrorKind::InvalidCharacter);
        assert_eq!(invalid.byte, 6);

        let mismatch = parse_compact_document("<root><a></b></root>".to_owned()).unwrap_err();
        assert!(matches!(
            mismatch.kind,
            XmlErrorKind::MismatchedEndTag { .. }
        ));
    }
}
