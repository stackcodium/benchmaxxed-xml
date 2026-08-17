#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

const DEFAULT_INPUT_DIR: &str = "references/json-benchmark/data";
const DEFAULT_OUTPUT_DIR: &str = ".local/xml-datasets";
const DEFAULT_DATASETS: &[&str] = &["canada", "citm_catalog", "twitter"];

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
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".to_owned());

    match command.as_str() {
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        "json-to-xml" => {
            let options = SingleConvertOptions::parse(args.collect())?;
            convert_file(&options.input, &options.output)
        }
        "convert-benchmark-json" => {
            let options = BenchmarkConvertOptions::parse(args.collect())?;
            convert_benchmark_datasets(&options)
        }
        "render-benchmark-report" => {
            let options = BenchmarkReportOptions::parse(args.collect())?;
            render_benchmark_report(&options)
        }
        unknown => Err(format!("unknown command: {unknown}")),
    }
}

fn print_help() {
    println!("xml-converters");
    println!();
    println!("Commands:");
    println!("  json-to-xml --input FILE --output FILE");
    println!("      Convert one JSON document to XML.");
    println!("  convert-benchmark-json [--input-dir DIR] [--output-dir DIR] [--extension EXT]");
    println!("      Convert canada, citm_catalog, and twitter benchmark JSON to XML.");
    println!("      Defaults to .xml output files under .local/xml-datasets.");
    println!(
        "  render-benchmark-report --results FILE --output FILE [--metrics FILE] [--title TEXT]"
    );
    println!("      Render bench_parse TSV output as an HTML benchmark report.");
}

struct SingleConvertOptions {
    input: PathBuf,
    output: PathBuf,
}

impl SingleConvertOptions {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut input = None;
        let mut output = None;
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--input" => {
                    index += 1;
                    input = Some(PathBuf::from(
                        args.get(index)
                            .ok_or_else(|| "--input requires a file".to_owned())?,
                    ));
                }
                "--output" => {
                    index += 1;
                    output = Some(PathBuf::from(
                        args.get(index)
                            .ok_or_else(|| "--output requires a file".to_owned())?,
                    ));
                }
                other => return Err(format!("unexpected argument: {other}")),
            }
            index += 1;
        }

        Ok(Self {
            input: input.ok_or_else(|| "--input is required".to_owned())?,
            output: output.ok_or_else(|| "--output is required".to_owned())?,
        })
    }
}

struct BenchmarkConvertOptions {
    input_dir: PathBuf,
    output_dir: PathBuf,
    extension: String,
}

struct BenchmarkReportOptions {
    results: PathBuf,
    metrics: Option<PathBuf>,
    output: PathBuf,
    title: String,
}

impl BenchmarkReportOptions {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut results = None;
        let mut metrics = None;
        let mut output = None;
        let mut title = "XML parser benchmark".to_owned();
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--results" => {
                    index += 1;
                    results = Some(PathBuf::from(
                        args.get(index)
                            .ok_or_else(|| "--results requires a file".to_owned())?,
                    ));
                }
                "--metrics" => {
                    index += 1;
                    metrics = Some(PathBuf::from(
                        args.get(index)
                            .ok_or_else(|| "--metrics requires a file".to_owned())?,
                    ));
                }
                "--output" => {
                    index += 1;
                    output = Some(PathBuf::from(
                        args.get(index)
                            .ok_or_else(|| "--output requires a file".to_owned())?,
                    ));
                }
                "--title" => {
                    index += 1;
                    title = args
                        .get(index)
                        .ok_or_else(|| "--title requires text".to_owned())?
                        .to_owned();
                }
                other => return Err(format!("unexpected argument: {other}")),
            }
            index += 1;
        }

        Ok(Self {
            results: results.ok_or_else(|| "--results is required".to_owned())?,
            metrics,
            output: output.ok_or_else(|| "--output is required".to_owned())?,
            title,
        })
    }
}

impl BenchmarkConvertOptions {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut input_dir = PathBuf::from(DEFAULT_INPUT_DIR);
        let mut output_dir = PathBuf::from(DEFAULT_OUTPUT_DIR);
        let mut extension = "xml".to_owned();
        let mut index = 0;

        while index < args.len() {
            match args[index].as_str() {
                "--input-dir" => {
                    index += 1;
                    input_dir = PathBuf::from(
                        args.get(index)
                            .ok_or_else(|| "--input-dir requires a directory".to_owned())?,
                    );
                }
                "--output-dir" => {
                    index += 1;
                    output_dir = PathBuf::from(
                        args.get(index)
                            .ok_or_else(|| "--output-dir requires a directory".to_owned())?,
                    );
                }
                "--extension" => {
                    index += 1;
                    extension = args
                        .get(index)
                        .ok_or_else(|| "--extension requires a value".to_owned())?
                        .trim_start_matches('.')
                        .to_owned();
                    if extension.is_empty() {
                        return Err("--extension must not be empty".to_owned());
                    }
                }
                other => return Err(format!("unexpected argument: {other}")),
            }
            index += 1;
        }

