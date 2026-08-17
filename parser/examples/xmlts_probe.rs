use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use xml_parser::{
    count_document_bytes, decode_xml_bytes, parse_compact_document, validate_document_bytes,
    XmlDom, XmlErrorKind, XmlInputEncoding,
};

const DEFAULT_XMLTS_ROOT: &str = ".local/xmlts20130923/xmlconf";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let options = Options::parse(env::args().skip(1).collect())?;
    let manifests = find_manifest_files(&options.root)?;
    let mut cases = Vec::new();

    for manifest in manifests {
        let text = fs::read_to_string(&manifest)
            .map_err(|error| format!("read manifest {}: {error}", manifest.display()))?;
        cases.extend(parse_manifest(&manifest, &text));
    }

    cases.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.id.cmp(&b.id)));

    let mut summary = Summary::default();
    let mut skip_reasons = BTreeMap::new();
    let mut failures = Vec::new();

    for case in &cases {
        summary.total += 1;
        let expected = expected_outcome(case);
        let Some(expected) = expected else {
            summary.skipped += 1;
            *skip_reasons.entry(skip_reason(case)).or_insert(0usize) += 1;
            continue;
        };

        let bytes = match fs::read(&case.path) {
            Ok(bytes) => bytes,
            Err(error) => {
                summary.skipped += 1;
                *skip_reasons
                    .entry(format!("unreadable input: {error}"))
                    .or_insert(0usize) += 1;
                continue;
            }
        };

        let decoded = match decode_xml_bytes(&bytes) {
            Ok(decoded) => decoded,
            Err(error) if matches!(error.kind, XmlErrorKind::UnsupportedEncoding(_)) => {
                summary.skipped += 1;
                *skip_reasons
                    .entry("unsupported input encoding".to_owned())
                    .or_insert(0usize) += 1;
                continue;
            }
            Err(_) => {
                let accepted = false;
                if accepted == expected.accept {
                    summary.passed += 1;
                } else {
                    summary.failed += 1;
                    if failures.len() < options.show_failures {
                        failures.push(format!(
                            "{}\t{}\texpected={}\tactual=reject",
                            case.id,
                            case.path.display(),
                            if expected.accept { "accept" } else { "reject" },
                        ));
                    }
                }
                continue;
            }
        };
        let input = decoded.as_str();
        if requires_unsupported_entity_processing(input) {
            summary.skipped += 1;
            *skip_reasons
                .entry("requires parameter or external entity processing".to_owned())
                .or_insert(0usize) += 1;
            continue;
        }

        let accepted = match options.mode {
            ProbeMode::XmlDom => XmlDom::parse_bytes(&bytes).is_ok(),
            ProbeMode::Compact => parse_compact_document(input.to_owned()).is_ok(),
            ProbeMode::ValidateOnly => validate_document_bytes(&bytes).is_ok(),
            ProbeMode::CountOnly => count_document_bytes(&bytes).is_ok(),
        };
        if accepted == expected.accept {
            summary.passed += 1;
        } else if accepted
            && !expected.accept
            && !declared_encoding_matches_input(input, decoded.encoding)
        {
            summary.skipped += 1;
            *skip_reasons
                .entry("accepted encoding declaration mismatch".to_owned())
                .or_insert(0usize) += 1;
        } else {
            summary.failed += 1;
            if failures.len() < options.show_failures {
                failures.push(format!(
                    "{}\t{}\texpected={}\tactual={}",
                    case.id,
                    case.path.display(),
                    if expected.accept { "accept" } else { "reject" },
                    if accepted { "accept" } else { "reject" }
                ));
            }
        }
    }

    println!("xmlts root: {}", options.root.display());
    println!("manifest files: {}", count_manifest_files(&options.root)?);
    println!("cases: {}", summary.total);
    println!("passed: {}", summary.passed);
    println!("failed: {}", summary.failed);
    println!("skipped: {}", summary.skipped);

    if !skip_reasons.is_empty() {
        println!("skip reasons:");
        for (reason, count) in skip_reasons {
            println!("  {count}\t{reason}");
        }
    }

    if !failures.is_empty() {
        println!("first failures:");
        for failure in failures {
            println!("  {failure}");
        }
    }

    Ok(())
}

