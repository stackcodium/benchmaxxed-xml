use std::{env, fs, hint::black_box, time::Instant};

use xml_parser::{
    parse_compact_document_with_config, ParserConfig, XmlCompactDocument, XmlDom, XmlNodeKind,
};

const USAGE: &str = "usage: bench_compact_decision MODE INPUT ITERATIONS\n\
    MODE: compact-parse | dom-parse | compact-stats | dom-stats | compact-walk | dom-walk |\n\
          compact-retain | dom-retain | compact-serialize | dom-serialize | compact-clone |\n\
          dom-clone | dom-xpath | compact-to-dom-mutate | dom-mutate\n\
    INPUT: file path or gen:deep:N | gen:wide:N | gen:attrs:N | gen:text:N | gen:mixed:N | gen:entity:N";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let mode = args.next().ok_or(USAGE)?;
    if matches!(mode.as_str(), "-h" | "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let input_name = args.next().ok_or(USAGE)?;
    let iterations = args
        .next()
        .ok_or(USAGE)?
        .parse::<usize>()
        .map_err(|_| USAGE)?;
    if iterations == 0 || args.next().is_some() {
        return Err(USAGE.into());
    }

    let source = load_source(&input_name)?;
    let rss_before_kb = current_rss_kb();
    let start = Instant::now();
    let (checksum, retained) = run(&mode, &source, iterations)?;
    let elapsed = start.elapsed();
    let rss_after_kb = current_rss_kb();
    black_box(retained.held_items());

    println!(
        "mode\tinput\tinput_bytes\titerations\telapsed_ms\tns_per_iteration\trss_before_kb\trss_after_kb\tchecksum"
    );
    println!(
        "{}\t{}\t{}\t{}\t{:.3}\t{:.1}\t{}\t{}\t{}",
        mode,
        input_name,
        source.len(),
        iterations,
        elapsed.as_secs_f64() * 1_000.0,
        elapsed.as_secs_f64() * 1_000_000_000.0 / iterations as f64,
        rss_before_kb,
        rss_after_kb,
        checksum / iterations,
    );
    Ok(())
}

enum Retained {
    Compact(XmlCompactDocument),
    Dom(XmlDom),
    CompactIds(XmlCompactDocument, Vec<xml_parser::XmlViewNodeId>),
    DomNodes(XmlDom, Vec<xml_parser::XmlDomNode>),
}

impl Retained {
    fn held_items(&self) -> usize {
        match self {
            Self::Compact(document) => document.nodes().len(),
            Self::Dom(document) => std::mem::size_of_val(document),
            Self::CompactIds(document, ids) => document.nodes().len().wrapping_add(ids.len()),
            Self::DomNodes(document, nodes) => {
                std::mem::size_of_val(document).wrapping_add(nodes.len())
            }
        }
    }
}