        Ok(Self {
            input_dir,
            output_dir,
            extension,
        })
    }
}

fn convert_benchmark_datasets(options: &BenchmarkConvertOptions) -> Result<(), String> {
    fs::create_dir_all(&options.output_dir)
        .map_err(|error| format!("create {}: {error}", options.output_dir.display()))?;

    for dataset in DEFAULT_DATASETS {
        let input = options.input_dir.join(format!("{dataset}.json"));
        let output = options
            .output_dir
            .join(format!("{dataset}.{}", options.extension));
        convert_file(&input, &output)?;
    }

    Ok(())
}

fn convert_file(input: &Path, output: &Path) -> Result<(), String> {
    let json =
        fs::read_to_string(input).map_err(|error| format!("read {}: {error}", input.display()))?;
    let value = JsonParser::new(&json).parse()?;
    let mut xml = String::with_capacity(json.len() + json.len() / 3);

    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    write_value(&mut xml, &value, 0);

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    fs::write(output, xml).map_err(|error| format!("write {}: {error}", output.display()))?;
    println!("{} -> {}", input.display(), output.display());

    Ok(())
}

fn render_benchmark_report(options: &BenchmarkReportOptions) -> Result<(), String> {
    let results = read_benchmark_rows(&options.results)?;
    let metrics = match &options.metrics {
        Some(path) => Some(read_process_metrics(path)?),
        None => None,
    };
    let dataset_rows: Vec<&BenchmarkRow> =
        results.iter().filter(|row| row.file != "TOTAL").collect();
    let total = results.iter().find(|row| row.file == "TOTAL");
    let mode_summaries = summarize_modes(&dataset_rows);

    if dataset_rows.is_empty() {
        return Err("benchmark results did not include dataset rows".to_owned());
    }

    let max_throughput = dataset_rows
        .iter()
        .map(|row| row.parse_mib_s)
        .fold(0.0_f64, f64::max);
    let max_parse = dataset_rows
        .iter()
        .map(|row| row.parse_ms)
        .fold(0.0_f64, f64::max);
    let max_rss = dataset_rows
        .iter()
        .map(|row| row.hwm_kb as f64 / 1024.0)
        .fold(0.0_f64, f64::max);

    let mut html = String::new();
    html.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str("<title>");
    push_html_text(&mut html, &options.title);
    html.push_str("</title>\n<style>\n");
    html.push_str(REPORT_CSS);
    html.push_str("</style>\n</head>\n<body>\n<main>\n<header class=\"hero\">\n<h1>");
    push_html_text(&mut html, &options.title);
    html.push_str("</h1>\n<p>bench_parse release run on converted XML benchmark datasets. Throughput uses parser time only; total throughput includes file read and optional DOM counting.</p>\n</header>\n");

    if let Some(total) = total {
        html.push_str("<section class=\"summary\" aria-label=\"Summary metrics\">");
        push_metric_card(
            &mut html,
            "Total parse throughput",
            &format!("{:.1} MiB/s", total.parse_mib_s),
        );
        push_metric_card(
            &mut html,
            "Total end-to-end throughput",
            &format!("{:.1} MiB/s", total.total_mib_s),
        );
        push_metric_card(
            &mut html,
            "Mean parse time",
            &format!("{:.3} ms", total.parse_ms),
        );
        push_metric_card(
            &mut html,
            "High-water RSS",
            &format!("{:.1} MB", total.hwm_kb as f64 / 1024.0),
        );
        html.push_str("</section>\n");
    } else if !mode_summaries.is_empty() {
        let best_parse = mode_summaries
            .iter()
            .max_by(|left, right| left.parse_mib_s.total_cmp(&right.parse_mib_s))
            .expect("mode summaries are not empty");
        let best_total = mode_summaries
            .iter()
            .max_by(|left, right| left.total_mib_s.total_cmp(&right.total_mib_s))
            .expect("mode summaries are not empty");
        let lowest_rss = mode_summaries
            .iter()
            .min_by(|left, right| left.max_rss_mb.total_cmp(&right.max_rss_mb))
            .expect("mode summaries are not empty");

        html.push_str("<section class=\"summary\" aria-label=\"Summary metrics\">");
        push_metric_card(
            &mut html,
            "Best parse throughput",
            &format!("{:.1} MiB/s", best_parse.parse_mib_s),
        );
        push_metric_card(
            &mut html,
            "Best end-to-end throughput",
            &format!("{:.1} MiB/s", best_total.total_mib_s),
        );
        push_metric_card(
            &mut html,
            "Lowest mode RSS",
            &format!("{:.1} MB", lowest_rss.max_rss_mb),
        );
        push_metric_card(
            &mut html,
            "Compared modes",
            &mode_summaries.len().to_string(),
        );
        html.push_str("</section>\n");
    }

    if let Some(metrics) = &metrics {
        html.push_str("<section class=\"summary\" aria-label=\"Process metrics\">");
        push_metric_card(
            &mut html,
            "Process CPU",
            &format!("{:.0}%", metrics.cpu_percent),
        );
        push_metric_card(
            &mut html,
            "Process max RSS",
            &format!("{:.1} MB", metrics.max_rss_mb),
        );
        push_metric_card(
            &mut html,
            "Elapsed time",
            &format!("{:.3} s", metrics.elapsed_seconds),
        );
        push_metric_card(
            &mut html,
            "Run throughput",
            &format!(
                "{:.1} MiB/s",
                process_throughput_mib_s(&dataset_rows, metrics.elapsed_seconds)
            ),
        );
        html.push_str("</section>\n");
    }

    html.push_str("<section class=\"charts\">\n");
    push_bar_chart(
        &mut html,
        "Throughput",
        "MiB/sec - higher better",
        &dataset_rows,
        max_throughput,
        |row| row.parse_mib_s,
        |value| format!("{value:.1}"),
    );
    push_bar_chart(
        &mut html,
        "Parse time",
        "milliseconds - lower better",
        &dataset_rows,
        max_parse,
        |row| row.parse_ms,
        |value| format!("{value:.3}"),
    );
    push_bar_chart(
        &mut html,
        "RSS high-water",
        "MB - lower better",
        &dataset_rows,
        max_rss,
        |row| row.hwm_kb as f64 / 1024.0,
        |value| format!("{value:.1}"),
    );
    html.push_str("</section>\n");

    html.push_str("<section class=\"table-card\"><h2>Mode Throughput</h2><div class=\"table-wrap\"><table><thead><tr>");
    for heading in [
        "mode",
        "datasets",
        "bytes",
        "parse ms",
        "parse MiB/s",
        "total ms",
        "total MiB/s",
        "max RSS MB",
    ] {
        html.push_str("<th>");
        html.push_str(heading);
        html.push_str("</th>");
    }
    html.push_str("</tr></thead><tbody>");
    for summary in &mode_summaries {
        html.push_str("<tr><td>");
        push_html_text(&mut html, &summary.mode);
        html.push_str("</td><td>");
        html.push_str(&summary.datasets.to_string());
        html.push_str("</td><td>");
        html.push_str(&summary.bytes.to_string());
        html.push_str("</td><td>");
        html.push_str(&format!("{:.3}", summary.parse_ms));
        html.push_str("</td><td>");
        html.push_str(&format!("{:.1}", summary.parse_mib_s));
        html.push_str("</td><td>");
        html.push_str(&format!("{:.3}", summary.total_ms));
        html.push_str("</td><td>");
        html.push_str(&format!("{:.1}", summary.total_mib_s));
        html.push_str("</td><td>");
        html.push_str(&format!("{:.1}", summary.max_rss_mb));
        html.push_str("</td></tr>");
    }
    html.push_str("</tbody></table></div></section>\n");

    html.push_str("<section class=\"table-card\"><h2>Dataset Results</h2><div class=\"table-wrap\"><table><thead><tr>");
    for heading in [
        "mode",
        "dataset",
        "bytes",
        "parse ms",
        "parse MiB/s",
        "parse vs dom",
        "total MiB/s",
        "total vs dom",
        "RSS MB",
        "elements",
        "attributes",
        "nodes",
    ] {
        html.push_str("<th>");
        html.push_str(heading);
        html.push_str("</th>");
    }
    html.push_str("</tr></thead><tbody>");
    for row in &dataset_rows {
        html.push_str("<tr><td>");
        push_html_text(&mut html, &row.mode);
        html.push_str("</td><td>");
        push_html_text(&mut html, dataset_name(&row.file));
        html.push_str("</td><td>");
        html.push_str(&row.bytes.to_string());
        html.push_str("</td><td>");
        html.push_str(&format!("{:.3}", row.parse_ms));
        html.push_str("</td><td>");
        html.push_str(&format!("{:.1}", row.parse_mib_s));
        html.push_str("</td><td>");
        push_html_text(
            &mut html,
            &throughput_factor_label(
                row.parse_mib_s,
                dom_baseline_for(&dataset_rows, &row.file).map(|baseline| baseline.parse_mib_s),
            ),
        );
        html.push_str("</td><td>");
        html.push_str(&format!("{:.1}", row.total_mib_s));
        html.push_str("</td><td>");
        push_html_text(
            &mut html,
            &throughput_factor_label(
                row.total_mib_s,
                dom_baseline_for(&dataset_rows, &row.file).map(|baseline| baseline.total_mib_s),
            ),
        );
        html.push_str("</td><td>");
        html.push_str(&format!("{:.1}", row.hwm_kb as f64 / 1024.0));
        html.push_str("</td><td>");
        html.push_str(&row.elements.to_string());
        html.push_str("</td><td>");
        html.push_str(&row.attributes.to_string());
        html.push_str("</td><td>");
        html.push_str(&row.nodes.to_string());
        html.push_str("</td></tr>");
    }
    html.push_str("</tbody></table></div></section>\n");

    if let Some(metrics) = metrics {
        html.push_str("<section class=\"note\"><strong>Process metrics:</strong> CPU ");
        html.push_str(&format!("{:.0}%", metrics.cpu_percent));
        html.push_str(", max RSS ");
        html.push_str(&format!("{:.1} MB", metrics.max_rss_mb));
        html.push_str(", elapsed ");
        html.push_str(&format!("{:.3} s", metrics.elapsed_seconds));
        if metrics.elapsed_seconds > 0.0 {
            html.push_str(", run throughput ");
            html.push_str(&format!(
                "{:.1} MiB/s",
                process_throughput_mib_s(&dataset_rows, metrics.elapsed_seconds)
            ));
        }
        html.push_str(". Measured around the full benchmark command.</section>\n");
    }

    html.push_str("</main>\n</body>\n</html>\n");

    if let Some(parent) = options.output.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    fs::write(&options.output, html)
        .map_err(|error| format!("write {}: {error}", options.output.display()))?;
    println!("wrote {}", options.output.display());

    Ok(())
}

