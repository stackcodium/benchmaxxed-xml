use std::{
    env, fs,
    hint::black_box,
    path::Path,
    process::ExitCode,
    time::{Duration, Instant},
};

use quick_xml::{events::Event, reader::Reader};

const USAGE: &str = "usage: quickxml-bench [--runs N] [--iterations N] [--warmup N] [--min-duration-ms N] [--mode borrowed|buffered|trim-buffered] FILE...";

#[derive(Clone, Copy)]
enum Mode {
    Borrowed,
    Buffered,
    TrimBuffered,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Borrowed => "quickxml-borrowed",
            Self::Buffered => "quickxml-buffered",
            Self::TrimBuffered => "quickxml-trim-buffered",
        }
    }
}

struct Options {
    runs: usize,
    iterations: usize,
    warmup: usize,
    min_duration: Duration,
    mode: Mode,
    paths: Vec<String>,
}

#[derive(Clone, Copy, Default)]
struct Counts {
    elements: usize,
    attributes: usize,
    nodes: usize,
}

#[derive(Clone, Copy)]
struct Sample {
    iterations: usize,
    parse: Duration,
    counts: Counts,
}

#[derive(Default)]
struct SemanticText {
    active: bool,
    has_non_whitespace: bool,
}

impl SemanticText {
    fn push_text(&mut self, bytes: &[u8]) {
        self.active = true;
        self.has_non_whitespace |= !bytes.iter().all(u8::is_ascii_whitespace);
    }

    fn push_reference(
        &mut self,
        reference: &quick_xml::events::BytesRef<'_>,
    ) -> Result<(), String> {
        self.active = true;
        self.has_non_whitespace |= reference
            .resolve_char_ref()
            .map_err(|error| error.to_string())?
            .is_none_or(|character| !matches!(character, ' ' | '\t' | '\n' | '\r'));
        Ok(())
    }

    fn flush(&mut self, counts: &mut Counts) {
        if self.active && self.has_non_whitespace {
            counts.nodes += 1;
        }
        *self = Self::default();
    }
}

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
    let options = parse_options()?;
    println!(
        "file\tmode\titer\twarmup\tbytes\tread_ms\tparse_ms\tcount_ms\ttotal_ms\tparse_mib_s\ttotal_mib_s\trss_kb\thwm_kb\telements\tattributes\tnodes"
    );

    for path in &options.paths {
        let read_start = Instant::now();
        let input = fs::read(path).map_err(|error| format!("read {path}: {error}"))?;
        let read = read_start.elapsed();
        let iterations = calibrate(&input, &options)?;

        let mut best = None;
        for _ in 0..options.runs {
            let sample = bench_iterations(&input, &options, iterations)?;
            if best
                .as_ref()
                .is_none_or(|best: &Sample| sample.parse < best.parse)
            {
                best = Some(sample);
            }
        }

        let best = best.ok_or_else(|| "benchmark did not run".to_owned())?;
        let parse_ms = millis(best.parse);
        let count_ms = 0.0;
        let total_ms = millis(read) + parse_ms + count_ms;
        println!(
            "{}\t{}\t{}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.1}\t{:.1}\t{}\t{}\t{}\t{}\t{}",
            path,
            options.mode.label(),
            best.iterations,
            options.warmup,
            input.len(),
            millis(read),
            parse_ms,
            count_ms,
            total_ms,
            mib_s(input.len(), parse_ms),
            mib_s(input.len(), total_ms),
            current_rss_kb(),
            high_water_rss_kb(),
            best.counts.elements,
            best.counts.attributes,
            best.counts.nodes
        );
    }

    Ok(())
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        runs: 3,
        iterations: 1,
        warmup: 1,
        min_duration: Duration::from_millis(300),
        mode: Mode::Borrowed,
        paths: Vec::new(),
    };
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--runs" => options.runs = parse_next_usize(&mut args)?,
            "--iterations" => options.iterations = parse_next_usize(&mut args)?,
            "--warmup" => options.warmup = parse_next_usize(&mut args)?,
            "--min-duration-ms" => {
                options.min_duration = Duration::from_millis(parse_next_usize(&mut args)? as u64)
            }
            "--mode" => {
                let mode = args.next().ok_or_else(|| USAGE.to_owned())?;
                options.mode = match mode.as_str() {
                    "borrowed" => Mode::Borrowed,
                    "buffered" => Mode::Buffered,
                    "trim-buffered" => Mode::TrimBuffered,
                    _ => return Err(USAGE.to_owned()),
                };
            }
            _ if arg.starts_with('-') => return Err(USAGE.to_owned()),
            _ => options.paths.push(arg),
        }
    }

    if options.paths.is_empty()
        || options.runs == 0
        || options.iterations == 0
        || options.min_duration.is_zero()
    {
        return Err(USAGE.to_owned());
    }

    Ok(options)
}

fn parse_next_usize(args: &mut impl Iterator<Item = String>) -> Result<usize, String> {
    let value = args.next().ok_or_else(|| USAGE.to_owned())?;
    value.parse::<usize>().map_err(|_| USAGE.to_owned())
}

