use std::collections::{HashMap, HashSet};

use crate::{
    error::{XmlError, XmlErrorKind, XmlResult},
    parser::skip_xml_whitespace_bytes,
    syntax::{
        is_name_char, is_name_start_char, is_pubid_char, is_space, is_xml11_char,
        is_xml11_literal_char, is_xml_char, is_xml_target,
    },
};

const MAX_CONTENT_MODEL_DEPTH: usize = 128;

pub(crate) fn validate_internal_subset(input: &str, base: usize, xml11: bool) -> XmlResult<()> {
    parse_internal_subset_entities(input, base, xml11).map(|_| ())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum XmlGeneralEntity {
    Internal(String),
    External,
}

pub(crate) fn parse_internal_subset_entities(
    input: &str,
    base: usize,
    xml11: bool,
) -> XmlResult<HashMap<String, XmlGeneralEntity>> {
    DtdSubsetParser::new(input, base, xml11).parse()
}

fn validate_content_model(input: &str, base: usize) -> XmlResult<usize> {
    ContentModelParser::new(input, base).parse()
}

struct ContentModelParser<'a> {
    input: &'a str,
    bytes: &'a [u8],
    index: usize,
    base: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContentGroupKind {
    Children,
    Mixed,
}

impl<'a> ContentModelParser<'a> {
    fn new(input: &'a str, base: usize) -> Self {
        Self {
            input,
            bytes: input.as_bytes(),
            index: 0,
            base,
        }
    }

    fn parse(mut self) -> XmlResult<usize> {
        let group_kind = self.parse_group(1)?;
        let quantified_children = group_kind == ContentGroupKind::Children
            && matches!(self.peek(), Some(b'?' | b'*' | b'+'));
        let repeated_mixed = group_kind == ContentGroupKind::Mixed && self.peek() == Some(b'*');
        if quantified_children || repeated_mixed {
            self.index += 1;
        }
        Ok(self.index)
    }

    fn parse_group(&mut self, depth: usize) -> XmlResult<ContentGroupKind> {
        if depth > MAX_CONTENT_MODEL_DEPTH {
            return Err(self.error(XmlErrorKind::DepthLimitExceeded));
        }
        self.expect_byte(b'(')?;
        self.skip_whitespace();

        if self.consume_literal("#PCDATA") {
            self.parse_mixed_tail()?;
            return Ok(ContentGroupKind::Mixed);
        }

        let mut choice_names = HashSet::new();
        let first_name = self.parse_cp(depth)?;
        self.skip_whitespace();

        let mut separator = None;
        loop {
            match self.peek() {
                Some(b')') => {
                    self.index += 1;
                    return Ok(ContentGroupKind::Children);
                }
                Some(b',' | b'|') => {
                    let current = self.peek().unwrap();
                    if separator.is_some_and(|separator| separator != current) {
                        return Err(self.error(XmlErrorKind::InvalidDocumentStructure));
                    }
                    separator = Some(current);
                    if current == b'|' {
                        if let Some(name) = first_name.as_deref() {
                            choice_names.insert(name.to_owned());
                        }
                    }
                    self.index += 1;
                    self.skip_whitespace();
                    let next_name = self.parse_cp(depth)?;
                    if current == b'|' {
                        if let Some(name) = next_name {
                            if !choice_names.insert(name) {
                                return Err(self.error(XmlErrorKind::InvalidDocumentStructure));
                            }
                        }
                    }
                    self.skip_whitespace();
                }
                Some(_) => return Err(self.error(XmlErrorKind::InvalidDocumentStructure)),
                None => return Err(self.error(XmlErrorKind::UnexpectedEof)),
            }
        }
    }

    fn parse_mixed_tail(&mut self) -> XmlResult<()> {
        self.skip_whitespace();
        if self.consume_byte(b')') {
            return Ok(());
        }

        loop {
            self.expect_byte(b'|')?;
            self.skip_whitespace();
            self.parse_name()?;
            self.skip_whitespace();

            if self.consume_byte(b')') {
                self.expect_byte(b'*')?;
                return Ok(());
            }
        }
    }

    fn parse_cp(&mut self, depth: usize) -> XmlResult<Option<String>> {
        self.skip_whitespace();

        let name = match self.peek() {
            Some(b'(') => {
                if self.parse_group(depth + 1)? == ContentGroupKind::Mixed {
                    return Err(self.error(XmlErrorKind::InvalidDocumentStructure));
                }
                None
            }
            Some(_) => Some(self.parse_name()?.to_owned()),
            None => return Err(self.error(XmlErrorKind::UnexpectedEof)),
        };

        if matches!(self.peek(), Some(b'?' | b'*' | b'+')) {
            self.index += 1;
        }
        Ok(name)
    }

    fn parse_name(&mut self) -> XmlResult<&'a str> {
        let start = self.index;
        let mut chars = self.input[self.index..].char_indices();
        let Some((_, first)) = chars.next() else {
            return Err(self.error(XmlErrorKind::UnexpectedEof));
        };

        if !is_name_start_char(first) {
            return Err(self.error(XmlErrorKind::InvalidName));
        }

        self.index += first.len_utf8();
        for (_, ch) in chars {
            if !is_name_char(ch) {
                break;
            }
            self.index += ch.len_utf8();
        }

        Ok(&self.input[start..self.index])
    }

    fn consume_literal(&mut self, literal: &'static str) -> bool {
        if self.starts_bytes(literal.as_bytes()) {
            self.index += literal.len();
            true
        } else {
            false
        }
    }

    fn starts_bytes(&self, token: &[u8]) -> bool {
        self.bytes.get(self.index..self.index + token.len()) == Some(token)
    }

    fn expect_byte(&mut self, expected: u8) -> XmlResult<()> {
        if self.consume_byte(expected) {
            Ok(())
        } else {
            Err(self.error(XmlErrorKind::Expected("content model token")))
        }
    }

    fn consume_byte(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        self.index = skip_xml_whitespace_bytes(self.bytes, self.index);
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    fn error(&self, kind: XmlErrorKind) -> XmlError {
        XmlError::new(kind, self.base + self.index)
    }
}

struct DtdSubsetParser<'a> {
    input: &'a str,
    bytes: &'a [u8],
    index: usize,
    base: usize,
    declared_entities: HashSet<String>,
    general_entities: HashMap<String, XmlGeneralEntity>,
    xml11: bool,
}