fn read_benchmark_rows(path: &Path) -> Result<Vec<BenchmarkRow>, String> {
    let input =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut lines = input.lines();
    let header = lines
        .next()
        .ok_or_else(|| format!("{} is empty", path.display()))?;
    let columns: Vec<&str> = header.split('\t').collect();
    let mut rows = Vec::new();

    for (line_number, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let values: Vec<&str> = line.split('\t').collect();
        rows.push(BenchmarkRow {
            file: field(&columns, &values, "file", line_number + 2)?.to_owned(),
            mode: optional_field(&columns, &values, "mode")
                .unwrap_or("dom")
                .to_owned(),
            bytes: parse_field(&columns, &values, "bytes", line_number + 2)?,
            iterations: parse_field(&columns, &values, "iter", line_number + 2)?,
            parse_ms: parse_field(&columns, &values, "parse_ms", line_number + 2)?,
            total_ms: parse_field(&columns, &values, "total_ms", line_number + 2)?,
            total_mib_s: parse_field(&columns, &values, "total_mib_s", line_number + 2)?,
            parse_mib_s: parse_field(&columns, &values, "parse_mib_s", line_number + 2)?,
            hwm_kb: parse_field(&columns, &values, "hwm_kb", line_number + 2)?,
            elements: parse_field(&columns, &values, "elements", line_number + 2)?,
            attributes: parse_field(&columns, &values, "attributes", line_number + 2)?,
            nodes: parse_field(&columns, &values, "nodes", line_number + 2)?,
        });
    }

    Ok(rows)
}

