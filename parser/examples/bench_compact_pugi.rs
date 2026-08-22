use std::{
    env, fs,
    hint::black_box,
    process::ExitCode,
    time::{Duration, Instant},
};

use xml_parser::{
    ParserConfig, XmlCompactDocument, XmlDom, XmlNodeKind, XmlTreeStats,
    parse_compact_document_with_config,
};

const USAGE: &str = "usage: bench_compact_pugi --engine compact|dom --workload parse|walk|walk-indexed|parse-walk|retain-10pct|clone|serialize [--runs N] [--warmup N] [--iterations N] [--min-duration-ms N] [--trusted-xml-chars] [--trusted-references] [--trusted-attributes] XML_FILE";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    if matches!(env::args().nth(1).as_deref(), Some("-h" | "--help")) {
        println!("{USAGE}");
        return Ok(());
    }
    let options = Options::parse()?;
    let source = fs::read_to_string(&options.path).map_err(|error| error.to_string())?;
    let retained = ParsedDocument::parse(options.engine, source.clone(), options.config())?;

    for _ in 0..options.warmup {
        execute_batch(
            options.workload,
            options.engine,
            options.config(),
            &source,
            &retained,
            1,
        )?;
    }

    let iterations = calibrate(&options, &source, &retained)?;
    let mut best: Option<(Duration, Observation)> = None;
    for _ in 0..options.runs {
        let candidate = execute_batch(
            options.workload,
            options.engine,
            options.config(),
            &source,
            &retained,
            iterations,
        )?;
        if best
            .as_ref()
            .is_none_or(|(duration, _)| candidate.0 < *duration)
        {
            best = Some(candidate);
        }
    }
    let (duration, observation) = best.ok_or_else(|| "benchmark did not run".to_owned())?;
    black_box(&retained);
    let memory = read_process_memory();

    println!(
        "file\tengine\tworkload\titer\twarmup\tbytes\telapsed_ms\trss_kb\thwm_kb\telements\tattributes\tnodes\tretained\toutput_bytes\tchecksum"
    );
    println!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{:.6}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        options.path,
        options.engine.as_str(),
        options.workload.as_str(),
        iterations,
        options.warmup,
        source.len(),
        duration.as_secs_f64() * 1_000.0 / iterations as f64,
        memory.rss_kb,
        memory.hwm_kb,
        observation.stats.elements,
        observation.stats.attributes,
        observation.stats.nodes,
        observation.retained,
        observation.output_bytes,
        observation.checksum,
    );
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Engine {
    Compact,
    Dom,
}

impl Engine {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "compact" => Ok(Self::Compact),
            "dom" => Ok(Self::Dom),
            _ => Err(USAGE.to_owned()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "benchmaxxed-compact-full",
            Self::Dom => "benchmaxxed-dom-full",
        }
    }
}

#[derive(Clone, Copy)]
enum Workload {
    Parse,
    Walk,
    WalkIndexed,
    ParseWalk,
    RetainTenPercent,
    Clone,
    Serialize,
}

impl Workload {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "parse" => Ok(Self::Parse),
            "walk" => Ok(Self::Walk),
            "walk-indexed" => Ok(Self::WalkIndexed),
            "parse-walk" => Ok(Self::ParseWalk),
            "retain-10pct" => Ok(Self::RetainTenPercent),
            "clone" => Ok(Self::Clone),
            "serialize" => Ok(Self::Serialize),
            _ => Err(USAGE.to_owned()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Walk => "walk",
            Self::WalkIndexed => "walk-indexed",
            Self::ParseWalk => "parse-walk",
            Self::RetainTenPercent => "retain-10pct",
            Self::Clone => "clone",
            Self::Serialize => "serialize",
        }
    }
}

struct Options {
    engine: Engine,
    workload: Workload,
    runs: usize,
    warmup: usize,
    iterations: usize,
    min_duration: Duration,
    trusted_xml_chars: bool,
    trusted_references: bool,
    trusted_attributes: bool,
    path: String,
}