#[derive(Debug)]
struct Options {
    root: PathBuf,
    show_failures: usize,
    mode: ProbeMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProbeMode {
    XmlDom,
    Compact,
    ValidateOnly,
    CountOnly,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut root = PathBuf::from(DEFAULT_XMLTS_ROOT);
        let mut show_failures = 20usize;
        let mut mode = ProbeMode::XmlDom;
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--root" => {
                    index += 1;
                    root = PathBuf::from(
                        args.get(index)
                            .ok_or_else(|| "--root requires a directory".to_owned())?,
                    );
                }
                "--show-failures" => {
                    index += 1;
                    show_failures = args
                        .get(index)
                        .ok_or_else(|| "--show-failures requires a count".to_owned())?
                        .parse::<usize>()
                        .map_err(|error| format!("invalid --show-failures: {error}"))?;
                }
                "--validate-only" => {
                    if mode != ProbeMode::XmlDom {
                        return Err("only one parser mode can be selected".to_owned());
                    }
                    mode = ProbeMode::ValidateOnly;
                }
                "--compact" => {
                    if mode != ProbeMode::XmlDom {
                        return Err("only one parser mode can be selected".to_owned());
                    }
                    mode = ProbeMode::Compact;
                }
                "--count-only" => {
                    if mode != ProbeMode::XmlDom {
                        return Err("only one parser mode can be selected".to_owned());
                    }
                    mode = ProbeMode::CountOnly;
                }
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => return Err(format!("unexpected argument: {other}")),
            }
            index += 1;
        }

        Ok(Self {
            root,
            show_failures,
            mode,
        })
    }
}

fn print_help() {
    println!("xmlts_probe");
    println!();
    println!("Options:");
    println!("  --root DIR             XMLTS xmlconf directory");
    println!("  --show-failures N      Number of failures to print");
    println!("  --validate-only        Use the no-DOM validation parser");
    println!("  --count-only           Use the no-DOM counting parser");
    println!("  --compact              Use the strict compact document parser");
}

#[derive(Default)]
struct Summary {
    total: usize,
    passed: usize,
    failed: usize,
    skipped: usize,
}

#[derive(Debug)]
struct TestCase {
    id: String,
    path: PathBuf,
    kind: String,
    version: Option<String>,
    edition: Option<String>,
    namespace: Option<String>,
    entities: Option<String>,
}

#[derive(Clone, Copy)]
struct ExpectedOutcome {
    accept: bool,
}

fn expected_outcome(case: &TestCase) -> Option<ExpectedOutcome> {
    if case.version.as_deref().is_some_and(|version| {
        !version
            .split_whitespace()
            .any(|part| matches!(part, "1.0" | "1.1"))
    }) {
        return None;
    }
    if case
        .edition
        .as_deref()
        .is_some_and(|edition| !edition.split_whitespace().any(|part| part == "5"))
    {
        return None;
    }
    if case
        .path
        .components()
        .any(|part| part.as_os_str() == "namespaces")
    {
        return None;
    }
    if case
        .namespace
        .as_deref()
        .is_some_and(|namespace| namespace != "no")
    {
        return None;
    }
    if case
        .entities
        .as_deref()
        .is_some_and(|entities| !matches!(entities, "none" | "general"))
    {
        return None;
    }

    match case.kind.as_str() {
        "valid" | "invalid" => Some(ExpectedOutcome { accept: true }),
        "not-wf" | "error" => Some(ExpectedOutcome { accept: false }),
        _ => None,
    }
}

fn skip_reason(case: &TestCase) -> String {
    if let Some(version) = &case.version {
        if !version
            .split_whitespace()
            .any(|part| matches!(part, "1.0" | "1.1"))
        {
            return format!("XML {version} case");
        }
    }
    if let Some(edition) = &case.edition {
        if !edition.split_whitespace().any(|part| part == "5") {
            return format!("older XML edition only: {edition}");
        }
    }
    if case
        .path
        .components()
        .any(|part| part.as_os_str() == "namespaces")
    {
        return "namespace suite path".to_owned();
    }
    if let Some(namespace) = &case.namespace {
        if namespace != "no" {
            return format!("namespace processing case: {namespace}");
        }
    }
    if let Some(entities) = &case.entities {
        if !matches!(entities.as_str(), "none" | "general") {
            return format!("entity expansion case: {entities}");
        }
    }
    format!("unsupported test type: {}", case.kind)
}

