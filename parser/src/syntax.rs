pub(crate) fn is_xml_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x09 | 0x0a | 0x0d | 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff
    )
}

pub(crate) fn is_xml11_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x01..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff
    )
}

pub(crate) fn is_xml11_literal_char(ch: char) -> bool {
    is_xml11_char(ch)
        && !matches!(
            ch as u32,
            0x01..=0x08 | 0x0b..=0x0c | 0x0e..=0x1f | 0x7f..=0x84 | 0x86..=0x9f
        )
}

pub(crate) fn is_name_start_char(ch: char) -> bool {
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

pub(crate) fn is_name_char(ch: char) -> bool {
    is_name_start_char(ch)
        || matches!(
            ch as u32,
            0x2d | 0x2e | 0x30..=0x39 | 0xb7 | 0x300..=0x36f | 0x203f..=0x2040
        )
}

/// Checks a complete XML 1.x `Name` without allocating.
#[inline]
pub(crate) fn is_valid_name(name: &str) -> bool {
    if name.is_ascii() {
        let mut bytes = name.bytes();
        return bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b':'))
            && bytes.all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'-' | b'.')
            });
    }
    let mut characters = name.chars();
    characters.next().is_some_and(is_name_start_char) && characters.all(is_name_char)
}

pub(crate) fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

pub(crate) fn is_xml_target(target: &str) -> bool {
    target.eq_ignore_ascii_case("xml")
}

pub(crate) fn is_pubid_char(ch: char) -> bool {
    ch == ' '
        || ch == '\r'
        || ch == '\n'
        || ch.is_ascii_alphanumeric()
        || "-'()+,./:=?;!*#@$_%".contains(ch)
}