impl<'a> DtdSubsetParser<'a> {
    fn new(input: &'a str, base: usize, xml11: bool) -> Self {
        let declared_entities = ["amp", "lt", "gt", "apos", "quot"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        Self {
            input,
            bytes: input.as_bytes(),
            index: 0,
            base,
            declared_entities,
            general_entities: HashMap::new(),
            xml11,
        }
    }

    fn parse(mut self) -> XmlResult<HashMap<String, XmlGeneralEntity>> {
        while !self.is_eof() {
            self.skip_whitespace();
            if self.is_eof() {
                break;
            }

            if self.starts_with("<!--") {
                self.skip_comment()?;
            } else if self.starts_with("<?") {
                self.skip_pi()?;
            } else if self.starts_with("<!ELEMENT") {
                self.parse_element_decl()?;
            } else if self.starts_with("<!ATTLIST") {
                self.parse_attlist_decl()?;
            } else if self.starts_with("<!ENTITY") {
                self.parse_entity_decl()?;
            } else if self.starts_with("<!NOTATION") {
                self.parse_notation_decl()?;
            } else if self.starts_with("%") {
                self.skip_parameter_entity_reference()?;
            } else {
                return Err(self.error(XmlErrorKind::InvalidDocumentStructure));
            }
        }

        Ok(self.general_entities)
    }

    fn parse_element_decl(&mut self) -> XmlResult<()> {
        self.expect("<!ELEMENT")?;
        self.require_space()?;
        self.parse_name()?;
        self.require_space()?;

        if self.consume_keyword("EMPTY") || self.consume_keyword("ANY") {
            self.skip_whitespace();
            self.expect(">")?;
            return Ok(());
        }

        if self.peek() != Some(b'(') {
            return Err(self.error(XmlErrorKind::InvalidDocumentStructure));
        }
        let consumed = validate_content_model(&self.input[self.index..], self.base + self.index)?;
        self.index += consumed;
        self.skip_whitespace();
        self.expect(">")?;
        Ok(())
    }

    fn parse_attlist_decl(&mut self) -> XmlResult<()> {
        self.expect("<!ATTLIST")?;
        self.require_space()?;
        self.parse_name()?;

        loop {
            self.skip_whitespace();
            if self.consume(">") {
                return Ok(());
            }

            self.parse_name()?;
            self.require_space()?;
            self.parse_attribute_type()?;
            self.require_space()?;
            self.parse_default_decl()?;
        }
    }

    fn parse_attribute_type(&mut self) -> XmlResult<()> {
        if self.consume_keyword("CDATA")
            || self.consume_keyword("IDREFS")
            || self.consume_keyword("IDREF")
            || self.consume_keyword("ID")
            || self.consume_keyword("ENTITIES")
            || self.consume_keyword("ENTITY")
            || self.consume_keyword("NMTOKENS")
            || self.consume_keyword("NMTOKEN")
        {
            return Ok(());
        }

        if self.consume_keyword("NOTATION") {
            self.require_space()?;
            return self.parse_parenthesized_names();
        }

        if self.peek() == Some(b'(') {
            return self.parse_parenthesized_nmtokens();
        }

        Err(self.error(XmlErrorKind::InvalidDocumentStructure))
    }

    fn parse_default_decl(&mut self) -> XmlResult<()> {
        if self.consume_keyword("#REQUIRED") || self.consume_keyword("#IMPLIED") {
            return Ok(());
        }

        if self.consume_keyword("#FIXED") {
            self.require_space()?;
        }

        self.parse_att_value()
    }

    fn parse_parenthesized_names(&mut self) -> XmlResult<()> {
        self.expect("(")?;
        self.skip_whitespace();
        self.parse_name()?;
        self.skip_whitespace();

        while self.consume("|") {
            self.skip_whitespace();
            self.parse_name()?;
            self.skip_whitespace();
        }

        self.expect(")")
    }

    fn parse_parenthesized_nmtokens(&mut self) -> XmlResult<()> {
        self.expect("(")?;
        self.skip_whitespace();
        self.parse_nmtoken()?;
        self.skip_whitespace();

        while self.consume("|") {
            self.skip_whitespace();
            self.parse_nmtoken()?;
            self.skip_whitespace();
        }

        self.expect(")")
    }

    fn parse_entity_decl(&mut self) -> XmlResult<()> {
        self.expect("<!ENTITY")?;
        self.require_space()?;

        if self.consume("%") {
            self.require_space()?;
            self.parse_name()?;
            self.require_space()?;
            self.parse_parameter_entity_def()?;
        } else {
            let name = self.parse_name()?.to_owned();
            self.require_space()?;
            let declaration = self.parse_general_entity_def()?;
            self.declared_entities.insert(name.clone());
            self.general_entities.entry(name).or_insert(declaration);
        }

        self.skip_whitespace();
        self.expect(">")
    }

    fn parse_parameter_entity_def(&mut self) -> XmlResult<()> {
        if self.is_quote() {
            self.parse_entity_value().map(|_| ())
        } else {
            self.parse_external_id().map(|_| ())
        }
    }

    fn parse_general_entity_def(&mut self) -> XmlResult<XmlGeneralEntity> {
        if self.is_quote() {
            return self.parse_entity_value().map(XmlGeneralEntity::Internal);
        }

        self.parse_external_id()?;
        if self.consume_whitespace() && self.consume_keyword("NDATA") {
            self.require_space()?;
            self.parse_name()?;
        }
        Ok(XmlGeneralEntity::External)
    }

    fn parse_notation_decl(&mut self) -> XmlResult<()> {
        self.expect("<!NOTATION")?;
        self.require_space()?;
        self.parse_name()?;
        self.require_space()?;

        if self.starts_with("SYSTEM") {
            self.parse_external_id()?;
        } else if self.starts_with("PUBLIC") {
            self.expect("PUBLIC")?;
            self.require_space()?;
            self.parse_pubid_literal()?;
            self.skip_whitespace();
            if self.is_quote() {
                self.parse_system_literal()?;
            }
        } else {
            return Err(self.error(XmlErrorKind::InvalidDocumentStructure));
        }

        self.skip_whitespace();
        self.expect(">")
    }

    fn parse_external_id(&mut self) -> XmlResult<ExternalIdKind> {
        if self.consume_keyword("SYSTEM") {
            self.require_space()?;
            self.parse_system_literal()?;
            return Ok(ExternalIdKind::System);
        }

        if self.consume_keyword("PUBLIC") {
            self.require_space()?;
            self.parse_pubid_literal()?;
            self.require_space()?;
            self.parse_system_literal()?;
            return Ok(ExternalIdKind::Public);
        }

        Err(self.error(XmlErrorKind::InvalidDocumentStructure))
    }

    fn parse_system_literal(&mut self) -> XmlResult<()> {
        let quote = self.parse_quote()?;

        while let Some(byte) = self.peek() {
            if byte == quote {
                self.index += 1;
                return Ok(());
            }

            if byte == b'#' {
                return Err(self.error(XmlErrorKind::InvalidDocumentStructure));
            }

            let ch = self.peek_char()?;
            if !self.is_literal_char(ch) {
                return Err(self.error(XmlErrorKind::InvalidCharacter));
            }
            self.index += ch.len_utf8();
        }

        Err(self.error(XmlErrorKind::UnexpectedEof))
    }

    fn parse_pubid_literal(&mut self) -> XmlResult<()> {
        let quote = self.parse_quote()?;

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

    fn parse_entity_value(&mut self) -> XmlResult<String> {
        let quote = self.parse_quote()?;
        let start = self.index;

        while let Some(byte) = self.peek() {
            if byte == quote {
                let value = normalize_entity_value(
                    &self.input[start..self.index],
                    self.base + start,
                    self.xml11,
                )?;
                self.index += 1;
                return Ok(value);
            }

            match byte {
                b'&' => self.parse_general_reference_in_literal()?,
                b'%' => self.skip_parameter_entity_reference()?,
                _ => {
                    let ch = self.peek_char()?;
                    if !self.is_literal_char(ch) {
                        return Err(self.error(XmlErrorKind::InvalidCharacter));
                    }
                    self.index += ch.len_utf8();
                }
            }
        }

        Err(self.error(XmlErrorKind::UnexpectedEof))
    }

    fn parse_att_value(&mut self) -> XmlResult<()> {
        let quote = self.parse_quote()?;

        while let Some(byte) = self.peek() {
            if byte == quote {
                self.index += 1;
                return Ok(());
            }

            match byte {
                b'<' => return Err(self.error(XmlErrorKind::InvalidAttributeValue)),
                b'&' => self.parse_declared_reference_in_att_value()?,
                _ => {
                    let ch = self.peek_char()?;
                    if !self.is_literal_char(ch) {
                        return Err(self.error(XmlErrorKind::InvalidCharacter));
                    }
                    self.index += ch.len_utf8();
                }
            }
        }

        Err(self.error(XmlErrorKind::UnexpectedEof))
    }

    fn parse_declared_reference_in_att_value(&mut self) -> XmlResult<()> {
        self.expect("&")?;
        if self.consume("#") {
            return self.parse_character_reference_tail();
        }

        let name = self.parse_name()?;
        self.expect(";")?;
        if self.declared_entities.contains(name) {
            Ok(())
        } else {
            Err(self.error(XmlErrorKind::UndeclaredEntity(name.to_owned())))
        }
    }

    fn parse_general_reference_in_literal(&mut self) -> XmlResult<()> {
        self.expect("&")?;
        if self.consume("#") {
            return self.parse_character_reference_tail();
        }

        self.parse_name()?;
        self.expect(";")
    }

    fn parse_character_reference_tail(&mut self) -> XmlResult<()> {
        let radix = if self.consume("x") { 16 } else { 10 };
        let digit_start = self.index;

        while let Some(byte) = self.peek() {
            let is_digit = if radix == 16 {
                byte.is_ascii_hexdigit()
            } else {
                byte.is_ascii_digit()
            };
            if !is_digit {
                break;
            }
            self.index += 1;
        }

        if self.index == digit_start {
            return Err(self.error(XmlErrorKind::InvalidCharacterReference));
        }

        let digits = &self.input[digit_start..self.index];
        self.expect(";")?;

        let value = u32::from_str_radix(digits, radix)
            .map_err(|_| self.error(XmlErrorKind::InvalidCharacterReference))?;
        let Some(ch) = char::from_u32(value) else {
            return Err(self.error(XmlErrorKind::InvalidCharacterReference));
        };
        if !self.is_reference_char(ch) {
            return Err(self.error(XmlErrorKind::InvalidCharacterReference));
        }

        Ok(())
    }

    fn parse_quote(&mut self) -> XmlResult<u8> {
        let quote = match self.peek() {
            Some(b'"') => b'"',
            Some(b'\'') => b'\'',
            _ => return Err(self.error(XmlErrorKind::Expected("quoted literal"))),
        };
        self.index += 1;
        Ok(quote)
    }

    fn is_quote(&self) -> bool {
        matches!(self.peek(), Some(b'"' | b'\''))
    }

    fn skip_comment(&mut self) -> XmlResult<()> {
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
        Ok(())
    }

    fn skip_pi(&mut self) -> XmlResult<()> {
        self.expect("<?")?;
        let target = self.parse_name()?;
        if is_xml_target(target) {
            return Err(self.error(XmlErrorKind::InvalidProcessingInstructionTarget));
        }
        if !self.starts_with("?>") {
            self.require_space()?;
        }
        let end = self
            .input
            .get(self.index..)
            .and_then(|tail| tail.find("?>"))
            .map(|offset| self.index + offset)
            .ok_or_else(|| self.error(XmlErrorKind::UnexpectedEof))?;
        self.index = end + 2;
        Ok(())
    }

    fn skip_parameter_entity_reference(&mut self) -> XmlResult<()> {
        self.expect("%")?;
        self.parse_name()?;
        self.expect(";")
    }

    fn parse_name(&mut self) -> XmlResult<&'a str> {
        let start = self.index;
        let mut chars = self.input[self.index..].char_indices();
        let Some((_, first)) = chars.next() else {
            return Err(self.error(XmlErrorKind::UnexpectedEof));
        };

        if !is_name_start_char(first) {
            return Err(self.error(XmlErrorKind::InvalidName));
        }

        self.index += first.len_utf8();
        for (_, ch) in chars {
            if !is_name_char(ch) {
                break;
            }
            self.index += ch.len_utf8();
        }

        Ok(&self.input[start..self.index])
    }

    fn parse_nmtoken(&mut self) -> XmlResult<&'a str> {
        let start = self.index;
        let mut chars = self.input[self.index..].char_indices();
        let Some((_, first)) = chars.next() else {
            return Err(self.error(XmlErrorKind::UnexpectedEof));
        };

        if !is_name_char(first) {
            return Err(self.error(XmlErrorKind::InvalidName));
        }

        self.index += first.len_utf8();
        for (_, ch) in chars {
            if !is_name_char(ch) {
                break;
            }
            self.index += ch.len_utf8();
        }

        Ok(&self.input[start..self.index])
    }