impl Options {
    fn parse() -> Result<Self, String> {
        let mut engine = None;
        let mut workload = None;
        let mut runs = 7;
        let mut warmup = 1;
        let mut iterations = 1;
        let mut min_duration_ms = 500;
        let mut trusted_xml_chars = false;
        let mut trusted_references = false;
        let mut trusted_attributes = false;
        let mut path = None;
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--engine" => {
                    engine = Some(Engine::parse(
                        &args.next().ok_or_else(|| USAGE.to_owned())?,
                    )?);
                }
                "--workload" => {
                    workload = Some(Workload::parse(
                        &args.next().ok_or_else(|| USAGE.to_owned())?,
                    )?);
                }
                "--runs" => runs = positive(&mut args)?,
                "--warmup" => warmup = nonnegative(&mut args)?,
                "--iterations" => iterations = positive(&mut args)?,
                "--min-duration-ms" => min_duration_ms = positive(&mut args)?,
                "--trusted-xml-chars" => trusted_xml_chars = true,
                "--trusted-references" => trusted_references = true,
                "--trusted-attributes" => trusted_attributes = true,
                _ if arg.starts_with('-') || path.is_some() => return Err(USAGE.to_owned()),
                _ => path = Some(arg),
            }
        }
        Ok(Self {
            engine: engine.ok_or_else(|| USAGE.to_owned())?,
            workload: workload.ok_or_else(|| USAGE.to_owned())?,
            runs,
            warmup,
            iterations,
            min_duration: Duration::from_millis(min_duration_ms as u64),
            trusted_xml_chars,
            trusted_references,
            trusted_attributes,
            path: path.ok_or_else(|| USAGE.to_owned())?,
        })
    }

    fn config(&self) -> ParserConfig {
        ParserConfig::preserve_all()
            .validate_characters(!self.trusted_xml_chars)
            .validate_references(!self.trusted_references)
            .validate_duplicate_attributes(!self.trusted_attributes)
    }
}

fn positive(args: &mut impl Iterator<Item = String>) -> Result<usize, String> {
    let value = nonnegative(args)?;
    (value > 0).then_some(value).ok_or_else(|| USAGE.to_owned())
}

fn nonnegative(args: &mut impl Iterator<Item = String>) -> Result<usize, String> {
    args.next()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| USAGE.to_owned())
}

// Keep the compact value inline: boxing it would add an allocation to the parse workload that the
// public `XmlCompactDocument` API does not require.
#[allow(clippy::large_enum_variant)]
enum ParsedDocument {
    Compact(XmlCompactDocument),
    Dom(XmlDom),
}

impl ParsedDocument {
    fn parse(engine: Engine, source: String, config: ParserConfig) -> Result<Self, String> {
        match engine {
            Engine::Compact => parse_compact_document_with_config(source, config)
                .map(Self::Compact)
                .map_err(|error| error.to_string()),
            Engine::Dom => XmlDom::parse_with_config(source, config)
                .map(Self::Dom)
                .map_err(|error| error.to_string()),
        }
    }

    fn stats(&self) -> XmlTreeStats {
        match self {
            Self::Compact(document) => {
                let mut stats = document.tree_stats();
                stats.nodes += usize::from(document.declaration().is_some())
                    + usize::from(document.doctype().is_some())
                    + document.misc_before_root().len()
                    + document.misc_after_root().len();
                stats
            }
            Self::Dom(document) => document.document_stats(),
        }
    }