fn calibrate(input: &[u8], options: &Options) -> Result<usize, String> {
    let mut iterations = options.iterations;
    loop {
        let sample = bench_iterations(input, options, iterations)?;
        let measured = sample.parse.as_secs_f64() * iterations as f64;
        if measured >= options.min_duration.as_secs_f64() {
            return Ok(iterations);
        }

        let scale = if measured == 0.0 {
            10
        } else {
            (options.min_duration.as_secs_f64() / measured).ceil() as usize
        };
        iterations = iterations.saturating_mul(scale.max(2));
        if iterations == usize::MAX {
            return Ok(iterations);
        }
    }
}

fn bench_iterations(input: &[u8], options: &Options, iterations: usize) -> Result<Sample, String> {
    for _ in 0..options.warmup {
        black_box(parse_once(input, options.mode)?);
    }

    let start = Instant::now();
    let mut counts = Counts::default();
    for _ in 0..iterations {
        counts = parse_once(input, options.mode)?;
        black_box(counts.nodes);
    }
    let elapsed = start.elapsed() / iterations as u32;

    Ok(Sample {
        iterations,
        parse: elapsed,
        counts,
    })
}

fn parse_once(input: &[u8], mode: Mode) -> Result<Counts, String> {
    match mode {
        Mode::Borrowed => parse_borrowed(input),
        Mode::Buffered | Mode::TrimBuffered => {
            parse_buffered(input, matches!(mode, Mode::TrimBuffered))
        }
    }
}

fn parse_borrowed(input: &[u8]) -> Result<Counts, String> {
    let text = std::str::from_utf8(input).map_err(|error| error.to_string())?;
    let mut reader = Reader::from_str(text);
    let mut counts = Counts::default();
    let mut text = SemanticText::default();

    loop {
        match reader.read_event().map_err(|error| error.to_string())? {
            Event::Start(event) => {
                text.flush(&mut counts);
                counts.elements += 1;
                counts.nodes += 1;
                counts.attributes += count_attributes(event.attributes())?;
            }
            Event::Empty(event) => {
                text.flush(&mut counts);
                counts.elements += 1;
                counts.nodes += 1;
                counts.attributes += count_attributes(event.attributes())?;
            }
            Event::Text(event) => text.push_text(event.as_ref()),
            Event::CData(_) | Event::Comment(_) | Event::PI(_) | Event::DocType(_) => {
                text.flush(&mut counts);
                counts.nodes += 1;
            }
            Event::Decl(_) | Event::End(_) => text.flush(&mut counts),
            Event::Eof => {
                text.flush(&mut counts);
                break;
            }
            Event::GeneralRef(reference) => text.push_reference(&reference)?,
        }
    }

    Ok(counts)
}

fn parse_buffered(input: &[u8], trim_text: bool) -> Result<Counts, String> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(trim_text);
    let mut buf = Vec::new();
    let mut counts = Counts::default();
    let mut text = SemanticText::default();

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|error| error.to_string())?
        {
            Event::Start(event) => {
                text.flush(&mut counts);
                counts.elements += 1;
                counts.nodes += 1;
                counts.attributes += count_attributes(event.attributes())?;
            }
            Event::Empty(event) => {
                text.flush(&mut counts);
                counts.elements += 1;
                counts.nodes += 1;
                counts.attributes += count_attributes(event.attributes())?;
            }
            Event::Text(event) => text.push_text(event.as_ref()),
            Event::CData(_) | Event::Comment(_) | Event::PI(_) | Event::DocType(_) => {
                text.flush(&mut counts);
                counts.nodes += 1;
            }
            Event::Decl(_) | Event::End(_) => text.flush(&mut counts),
            Event::Eof => {
                text.flush(&mut counts);
                break;
            }
            Event::GeneralRef(reference) => text.push_reference(&reference)?,
        }
        buf.clear();
    }

    Ok(counts)
}

fn count_attributes<'a>(
    attributes: quick_xml::events::attributes::Attributes<'a>,
) -> Result<usize, String> {
    let mut count = 0;
    for attribute in attributes {
        let attribute = attribute.map_err(|error| error.to_string())?;
        black_box(attribute.key);
        black_box(attribute.value);
        count += 1;
    }
    Ok(count)
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn mib_s(bytes: usize, milliseconds: f64) -> f64 {
    if milliseconds <= 0.0 {
        return 0.0;
    }
    (bytes as f64 / (1024.0 * 1024.0)) / (milliseconds / 1000.0)
}

fn current_rss_kb() -> u64 {
    status_kb("VmRSS").unwrap_or(0)
}

fn high_water_rss_kb() -> u64 {
    status_kb("VmHWM").unwrap_or_else(current_rss_kb)
}

fn status_kb(key: &str) -> Option<u64> {
    let status = fs::read_to_string(Path::new("/proc/self/status")).ok()?;
    for line in status.lines() {
        let (name, value) = line.split_once(':')?;
        if name == key {
            return value.split_whitespace().next()?.parse().ok();
        }
    }
    None
}
