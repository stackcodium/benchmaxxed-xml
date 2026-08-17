use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use xml_parser::{
    decode_xml_bytes, validate_document_bytes_with_config, ParserConfig, XmlErrorKind,
    XmlInputEncoding,
};

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
    let options = Options::parse(env::args().skip(1))?;
    for root in &options.roots {
        let summary = scan_root(root, options.validate_supported)?;
        summary.print(root);
    }
    Ok(())
}

#[derive(Debug)]
struct Options {
    roots: Vec<PathBuf>,
    validate_supported: bool,
}

impl Options {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut roots = Vec::new();
        let mut validate_supported = false;

        for arg in args {
            match arg.as_str() {
                "--validate-supported" => validate_supported = true,
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                _ => roots.push(PathBuf::from(arg)),
            }
        }

        if roots.is_empty() {
            roots = vec!["tests/fixtures/encoding".into()];
        }

        Ok(Self {
            roots,
            validate_supported,
        })
    }
}

fn print_help() {
    println!("encoding_probe [--validate-supported] [ROOT...]");
}

#[derive(Default)]
struct Summary {
    files: usize,
    decoded: usize,
    validated: usize,
    unsupported: usize,
    decode_errors: usize,
    validation_errors: usize,
    utf8: usize,
    ascii: usize,
    utf16: usize,
    utf32: usize,
    latin1: usize,
    first_errors: Vec<String>,
}

impl Summary {
    fn print(&self, root: &Path) {
        println!("root: {}", root.display());
        println!("files: {}", self.files);
        println!("decoded: {}", self.decoded);
        println!("validated: {}", self.validated);
        println!("unsupported: {}", self.unsupported);
        println!("decode_errors: {}", self.decode_errors);
        println!("validation_errors: {}", self.validation_errors);
        println!(
            "encodings: utf8={} ascii={} utf16={} utf32={} latin1={}",
            self.utf8, self.ascii, self.utf16, self.utf32, self.latin1
        );
        if !self.first_errors.is_empty() {
            println!("first errors:");
            for error in &self.first_errors {
                println!("  {error}");
            }
        }
    }

    fn record_error(&mut self, error: String) {
        if self.first_errors.len() < 12 {
            self.first_errors.push(error);
        }
    }
}

fn scan_root(root: &Path, validate_supported: bool) -> Result<Summary, String> {
    let mut files = Vec::new();
    collect_xml_like_files(root, &mut files)?;
    files.sort();

    let mut summary = Summary::default();
    let config = ParserConfig::default().validate_characters(false);

    for path in files {
        summary.files += 1;
        let bytes = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
        match decode_xml_bytes(&bytes) {
            Ok(decoded) => {
                summary.decoded += 1;
                summary.record_encoding(decoded.encoding);
                if validate_supported {
                    match validate_document_bytes_with_config(&bytes, config) {
                        Ok(()) => summary.validated += 1,
                        Err(error) => {
                            summary.validation_errors += 1;
                            summary.record_error(format!("validate {}: {error}", path.display()));
                        }
                    }
                }
            }
            Err(error) if matches!(error.kind, XmlErrorKind::UnsupportedEncoding(_)) => {
                summary.unsupported += 1;
            }
            Err(error) => {
                summary.decode_errors += 1;
                summary.record_error(format!("decode {}: {error}", path.display()));
            }
        }
    }

    Ok(summary)
}

impl Summary {
    fn record_encoding(&mut self, encoding: XmlInputEncoding) {
        match encoding {
            XmlInputEncoding::Utf8 => self.utf8 += 1,
            XmlInputEncoding::UsAscii => self.ascii += 1,
            XmlInputEncoding::Utf16Le | XmlInputEncoding::Utf16Be => self.utf16 += 1,
            XmlInputEncoding::Utf32Le | XmlInputEncoding::Utf32Be => self.utf32 += 1,
            XmlInputEncoding::Latin1 => self.latin1 += 1,
        }
    }
}

fn collect_xml_like_files(path: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    if path
        .components()
        .any(|component| component.as_os_str() == ".git")
    {
        return Ok(());
    }

    let metadata =
        fs::metadata(path).map_err(|error| format!("metadata {}: {error}", path.display()))?;
    if metadata.is_file() {
        if is_xml_like_path(path) {
            output.push(path.to_owned());
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        return Ok(());
    }

    let entries =
        fs::read_dir(path).map_err(|error| format!("read dir {}: {error}", path.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read dir entry {}: {error}", path.display()))?;
        collect_xml_like_files(&entry.path(), output)?;
    }
    Ok(())
}

fn is_xml_like_path(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "xml" | "xsl" | "xsd" | "dtd" | "ent" | "rss" | "rdf" | "atom" | "feed" | "html" | "xhtml"
    )
}