fn read_process_metrics(path: &Path) -> Result<ProcessMetrics, String> {
    let input =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut lines = input.lines();
    let header = lines
        .next()
        .ok_or_else(|| format!("{} is empty", path.display()))?;
    let values = lines
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| format!("{} has no metrics row", path.display()))?;
    let columns: Vec<&str> = header.split('\t').collect();
    let values: Vec<&str> = values.split('\t').collect();

    Ok(ProcessMetrics {
        cpu_percent: parse_field(&columns, &values, "cpu_percent", 2)?,
        max_rss_mb: parse_field(&columns, &values, "max_rss_mb", 2)?,
        elapsed_seconds: parse_field(&columns, &values, "elapsed_seconds", 2)?,
    })
}

fn field<'a>(
    columns: &[&str],
    values: &'a [&str],
    name: &str,
    line_number: usize,
) -> Result<&'a str, String> {
    let index = columns
        .iter()
        .position(|column| *column == name)
        .ok_or_else(|| format!("missing column {name}"))?;
    values
        .get(index)
        .copied()
        .ok_or_else(|| format!("line {line_number}: missing value for {name}"))
}

fn optional_field<'a>(columns: &[&str], values: &'a [&str], name: &str) -> Option<&'a str> {
    let index = columns.iter().position(|column| *column == name)?;
    values.get(index).copied()
}

