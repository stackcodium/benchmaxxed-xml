use std::{env, fs, hint::black_box, io, time::Instant};

use xml_parser::XmlDom;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let elements = match args.next().as_deref() {
        None => 20_000,
        Some("-h" | "--help") => {
            println!("usage: bench_mutation_lifecycle [--elements N]");
            return Ok(());
        }
        Some("--elements") => args
            .next()
            .ok_or("missing element count")?
            .parse::<usize>()?,
        Some(_) => return Err("usage: bench_mutation_lifecycle [--elements N]".into()),
    };
    if elements == 0 || args.next().is_some() {
        return Err("usage: bench_mutation_lifecycle [--elements N]".into());
    }

    let mut source = String::with_capacity(elements * 24);
    source.push_str("<r>");
    for index in 0..elements {
        source.push_str("<n value='");
        source.push_str(&index.to_string());
        source.push_str("'>old</n>");
    }
    source.push_str("</r>");

    let parse_start = Instant::now();
    let document = XmlDom::parse(source)?;
    let parse_ms = millis(parse_start.elapsed());
    let root = document.root();
    let children: Vec<_> = root.children_named("n")?.collect();

    let rewrite_start = Instant::now();
    for (index, child) in children.iter().enumerate() {
        child.set_attribute_typed("value", index + 1)?;
        child.set_text_typed(index + 1)?;
    }
    let rewrite_ms = millis(rewrite_start.elapsed());
    let rss_after_rewrite_kb = current_rss_kb();

    let walk_start = Instant::now();
    let stats = black_box(document.tree_stats());
    let walk_ms = millis(walk_start.elapsed());

    let serialize_start = Instant::now();
    let mut output = CountingWriter::default();
    document.write_xml(&mut output)?;
    let serialize_ms = millis(serialize_start.elapsed());
    let rss_after_serialize_kb = current_rss_kb();

    let root = document.root();
    let clear_start = Instant::now();
    root.clear()?;
    let clear_ms = millis(clear_start.elapsed());
    let after_clear = document.tree_stats();
    let rss_after_clear_kb = current_rss_kb();

    println!(
        "elements\tparse_ms\trewrite_ms\twalk_ms\tserialize_ms\tclear_ms\trss_after_rewrite_kb\trss_after_serialize_kb\trss_after_clear_kb\tvisible_elements\tvisible_attributes\tvisible_nodes\tafter_clear_nodes\toutput_bytes"
    );
    println!(
        "{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        elements,
        parse_ms,
        rewrite_ms,
        walk_ms,
        serialize_ms,
        clear_ms,
        rss_after_rewrite_kb,
        rss_after_serialize_kb,
        rss_after_clear_kb,
        stats.elements,
        stats.attributes,
        stats.nodes,
        after_clear.nodes,
        output.bytes,
    );
    Ok(())
}

#[derive(Default)]
struct CountingWriter {
    bytes: usize,
}

impl io::Write for CountingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn millis(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
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
