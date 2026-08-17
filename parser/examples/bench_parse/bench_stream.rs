use super::*;

pub(super) fn count_xml_stream_reader(
    input: &mut impl Read,
    config: ParserConfig,
) -> Result<Counts, String> {
    StreamingXmlCounter::new(input, config).parse()
}

const STREAM_CHUNK_SIZE: usize = 64 * 1024;

struct StreamingXmlCounter<R> {
    input: R,
    buffer: Vec<u8>,
    position: usize,
    eof: bool,
    config: ParserConfig,
}

impl<R: Read> StreamingXmlCounter<R> {
    fn new(input: R, config: ParserConfig) -> Self {
        Self {
            input,
            buffer: Vec::with_capacity(STREAM_CHUNK_SIZE * 2),
            position: 0,
            eof: false,
            config,
        }
    }

    fn parse(mut self) -> Result<Counts, String> {
        self.skip_bom()?;
        if self.starts_xml_declaration()? {
            self.skip_processing_instruction(TargetMode::XmlDeclaration)?;
        }

        self.skip_misc()?;
        if self.consume(b"<!DOCTYPE")? {
            self.skip_doctype_after_keyword()?;
            self.skip_misc()?;
        }

        let mut counts = Counts::default();
        self.parse_element(&mut counts)?;
        self.skip_misc()?;

        if self.peek()?.is_some() {
            return Err("stream XML count: trailing content after root element".to_owned());
        }
        Ok(counts)
    }

    fn parse_element(&mut self, counts: &mut Counts) -> Result<(), String> {
        self.expect(b"<", "'<'")?;
        let name = self.parse_name()?;
        let attributes = self.parse_attributes()?;
        self.skip_whitespace()?;

        counts.elements += 1;
        counts.attributes += attributes;
        counts.nodes += 1;

        if self.consume(b"/>")? {
            return Ok(());
        }

        self.expect(b">", "'>'")?;
        self.parse_content(&name, counts)
    }

    fn parse_content(
        &mut self,
        element_name: &StreamName,
        counts: &mut Counts,
    ) -> Result<(), String> {
        loop {
            let Some(byte) = self.peek()? else {
                return Err("stream XML count: unexpected end of input".to_owned());
            };

            if byte == b'<' {
                if self.consume(b"</")? {
                    let end_name = self.parse_name()?;
                    self.skip_whitespace()?;
                    self.expect(b">", "'>'")?;
                    if end_name != *element_name {
                        return Err(format!(
                            "stream XML count: mismatched end tag, expected {:?}, found {:?}",
                            element_name.as_lossy(),
                            end_name.as_lossy()
                        ));
                    }
                    return Ok(());
                }

                if self.starts_with(b"<!--")? {
                    self.skip_comment()?;
                    if self.config.preserves_comments() {
                        counts.nodes += 1;
                    }
                } else if self.starts_with(b"<![CDATA[")? {
                    let has_content = self.skip_cdata_count()?;
                    if self.config.preserves_cdata_nodes() || has_content {
                        counts.nodes += 1;
                    }
                } else if self.starts_with(b"<!")? {
                    return Err("stream XML count: unexpected declaration in content".to_owned());
                } else if self.starts_with(b"<?")? {
                    self.skip_processing_instruction(TargetMode::ProcessingInstruction)?;
                    if self.config.preserves_processing_instructions() {
                        counts.nodes += 1;
                    }
                } else {
                    self.parse_element(counts)?;
                }
            } else if self.parse_text_node()? {
                counts.nodes += 1;
            }
        }
    }

    fn parse_attributes(&mut self) -> Result<usize, String> {
        let mut names = Vec::<StreamName>::new();
        let mut needs_space = false;
        let mut count = 0usize;

        loop {
            let had_space = self.skip_whitespace()?;
            if self.starts_with(b">")? || self.starts_with(b"/>")? {
                return Ok(count);
            }
            if needs_space && !had_space {
                return Err("stream XML count: expected whitespace before attribute".to_owned());
            }

            let name = self.parse_name()?;
            if names.iter().any(|stored| stored == &name) {
                return Err(format!(
                    "stream XML count: duplicate attribute {:?}",
                    name.as_lossy()
                ));
            }
            self.skip_whitespace()?;
            self.expect(b"=", "'='")?;
            self.skip_whitespace()?;

            if name.as_bytes() == b"xml:space" {
                let value = self.parse_attribute_value()?;
                if value != "default" && value != "preserve" {
                    return Err("stream XML count: invalid xml:space value".to_owned());
                }
            } else {
                self.skip_attribute_value()?;
            }

            names.push(name);
            count += 1;
            needs_space = true;
        }
    }

