use std::{
    env, fs,
    hint::black_box,
    io::{Read, Seek, SeekFrom},
    process::ExitCode,
    str,
    time::{Duration, Instant},
};

use xml_parser::{
    count_document_with_config, parse_compact_document_with_config,
    parse_document_view_with_config, validate_document_with_config, ParserConfig,
    XmlCompactDocument, XmlDocumentView, XmlDom, XmlNodeKind, XmlTextWhitespacePolicy,
    XmlTreeStats, XmlViewNodeId,
};

#[path = "bench_parse/bench_stream.rs"]
mod bench_stream;

use bench_stream::{
    count_generated_xml_bytes, count_generated_xml_stream_reader, count_xml_stream_reader,
};

const USAGE: &str =
    "usage: bench_parse [--quiet] [--xml-dom-full-dom|--compact-full-dom|--parse-only|--view-only|--view-walk|--count-only|--stream-count-only|--generated-count-only|--stream-generated-count-only|--validate-only] [--skip-whitespace-text|--compact-dom|--drop-text] [--trusted-xml-chars|--trusted-references|--trusted-attributes] [--mode-label LABEL] [--warmup N] [--iterations N] [--runs N] [--min-duration-ms N] FILE...";

fn main() -> ExitCode {
    let mut options = BenchOptions {
        iterations: 1,
        warmup: 0,
        runs: 1,
        min_duration: None,
        mode: BenchMode::XmlDomFullDom,
        skip_whitespace_text: false,
        compact_dom: false,
        drop_text: false,
        trusted_xml_chars: false,
        trusted_references: false,
        trusted_attributes: false,
        mode_label: None,
        quiet: false,
        paths: Vec::new(),
    };
    let mut args = env::args().skip(1).peekable();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--iterations" => {
                let Some(iterations) = args.next().and_then(|value| value.parse::<usize>().ok())
                else {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                };
                if iterations == 0 {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
                options.iterations = iterations;
            }
            "--warmup" => {
                let Some(warmup) = args.next().and_then(|value| value.parse::<usize>().ok()) else {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                };
                options.warmup = warmup;
            }
            "--runs" => {
                let Some(runs) = args.next().and_then(|value| value.parse::<usize>().ok()) else {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                };
                if runs == 0 {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
                options.runs = runs;
            }
            "--min-duration-ms" => {
                let Some(milliseconds) = args.next().and_then(|value| value.parse::<u64>().ok())
                else {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                };
                if milliseconds == 0 {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
                options.min_duration = Some(Duration::from_millis(milliseconds));
            }
            "--parse-only" => {
                if options.mode != BenchMode::XmlDomFullDom {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
                options.mode = BenchMode::ParseOnly;
            }
            "--compact-full-dom" => {
                if options.mode != BenchMode::XmlDomFullDom {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
                options.mode = BenchMode::CompactFullDom;
            }
            "--xml-dom-full-dom" => {
                if options.mode != BenchMode::XmlDomFullDom {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
                options.mode = BenchMode::XmlDomFullDom;
            }
            "--view-only" => {
                if options.mode != BenchMode::XmlDomFullDom {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
                options.mode = BenchMode::ViewOnly;
            }
            "--view-walk" => {
                if options.mode != BenchMode::XmlDomFullDom {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
                options.mode = BenchMode::ViewWalk;
            }
            "--validate-only" => {
                if options.mode != BenchMode::XmlDomFullDom {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
                options.mode = BenchMode::ValidateOnly;
            }
            "--count-only" => {
                if options.mode != BenchMode::XmlDomFullDom {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
                options.mode = BenchMode::CountOnly;
            }
            "--stream-count-only" => {
                if options.mode != BenchMode::XmlDomFullDom {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
                options.mode = BenchMode::StreamCountOnly;
            }
            "--generated-count-only" => {
                if options.mode != BenchMode::XmlDomFullDom {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
                options.mode = BenchMode::GeneratedCountOnly;
                options.skip_whitespace_text = true;
                options.compact_dom = true;
                options.trusted_xml_chars = true;
            }
            "--stream-generated-count-only" => {
                if options.mode != BenchMode::XmlDomFullDom {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                }
                options.mode = BenchMode::StreamGeneratedCountOnly;
                options.skip_whitespace_text = true;
                options.compact_dom = true;
                options.trusted_xml_chars = true;
            }
            "--skip-whitespace-text" => options.skip_whitespace_text = true,
            "--compact-dom" => {
                options.skip_whitespace_text = true;
                options.compact_dom = true;
            }
            "--drop-text" => options.drop_text = true,
            "--trusted-xml-chars" => options.trusted_xml_chars = true,
            "--trusted-references" => options.trusted_references = true,
            "--trusted-attributes" => options.trusted_attributes = true,
            "--mode-label" => {
                let Some(label) = args.next() else {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                };
                options.mode_label = Some(label);
            }
            "--quiet" => options.quiet = true,
            _ => options.paths.push(arg),
        }
    }

    run(options)
}

fn run(options: BenchOptions) -> ExitCode {
    if options.paths.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    }

    let mut ok = true;
    let mut aggregate = BenchSummary::default();
    let mut aggregate_count = 0usize;
    println!(
        "file\tmode\titer\twarmup\tbytes\tread_ms\tparse_ms\tcount_ms\ttotal_ms\tparse_mib_s\ttotal_mib_s\trss_kb\thwm_kb\telements\tattributes\tnodes"
    );
    for path in &options.paths {
        match bench_path(path, &options) {
            Ok(summary) => {
                if !options.quiet {
                    print_summary(path, &options, &summary);
                }
                aggregate.add(&summary);
                aggregate_count += 1;
            }
            Err(error) => {
                eprintln!("{path}: {error}");
                ok = false;
            }
        }
    }

    if aggregate_count > 1 || options.quiet {
        print_summary("TOTAL", &options, &aggregate);
    }

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

struct BenchOptions {
    iterations: usize,
    warmup: usize,
    runs: usize,
    min_duration: Option<Duration>,
    mode: BenchMode,
    skip_whitespace_text: bool,
    compact_dom: bool,
    drop_text: bool,
    trusted_xml_chars: bool,
    trusted_references: bool,
    trusted_attributes: bool,
    mode_label: Option<String>,
    quiet: bool,
    paths: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BenchMode {
    XmlDomFullDom,
    CompactFullDom,
    ParseOnly,
    ViewOnly,
    ViewWalk,
    CountOnly,
    StreamCountOnly,
    GeneratedCountOnly,
    StreamGeneratedCountOnly,
    ValidateOnly,
}

impl BenchMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::XmlDomFullDom => "xml-dom-full-dom",
            Self::CompactFullDom => "compact-full-dom",
            Self::ParseOnly => "parse-only",
            Self::ViewOnly => "view-only",
            Self::ViewWalk => "view-walk",
            Self::CountOnly => "count-only",
            Self::StreamCountOnly => "stream-count-only",
            Self::GeneratedCountOnly => "generated-count-only",
            Self::StreamGeneratedCountOnly => "stream-generated-count-only",
            Self::ValidateOnly => "validate-only",
        }
    }
}

fn print_summary(label: &str, options: &BenchOptions, summary: &BenchSummary) {
    let total_time = summary.read + summary.parse + summary.count;
    println!(
        "{}\t{}\t{}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.1}\t{:.1}\t{}\t{}\t{}\t{}\t{}",
        label,
        options.mode_label(),
        summary.iterations,
        summary.warmup,
        summary.bytes,
        millis(summary.read),
        millis(summary.parse),
        millis(summary.count),
        millis(total_time),
        throughput_mib_s(summary.bytes, summary.parse),
        throughput_mib_s(summary.bytes, total_time),
        summary.rss_kb,
        summary.hwm_kb,
        summary.elements,
        summary.attributes,
        summary.nodes
    );
}

fn bench_path(path: &str, options: &BenchOptions) -> Result<BenchSummary, String> {
    let iterations = if let Some(min_duration) = options.min_duration {
        calibrate_iterations(path, options, min_duration)?
    } else {
        options.iterations
    };

    let mut best = None;
    for _ in 0..options.runs {
        let summary = bench_path_iterations(path, options, iterations, options.warmup)?;
        if best
            .as_ref()
            .is_none_or(|best: &BenchSummary| summary.parser_time() < best.parser_time())
        {
            best = Some(summary);
        }
    }

    best.ok_or_else(|| "benchmark did not run".to_owned())
}

fn calibrate_iterations(
    path: &str,
    options: &BenchOptions,
    min_duration: Duration,
) -> Result<usize, String> {
    let mut iterations = options.iterations;

    loop {
        let summary = bench_path_iterations(path, options, iterations, 0)?;
        let measured = summary.parser_time().as_secs_f64() * iterations as f64;
        if measured >= min_duration.as_secs_f64() {
            return Ok(iterations);
        }

        let scale = if measured == 0.0 {
            10
        } else {
            (min_duration.as_secs_f64() / measured).ceil() as usize
        };
        iterations = iterations.saturating_mul(scale.max(2));
    }
}

fn bench_path_iterations(
    path: &str,
    options: &BenchOptions,
    iterations: usize,
    warmup: usize,
) -> Result<BenchSummary, String> {
    let mut total = BenchSummary {
        iterations,
        warmup,
        ..BenchSummary::default()
    };
    let stream_bytes = if options.mode.is_streaming() {
        Some(fs::metadata(path).map_err(|error| error.to_string())?.len() as usize)
    } else {
        None
    };
    let mut stream_file = if options.mode.is_streaming() {
        Some(fs::File::open(path).map_err(|error| error.to_string())?)
    } else {
        None
    };

    for iteration in 0..iterations + warmup {
        if options.mode.is_streaming() {
            let file = stream_file.as_mut().expect("stream file opened");
            file.seek(SeekFrom::Start(0))
                .map_err(|error| error.to_string())?;
            let parse_start = Instant::now();
            let counts = match options.mode {
                BenchMode::StreamCountOnly => {
                    count_xml_stream_reader(file, config_from_options(options))?
                }
                BenchMode::StreamGeneratedCountOnly => count_generated_xml_stream_reader(file)?,
                _ => unreachable!("non-stream mode handled below"),
            };
            let parse = parse_start.elapsed();
            black_box(counts.nodes);

            if iteration < warmup {
                continue;
            }

            total.bytes = stream_bytes.expect("stream metadata loaded");
            total.parse += parse;
            total.elements = counts.elements;
            total.attributes = counts.attributes;
            total.nodes = counts.nodes;
            let memory = read_process_memory();
            total.rss_kb = total.rss_kb.max(memory.rss_kb);
            total.hwm_kb = total.hwm_kb.max(memory.hwm_kb);
            continue;
        }

        let read_start = Instant::now();
        let input = fs::read_to_string(path).map_err(|error| error.to_string())?;
        let input_len = input.len();
        let has_declaration = input.trim_start_matches('\u{feff}').starts_with("<?xml");
        let read = read_start.elapsed();

        let mut counts = Counts::default();
        let mut view_walk_count = Duration::ZERO;
        let mut compact_document = None;
        let mut dom_document = None;
        let parse_start = Instant::now();
        match options.mode {
            BenchMode::XmlDomFullDom | BenchMode::ParseOnly => {
                dom_document = Some(
                    XmlDom::parse_with_config(input, config_from_options(options))
                        .map_err(|error| error.to_string())?,
                );
            }
            BenchMode::ViewOnly | BenchMode::ViewWalk => {
                let view = parse_document_view_with_config(&input, config_from_options(options))
                    .map_err(|error| error.to_string())?;
                counts = Counts::from(view.tree_stats());
                if options.mode == BenchMode::ViewWalk {
                    let count_start = Instant::now();
                    counts = walk_document_view(&view);
                    view_walk_count = count_start.elapsed();
                } else {
                    black_box(view.nodes().len());
                }
            }
            BenchMode::CompactFullDom => {
                let compact =
                    parse_compact_document_with_config(input, config_from_options(options))
                        .map_err(|error| error.to_string())?;
                black_box(compact.nodes().len());
                compact_document = Some(compact);
            }
            BenchMode::CountOnly => {
                counts = count_document_with_config(&input, config_from_options(options))
                    .map(Counts::from)
                    .map_err(|error| error.to_string())?;
            }
            BenchMode::GeneratedCountOnly => {
                counts = count_generated_xml_bytes(input.as_bytes())?;
            }
            BenchMode::ValidateOnly => {
                validate_document_with_config(&input, config_from_options(options))
                    .map_err(|error| error.to_string())?;
            }
            BenchMode::StreamCountOnly | BenchMode::StreamGeneratedCountOnly => {
                unreachable!("stream mode handled above")
            }
        }
        let parse = parse_start.elapsed();
        black_box(dom_document.as_ref());

        let count_start = Instant::now();
        if let (BenchMode::XmlDomFullDom, Some(document)) = (options.mode, &dom_document) {
            counts = Counts::from(document.tree_stats());
        }
        if let Some(document) = &compact_document {
            counts = walk_compact_document(document);
            if has_declaration {
                counts.nodes += 1;
            }
        }
        let count = count_start.elapsed();

        if iteration < warmup {
            continue;
        }

        total.bytes = input_len;
        total.read += read;
        total.parse += parse;
        if options.mode != BenchMode::ViewWalk {
            total.count += count;
        } else {
            total.count += view_walk_count;
        }
        total.elements = counts.elements;
        total.attributes = counts.attributes;
        total.nodes = counts.nodes;
        let memory = read_process_memory();
        total.rss_kb = total.rss_kb.max(memory.rss_kb);
        total.hwm_kb = total.hwm_kb.max(memory.hwm_kb);
    }

    total.read /= iterations as u32;
    total.parse /= iterations as u32;
    total.count /= iterations as u32;
    Ok(total)
}

impl BenchOptions {
    fn mode_label(&self) -> String {
        if let Some(label) = &self.mode_label {
            return label.clone();
        }

        let mut label = self.mode.as_str().to_owned();
        if self.compact_dom && self.mode != BenchMode::ValidateOnly {
            label.push_str("+compact");
        } else if self.skip_whitespace_text && self.mode != BenchMode::ValidateOnly {
            label.push_str("+skip-ws");
        }
        if self.trusted_xml_chars {
            label.push_str("+trusted-chars");
        }
        label
    }
}

impl BenchMode {
    fn is_streaming(self) -> bool {
        matches!(self, Self::StreamCountOnly | Self::StreamGeneratedCountOnly)
    }
}

fn config_from_options(options: &BenchOptions) -> ParserConfig {
    ParserConfig::default()
        .preserve_comments(!options.compact_dom)
        .preserve_processing_instructions(!options.compact_dom)
        .preserve_cdata_nodes(!options.compact_dom)
        .preserve_text_nodes(!options.drop_text)
        .text_whitespace(if options.skip_whitespace_text {
            XmlTextWhitespacePolicy::DiscardWhitespaceOnly
        } else {
            XmlTextWhitespacePolicy::Preserve
        })
        .validate_characters(!options.trusted_xml_chars)
        .validate_references(!options.trusted_references)
        .validate_duplicate_attributes(!options.trusted_attributes)
}

fn walk_document_view(view: &XmlDocumentView<'_>) -> Counts {
    let mut counts = Counts::default();
    walk_view_node(view, view.root(), &mut counts);
    black_box(counts.checksum);
    counts
}

fn walk_compact_document(document: &XmlCompactDocument) -> Counts {
    let mut counts = Counts::default();
    for id in document.node_ids() {
        let node = document.node(id).expect("compact node id");
        counts.nodes += 1;
        match node.kind() {
            XmlNodeKind::Element => {
                counts.elements += 1;
                counts.checksum = counts
                    .checksum
                    .wrapping_add(document.node_name(id).expect("compact node range").len());
                for attribute_index in node.attribute_range() {
                    counts.attributes += 1;
                    counts.checksum = counts
                        .checksum
                        .wrapping_add(
                            document
                                .attribute_name(attribute_index)
                                .expect("compact attribute name")
                                .len(),
                        )
                        .wrapping_add(
                            document
                                .attribute_value(attribute_index)
                                .expect("compact attribute value")
                                .len(),
                        );
                }
            }
            XmlNodeKind::Text | XmlNodeKind::Cdata => {
                counts.checksum = counts
                    .checksum
                    .wrapping_add(document.node_value(id).expect("compact node value").len());
            }
            XmlNodeKind::ProcessingInstruction => {
                counts.checksum = counts
                    .checksum
                    .wrapping_add(document.node_name(id).expect("compact node range").len());
            }
            XmlNodeKind::Comment => {}
        }
    }
    black_box(counts.checksum);
    counts
}

fn walk_view_node(view: &XmlDocumentView<'_>, id: XmlViewNodeId, counts: &mut Counts) {
    let Some(node) = view.node(id) else {
        return;
    };
    counts.nodes += 1;
    match node.kind() {
        XmlNodeKind::Element => {
            counts.elements += 1;
            counts.checksum = counts
                .checksum
                .wrapping_add(view.node_name(id).expect("view node range").len());
            for attribute_index in node.attribute_range() {
                counts.attributes += 1;
                counts.checksum = counts
                    .checksum
                    .wrapping_add(
                        view.attribute_name(attribute_index)
                            .expect("view attribute name")
                            .len(),
                    )
                    .wrapping_add(
                        view.attribute_value(attribute_index)
                            .expect("view attribute value")
                            .len(),
                    );
            }

            for child in view.children(id) {
                walk_view_node(view, child, counts);
            }
        }
        XmlNodeKind::Text | XmlNodeKind::Cdata => {
            counts.checksum = counts
                .checksum
                .wrapping_add(view.node_value(id).expect("view node value").len());
        }
        XmlNodeKind::ProcessingInstruction => {
            counts.checksum = counts
                .checksum
                .wrapping_add(view.node_name(id).expect("view node range").len());
        }
        XmlNodeKind::Comment => {}
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn throughput_mib_s(bytes: usize, duration: Duration) -> f64 {
    let seconds = duration.as_secs_f64();
    if seconds == 0.0 {
        return f64::INFINITY;
    }
    (bytes as f64 / 1_048_576.0) / seconds
}

fn read_process_memory() -> ProcessMemory {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return ProcessMemory::default();
    };

    let mut memory = ProcessMemory::default();
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            memory.rss_kb = parse_status_kb(value);
        } else if let Some(value) = line.strip_prefix("VmHWM:") {
            memory.hwm_kb = parse_status_kb(value);
        }
    }
    memory
}

fn parse_status_kb(value: &str) -> usize {
    value
        .split_whitespace()
        .next()
        .and_then(|number| number.parse().ok())
        .unwrap_or(0)
}

#[derive(Default)]
struct ProcessMemory {
    rss_kb: usize,
    hwm_kb: usize,
}

#[derive(Default)]
struct BenchSummary {
    datasets: usize,
    iterations: usize,
    warmup: usize,
    bytes: usize,
    read: Duration,
    parse: Duration,
    count: Duration,
    rss_kb: usize,
    hwm_kb: usize,
    elements: usize,
    attributes: usize,
    nodes: usize,
}

impl BenchSummary {
    fn parser_time(&self) -> Duration {
        self.parse + self.count
    }

    fn add(&mut self, other: &Self) {
        self.iterations = if self.datasets == 0 {
            other.iterations
        } else if self.iterations == other.iterations {
            self.iterations
        } else {
            0
        };
        self.warmup = if self.datasets == 0 {
            other.warmup
        } else if self.warmup == other.warmup {
            self.warmup
        } else {
            0
        };
        self.datasets += 1;
        self.bytes += other.bytes;
        self.read += other.read;
        self.parse += other.parse;
        self.count += other.count;
        self.rss_kb = self.rss_kb.max(other.rss_kb);
        self.hwm_kb = self.hwm_kb.max(other.hwm_kb);
        self.elements += other.elements;
        self.attributes += other.attributes;
        self.nodes += other.nodes;
    }
}

#[derive(Debug, Default)]
struct Counts {
    elements: usize,
    attributes: usize,
    nodes: usize,
    checksum: usize,
}

impl From<XmlTreeStats> for Counts {
    fn from(stats: XmlTreeStats) -> Self {
        Self {
            elements: stats.elements,
            attributes: stats.attributes,
            nodes: stats.nodes,
            checksum: stats
                .nodes
                .wrapping_add(stats.elements)
                .wrapping_add(stats.attributes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_keeps_mixed_iteration_count_marked_as_zero() {
        let mut aggregate = BenchSummary::default();

        aggregate.add(&summary_with_iterations(3, 1));
        assert_eq!(aggregate.iterations, 3);
        assert_eq!(aggregate.warmup, 1);

        aggregate.add(&summary_with_iterations(10, 1));
        assert_eq!(aggregate.iterations, 0);
        assert_eq!(aggregate.warmup, 1);

        aggregate.add(&summary_with_iterations(3, 1));
        assert_eq!(aggregate.iterations, 0);
        assert_eq!(aggregate.warmup, 1);
    }

    #[test]
    fn aggregate_keeps_mixed_warmup_marked_as_zero() {
        let mut aggregate = BenchSummary::default();

        aggregate.add(&summary_with_iterations(5, 1));
        aggregate.add(&summary_with_iterations(5, 2));
        aggregate.add(&summary_with_iterations(5, 1));

        assert_eq!(aggregate.iterations, 5);
        assert_eq!(aggregate.warmup, 0);
    }

    #[test]
    fn explicit_mode_label_overrides_derived_label() {
        let options = BenchOptions {
            iterations: 1,
            warmup: 0,
            runs: 1,
            min_duration: None,
            mode: BenchMode::CountOnly,
            skip_whitespace_text: true,
            compact_dom: true,
            drop_text: false,
            trusted_xml_chars: true,
            trusted_references: false,
            trusted_attributes: false,
            mode_label: Some("custom-label".to_owned()),
            quiet: false,
            paths: Vec::new(),
        };

        assert_eq!(options.mode_label(), "custom-label");
    }

    #[test]
    fn stream_count_matches_general_count_default_policy() {
        let input = br#"<?xml version="1.0" encoding="UTF-8"?>
<!--before-->
<root a="1" xml:space="preserve"><child/>text<![CDATA[raw]]><?pi ok?></root>
<?after ok?>"#;
        let expected =
            count_document_with_config(str::from_utf8(input).unwrap(), ParserConfig::default())
                .map(Counts::from)
                .unwrap();
        let actual = count_xml_stream_reader(&mut &input[..], ParserConfig::default()).unwrap();

        assert_eq!(actual.elements, expected.elements);
        assert_eq!(actual.attributes, expected.attributes);
        assert_eq!(actual.nodes, expected.nodes);
    }

    #[test]
    fn stream_count_matches_general_count_compact_policy() {
        let input = b"<r>\n  <a/><!--skip--><![CDATA[data]]><?pi ok?>\n  <b/>\n</r>";
        let config = ParserConfig::default()
            .preserve_declaration(false)
            .preserve_doctype(false)
            .preserve_comments(false)
            .preserve_processing_instructions(false)
            .preserve_cdata_nodes(false)
            .preserve_text_nodes(true)
            .text_whitespace(XmlTextWhitespacePolicy::DiscardWhitespaceOnly)
            .validate_characters(true)
            .validate_references(true)
            .validate_duplicate_attributes(true);
        let expected = count_document_with_config(str::from_utf8(input).unwrap(), config)
            .map(Counts::from)
            .unwrap();
        let actual = count_xml_stream_reader(&mut &input[..], config).unwrap();

        assert_eq!(actual.elements, expected.elements);
        assert_eq!(actual.attributes, expected.attributes);
        assert_eq!(actual.nodes, expected.nodes);
    }

    #[test]
    fn stream_count_rejects_mismatched_end_tag() {
        let error =
            count_xml_stream_reader(&mut &b"<a><b></a>"[..], ParserConfig::default()).unwrap_err();

        assert!(error.contains("mismatched end tag"));
    }

    fn summary_with_iterations(iterations: usize, warmup: usize) -> BenchSummary {
        BenchSummary {
            iterations,
            warmup,
            bytes: 1,
            parse: Duration::from_millis(1),
            ..BenchSummary::default()
        }
    }
}