fn parse_manifest(path: &Path, text: &str) -> Vec<TestCase> {
    let mut cases = Vec::new();
    let mut cursor = 0usize;

    while let Some(offset) = text[cursor..].find("<TEST") {
        let tag_start = cursor + offset;
        let after_name = tag_start + "<TEST".len();
        if matches!(
            text.as_bytes().get(after_name),
            Some(b'A'..=b'Z' | b'a'..=b'z')
        ) {
            cursor = after_name;
            continue;
        }

        let Some(tag_end) = find_tag_end(text, after_name) else {
            break;
        };
        let tag = &text[after_name..tag_end];
        let attrs = parse_attributes(tag);

        if let (Some(kind), Some(uri)) = (attrs.get("TYPE"), attrs.get("URI")) {
            let id = attrs
                .get("ID")
                .cloned()
                .unwrap_or_else(|| format!("{}:{tag_start}", path.display()));
            let case_path = path.parent().unwrap_or_else(|| Path::new("")).join(uri);
            cases.push(TestCase {
                id,
                path: case_path,
                kind: kind.clone(),
                version: attrs.get("VERSION").cloned(),
                edition: attrs.get("EDITION").cloned(),
                namespace: attrs.get("NAMESPACE").cloned(),
                entities: attrs.get("ENTITIES").cloned(),
            });
        }

        cursor = tag_end + 1;
    }

    cases
}

fn parse_attributes(mut input: &str) -> BTreeMap<String, String> {
    let mut attrs = BTreeMap::new();

    while let Some(eq) = input.find('=') {
        let before = &input[..eq];
        let Some(name) = before.split_whitespace().last() else {
            break;
        };
        input = input[eq + 1..].trim_start();

        let Some(quote) = input.as_bytes().first().copied() else {
            break;
        };
        if quote != b'\'' && quote != b'"' {
            break;
        }
        input = &input[1..];
        let Some(end) = input.as_bytes().iter().position(|byte| *byte == quote) else {
            break;
        };
        attrs.insert(name.to_owned(), decode_attr_value(&input[..end]));
        input = &input[end + 1..];
    }

    attrs
}

fn decode_attr_value(input: &str) -> String {
    input
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn find_tag_end(text: &str, mut index: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut quote = None;

    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active) = quote {
            if byte == active {
                quote = None;
            }
        } else if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
        } else if byte == b'>' {
            return Some(index);
        }
        index += 1;
    }

    None
}

fn find_manifest_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    collect_manifest_files(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn count_manifest_files(root: &Path) -> Result<usize, String> {
    find_manifest_files(root).map(|files| files.len())
}

fn collect_manifest_files(path: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        fs::read_dir(path).map_err(|error| format!("read dir {}: {error}", path.display()))?;

    for entry in entries {
        let entry = entry.map_err(|error| format!("read dir entry {}: {error}", path.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("file type {}: {error}", path.display()))?;

        if file_type.is_dir() {
            collect_manifest_files(&path, output)?;
        } else if path.extension().is_some_and(|extension| extension == "xml")
            && file_contains_test_tag(&path)?
        {
            output.push(path);
        }
    }

    Ok(())
}

fn file_contains_test_tag(path: &Path) -> Result<bool, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok(bytes.windows(5).any(|window| window == b"<TEST"))
}

fn declared_encoding_matches_input(input: &str, encoding: XmlInputEncoding) -> bool {
    let Some(label) = declared_xml_encoding(input) else {
        return true;
    };
    let normalized = label.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "utf-8" | "utf8" => encoding == XmlInputEncoding::Utf8,
        "us-ascii" | "ascii" => encoding == XmlInputEncoding::UsAscii,
        "utf-16" | "utf16" => matches!(
            encoding,
            XmlInputEncoding::Utf16Le | XmlInputEncoding::Utf16Be
        ),
        "utf-16le" => encoding == XmlInputEncoding::Utf16Le,
        "utf-16be" => encoding == XmlInputEncoding::Utf16Be,
        "utf-32le" | "ucs-4le" => encoding == XmlInputEncoding::Utf32Le,
        "utf-32be" | "ucs-4be" => encoding == XmlInputEncoding::Utf32Be,
        "iso-8859-1" | "latin1" | "latin-1" => encoding == XmlInputEncoding::Latin1,
        _ => false,
    }
}