    fn parse_attribute_value(&mut self) -> Result<String, String> {
        let quote = self.parse_quote()?;
        let mut value = String::new();
        let mut pending_cr = false;

        loop {
            let Some(byte) = self.peek()? else {
                return Err("stream XML count: unexpected end in attribute value".to_owned());
            };
            if byte == quote {
                self.advance(1);
                return Ok(value);
            }
            match byte {
                b'<' => return Err("stream XML count: '<' in attribute value".to_owned()),
                b'&' => {
                    if pending_cr {
                        value.push('\n');
                        pending_cr = false;
                    }
                    let resolved = self.parse_reference_string()?;
                    value.push_str(&resolved);
                }
                b'\r' => {
                    self.advance(1);
                    if self.consume(b"\n")? {
                        value.push('\n');
                    } else {
                        pending_cr = true;
                    }
                }
                _ => {
                    if pending_cr {
                        value.push('\n');
                        pending_cr = false;
                    }
                    value.push(self.read_char()?);
                }
            }
        }
    }

    fn skip_attribute_value(&mut self) -> Result<(), String> {
        let quote = self.parse_quote()?;
        loop {
            let Some(byte) = self.peek()? else {
                return Err("stream XML count: unexpected end in attribute value".to_owned());
            };
            if byte == quote {
                self.advance(1);
                return Ok(());
            }
            match byte {
                b'<' => return Err("stream XML count: '<' in attribute value".to_owned()),
                b'&' => self.skip_reference()?,
                _ => {
                    self.read_char()?;
                }
            }
        }
    }

    fn parse_text_node(&mut self) -> Result<bool, String> {
        let mut saw_text = false;
        let mut saw_non_whitespace = false;
        let mut previous = [0u8; 2];

        loop {
            self.fill(1)?;
            if self.available() == 0 {
                return Err("stream XML count: unexpected end in text".to_owned());
            }

            let available = &self.buffer[self.position..];
            let Some((offset, delimiter)) = find_byte3(available, b'<', b'&', b'>') else {
                let segment_len = available.len();
                if segment_len != 0 {
                    if self.config.validates_characters() {
                        validate_xml_chars_in_segment(available)?;
                    }
                    saw_text = true;
                    saw_non_whitespace |= segment_has_non_xml_space(available);
                    previous = trailing_two(previous, available);
                    self.advance(segment_len);
                }
                continue;
            };

            if offset != 0 {
                let segment = &available[..offset];
                if self.config.validates_characters() {
                    validate_xml_chars_in_segment(segment)?;
                }
                saw_text = true;
                saw_non_whitespace |= segment_has_non_xml_space(segment);
                previous = trailing_two(previous, segment);
                self.advance(offset);
            }

            match delimiter {
                b'<' => {
                    return Ok(saw_text
                        && (self.config.text_whitespace_policy()
                            == XmlTextWhitespacePolicy::Preserve
                            || saw_non_whitespace));
                }
                b'&' => {
                    saw_text = true;
                    if !self.skip_reference_is_xml_space()? {
                        saw_non_whitespace = true;
                    }
                    previous = [0, 0];
                }
                b'>' => {
                    if previous == [b']', b']'] {
                        return Err("stream XML count: ']]>' in character data".to_owned());
                    }
                    saw_text = true;
                    saw_non_whitespace = true;
                    previous = [previous[1], b'>'];
                    self.advance(1);
                }
                _ => unreachable!("text delimiter must be '<', '&', or '>'"),
            }
        }
    }

    fn skip_comment(&mut self) -> Result<(), String> {
        self.expect(b"<!--", "'<!--'")?;
        loop {
            if self.consume(b"-->")? {
                return Ok(());
            }
            if self.starts_with(b"--")? {
                return Err("stream XML count: invalid '--' in comment".to_owned());
            }
            if self.peek()?.is_none() {
                return Err("stream XML count: unexpected end in comment".to_owned());
            }
            self.read_char()?;
        }
    }

