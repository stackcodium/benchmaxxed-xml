import { readFileSync } from "node:fs";
import { basename } from "node:path";
import { XML } from "bun";

const USAGE =
  "usage: bun-xml-bench [--runs N] [--iterations N] [--warmup N] [--min-duration-ms N] FILE...";

interface Options {
  runs: number;
  iterations: number;
  warmup: number;
  minDurationMs: number;
  paths: string[];
}

interface Counts {
  elements: number;
  attributes: number;
  nodes: number;
  checksum: number;
}

interface Sample {
  iterations: number;
  parseMs: number;
  countMs: number;
  counts: Counts;
}

interface XmlNode {
  name: string;
  attributes?: Record<string, string>;
  children?: Array<XmlNode | string>;
}

function parsePositiveInteger(value: string | undefined): number {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) throw new Error(USAGE);
  return parsed;
}

function parseNonNegativeInteger(value: string | undefined): number {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 0) throw new Error(USAGE);
  return parsed;
}

function parseOptions(args: string[]): Options {
  const options: Options = {
    runs: 3,
    iterations: 1,
    warmup: 1,
    minDurationMs: 300,
    paths: [],
  };

  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    switch (arg) {
      case "--runs":
        options.runs = parsePositiveInteger(args[++index]);
        break;
      case "--iterations":
        options.iterations = parsePositiveInteger(args[++index]);
        break;
      case "--warmup":
        options.warmup = parseNonNegativeInteger(args[++index]);
        break;
      case "--min-duration-ms":
        options.minDurationMs = parsePositiveInteger(args[++index]);
        break;
      default:
        if (arg.startsWith("-")) throw new Error(USAGE);
        options.paths.push(arg);
    }
  }

  if (options.paths.length === 0) throw new Error(USAGE);
  return options;
}

function isWhitespaceOnly(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code !== 0x20 && code !== 0x09 && code !== 0x0a && code !== 0x0d) return false;
  }
  return true;
}

function countDocument(root: XmlNode): Counts {
  const counts: Counts = { elements: 0, attributes: 0, nodes: 0, checksum: 0 };
  countNode(root, counts);
  return counts;
}

function countNode(node: XmlNode, counts: Counts): void {
  counts.elements += 1;
  counts.nodes += 1;
  counts.checksum = (counts.checksum + node.name.length) >>> 0;

  if (node.attributes) {
    for (const name in node.attributes) {
      counts.attributes += 1;
      counts.checksum = (counts.checksum + name.length + node.attributes[name].length) >>> 0;
    }
  }

  if (!node.children) return;
  for (const child of node.children) {
    if (typeof child === "string") {
      if (!isWhitespaceOnly(child)) {
        counts.nodes += 1;
        counts.checksum = (counts.checksum + child.length) >>> 0;
      }
    } else {
      countNode(child, counts);
    }
  }
}

function parseDocument(input: Uint8Array): XmlNode {
  return XML.parse(input, { compact: false }) as XmlNode;
}

function runSample(input: Uint8Array, iterations: number, warmup: number): Sample {
  for (let index = 0; index < warmup; index += 1) {
    const document = parseDocument(input);
    countDocument(document);
  }

  let parseNanoseconds = 0;
  let countNanoseconds = 0;
  let counts: Counts = { elements: 0, attributes: 0, nodes: 0, checksum: 0 };

  for (let index = 0; index < iterations; index += 1) {
    const parseStart = Bun.nanoseconds();
    const document = parseDocument(input);
    parseNanoseconds += Bun.nanoseconds() - parseStart;

    const countStart = Bun.nanoseconds();
    counts = countDocument(document);
    countNanoseconds += Bun.nanoseconds() - countStart;
  }

  return {
    iterations,
    parseMs: parseNanoseconds / iterations / 1_000_000,
    countMs: countNanoseconds / iterations / 1_000_000,
    counts,
  };
}

function calibrate(input: Uint8Array, options: Options): number {
  let iterations = options.iterations;
  while (true) {
    const sample = runSample(input, iterations, 0);
    const measuredMs = (sample.parseMs + sample.countMs) * iterations;
    if (measuredMs >= options.minDurationMs) return iterations;
    const multiplier = Math.max(2, Math.ceil(options.minDurationMs / Math.max(measuredMs, 0.001)));
    iterations *= multiplier;
  }
}

function processMemory(): { rssKb: number; hwmKb: number } {
  const status = readFileSync("/proc/self/status", "utf8");
  const read = (key: string): number => {
    const match = status.match(new RegExp(`^${key}:\\s+(\\d+)`, "m"));
    return match ? Number(match[1]) : 0;
  };
  return { rssKb: read("VmRSS"), hwmKb: read("VmHWM") };
}

function mibPerSecond(bytes: number, milliseconds: number): number {
  return milliseconds > 0 ? bytes / 1_048_576 / (milliseconds / 1_000) : 0;
}

function main(): void {
  const options = parseOptions(Bun.argv.slice(2));
  console.log(
    "file\tmode\titer\twarmup\tbytes\tread_ms\tparse_ms\tcount_ms\ttotal_ms\tparse_mib_s\ttotal_mib_s\trss_kb\thwm_kb\telements\tattributes\tnodes",
  );

  for (const path of options.paths) {
    const readStart = Bun.nanoseconds();
    const input = readFileSync(path);
    const readMs = (Bun.nanoseconds() - readStart) / 1_000_000;
    const iterations = calibrate(input, options);

    let best: Sample | undefined;
    for (let run = 0; run < options.runs; run += 1) {
      const sample = runSample(input, iterations, options.warmup);
      if (!best || sample.parseMs + sample.countMs < best.parseMs + best.countMs) best = sample;
    }
    if (!best) throw new Error(`benchmark did not run for ${basename(path)}`);

    const memory = processMemory();
    const parserMs = best.parseMs + best.countMs;
    console.log(
      [
        path,
        "bun-xml-ordered-walk",
        best.iterations,
        options.warmup,
        input.byteLength,
        readMs.toFixed(3),
        best.parseMs.toFixed(3),
        best.countMs.toFixed(3),
        (readMs + parserMs).toFixed(3),
        mibPerSecond(input.byteLength, best.parseMs).toFixed(1),
        mibPerSecond(input.byteLength, readMs + parserMs).toFixed(1),
        memory.rssKb,
        memory.hwmKb,
        best.counts.elements,
        best.counts.attributes,
        best.counts.nodes,
      ].join("\t"),
    );
  }
}

try {
  main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}
