use std::{
    env, fs,
    hint::black_box,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

use xml_parser::{XmlDom, XmlDomError, XmlNode, XmlTreeStats};

const USAGE: &str = "usage: bench_mutation --workload parse-walk|xpath-query|sparse-edit|repeated-mutation|structural-edit|retained-reorder|document-build \
    [--edits N] [--runs N] [--iterations N] [--warmup N] [--min-duration-ms N] \
    [--emit FILE] XML_FILE\n       bench_mutation --verify-equivalent RUST_XML PUGIXML_XML";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if matches!(arguments.first().map(String::as_str), Some("-h" | "--help")) {
        println!("{USAGE}");
        return Ok(());
    }
    if arguments.first().map(String::as_str) == Some("--verify-equivalent") {
        if arguments.len() != 3 {
            return Err(USAGE.into());
        }
        return verify_equivalent(&arguments[1], &arguments[2]);
    }

    let options = Options::parse(arguments)?;
    let input_bytes: usize = fs::metadata(&options.path)?.len().try_into()?;
    let iterations = calibrate(&options)?;
    let mut best: Option<Sample> = None;
    for _ in 0..options.runs {
        let sample = run_sample(&options, iterations)?;
        if best
            .as_ref()
            .is_none_or(|current| sample.end_to_end_ms() < current.end_to_end_ms())
        {
            best = Some(sample);
        }
    }
    let best = best.expect("runs is nonzero");

    println!(
        "file\tparser\tworkload\tedits\titer\twarmup\tbytes\tparse_ms\tfirst_edit_ms\tmutate_ms\twalk_ms\tserialize_ms\tend_to_end_ms\tmib_s\trss_kb\thwm_kb\telements\tattributes\tnodes\tselected\toutput_bytes\toutput_checksum"
    );
    println!(
        "{}\tbenchmaxxed-xml\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.1}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        options.path.display(),
        options.workload.label(),
        options.edits,
        iterations,
        options.warmup,
        input_bytes,
        best.parse_ms,
        best.first_edit_ms,
        best.mutate_ms,
        best.walk_ms,
        best.serialize_ms,
        best.end_to_end_ms(),
        mib_per_second(input_bytes, best.end_to_end_ms()),
        current_rss_kb(),
        high_water_rss_kb(),
        best.stats.elements,
        best.stats.attributes,
        best.stats.nodes,
        best.selected,
        best.output_bytes,
        best.output_checksum,
    );

    if let Some(path) = &options.emit {
        let document = prepare_document(
            fs::read_to_string(&options.path)?,
            options.workload,
            options.edits,
        )?;
        let mut output = fs::File::create(path)?;
        document.write_xml(&mut output)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Workload {
    ParseWalk,
    XPathQuery,
    SparseEdit,
    RepeatedMutation,
    StructuralEdit,
    RetainedReorder,
    DocumentBuild,
}

impl Workload {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "parse-walk" => Some(Self::ParseWalk),
            "xpath-query" => Some(Self::XPathQuery),
            "sparse-edit" => Some(Self::SparseEdit),
            "repeated-mutation" => Some(Self::RepeatedMutation),
            "structural-edit" => Some(Self::StructuralEdit),
            "retained-reorder" => Some(Self::RetainedReorder),
            "document-build" => Some(Self::DocumentBuild),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::ParseWalk => "parse-walk",
            Self::XPathQuery => "xpath-query",
            Self::SparseEdit => "sparse-edit",
            Self::RepeatedMutation => "repeated-mutation",
            Self::StructuralEdit => "structural-edit",
            Self::RetainedReorder => "retained-reorder",
            Self::DocumentBuild => "document-build",
        }
    }

    fn serializes(self) -> bool {
        matches!(
            self,
            Self::RepeatedMutation
                | Self::StructuralEdit
                | Self::RetainedReorder
                | Self::DocumentBuild
        )
    }
}

struct Options {
    workload: Workload,
    edits: usize,
    runs: usize,
    iterations: usize,
    warmup: usize,
    min_duration: Duration,
    emit: Option<PathBuf>,
    path: PathBuf,
}