    fn skip_cdata_count(&mut self) -> Result<bool, String> {
        self.expect(b"<![CDATA[", "'<![CDATA['")?;
        let mut has_content = false;
        loop {
            if self.consume(b"]]>")? {
                return Ok(has_content);
            }
            if self.peek()?.is_none() {
                return Err("stream XML count: unexpected end in CDATA".to_owned());
            }
            self.read_char()?;
            has_content = true;
        }
    }

    fn skip_processing_instruction(&mut self, mode: TargetMode) -> Result<(), String> {
        self.expect(b"<?", "'<?'")?;
        let target = self.parse_name()?;
        if mode == TargetMode::ProcessingInstruction && target.eq_ignore_ascii_case(b"xml") {
            return Err("stream XML count: invalid processing instruction target".to_owned());
        }

        if self.consume(b"?>")? {
            return Ok(());
        }

        self.require_space()?;
        loop {
            if self.consume(b"?>")? {
                return Ok(());
            }
            if self.peek()?.is_none() {
                return Err("stream XML count: unexpected end in processing instruction".to_owned());
            }
            self.read_char()?;
        }
    }

    fn skip_doctype_after_keyword(&mut self) -> Result<(), String> {
        self.require_space()?;
        self.parse_name()?;
        let mut quote = None;
        let mut bracket_depth = 0usize;

        loop {
            let Some(byte) = self.peek()? else {
                return Err("stream XML count: unexpected end in DOCTYPE".to_owned());
            };

            if let Some(active_quote) = quote {
                self.read_char()?;
                if byte == active_quote {
                    quote = None;
                }
                continue;
            }

            match byte {
                b'"' | b'\'' => {
                    quote = Some(byte);
                    self.advance(1);
                }
                b'[' => {
                    bracket_depth += 1;
                    self.advance(1);
                }
                b']' if bracket_depth > 0 => {
                    bracket_depth -= 1;
                    self.advance(1);
                }
                b'>' if bracket_depth == 0 => {
                    self.advance(1);
                    return Ok(());
                }
                _ if bracket_depth > 0 && self.starts_with(b"<!--")? => self.skip_comment()?,
                _ if bracket_depth > 0 && self.starts_with(b"<?")? => {
                    self.skip_processing_instruction(TargetMode::ProcessingInstruction)?
                }
                _ => {
                    self.read_char()?;
                }
            }
        }
    }

    fn skip_misc(&mut self) -> Result<(), String> {
        loop {
            self.skip_whitespace()?;
            if self.starts_with(b"<!--")? {
                self.skip_comment()?;
            } else if self.starts_with(b"<?")? {
                self.skip_processing_instruction(TargetMode::ProcessingInstruction)?;
            } else {
                return Ok(());
            }
        }
    }

    fn parse_reference_string(&mut self) -> Result<String, String> {
        self.expect(b"&", "'&'")?;
        if self.consume(b"#x")? {
            return self.parse_char_reference(16).map(|ch| ch.to_string());
        }
        if self.consume(b"#")? {
            return self.parse_char_reference(10).map(|ch| ch.to_string());
        }

        let name = self.parse_name()?;
        self.expect(b";", "';'")?;
        match name.as_bytes() {
            b"amp" => Ok("&".to_owned()),
            b"lt" => Ok("<".to_owned()),
            b"gt" => Ok(">".to_owned()),
            b"apos" => Ok("'".to_owned()),
            b"quot" => Ok("\"".to_owned()),
            _ => Err(format!(
                "stream XML count: undeclared entity {:?}",
                name.as_lossy()
            )),
        }
    }

    fn skip_reference(&mut self) -> Result<(), String> {
        self.skip_reference_is_xml_space().map(|_| ())
    }

    fn skip_reference_is_xml_space(&mut self) -> Result<bool, String> {
        self.expect(b"&", "'&'")?;
        if self.consume(b"#x")? {
            return self.parse_char_reference(16).map(is_xml_space_char);
        }
        if self.consume(b"#")? {
            return self.parse_char_reference(10).map(is_xml_space_char);
        }

        let name = self.parse_name()?;
        self.expect(b";", "';'")?;
        match name.as_bytes() {
            b"amp" | b"lt" | b"gt" | b"apos" | b"quot" => Ok(false),
            _ => Err(format!(
                "stream XML count: undeclared entity {:?}",
                name.as_lossy()
            )),
        }
    }