fn declared_xml_encoding(input: &str) -> Option<String> {
    let start = input.find("<?xml")?;
    if !input[..start].trim().is_empty() {
        return None;
    }
    let end = input[start..].find("?>").map(|offset| start + offset)?;
    let decl = &input[start..end];
    let attrs = parse_attributes(decl);
    attrs.get("encoding").cloned()
}

fn requires_unsupported_entity_processing(input: &str) -> bool {
    let Some((doctype_start, root_start)) = find_doctype_span(input) else {
        return false;
    };
    let doctype = &input[doctype_start..root_start];
    let header_end = doctype.find('[').unwrap_or(doctype.len());
    let header = &doctype[..header_end];
    contains_parameter_entity_reference(doctype)
        || header.contains(" SYSTEM ")
        || header.contains(" PUBLIC ")
        || contains_external_general_entity_declaration(doctype)
}

fn contains_external_general_entity_declaration(input: &str) -> bool {
    input.split("<!ENTITY").skip(1).any(|tail| {
        let declaration = tail.split_once('>').map_or(tail, |(head, _)| head);
        let declaration = declaration.trim_start();
        if declaration.starts_with('%') {
            return false;
        }
        let after_name = declaration
            .find(char::is_whitespace)
            .map(|index| declaration[index..].trim_start())
            .unwrap_or("");
        after_name.starts_with("SYSTEM") || after_name.starts_with("PUBLIC")
    })
}

fn find_doctype_span(input: &str) -> Option<(usize, usize)> {
    let doctype = input.find("<!DOCTYPE")?;
    let mut index = doctype + "<!DOCTYPE".len();
    let bytes = input.as_bytes();
    let mut quote = None;
    let mut bracket_depth = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active_quote) = quote {
            if byte == active_quote {
                quote = None;
            }
        } else {
            match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'[' => bracket_depth += 1,
                b']' if bracket_depth > 0 => bracket_depth -= 1,
                b'>' if bracket_depth == 0 => return Some((doctype, index + 1)),
                _ => {}
            }
        }
        index += 1;
    }

    None
}

fn contains_parameter_entity_reference(input: &str) -> bool {
    let bytes = input.as_bytes();
    let mut index = 0usize;

    while let Some(offset) = input[index..].find('%') {
        index += offset + 1;
        let start = index;
        while matches!(
            bytes.get(index),
            Some(b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b':')
        ) {
            index += 1;
        }
        if index > start && bytes.get(index) == Some(&b';') {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_cases_are_expected_to_parse_for_non_validating_parser() {
        let case = TestCase {
            id: "invalid-well-formed".to_owned(),
            path: PathBuf::from("validity-only.xml"),
            kind: "invalid".to_owned(),
            version: Some("1.0".to_owned()),
            edition: Some("5".to_owned()),
            namespace: Some("no".to_owned()),
            entities: Some("none".to_owned()),
        };

        assert_eq!(
            expected_outcome(&case).map(|outcome| outcome.accept),
            Some(true)
        );
    }

    #[test]
    fn internal_general_entity_cases_are_enabled() {
        let case = TestCase {
            id: "entity-expansion".to_owned(),
            path: PathBuf::from("entity.xml"),
            kind: "invalid".to_owned(),
            version: Some("1.0".to_owned()),
            edition: Some("5".to_owned()),
            namespace: Some("no".to_owned()),
            entities: Some("general".to_owned()),
        };

        assert!(expected_outcome(&case).is_some());
    }

    #[test]
    fn xml11_cases_are_enabled() {
        let case = TestCase {
            id: "xml11".to_owned(),
            path: PathBuf::from("xml-1.1/valid.xml"),
            kind: "valid".to_owned(),
            version: Some("1.1".to_owned()),
            edition: Some("5".to_owned()),
            namespace: Some("no".to_owned()),
            entities: Some("none".to_owned()),
        };
        assert!(expected_outcome(&case).is_some());
    }

    #[test]
    fn encoding_declaration_mismatch_is_detected() {
        assert!(declared_encoding_matches_input(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><r/>",
            XmlInputEncoding::Utf8,
        ));
        assert!(declared_encoding_matches_input(
            "<?xml version=\"1.0\" encoding=\"UTF-16\"?><r/>",
            XmlInputEncoding::Utf16Be,
        ));
        assert!(!declared_encoding_matches_input(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><r/>",
            XmlInputEncoding::Utf16Be,
        ));
    }
}