fn run(
    mode: &str,
    source: &str,
    iterations: usize,
) -> Result<(usize, Retained), Box<dyn std::error::Error>> {
    let config = ParserConfig::default();
    match mode {
        "compact-parse" => {
            let mut checksum = 0usize;
            let mut retained = None;
            for _ in 0..iterations {
                let document = parse_compact_document_with_config(source.to_owned(), config)?;
                black_box(&document);
                checksum = checksum.wrapping_add(source.len());
                retained = Some(document);
            }
            Ok((
                checksum,
                Retained::Compact(retained.expect("positive iterations")),
            ))
        }
        "dom-parse" => {
            let mut checksum = 0usize;
            let mut retained = None;
            for _ in 0..iterations {
                let document = XmlDom::parse_with_config(source.to_owned(), config)?;
                black_box(&document);
                checksum = checksum.wrapping_add(source.len());
                retained = Some(document);
            }
            Ok((
                checksum,
                Retained::Dom(retained.expect("positive iterations")),
            ))
        }
        "compact-stats" => {
            let document = parse_compact_document_with_config(source.to_owned(), config)?;
            let mut checksum = 0usize;
            for _ in 0..iterations {
                let stats = black_box(document.tree_stats());
                checksum = checksum.wrapping_add(stats.elements ^ stats.attributes ^ stats.nodes);
            }
            Ok((checksum, Retained::Compact(document)))
        }
        "dom-stats" => {
            let document = XmlDom::parse_with_config(source.to_owned(), config)?;
            let mut checksum = 0usize;
            for _ in 0..iterations {
                let stats = black_box(document.tree_stats());
                checksum = checksum.wrapping_add(stats.elements ^ stats.attributes ^ stats.nodes);
            }
            Ok((checksum, Retained::Dom(document)))
        }
        "compact-walk" => {
            let document = parse_compact_document_with_config(source.to_owned(), config)?;
            let mut checksum = 0usize;
            for _ in 0..iterations {
                checksum = checksum.wrapping_add(walk_compact(&document));
            }
            Ok((checksum, Retained::Compact(document)))
        }
        "dom-walk" => {
            let document = XmlDom::parse_with_config(source.to_owned(), config)?;
            let mut checksum = 0usize;
            for _ in 0..iterations {
                checksum = checksum.wrapping_add(walk_dom(&document)?);
            }
            Ok((checksum, Retained::Dom(document)))
        }
        "compact-retain" => {
            let document = parse_compact_document_with_config(source.to_owned(), config)?;
            let mut selected = Vec::new();
            let mut elements = 0usize;
            for id in document.node_ids() {
                if document
                    .node(id)
                    .is_some_and(|node| node.kind() == XmlNodeKind::Element)
                {
                    if elements % 10 == 0 {
                        selected.push(id);
                    }
                    elements += 1;
                }
            }
            let checksum = selected.len();
            Ok((checksum, Retained::CompactIds(document, selected)))
        }
        "dom-retain" => {
            let document = XmlDom::parse_with_config(source.to_owned(), config)?;
            let selected = document
                .root()
                .walk()?
                .filter(|node| node.kind() == Ok(XmlNodeKind::Element))
                .enumerate()
                .filter_map(|(index, node)| (index % 10 == 0).then_some(node))
                .collect::<Vec<_>>();
            let checksum = selected.len();
            Ok((checksum, Retained::DomNodes(document, selected)))
        }
        "compact-serialize" => {
            let document = parse_compact_document_with_config(source.to_owned(), config)?;
            let mut checksum = 0usize;
            for _ in 0..iterations {
                checksum = checksum.wrapping_add(black_box(document.to_xml_string()?).len());
            }
            Ok((checksum, Retained::Compact(document)))
        }
        "dom-serialize" => {
            let document = XmlDom::parse_with_config(source.to_owned(), config)?;
            let mut checksum = 0usize;
            for _ in 0..iterations {
                checksum = checksum.wrapping_add(black_box(document.to_xml_string()?).len());
            }
            Ok((checksum, Retained::Dom(document)))
        }
        "compact-clone" => {
            let document = parse_compact_document_with_config(source.to_owned(), config)?;
            let mut checksum = 0usize;
            for _ in 0..iterations {
                let clone = black_box(document.clone());
                black_box(clone);
                checksum = checksum.wrapping_add(source.len());
            }
            Ok((checksum, Retained::Compact(document)))
        }
        "dom-clone" => {
            let document = XmlDom::parse_with_config(source.to_owned(), config)?;
            let mut checksum = 0usize;
            for _ in 0..iterations {
                let clone = black_box(document.clone());
                black_box(clone);
                checksum = checksum.wrapping_add(source.len());
            }
            Ok((checksum, Retained::Dom(document)))
        }
        "dom-xpath" => {
            let document = XmlDom::parse_with_config(source.to_owned(), config)?;
            let mut checksum = 0usize;
            for _ in 0..iterations {
                checksum = checksum.wrapping_add(document.select_elements("//*")?.len());
            }
            Ok((checksum, Retained::Dom(document)))
        }
        "compact-to-dom-mutate" => {
            let mut checksum = 0usize;
            let mut retained = None;
            for index in 0..iterations {
                let compact = parse_compact_document_with_config(source.to_owned(), config)?;
                let document = XmlDom::from_compact(compact);
                document
                    .root()
                    .set_attribute_typed("decision-probe", index)?;
                checksum = checksum.wrapping_add(source.len());
                retained = Some(document);
            }
            Ok((
                checksum,
                Retained::Dom(retained.expect("positive iterations")),
            ))
        }
        "dom-mutate" => {
            let mut checksum = 0usize;
            let mut retained = None;
            for index in 0..iterations {
                let document = XmlDom::parse_with_config(source.to_owned(), config)?;
                document
                    .root()
                    .set_attribute_typed("decision-probe", index)?;
                checksum = checksum.wrapping_add(source.len());
                retained = Some(document);
            }
            Ok((
                checksum,
                Retained::Dom(retained.expect("positive iterations")),
            ))
        }
        _ => Err(USAGE.into()),
    }
}