    fn parse_char_reference(&mut self, radix: u32) -> Result<char, String> {
        let mut digits = Vec::new();
        loop {
            let Some(byte) = self.peek()? else {
                return Err("stream XML count: unexpected end in character reference".to_owned());
            };
            let valid = if radix == 16 {
                byte.is_ascii_hexdigit()
            } else {
                byte.is_ascii_digit()
            };
            if !valid {
                break;
            }
            digits.push(byte);
            self.advance(1);
        }
        if digits.is_empty() {
            return Err("stream XML count: invalid character reference".to_owned());
        }
        self.expect(b";", "';'")?;
        let digits = str::from_utf8(&digits).map_err(|error| error.to_string())?;
        let value = u32::from_str_radix(digits, radix)
            .map_err(|_| "stream XML count: invalid character reference".to_owned())?;
        char::from_u32(value)
            .filter(|ch| is_xml_char(*ch))
            .ok_or_else(|| "stream XML count: invalid character reference".to_owned())
    }

    fn parse_name(&mut self) -> Result<StreamName, String> {
        let mut name = StreamName::new();
        let Some(first) = self.peek()? else {
            return Err("stream XML count: unexpected end in name".to_owned());
        };
        if first.is_ascii() {
            if !is_name_start_byte(first) {
                return Err("stream XML count: invalid XML name".to_owned());
            }
            name.push(first);
            self.advance(1);
        } else {
            let (ch, width) = self.peek_char()?;
            if !is_name_start_char(ch) {
                return Err("stream XML count: invalid XML name".to_owned());
            }
            name.push_slice(&self.buffer[self.position..self.position + width]);
            self.advance(width);
        }

        while let Some(byte) = self.peek()? {
            if byte.is_ascii() {
                if !is_name_byte(byte) {
                    break;
                }
                name.push(byte);
                self.advance(1);
            } else {
                let (ch, width) = self.peek_char()?;
                if !is_name_char(ch) {
                    break;
                }
                name.push_slice(&self.buffer[self.position..self.position + width]);
                self.advance(width);
            }
        }

        Ok(name)
    }

    fn skip_whitespace(&mut self) -> Result<bool, String> {
        let mut skipped = false;
        loop {
            self.fill(1)?;
            if self.available() == 0 {
                return Ok(skipped);
            }

            let available = &self.buffer[self.position..];
            let next = skip_xml_space_slice(available);
            let available_len = available.len();
            if next == 0 {
                return Ok(skipped);
            }
            self.advance(next);
            skipped = true;

            if next < available_len {
                return Ok(skipped);
            }
        }
    }

    fn require_space(&mut self) -> Result<(), String> {
        if !matches!(self.peek()?, Some(byte) if is_xml_space_byte(byte)) {
            return Err("stream XML count: expected whitespace".to_owned());
        }
        self.skip_whitespace()?;
        Ok(())
    }

    fn parse_quote(&mut self) -> Result<u8, String> {
        match self.peek()? {
            Some(quote @ (b'"' | b'\'')) => {
                self.advance(1);
                Ok(quote)
            }
            _ => Err("stream XML count: expected quote".to_owned()),
        }
    }

    fn read_char(&mut self) -> Result<char, String> {
        let (ch, width) = self.peek_char()?;
        if self.config.validates_characters() && !is_xml_char(ch) {
            return Err("stream XML count: invalid XML character".to_owned());
        }
        self.advance(width);
        Ok(ch)
    }

    fn peek_char(&mut self) -> Result<(char, usize), String> {
        let Some(first) = self.peek()? else {
            return Err("stream XML count: unexpected end of input".to_owned());
        };
        let width = utf8_char_width(first)?;
        self.fill(width)?;
        if self.available() < width {
            return Err("stream XML count: truncated UTF-8 sequence".to_owned());
        }
        let slice = &self.buffer[self.position..self.position + width];
        let text = str::from_utf8(slice).map_err(|error| error.to_string())?;
        let ch = text
            .chars()
            .next()
            .ok_or_else(|| "stream XML count: invalid UTF-8".to_owned())?;
        Ok((ch, width))
    }