impl Options {
    fn parse(arguments: Vec<String>) -> Result<Self, Box<dyn std::error::Error>> {
        let mut workload = None;
        let mut edits = 100;
        let mut runs = 7;
        let mut iterations = 1;
        let mut warmup = 1;
        let mut min_duration = Duration::from_millis(500);
        let mut emit = None;
        let mut path = None;
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--workload" => {
                    workload = Some(Workload::parse(&arguments.next().ok_or(USAGE)?).ok_or(USAGE)?);
                }
                "--edits" => edits = parse_positive(arguments.next())?,
                "--runs" => runs = parse_positive(arguments.next())?,
                "--iterations" => iterations = parse_positive(arguments.next())?,
                "--warmup" => warmup = arguments.next().ok_or(USAGE)?.parse()?,
                "--min-duration-ms" => {
                    let milliseconds: u64 = parse_positive(arguments.next())?.try_into()?;
                    min_duration = Duration::from_millis(milliseconds);
                }
                "--emit" => emit = Some(arguments.next().ok_or(USAGE)?.into()),
                value if !value.starts_with('-') && path.is_none() => path = Some(value.into()),
                _ => return Err(USAGE.into()),
            }
        }
        Ok(Self {
            workload: workload.ok_or(USAGE)?,
            edits,
            runs,
            iterations,
            warmup,
            min_duration,
            emit,
            path: path.ok_or(USAGE)?,
        })
    }
}

fn parse_positive(value: Option<String>) -> Result<usize, Box<dyn std::error::Error>> {
    let value = value.ok_or(USAGE)?.parse::<usize>()?;
    if value == 0 {
        return Err(USAGE.into());
    }
    Ok(value)
}

#[derive(Default)]
struct Sample {
    parse_ms: f64,
    first_edit_ms: f64,
    mutate_ms: f64,
    walk_ms: f64,
    serialize_ms: f64,
    stats: XmlTreeStats,
    output_bytes: usize,
    output_checksum: u64,
    selected: usize,
}

impl Sample {
    fn end_to_end_ms(&self) -> f64 {
        self.parse_ms + self.first_edit_ms + self.mutate_ms + self.walk_ms + self.serialize_ms
    }

    fn add(&mut self, other: &Self) {
        self.parse_ms += other.parse_ms;
        self.first_edit_ms += other.first_edit_ms;
        self.mutate_ms += other.mutate_ms;
        self.walk_ms += other.walk_ms;
        self.serialize_ms += other.serialize_ms;
        self.stats = other.stats;
        self.output_bytes = other.output_bytes;
        self.output_checksum = other.output_checksum;
        self.selected = other.selected;
    }

    fn divide(&mut self, divisor: usize) {
        let divisor = divisor as f64;
        self.parse_ms /= divisor;
        self.first_edit_ms /= divisor;
        self.mutate_ms /= divisor;
        self.walk_ms /= divisor;
        self.serialize_ms /= divisor;
    }
}

fn calibrate(options: &Options) -> Result<usize, Box<dyn std::error::Error>> {
    let mut iterations = options.iterations;
    loop {
        let sample = run_sample(options, iterations)?;
        let measured = sample.end_to_end_ms() * iterations as f64;
        if measured >= options.min_duration.as_secs_f64() * 1_000.0 {
            return Ok(iterations);
        }
        let scale = if measured <= 0.0 {
            10.0
        } else {
            options.min_duration.as_secs_f64() * 1_000.0 / measured
        };
        let multiplier = (scale.ceil() as usize).max(2);
        let Some(next) = iterations.checked_mul(multiplier) else {
            return Ok(iterations);
        };
        iterations = next;
    }
}

fn run_sample(options: &Options, iterations: usize) -> Result<Sample, Box<dyn std::error::Error>> {
    for _ in 0..options.warmup {
        black_box(run_once(&options.path, options.workload, options.edits)?);
    }
    let mut total = Sample::default();
    for _ in 0..iterations {
        total.add(&run_once(&options.path, options.workload, options.edits)?);
    }
    total.divide(iterations);
    Ok(total)
}