fn parse_field<T: std::str::FromStr>(
    columns: &[&str],
    values: &[&str],
    name: &str,
    line_number: usize,
) -> Result<T, String> {
    field(columns, values, name, line_number)?
        .parse()
        .map_err(|_| format!("line {line_number}: invalid {name}"))
}

fn push_bar_chart<F, G>(
    output: &mut String,
    title: &str,
    hint: &str,
    rows: &[&BenchmarkRow],
    max_value: f64,
    value: F,
    format_value: G,
) where
    F: Fn(&BenchmarkRow) -> f64,
    G: Fn(f64) -> String,
{
    let has_multiple_modes = rows
        .first()
        .is_some_and(|first| rows.iter().any(|row| row.mode != first.mode));
    output.push_str("<article class=\"chart\"><div class=\"chart-head\"><h2>");
    output.push_str(title);
    output.push_str("</h2><span>");
    output.push_str(hint);
    output.push_str("</span></div><div class=\"plot\">");

    for row in rows {
        let metric = value(row);
        let height = if max_value <= 0.0 {
            0.0
        } else {
            (metric / max_value * 100.0).clamp(2.0, 100.0)
        };
        output.push_str("<div class=\"bar-wrap\"><div class=\"bar\" style=\"height:");
        output.push_str(&format!("{height:.1}%"));
        output.push_str("\"><span>");
        push_html_text(output, &format_value(metric));
        output.push_str("</span></div><strong>");
        if has_multiple_modes {
            push_html_text(output, &row.mode);
            output.push_str("<br>");
        }
        push_html_text(output, dataset_name(&row.file));
        output.push_str("</strong></div>");
    }
    output.push_str("</div></article>\n");
}

fn push_metric_card(output: &mut String, label: &str, value: &str) {
    output.push_str("<article><span>");
    output.push_str(label);
    output.push_str("</span><strong>");
    output.push_str(value);
    output.push_str("</strong></article>");
}

fn dataset_name(path: &str) -> &str {
    Path::new(path)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

fn dom_baseline_for<'a>(rows: &[&'a BenchmarkRow], file: &str) -> Option<&'a BenchmarkRow> {
    rows.iter()
        .copied()
        .find(|row| row.file == file && row.mode == "dom")
}

fn summarize_modes(rows: &[&BenchmarkRow]) -> Vec<ModeSummary> {
    let mut summaries = BTreeMap::<String, ModeSummary>::new();

    for row in rows {
        let summary = summaries
            .entry(row.mode.clone())
            .or_insert_with(|| ModeSummary::new(row.mode.clone()));
        summary.datasets += 1;
        summary.bytes = summary.bytes.saturating_add(row.bytes);
        summary.parse_ms += row.parse_ms;
        summary.total_ms += row.total_ms;
        summary.max_rss_mb = summary.max_rss_mb.max(row.hwm_kb as f64 / 1024.0);
    }

    let mut summaries: Vec<ModeSummary> = summaries
        .into_values()
        .map(|mut summary| {
            summary.parse_mib_s = mib_per_second(summary.bytes, summary.parse_ms);
            summary.total_mib_s = mib_per_second(summary.bytes, summary.total_ms);
            summary
        })
        .collect();

    summaries.sort_by(|left, right| right.parse_mib_s.total_cmp(&left.parse_mib_s));
    summaries
}

fn mib_per_second(bytes: usize, milliseconds: f64) -> f64 {
    if milliseconds <= 0.0 {
        return 0.0;
    }
    bytes as f64 / (milliseconds / 1000.0) / 1_048_576.0
}

fn throughput_factor_label(value: f64, baseline: Option<f64>) -> String {
    let Some(baseline) = baseline else {
        return "n/a".to_owned();
    };
    if baseline <= 0.0 {
        return "n/a".to_owned();
    }
    format!("{:.2}x", value / baseline)
}

fn process_throughput_mib_s(rows: &[&BenchmarkRow], elapsed_seconds: f64) -> f64 {
    if elapsed_seconds <= 0.0 {
        return 0.0;
    }
    let processed_bytes: usize = rows
        .iter()
        .map(|row| row.bytes.saturating_mul(row.iterations))
        .sum();
    processed_bytes as f64 / elapsed_seconds / 1_048_576.0
}

fn push_html_text(output: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(ch),
        }
    }
}

#[derive(Debug)]
struct BenchmarkRow {
    file: String,
    mode: String,
    bytes: usize,
    iterations: usize,
    parse_ms: f64,
    total_ms: f64,
    parse_mib_s: f64,
    total_mib_s: f64,
    hwm_kb: usize,
    elements: usize,
    attributes: usize,
    nodes: usize,
}