    fn walk(&self) -> Result<Observation, String> {
        let mut observation = Observation {
            stats: self.stats(),
            ..Observation::default()
        };
        match self {
            Self::Compact(document) => {
                let source = document.raw_source();
                for node in document.nodes() {
                    match node.kind() {
                        XmlNodeKind::Element => {
                            observation.checksum = observation
                                .checksum
                                .wrapping_add(node.name_with_source(source).map_or(0, str::len));
                            for index in node.attribute_range() {
                                observation.checksum = observation
                                    .checksum
                                    .wrapping_add(
                                        document.attribute_name(index).map_or(0, str::len),
                                    )
                                    .wrapping_add(
                                        document.attribute_value(index).map_or(0, str::len),
                                    );
                            }
                        }
                        XmlNodeKind::Text | XmlNodeKind::Comment | XmlNodeKind::Cdata => {
                            observation.checksum = observation
                                .checksum
                                .wrapping_add(node.value_with_source(source).map_or(0, str::len));
                        }
                        XmlNodeKind::ProcessingInstruction => {
                            observation.checksum = observation
                                .checksum
                                .wrapping_add(node.name_with_source(source).map_or(0, str::len))
                                .wrapping_add(node.value_with_source(source).map_or(0, str::len));
                        }
                    }
                }
            }
            Self::Dom(document) => {
                document
                    .root()
                    .scan(|node| {
                        if let Some(name) = node.name() {
                            observation.checksum = observation.checksum.wrapping_add(name.len());
                        }
                        if node.kind() == XmlNodeKind::Element {
                            for attribute in node.attributes() {
                                let attribute = attribute?;
                                observation.checksum = observation
                                    .checksum
                                    .wrapping_add(attribute.name().len())
                                    .wrapping_add(attribute.value().len());
                            }
                        } else if let Some(value) = node.value()? {
                            observation.checksum = observation.checksum.wrapping_add(value.len());
                        }
                        Ok(())
                    })
                    .map_err(|error| error.to_string())?;
            }
        }
        black_box(observation.checksum);
        Ok(observation)
    }

    fn walk_indexed(&self) -> Result<Observation, String> {
        let Self::Compact(document) = self else {
            return self.walk();
        };
        let mut observation = Observation {
            stats: self.stats(),
            ..Observation::default()
        };
        for id in document.node_ids() {
            let node = document.node(id).expect("valid compact node id");
            match node.kind() {
                XmlNodeKind::Element => {
                    observation.checksum = observation
                        .checksum
                        .wrapping_add(document.node_name(id).map_or(0, str::len));
                    for index in node.attribute_range() {
                        observation.checksum = observation
                            .checksum
                            .wrapping_add(document.attribute_name(index).map_or(0, str::len))
                            .wrapping_add(document.attribute_value(index).map_or(0, str::len));
                    }
                }
                XmlNodeKind::Text | XmlNodeKind::Comment | XmlNodeKind::Cdata => {
                    observation.checksum = observation
                        .checksum
                        .wrapping_add(document.node_value(id).map_or(0, str::len));
                }
                XmlNodeKind::ProcessingInstruction => {
                    observation.checksum = observation
                        .checksum
                        .wrapping_add(document.node_name(id).map_or(0, str::len))
                        .wrapping_add(document.node_value(id).map_or(0, str::len));
                }
            }
        }
        black_box(observation.checksum);
        Ok(observation)
    }

    fn retain_ten_percent(&self) -> Result<Observation, String> {
        let mut observation = Observation {
            stats: self.stats(),
            ..Observation::default()
        };
        match self {
            Self::Compact(document) => {
                let retained: Vec<_> = document
                    .node_ids()
                    .filter(|id| {
                        document
                            .node(*id)
                            .is_some_and(|node| node.kind() == XmlNodeKind::Element)
                    })
                    .enumerate()
                    .filter_map(|(index, id)| (index % 10 == 0).then_some(id))
                    .collect();
                observation.retained = retained.len();
                for id in &retained {
                    observation.checksum = observation
                        .checksum
                        .wrapping_add(document.node_name(*id).map_or(0, str::len));
                }
                black_box(retained);
            }
            Self::Dom(document) => {
                let walked = document
                    .walk_elements()
                    .map_err(|error| error.to_string())?;
                let mut retained = Vec::new();
                for (element_index, node) in walked.enumerate() {
                    if element_index % 10 == 0 {
                        retained.push(node);
                    }
                }
                observation.retained = retained.len();
                for node in &retained {
                    observation.checksum = observation.checksum.wrapping_add(
                        node.name_len()
                            .map_err(|error| error.to_string())?
                            .unwrap_or(0),
                    );
                }
                black_box(retained);
            }
        }
        black_box(observation.checksum);
        Ok(observation)
    }