    fn skip_bom(&mut self) -> Result<(), String> {
        if self.starts_with(&[0xef, 0xbb, 0xbf])? {
            self.advance(3);
        }
        Ok(())
    }

    fn starts_xml_declaration(&mut self) -> Result<bool, String> {
        if !self.starts_with(b"<?xml")? {
            return Ok(false);
        }
        self.fill(6)?;
        Ok(matches!(
            self.buffer.get(self.position + 5).copied(),
            Some(b' ' | b'\t' | b'\r' | b'\n')
        ))
    }

    fn expect(&mut self, token: &[u8], expected: &'static str) -> Result<(), String> {
        if self.consume(token)? {
            Ok(())
        } else {
            Err(format!("stream XML count: expected {expected}"))
        }
    }

    fn consume(&mut self, token: &[u8]) -> Result<bool, String> {
        if self.starts_with(token)? {
            self.advance(token.len());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn starts_with(&mut self, token: &[u8]) -> Result<bool, String> {
        self.fill(token.len())?;
        Ok(self.available() >= token.len()
            && self.buffer[self.position..self.position + token.len()] == *token)
    }

    fn peek(&mut self) -> Result<Option<u8>, String> {
        self.fill(1)?;
        Ok(self.buffer.get(self.position).copied())
    }

    fn fill(&mut self, needed: usize) -> Result<(), String> {
        while self.available() < needed && !self.eof {
            if self.position > STREAM_CHUNK_SIZE {
                self.buffer.drain(..self.position);
                self.position = 0;
            }
            let start = self.buffer.len();
            self.buffer.resize(start + STREAM_CHUNK_SIZE, 0);
            let read = self
                .input
                .read(&mut self.buffer[start..])
                .map_err(|error| error.to_string())?;
            self.buffer.truncate(start + read);
            if read == 0 {
                self.eof = true;
            }
        }
        Ok(())
    }

    fn available(&self) -> usize {
        self.buffer.len().saturating_sub(self.position)
    }

    fn advance(&mut self, amount: usize) {
        self.position += amount;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetMode {
    XmlDeclaration,
    ProcessingInstruction,
}

const INLINE_NAME_CAPACITY: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
struct StreamName {
    inline: [u8; INLINE_NAME_CAPACITY],
    len: usize,
    spill: Vec<u8>,
}

impl StreamName {
    fn new() -> Self {
        Self {
            inline: [0; INLINE_NAME_CAPACITY],
            len: 0,
            spill: Vec::new(),
        }
    }

    fn push(&mut self, byte: u8) {
        if self.spill.is_empty() && self.len < self.inline.len() {
            self.inline[self.len] = byte;
        } else {
            if self.spill.is_empty() {
                self.spill.extend_from_slice(&self.inline[..self.len]);
            }
            self.spill.push(byte);
        }
        self.len += 1;
    }

    fn push_slice(&mut self, bytes: &[u8]) {
        if self.spill.is_empty() && self.len + bytes.len() <= self.inline.len() {
            self.inline[self.len..self.len + bytes.len()].copy_from_slice(bytes);
            self.len += bytes.len();
        } else {
            if self.spill.is_empty() {
                self.spill.extend_from_slice(&self.inline[..self.len]);
            }
            self.spill.extend_from_slice(bytes);
            self.len += bytes.len();
        }
    }

    fn as_bytes(&self) -> &[u8] {
        if self.spill.is_empty() {
            &self.inline[..self.len]
        } else {
            &self.spill
        }
    }

    fn eq_ignore_ascii_case(&self, other: &[u8]) -> bool {
        self.as_bytes().eq_ignore_ascii_case(other)
    }

    fn as_lossy(&self) -> String {
        String::from_utf8_lossy(self.as_bytes()).into_owned()
    }
}

fn is_name_start_byte(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b':' | b'_')
}

fn is_name_byte(byte: u8) -> bool {
    is_name_start_byte(byte) || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
}

fn is_name_start_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3a
            | 0x41..=0x5a
            | 0x5f
            | 0x61..=0x7a
            | 0xc0..=0xd6
            | 0xd8..=0xf6
            | 0xf8..=0x2ff
            | 0x370..=0x37d
            | 0x37f..=0x1fff
            | 0x200c..=0x200d
            | 0x2070..=0x218f
            | 0x2c00..=0x2fef
            | 0x3001..=0xd7ff
            | 0xf900..=0xfdcf
            | 0xfdf0..=0xfffd
            | 0x10000..=0xeffff
    )
}

fn is_name_char(ch: char) -> bool {
    is_name_start_char(ch)
        || matches!(
            ch as u32,
            0x2d | 0x2e | 0x30..=0x39 | 0xb7 | 0x300..=0x36f | 0x203f..=0x2040
        )
}

fn is_xml_space_byte(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn is_xml_space_char(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\r' | '\n')
}

fn is_xml_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x09 | 0x0a | 0x0d | 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff
    )
}