    fn require_space(&mut self) -> XmlResult<()> {
        if !matches!(self.peek(), Some(byte) if is_space(byte)) {
            return Err(self.error(XmlErrorKind::Expected("whitespace")));
        }
        self.skip_whitespace();
        Ok(())
    }

    fn skip_whitespace(&mut self) {
        self.index = skip_xml_whitespace_bytes(self.bytes, self.index);
    }

    fn consume_whitespace(&mut self) -> bool {
        let start = self.index;
        self.skip_whitespace();
        self.index != start
    }

    fn consume_keyword(&mut self, keyword: &'static str) -> bool {
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

    fn expect(&mut self, token: &'static str) -> XmlResult<()> {
        if self.consume(token) {
            Ok(())
        } else {
            Err(self.error(XmlErrorKind::Expected(token)))
        }
    }

    fn consume(&mut self, token: &str) -> bool {
        if self.starts_with(token) {
            self.index += token.len();
            true
        } else {
            false
        }
    }

    fn starts_with(&self, token: &str) -> bool {
        self.starts_bytes(token.as_bytes())
    }

    fn starts_bytes(&self, token: &[u8]) -> bool {
        self.bytes.get(self.index..self.index + token.len()) == Some(token)
    }

    fn peek_char(&self) -> XmlResult<char> {
        self.input[self.index..]
            .chars()
            .next()
            .ok_or_else(|| self.error(XmlErrorKind::UnexpectedEof))
    }

    fn is_eof(&self) -> bool {
        self.index >= self.bytes.len()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    fn error(&self, kind: XmlErrorKind) -> XmlError {
        self.error_at(kind, self.index)
    }

    fn error_at(&self, kind: XmlErrorKind, byte: usize) -> XmlError {
        XmlError::new(kind, self.base + byte)
    }

    fn is_literal_char(&self, ch: char) -> bool {
        if self.xml11 {
            is_xml11_literal_char(ch)
        } else {
            is_xml_char(ch)
        }
    }

    fn is_reference_char(&self, ch: char) -> bool {
        if self.xml11 {
            is_xml11_char(ch)
        } else {
            is_xml_char(ch)
        }
    }
}

fn normalize_entity_value(input: &str, base: usize, xml11: bool) -> XmlResult<String> {
    let mut output = String::with_capacity(input.len());
    let mut index = 0usize;
    while index < input.len() {
        if !input[index..].starts_with("&#") {
            let ch = input[index..].chars().next().unwrap();
            output.push(ch);
            index += ch.len_utf8();
            continue;
        }
        let Some(relative_end) = input[index + 2..].find(';') else {
            return Err(XmlError::new(
                XmlErrorKind::InvalidCharacterReference,
                base + index,
            ));
        };
        let end = index + 2 + relative_end;
        let reference = &input[index + 2..end];
        let (digits, radix) = reference
            .strip_prefix('x')
            .map_or((reference, 10), |digits| (digits, 16));
        let ch = u32::from_str_radix(digits, radix)
            .ok()
            .and_then(char::from_u32)
            .filter(|ch| {
                if xml11 {
                    is_xml11_char(*ch)
                } else {
                    is_xml_char(*ch)
                }
            })
            .ok_or_else(|| XmlError::new(XmlErrorKind::InvalidCharacterReference, base + index))?;
        if xml11
            && matches!(
                ch as u32,
                0x01..=0x08 | 0x0b..=0x0c | 0x0e..=0x1f
            )
        {
            output.push_str(&input[index..=end]);
        } else {
            output.push(ch);
        }
        index = end + 1;
    }
    Ok(output)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExternalIdKind {
    System,
    Public,
}