fn run_once(
    path: &PathBuf,
    workload: Workload,
    edits: usize,
) -> Result<Sample, Box<dyn std::error::Error>> {
    let source = if workload == Workload::DocumentBuild {
        String::new()
    } else {
        fs::read_to_string(path)?
    };
    let parse_start = Instant::now();
    let mut document = if workload == Workload::DocumentBuild {
        XmlDom::new("catalog")?
    } else {
        XmlDom::parse(source)?
    };
    let parse_ms = millis(parse_start.elapsed());

    let mut first_edit_ms = 0.0;
    let mut mutate_ms = 0.0;
    let mut selected = 0usize;
    match workload {
        Workload::ParseWalk => {}
        Workload::XPathQuery => {
            let start = Instant::now();
            selected = black_box(document.select_elements("//member[@name]")?.len());
            mutate_ms = millis(start.elapsed());
        }
        Workload::SparseEdit => {
            let start = Instant::now();
            sparse_edit(&document)?;
            first_edit_ms = millis(start.elapsed());
        }
        Workload::RepeatedMutation => {
            let start = Instant::now();
            mutation_batch(&document, 0)?;
            first_edit_ms = millis(start.elapsed());
            let start = Instant::now();
            for iteration in 1..edits {
                mutation_batch(&document, iteration)?;
            }
            relocate_last_element_to_front(&document)?;
            mutate_ms = millis(start.elapsed());
        }
        Workload::StructuralEdit => {
            let start = Instant::now();
            structural_edit(&document)?;
            mutate_ms = millis(start.elapsed());
        }
        Workload::RetainedReorder => {
            let start = Instant::now();
            selected = retained_handle_reorder(&document, edits)?;
            mutate_ms = millis(start.elapsed());
        }
        Workload::DocumentBuild => {
            let start = Instant::now();
            document = build_compact_document(edits)?;
            mutate_ms = millis(start.elapsed());
        }
    }

    let walk_start = Instant::now();
    let stats = black_box(document.document_stats());
    let walk_ms = millis(walk_start.elapsed());

    let mut output = CountingWriter::default();
    let serialize_ms = if workload.serializes() {
        let start = Instant::now();
        document.write_xml(&mut output)?;
        millis(start.elapsed())
    } else {
        0.0
    };
    black_box(output.checksum);

    Ok(Sample {
        parse_ms,
        first_edit_ms,
        mutate_ms,
        walk_ms,
        serialize_ms,
        stats,
        selected,
        output_bytes: output.bytes,
        output_checksum: output.checksum,
    })
}

fn prepare_document(
    source: String,
    workload: Workload,
    edits: usize,
) -> Result<XmlDom, XmlDomError> {
    let document = if workload == Workload::DocumentBuild {
        build_compact_document(edits)?
    } else {
        XmlDom::parse(source).map_err(XmlDomError::from)?
    };
    match workload {
        Workload::ParseWalk | Workload::XPathQuery => {}
        Workload::SparseEdit => sparse_edit(&document)?,
        Workload::RepeatedMutation => {
            for iteration in 0..edits {
                mutation_batch(&document, iteration)?;
            }
            relocate_last_element_to_front(&document)?;
        }
        Workload::StructuralEdit => structural_edit(&document)?,
        Workload::RetainedReorder => {
            retained_handle_reorder(&document, edits)?;
        }
        Workload::DocumentBuild => {}
    }
    Ok(document)
}

fn sparse_edit(document: &XmlDom) -> Result<(), XmlDomError> {
    document.root().set_attribute("bench-sparse", "1")
}

fn mutation_batch(document: &XmlDom, iteration: usize) -> Result<(), XmlDomError> {
    let root = document.root();
    root.set_attribute_typed("bench-iteration", iteration)?;
    let added = root.append_element("mutation-probe")?;
    added.set_attribute_typed("iteration", iteration)?;
    added.set_text("initial")?;
    added.set_text_typed(iteration)?;
    black_box(added.remove()?);
    Ok(())
}

fn relocate_last_element_to_front(document: &XmlDom) -> Result<(), XmlDomError> {
    let root = document.root();
    let children: Vec<_> = root.children()?.collect();
    if let Some(source) = children
        .iter()
        .rev()
        .find(|child| child.name().is_ok_and(|name| name.is_some()))
    {
        black_box(source.move_to(&root, 0)?);
    }
    Ok(())
}

fn structural_edit(document: &XmlDom) -> Result<(), XmlDomError> {
    let root = document.root();
    root.prepend_attribute("bench-first", "1")?;
    root.set_attribute("bench-mode", "structural")?;
    let elements: Vec<_> = root
        .children()?
        .filter(|child| child.name().is_ok_and(|name| name.is_some()))
        .take(2)
        .collect();
    if elements.len() == 2 {
        let copied = root.append_copy(&elements[0])?;
        copied.set_name("bench-copy")?;
        let source = elements[0]
            .children()?
            .rev()
            .find(|child| child.name().is_ok_and(|name| name.is_some()));
        if let Some(source) = source {
            source.move_to(&elements[1], 0)?;
        }
    }
    let root = document.root();
    root.append_node(XmlNode::Comment("bench-structural".into()))?;
    root.append_node(XmlNode::ProcessingInstruction(
        xml_parser::XmlProcessingInstruction::new("bench", "complete")?,
    ))?;
    Ok(())
}

fn retained_handle_reorder(document: &XmlDom, limit: usize) -> Result<usize, XmlDomError> {
    let root = document.root();
    let mut retained: Vec<_> = root
        .children()?
        .filter(|child| child.name().is_ok_and(|name| name.is_some()))
        .take(limit)
        .collect();
    retained.reverse();
    let retained_ids: Vec<_> = retained.iter().map(|node| node.id()).collect();
    for node in &retained {
        let end = root.children()?.count();
        node.move_to(&root, end)?;
    }
    if retained
        .iter()
        .zip(retained_ids)
        .any(|(node, id)| node.id() != id)
    {
        return Err(XmlDomError::InvalidTarget);
    }
    Ok(retained.len())
}