    fn deep_clone_observed(&self) -> Observation {
        match self {
            Self::Compact(document) => {
                let cloned = document.clone();
                let observation = Observation {
                    checksum: cloned.input().len(),
                    ..Observation::default()
                };
                black_box(cloned);
                observation
            }
            Self::Dom(document) => {
                let cloned = document.deep_clone();
                let observation = Observation::default();
                black_box(cloned);
                observation
            }
        }
    }

    fn serialize_observed(&self) -> Result<Observation, String> {
        let output = match self {
            Self::Compact(document) => document
                .to_xml_string()
                .map_err(|error| error.to_string())?,
            Self::Dom(document) => document
                .to_xml_string()
                .map_err(|error| error.to_string())?,
        };
        let observation = Observation {
            stats: self.stats(),
            output_bytes: output.len(),
            checksum: output
                .as_bytes()
                .iter()
                .fold(0usize, |sum, byte| sum.wrapping_add(*byte as usize)),
            ..Observation::default()
        };
        black_box(output);
        Ok(observation)
    }
}

#[derive(Clone, Copy, Default)]
struct Observation {
    stats: XmlTreeStats,
    retained: usize,
    output_bytes: usize,
    checksum: usize,
}

fn execute_batch(
    workload: Workload,
    engine: Engine,
    config: ParserConfig,
    source: &str,
    retained: &ParsedDocument,
    iterations: usize,
) -> Result<(Duration, Observation), String> {
    if matches!(workload, Workload::Parse) {
        let start = Instant::now();
        match engine {
            Engine::Compact => {
                for _ in 0..iterations {
                    let document = parse_compact_document_with_config(source.to_owned(), config)
                        .map_err(|error| error.to_string())?;
                    black_box(document);
                }
            }
            Engine::Dom => {
                for _ in 0..iterations {
                    let document = XmlDom::parse_with_config(source.to_owned(), config)
                        .map_err(|error| error.to_string())?;
                    black_box(document);
                }
            }
        }
        return Ok((
            start.elapsed(),
            Observation {
                stats: retained.stats(),
                ..Observation::default()
            },
        ));
    }
    let start = Instant::now();
    let mut observation = Observation::default();
    for _ in 0..iterations {
        observation = match workload {
            Workload::Parse => {
                let document = ParsedDocument::parse(engine, source.to_owned(), config)?;
                black_box(document);
                Observation::default()
            }
            Workload::Walk => retained.walk()?,
            Workload::WalkIndexed => retained.walk_indexed()?,
            Workload::ParseWalk => {
                let document = ParsedDocument::parse(engine, source.to_owned(), config)?;
                let observed = document.walk()?;
                black_box(document);
                observed
            }
            Workload::RetainTenPercent => retained.retain_ten_percent()?,
            Workload::Clone => retained.deep_clone_observed(),
            Workload::Serialize => retained.serialize_observed()?,
        };
    }
    let elapsed = start.elapsed();
    observation.stats = retained.stats();
    Ok((elapsed, observation))
}

fn calibrate(options: &Options, source: &str, retained: &ParsedDocument) -> Result<usize, String> {
    let mut iterations = options.iterations;
    loop {
        let (elapsed, _) = execute_batch(
            options.workload,
            options.engine,
            options.config(),
            source,
            retained,
            iterations,
        )?;
        if elapsed >= options.min_duration {
            return Ok(iterations);
        }
        let scale = if elapsed.is_zero() {
            10
        } else {
            (options.min_duration.as_secs_f64() / elapsed.as_secs_f64()).ceil() as usize
        };
        iterations = iterations.saturating_mul(scale.max(2));
        if iterations == usize::MAX {
            return Err("benchmark iteration calibration overflowed".to_owned());
        }
    }
}

#[derive(Default)]
struct ProcessMemory {
    rss_kb: usize,
    hwm_kb: usize,
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