struct ModeSummary {
    mode: String,
    datasets: usize,
    bytes: usize,
    parse_ms: f64,
    parse_mib_s: f64,
    total_ms: f64,
    total_mib_s: f64,
    max_rss_mb: f64,
}

impl ModeSummary {
    fn new(mode: String) -> Self {
        Self {
            mode,
            datasets: 0,
            bytes: 0,
            parse_ms: 0.0,
            parse_mib_s: 0.0,
            total_ms: 0.0,
            total_mib_s: 0.0,
            max_rss_mb: 0.0,
        }
    }
}

struct ProcessMetrics {
    cpu_percent: f64,
    max_rss_mb: f64,
    elapsed_seconds: f64,
}

const REPORT_CSS: &str = r#"
:root { --paper: #f2e8d2; --ink: #111; --cream: #fff8e8; --line: #111; --green: #20ba74; --red: #ef5a4f; --gold: #efb342; --muted: #665f55; }
* { box-sizing: border-box; }
body { margin: 0; color: var(--ink); background: linear-gradient(90deg, rgba(17,17,17,.07) 1px, transparent 1px), linear-gradient(0deg, rgba(17,17,17,.07) 1px, transparent 1px), var(--paper); background-size: 12px 12px; font-family: "Courier New", Courier, monospace; letter-spacing: 0; }
main { width: min(1120px, calc(100vw - 24px)); margin: 0 auto; padding: 14px 0 22px; }
.hero, .summary article, .chart, .table-card, .note { background: var(--cream); border: 4px solid var(--line); box-shadow: 6px 6px 0 #000; }
.hero { padding: 14px 16px; margin-bottom: 16px; }
h1 { margin: 0 0 8px; font-size: clamp(26px, 5vw, 52px); line-height: .96; text-transform: uppercase; }
.hero p, .chart-head span, .summary span, .note { color: var(--muted); }
.hero p { max-width: 820px; margin: 0; font-size: 14px; line-height: 1.35; }
.summary { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 14px; margin-bottom: 16px; }
.summary article { padding: 12px; }
.summary span { display: block; margin-bottom: 8px; font-size: 12px; font-weight: 700; text-transform: uppercase; }
.summary strong { font-size: 22px; }
.charts { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 16px; margin-bottom: 16px; }
.chart { padding: 14px; }
.chart-head { min-height: 56px; display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; margin-bottom: 12px; }
h2 { margin: 0; font-size: 18px; line-height: 1.1; text-transform: uppercase; }
.chart-head span { max-width: 150px; font-size: 12px; text-align: right; }
.plot { height: 260px; display: flex; align-items: stretch; justify-content: space-around; gap: 12px; padding: 12px 10px 8px; background: #e6dac2; border: 4px solid var(--line); }
.bar-wrap { min-width: 0; flex: 1; display: flex; flex-direction: column; justify-content: end; align-items: center; gap: 8px; text-align: center; }
.bar { width: min(64px, 76%); min-height: 5px; position: relative; background: var(--green); border: 3px solid var(--line); box-shadow: inset 0 -8px 0 rgba(0,0,0,.18); }
.bar span { position: absolute; left: 50%; bottom: calc(100% + 6px); transform: translateX(-50%); padding: 3px 5px; color: var(--cream); background: var(--ink); border: 2px solid var(--line); font-size: 11px; font-weight: 700; white-space: nowrap; }
.bar-wrap strong { max-width: 100%; overflow-wrap: anywhere; font-size: 12px; text-transform: uppercase; }
.table-card { padding: 14px; }
.table-wrap { overflow-x: auto; }
table { width: 100%; border-collapse: collapse; background: #e6dac2; border: 3px solid var(--line); font-size: 13px; }
th, td { padding: 8px 9px; border: 2px solid var(--line); text-align: right; white-space: nowrap; }
th:first-child, td:first-child { text-align: left; }
th { background: var(--gold); text-transform: uppercase; }
.note { padding: 10px 12px; margin-top: 16px; font-size: 13px; line-height: 1.35; }
@media (max-width: 900px) { .summary, .charts { grid-template-columns: 1fr; } .plot { height: 230px; } }
"#;

#[derive(Debug, PartialEq)]
enum JsonValue {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

struct JsonParser<'a> {
    input: &'a str,
    bytes: &'a [u8],
    index: usize,
}

impl<'a> JsonParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            bytes: input.as_bytes(),
            index: 0,
        }
    }

    fn parse(mut self) -> Result<JsonValue, String> {
        let value = self.parse_value()?;
        self.skip_whitespace();
        if self.index != self.bytes.len() {
            return Err(self.error("trailing data"));
        }
        Ok(value)
    }

    fn parse_value(&mut self) -> Result<JsonValue, String> {
        self.skip_whitespace();
        match self.peek() {
            Some(b'n') => {
                self.expect_literal("null")?;
                Ok(JsonValue::Null)
            }
            Some(b't') => {
                self.expect_literal("true")?;
                Ok(JsonValue::Bool(true))
            }
            Some(b'f') => {
                self.expect_literal("false")?;
                Ok(JsonValue::Bool(false))
            }
            Some(b'"') => self.parse_string().map(JsonValue::String),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_object(),
            Some(b'-' | b'0'..=b'9') => self.parse_number().map(JsonValue::Number),
            Some(other) => Err(self.error(&format!("unexpected byte 0x{other:02x}"))),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn parse_array(&mut self) -> Result<JsonValue, String> {
        self.expect_byte(b'[')?;
        self.skip_whitespace();

        let mut values = Vec::new();
        if self.consume_byte(b']') {
            return Ok(JsonValue::Array(values));
        }

        loop {
            values.push(self.parse_value()?);
            self.skip_whitespace();

            if self.consume_byte(b']') {
                break;
            }
            self.expect_byte(b',')?;
        }

        Ok(JsonValue::Array(values))
    }

    fn parse_object(&mut self) -> Result<JsonValue, String> {
        self.expect_byte(b'{')?;
        self.skip_whitespace();

        let mut members = Vec::new();
        if self.consume_byte(b'}') {
            return Ok(JsonValue::Object(members));
        }

        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect_byte(b':')?;
            let value = self.parse_value()?;
            members.push((key, value));
            self.skip_whitespace();

            if self.consume_byte(b'}') {
                break;
            }
            self.expect_byte(b',')?;
        }

        Ok(JsonValue::Object(members))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect_byte(b'"')?;
        let mut output = String::new();
        let mut segment_start = self.index;

        while let Some(byte) = self.peek() {
            match byte {
                b'"' => {
                    output.push_str(&self.input[segment_start..self.index]);
                    self.index += 1;
                    return Ok(output);
                }
                b'\\' => {
                    output.push_str(&self.input[segment_start..self.index]);
                    self.index += 1;
                    self.parse_escape(&mut output)?;
                    segment_start = self.index;
                }
                0x00..=0x1f => return Err(self.error("unescaped control character in string")),
                _ => self.index += utf8_char_width(byte),
            }
        }

        Err(self.error("unterminated string"))
    }

    fn parse_escape(&mut self, output: &mut String) -> Result<(), String> {
        match self.next_byte() {
            Some(b'"') => output.push('"'),
            Some(b'\\') => output.push('\\'),
            Some(b'/') => output.push('/'),
            Some(b'b') => output.push('\u{0008}'),
            Some(b'f') => output.push('\u{000c}'),
            Some(b'n') => output.push('\n'),
            Some(b'r') => output.push('\r'),
            Some(b't') => output.push('\t'),
            Some(b'u') => {
                let code = self.parse_hex_quad()?;
                if (0xd800..=0xdbff).contains(&code) {
                    self.expect_byte(b'\\')?;
                    self.expect_byte(b'u')?;
                    let low = self.parse_hex_quad()?;
                    if !(0xdc00..=0xdfff).contains(&low) {
                        return Err(self.error("invalid low surrogate"));
                    }
                    let combined =
                        0x10000 + (((code - 0xd800) as u32) << 10) + (low - 0xdc00) as u32;
                    output.push(
                        char::from_u32(combined)
                            .ok_or_else(|| self.error("invalid unicode scalar"))?,
                    );
                } else if (0xdc00..=0xdfff).contains(&code) {
                    return Err(self.error("unexpected low surrogate"));
                } else {
                    output.push(
                        char::from_u32(code as u32)
                            .ok_or_else(|| self.error("invalid unicode scalar"))?,
                    );
                }
            }
            Some(other) => return Err(self.error(&format!("invalid escape 0x{other:02x}"))),
            None => return Err(self.error("unterminated escape")),
        }

        Ok(())
    }

    fn parse_hex_quad(&mut self) -> Result<u16, String> {
        let mut value = 0u16;
        for _ in 0..4 {
            let byte = self
                .next_byte()
                .ok_or_else(|| self.error("unterminated unicode escape"))?;
            value = (value << 4)
                | match byte {
                    b'0'..=b'9' => (byte - b'0') as u16,
                    b'a'..=b'f' => (byte - b'a' + 10) as u16,
                    b'A'..=b'F' => (byte - b'A' + 10) as u16,
                    _ => return Err(self.error("invalid unicode escape")),
                };
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<String, String> {
        let start = self.index;

        if self.consume_byte(b'-') && self.peek().is_none() {
            return Err(self.error("incomplete number"));
        }

        match self.peek() {
            Some(b'0') => {
                self.index += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.error("leading zero in number"));
                }
            }
            Some(b'1'..=b'9') => {
                self.index += 1;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.index += 1;
                }
            }
            _ => return Err(self.error("invalid number")),
        }

        if self.consume_byte(b'.') {
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("fraction requires at least one digit"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.index += 1;
            }
        }

        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.index += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.index += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("exponent requires at least one digit"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.index += 1;
            }
        }

        Ok(self.input[start..self.index].to_owned())
    }

    fn expect_literal(&mut self, literal: &str) -> Result<(), String> {
        if self.input[self.index..].starts_with(literal) {
            self.index += literal.len();
            Ok(())
        } else {
            Err(self.error(&format!("expected {literal}")))
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.index += 1;
        }
    }

    fn expect_byte(&mut self, expected: u8) -> Result<(), String> {
        if self.consume_byte(expected) {
            Ok(())
        } else {
            Err(self.error(&format!("expected '{}'", expected as char)))
        }
    }

    fn consume_byte(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn next_byte(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.index += 1;
        Some(byte)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    fn error(&self, message: &str) -> String {
        format!("{message} at byte {}", self.index)
    }
}

fn utf8_char_width(byte: u8) -> usize {
    match byte {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

fn write_value(output: &mut String, value: &JsonValue, indent: usize) {
    match value {
        JsonValue::Null => {
            write_indent(output, indent);
            output.push_str("<null/>\n");
        }
        JsonValue::Bool(value) => {
            write_indent(output, indent);
            output.push_str("<boolean>");
            output.push_str(if *value { "true" } else { "false" });
            output.push_str("</boolean>\n");
        }
        JsonValue::Number(value) => {
            write_indent(output, indent);
            output.push_str("<number>");
            output.push_str(value);
            output.push_str("</number>\n");
        }
        JsonValue::String(value) => {
            write_indent(output, indent);
            output.push_str("<string>");
            push_xml_text(output, value);
            output.push_str("</string>\n");
        }
        JsonValue::Array(values) => {
            write_indent(output, indent);
            output.push_str("<array>\n");
            for value in values {
                write_indent(output, indent + 1);
                output.push_str("<item>\n");
                write_value(output, value, indent + 2);
                write_indent(output, indent + 1);
                output.push_str("</item>\n");
            }
            write_indent(output, indent);
            output.push_str("</array>\n");
        }
        JsonValue::Object(members) => {
            write_indent(output, indent);
            output.push_str("<object>\n");
            for (key, value) in members {
                write_indent(output, indent + 1);
                output.push_str("<member name=\"");
                push_xml_attr(output, key);
                output.push_str("\">\n");
                write_value(output, value, indent + 2);
                write_indent(output, indent + 1);
                output.push_str("</member>\n");
            }
            write_indent(output, indent);
            output.push_str("</object>\n");
        }
    }
}

fn write_indent(output: &mut String, indent: usize) {
    for _ in 0..indent {
        output.push_str("  ");
    }
}

fn push_xml_text(output: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            ch if is_xml_char(ch) => output.push(ch),
            _ => output.push('\u{fffd}'),
        }
    }
}

fn push_xml_attr(output: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            ch if is_xml_char(ch) => output.push(ch),
            _ => output.push('\u{fffd}'),
        }
    }
}