fn walk_compact(document: &XmlCompactDocument) -> usize {
    let mut probe = 0usize;
    let mut elements = 0usize;
    let mut attributes = 0usize;
    let mut nodes = 0usize;
    for id in document.node_ids() {
        let node = document.node(id).expect("valid compact node id");
        nodes += 1;
        elements += usize::from(node.kind() == XmlNodeKind::Element);
        probe = probe.wrapping_add(node.kind() as usize + 1);
        probe = probe.wrapping_add(document.node_name(id).map_or(0, str::len));
        probe = probe.wrapping_add(document.node_value(id).map_or(0, str::len));
        if node.kind() == XmlNodeKind::Element {
            for index in node.attribute_range() {
                attributes += 1;
                probe = probe
                    .wrapping_add(document.attribute_name(index).map_or(0, str::len))
                    .wrapping_add(document.attribute_value(index).map_or(0, str::len));
            }
        }
    }
    black_box(probe);
    structural_checksum(elements, attributes, nodes)
}

fn walk_dom(document: &XmlDom) -> Result<usize, xml_parser::XmlDomError> {
    let mut probe = 0usize;
    let mut elements = 0usize;
    let mut attributes = 0usize;
    let mut nodes = 0usize;
    for node in document.root().walk()? {
        let kind = node.kind()?;
        nodes += 1;
        elements += usize::from(kind == XmlNodeKind::Element);
        probe = probe.wrapping_add(kind as usize + 1);
        probe = probe.wrapping_add(node.name()?.as_deref().map_or(0, str::len));
        probe = probe.wrapping_add(node.value()?.as_deref().map_or(0, str::len));
        for (name, value) in node.attributes().unwrap_or_else(|_| Vec::new().into_iter()) {
            attributes += 1;
            probe = probe.wrapping_add(name.len()).wrapping_add(value.len());
        }
    }
    black_box(probe);
    Ok(structural_checksum(elements, attributes, nodes))
}

fn structural_checksum(elements: usize, attributes: usize, nodes: usize) -> usize {
    elements
        .wrapping_mul(1_000_003)
        .wrapping_add(attributes.wrapping_mul(10_007))
        .wrapping_add(nodes)
}

fn load_source(input: &str) -> Result<String, Box<dyn std::error::Error>> {
    let Some(spec) = input.strip_prefix("gen:") else {
        return Ok(fs::read_to_string(input)?);
    };
    let (shape, count) = spec.rsplit_once(':').ok_or(USAGE)?;
    let count = count.parse::<usize>().map_err(|_| USAGE)?;
    Ok(match shape {
        "deep" => format!("{}x{}", "<n>".repeat(count), "</n>".repeat(count)),
        "wide" => {
            let mut source = String::with_capacity(count.saturating_mul(19) + 7);
            source.push_str("<r>");
            for index in 0..count {
                source.push_str("<n a='");
                source.push_str(&(index % 10).to_string());
                source.push_str("'>x</n>");
            }
            source.push_str("</r>");
            source
        }
        "attrs" => {
            let mut source = String::with_capacity(count.saturating_mul(14) + 5);
            source.push_str("<r");
            for index in 0..count {
                source.push_str(" a");
                source.push_str(&index.to_string());
                source.push_str("='value'");
            }
            source.push_str("/>");
            source
        }
        "text" => format!("<r>{}</r>", "x".repeat(count)),
        "mixed" => {
            let mut source = String::with_capacity(count.saturating_mul(55) + 7);
            source.push_str("<r>");
            for index in 0..count {
                source.push_str("<!--c--><?p d?><n a='");
                source.push_str(&(index % 10).to_string());
                source.push_str("'><![CDATA[x]]>text</n>");
            }
            source.push_str("</r>");
            source
        }
        "entity" => format!(
            "<!DOCTYPE r [<!ENTITY x 'expanded'>]><r>{}</r>",
            "&x;".repeat(count)
        ),
        _ => return Err(USAGE.into()),
    })
}

fn current_rss_kb() -> usize {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmRSS:")?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
        })
        .unwrap_or(0)
}