fn utf8_char_width(first: u8) -> Result<usize, String> {
    match first {
        0x00..=0x7f => Ok(1),
        0xc2..=0xdf => Ok(2),
        0xe0..=0xef => Ok(3),
        0xf0..=0xf4 => Ok(4),
        _ => Err("stream XML count: invalid UTF-8".to_owned()),
    }
}

fn find_byte3(bytes: &[u8], first: u8, second: u8, third: u8) -> Option<(usize, u8)> {
    let mut index = 0usize;
    while index + 8 <= bytes.len() {
        let chunk = u64::from_ne_bytes(bytes[index..index + 8].try_into().unwrap());
        let mask = contains_zero_byte(chunk ^ repeated_byte(first))
            | contains_zero_byte(chunk ^ repeated_byte(second))
            | contains_zero_byte(chunk ^ repeated_byte(third));
        if mask != 0 {
            let found = index + ((mask.trailing_zeros() >> 3) as usize);
            return Some((found, bytes[found]));
        }
        index += 8;
    }

    while let Some(byte) = bytes.get(index).copied() {
        if byte == first || byte == second || byte == third {
            return Some((index, byte));
        }
        index += 1;
    }

    None
}

fn validate_xml_chars_in_segment(bytes: &[u8]) -> Result<(), String> {
    let text = str::from_utf8(bytes).map_err(|error| error.to_string())?;
    if text.chars().all(is_xml_char) {
        Ok(())
    } else {
        Err("stream XML count: invalid XML character".to_owned())
    }
}

fn segment_has_non_xml_space(bytes: &[u8]) -> bool {
    bytes.iter().any(|byte| !is_xml_space_byte(*byte))
}

fn skip_xml_space_slice(bytes: &[u8]) -> usize {
    let mut index = 0usize;
    while index + 8 <= bytes.len() {
        let chunk = u64::from_ne_bytes(bytes[index..index + 8].try_into().unwrap());
        let mask = contains_zero_byte(chunk ^ repeated_byte(b' '))
            | contains_zero_byte(chunk ^ repeated_byte(b'\t'))
            | contains_zero_byte(chunk ^ repeated_byte(b'\r'))
            | contains_zero_byte(chunk ^ repeated_byte(b'\n'));
        if mask != 0x8080_8080_8080_8080 {
            break;
        }
        index += 8;
    }

    while matches!(bytes.get(index), Some(byte) if is_xml_space_byte(*byte)) {
        index += 1;
    }
    index
}

fn trailing_two(mut previous: [u8; 2], bytes: &[u8]) -> [u8; 2] {
    match bytes.len() {
        0 => previous,
        1 => [previous[1], bytes[0]],
        _ => {
            previous[0] = bytes[bytes.len() - 2];
            previous[1] = bytes[bytes.len() - 1];
            previous
        }
    }
}

pub(super) fn count_generated_xml_stream_reader(input: &mut impl Read) -> Result<Counts, String> {
    GeneratedXmlStreamCounter::new(input).parse()
}

pub(super) fn count_generated_xml_bytes(bytes: &[u8]) -> Result<Counts, String> {
    let mut pattern_counts = GeneratedPatternCounts::default();
    count_generated_patterns(bytes, bytes.len(), &mut pattern_counts);
    let counts = pattern_counts.into_counts()?;
    if counts.elements == 0 {
        return Err("generated XML count: missing root element".to_owned());
    }
    Ok(counts)
}