fn is_xml_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x09 | 0x0a | 0x0d | 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(input: &str) -> String {
        let value = JsonParser::new(input).parse().unwrap();
        let mut output = String::new();
        write_value(&mut output, &value, 0);
        output
    }

    #[test]
    fn parses_nested_json() {
        let value = JsonParser::new(r#"{"a":[1,true,null,"x"]}"#)
            .parse()
            .unwrap();

        assert_eq!(
            value,
            JsonValue::Object(vec![(
                "a".to_owned(),
                JsonValue::Array(vec![
                    JsonValue::Number("1".to_owned()),
                    JsonValue::Bool(true),
                    JsonValue::Null,
                    JsonValue::String("x".to_owned())
                ])
            )])
        );
    }

    #[test]
    fn decodes_string_escapes() {
        let value = JsonParser::new(r#""a\n\uD834\uDD1E""#).parse().unwrap();
        assert_eq!(value, JsonValue::String("a\n\u{1d11e}".to_owned()));
    }

    #[test]
    fn rejects_trailing_data() {
        assert!(JsonParser::new("true false").parse().is_err());
    }

    #[test]
    fn emits_xml_representation() {
        let output = convert(r#"{"a&b":"<value>","n":-1.25e+3}"#);

        assert!(output.contains("<member name=\"a&amp;b\">"));
        assert!(output.contains("<string>&lt;value&gt;</string>"));
        assert!(output.contains("<number>-1.25e+3</number>"));
    }
}