fn build_compact_document(elements: usize) -> Result<XmlDom, XmlDomError> {
    XmlDom::build_with_capacity("catalog", 32 + elements.saturating_mul(64), |root| {
        for index in 0..elements {
            root.element("item", |item| {
                item.attribute_display("id", index)
                    .attribute_display("enabled", index % 2 == 0)
                    .element("name", |name| {
                        name.text_display(format_args!("item-{index}"));
                    });
            });
        }
    })
    .map_err(XmlDomError::from)
}

fn verify_equivalent(
    rust_path: &str,
    pugixml_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let rust = XmlDom::load(rust_path)?;
    let pugixml = XmlDom::load(pugixml_path)?;
    let rust_stats = rust.tree_stats();
    let pugixml_stats = pugixml.tree_stats();
    if rust_stats != pugixml_stats {
        return Err(format!("structural count mismatch: {rust_path} != {pugixml_path}").into());
    }
    let rust_xml = canonical_xml_line_endings(&fs::read_to_string(rust_path)?);
    let pugixml_xml = canonical_xml_line_endings(&fs::read_to_string(pugixml_path)?);
    if rust_xml != pugixml_xml {
        return Err(format!("semantic document mismatch: {rust_path} != {pugixml_path}").into());
    }
    let checksum = canonical_checksum(&rust_xml);
    println!(
        "equivalent\t{}\t{}\t{}\t{}\t{}",
        rust_path, pugixml_path, rust_stats.elements, rust_stats.nodes, checksum,
    );
    Ok(())
}

fn canonical_xml_line_endings(xml: &str) -> String {
    let normalized = xml.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed_end = normalized.trim_end_matches([' ', '\t', '\n']);
    let Some(declaration_end) = trimmed_end
        .strip_prefix("<?xml")
        .and_then(|remainder| remainder.find("?>"))
        .map(|relative| "<?xml".len() + relative + "?>".len())
    else {
        return canonicalize_empty_elements(trimmed_end);
    };
    let content = trimmed_end[declaration_end..].trim_start_matches([' ', '\t', '\n']);
    let mut canonical = String::with_capacity(declaration_end + content.len());
    canonical.push_str(&trimmed_end[..declaration_end]);
    canonical.push_str(content);
    canonicalize_empty_elements(&canonical)
}

fn canonicalize_empty_elements(xml: &str) -> String {
    let mut canonical = String::with_capacity(xml.len());
    let mut cursor = 0usize;
    while let Some(relative) = xml[cursor..].find("></") {
        let open_end = cursor + relative;
        let Some(open_start) = xml[..open_end].rfind('<') else {
            break;
        };
        let open = &xml[open_start + 1..open_end];
        let open_name = open
            .split_ascii_whitespace()
            .next()
            .filter(|name| !name.is_empty() && !name.starts_with(['/', '!', '?']));
        let close_name_start = open_end + "></".len();
        let Some(close_name_end) = xml[close_name_start..].find('>') else {
            break;
        };
        let close_name_end = close_name_start + close_name_end;
        let close_name = &xml[close_name_start..close_name_end];
        if open_name == Some(close_name) && !open.ends_with('/') {
            canonical.push_str(&xml[cursor..open_end]);
            canonical.push_str("/>");
            cursor = close_name_end + 1;
        } else {
            canonical.push_str(&xml[cursor..=open_end]);
            cursor = open_end + 1;
        }
    }
    canonical.push_str(&xml[cursor..]);
    canonical
}

fn canonical_checksum(xml: &str) -> u64 {
    let mut checksum = 14_695_981_039_346_656_037u64;
    for byte in xml.bytes() {
        checksum ^= u64::from(byte);
        checksum = checksum.wrapping_mul(1_099_511_628_211);
    }
    checksum
}

#[derive(Default)]
struct CountingWriter {
    bytes: usize,
    checksum: u64,
}

impl Write for CountingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn mib_per_second(bytes: usize, milliseconds: f64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0) / (milliseconds / 1_000.0)
}

fn process_status_kb(key: &str) -> usize {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix(key)?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
        })
        .unwrap_or(0)
}

fn current_rss_kb() -> usize {
    process_status_kb("VmRSS:")
}

fn high_water_rss_kb() -> usize {
    process_status_kb("VmHWM:")
}