struct GeneratedXmlStreamCounter<R> {
    input: R,
}

impl<R: Read> GeneratedXmlStreamCounter<R> {
    fn new(input: R) -> Self {
        Self { input }
    }

    fn parse(mut self) -> Result<Counts, String> {
        let chunk_size = 512 * 1024;
        let mut buffer = vec![0u8; chunk_size + MAX_GENERATED_PATTERN_LEN];
        let mut pattern_counts = GeneratedPatternCounts::default();
        let mut carry_len = 0usize;
        loop {
            let read = self
                .input
                .read(&mut buffer[carry_len..carry_len + chunk_size])
                .map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            let total = carry_len + read;
            let count_until = total.saturating_sub(MAX_GENERATED_PATTERN_LEN - 1);
            count_generated_patterns(&buffer[..total], count_until, &mut pattern_counts);
            carry_len = total - count_until;
            buffer.copy_within(count_until..total, 0);
        }
        count_generated_patterns(&buffer[..carry_len], carry_len, &mut pattern_counts);
        let counts = pattern_counts.into_counts()?;
        if counts.elements == 0 {
            return Err("streaming XML count: missing root element".to_owned());
        }
        Ok(counts)
    }
}

const MAX_GENERATED_PATTERN_LEN: usize = b"<string></string>".len();

#[derive(Default)]
struct GeneratedPatternCounts {
    total_lt: usize,
    nulls: usize,
    members: usize,
    text_nodes: usize,
}

impl GeneratedPatternCounts {
    fn into_counts(self) -> Result<Counts, String> {
        if self.total_lt == 0 {
            return Ok(Counts::default());
        }
        let adjusted = self
            .total_lt
            .checked_add(self.nulls)
            .and_then(|value| value.checked_sub(1))
            .ok_or_else(|| "streaming XML count: invalid generated tag totals".to_owned())?;
        if adjusted % 2 != 0 {
            return Err("streaming XML count: unbalanced generated tag totals".to_owned());
        }
        let elements = adjusted / 2;
        Ok(Counts {
            elements,
            attributes: self.members,
            nodes: elements + self.text_nodes,
            checksum: elements
                .wrapping_add(self.members)
                .wrapping_add(self.text_nodes),
        })
    }
}

fn count_generated_patterns(bytes: &[u8], limit: usize, counts: &mut GeneratedPatternCounts) {
    let mut index = 0usize;
    let word_limit = limit.min(bytes.len());
    while index + 8 <= word_limit {
        let chunk = u64::from_ne_bytes(bytes[index..index + 8].try_into().unwrap());
        let mut mask = contains_zero_byte(chunk ^ repeated_byte(b'<'));
        while mask != 0 {
            let found = index + ((mask.trailing_zeros() >> 3) as usize);
            classify_generated_lt(bytes, found, counts);
            mask &= mask - 1;
        }
        index += 8;
    }

    while index < word_limit {
        if bytes[index] == b'<' {
            classify_generated_lt(bytes, index, counts);
        }
        index += 1;
    }
}

fn classify_generated_lt(bytes: &[u8], found: usize, counts: &mut GeneratedPatternCounts) {
    counts.total_lt += 1;
    let tail = &bytes[found..];
    match bytes.get(found + 1).copied() {
        Some(b'/') | Some(b'?') => {}
        Some(b'm') => {
            counts.members += 1;
        }
        Some(b's') => {
            if !tail.starts_with(b"<string></string>") {
                counts.text_nodes += 1;
            }
        }
        Some(b'n') => {
            if bytes.get(found + 2) == Some(&b'u') && bytes.get(found + 3) == Some(&b'm') {
                counts.text_nodes += 1;
            } else {
                counts.nulls += 1;
            }
        }
        Some(b'b') => {
            counts.text_nodes += 1;
        }
        Some(b'o' | b'a' | b'i') => {}
        _ => {}
    }
}

fn repeated_byte(byte: u8) -> u64 {
    u64::from_ne_bytes([byte; 8])
}

fn contains_zero_byte(word: u64) -> u64 {
    word.wrapping_sub(0x0101_0101_0101_0101) & !word & 0x8080_8080_8080_8080
}
