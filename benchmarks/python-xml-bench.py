#!/usr/bin/env python3
from __future__ import annotations

import argparse
import ctypes
import os
import time
from dataclasses import dataclass
from pathlib import Path


XML_ELEMENT_NODE = 1
XML_TEXT_NODE = 3
XML_CDATA_SECTION_NODE = 4
XML_PI_NODE = 7
XML_COMMENT_NODE = 8
XML_PARSE_NOBLANKS = 256
XML_PARSE_NONET = 2048
XML_PARSE_COMPACT = 65536


class XmlDoc(ctypes.Structure):
    pass


class XmlNode(ctypes.Structure):
    pass


class XmlAttr(ctypes.Structure):
    pass


XmlDocPointer = ctypes.POINTER(XmlDoc)
XmlNodePointer = ctypes.POINTER(XmlNode)
XmlAttrPointer = ctypes.POINTER(XmlAttr)

XmlNode._fields_ = [
    ("_private", ctypes.c_void_p),
    ("type", ctypes.c_int),
    ("name", ctypes.POINTER(ctypes.c_ubyte)),
    ("children", XmlNodePointer),
    ("last", XmlNodePointer),
    ("parent", XmlNodePointer),
    ("next", XmlNodePointer),
    ("prev", XmlNodePointer),
    ("doc", XmlDocPointer),
    ("ns", ctypes.c_void_p),
    ("content", ctypes.POINTER(ctypes.c_ubyte)),
    ("properties", XmlAttrPointer),
    ("ns_def", ctypes.c_void_p),
    ("psvi", ctypes.c_void_p),
    ("line", ctypes.c_ushort),
    ("extra", ctypes.c_ushort),
]

XmlAttr._fields_ = [
    ("_private", ctypes.c_void_p),
    ("type", ctypes.c_int),
    ("name", ctypes.POINTER(ctypes.c_ubyte)),
    ("children", XmlNodePointer),
    ("last", XmlNodePointer),
    ("parent", XmlNodePointer),
    ("next", XmlAttrPointer),
    ("prev", XmlAttrPointer),
    ("doc", XmlDocPointer),
    ("ns", ctypes.c_void_p),
    ("atype", ctypes.c_int),
    ("psvi", ctypes.c_void_p),
]


@dataclass(frozen=True)
class Counts:
    elements: int
    attributes: int
    nodes: int


@dataclass(frozen=True)
class Sample:
    iterations: int
    parse_ms: float
    count_ms: float
    counts: Counts


def options() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Benchmark Python libxml2 parse plus complete walk")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--iterations", type=int, default=1)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--min-duration-ms", type=int, default=300)
    parser.add_argument("paths", nargs="+")
    args = parser.parse_args()
    if args.runs < 1 or args.iterations < 1 or args.warmup < 0 or args.min_duration_ms < 1:
        parser.error("runs, iterations, and minimum duration must be positive; warmup must be non-negative")
    return args


def load_libxml() -> ctypes.CDLL:
    library = ctypes.CDLL("libxml2.so.2")
    library.xmlReadMemory.argtypes = [
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_char_p,
        ctypes.c_int,
    ]
    library.xmlReadMemory.restype = XmlDocPointer
    library.xmlDocGetRootElement.argtypes = [XmlDocPointer]
    library.xmlDocGetRootElement.restype = XmlNodePointer
    library.xmlFreeDoc.argtypes = [XmlDocPointer]
    library.xmlFreeDoc.restype = None
    return library


def semantic_text(content: ctypes.POINTER(ctypes.c_ubyte)) -> bool:
    return bool(content) and bool(ctypes.string_at(content).strip(b" \t\r\n"))


def count_document(root: XmlNodePointer) -> Counts:
    elements = 0
    attributes = 0
    nodes = 0
    stack = [root]
    while stack:
        node = stack.pop().contents
        elements += 1
        nodes += 1

        attribute = node.properties
        while attribute:
            attributes += 1
            attribute = attribute.contents.next

        child = node.children
        children: list[XmlNodePointer] = []
        while child:
            child_node = child.contents
            if child_node.type == XML_ELEMENT_NODE:
                children.append(child)
            elif child_node.type in (XML_TEXT_NODE, XML_CDATA_SECTION_NODE) and semantic_text(
                child_node.content
            ):
                nodes += 1
            elif child_node.type in (XML_PI_NODE, XML_COMMENT_NODE):
                nodes += 1
            child = child_node.next
        stack.extend(reversed(children))
    return Counts(elements, attributes, nodes)


def parse_document(library: ctypes.CDLL, data: bytes, path: bytes) -> XmlDocPointer:
    document = library.xmlReadMemory(
        data,
        len(data),
        path,
        None,
        XML_PARSE_NOBLANKS | XML_PARSE_NONET | XML_PARSE_COMPACT,
    )
    if not document:
        raise RuntimeError(f"XML parse failed: {os.fsdecode(path)}")
    return document


def run_sample(
    library: ctypes.CDLL, data: bytes, path: bytes, iterations: int, warmup: int
) -> Sample:
    for _ in range(warmup):
        document = parse_document(library, data, path)
        count_document(library.xmlDocGetRootElement(document))
        library.xmlFreeDoc(document)

    parse_ns = 0
    count_ns = 0
    counts = Counts(0, 0, 0)
    for _ in range(iterations):
        started = time.perf_counter_ns()
        document = parse_document(library, data, path)
        parse_ns += time.perf_counter_ns() - started

        started = time.perf_counter_ns()
        counts = count_document(library.xmlDocGetRootElement(document))
        count_ns += time.perf_counter_ns() - started
        library.xmlFreeDoc(document)

    return Sample(
        iterations,
        parse_ns / iterations / 1_000_000,
        count_ns / iterations / 1_000_000,
        counts,
    )


def calibrate(
    library: ctypes.CDLL, data: bytes, path: bytes, initial: int, minimum_ms: int
) -> int:
    iterations = initial
    while True:
        sample = run_sample(library, data, path, iterations, 0)
        measured_ms = (sample.parse_ms + sample.count_ms) * iterations
        if measured_ms >= minimum_ms:
            return iterations
        multiplier = max(2, int(minimum_ms / max(measured_ms, 0.001)) + 1)
        iterations *= multiplier


def memory_kb() -> tuple[int, int]:
    values = {"VmRSS": 0, "VmHWM": 0}
    try:
        for line in Path("/proc/self/status").read_text(encoding="utf-8").splitlines():
            key, _, remainder = line.partition(":")
            if key in values:
                values[key] = int(remainder.split()[0])
    except (OSError, ValueError, IndexError):
        pass
    return values["VmRSS"], values["VmHWM"]


def mib_per_second(byte_count: int, milliseconds: float) -> float:
    return byte_count / 1_048_576 / (milliseconds / 1_000) if milliseconds > 0 else 0.0


def main() -> None:
    args = options()
    library = load_libxml()
    print(
        "file\tmode\titer\twarmup\tbytes\tread_ms\tparse_ms\tcount_ms\ttotal_ms\t"
        "parse_mib_s\ttotal_mib_s\trss_kb\thwm_kb\telements\tattributes\tnodes"
    )
    for raw_path in args.paths:
        path = Path(raw_path)
        started = time.perf_counter_ns()
        data = path.read_bytes()
        encoded_path = os.fsencode(path)
        read_ms = (time.perf_counter_ns() - started) / 1_000_000
        iterations = calibrate(library, data, encoded_path, args.iterations, args.min_duration_ms)

        best: Sample | None = None
        for _ in range(args.runs):
            sample = run_sample(library, data, encoded_path, iterations, args.warmup)
            if best is None or sample.parse_ms + sample.count_ms < best.parse_ms + best.count_ms:
                best = sample
        assert best is not None

        rss_kb, hwm_kb = memory_kb()
        parser_ms = best.parse_ms + best.count_ms
        print(
            "\t".join(
                (
                    os.fspath(path),
                    "python-libxml2-compact-walk",
                    str(best.iterations),
                    str(args.warmup),
                    str(len(data)),
                    f"{read_ms:.3f}",
                    f"{best.parse_ms:.3f}",
                    f"{best.count_ms:.3f}",
                    f"{read_ms + parser_ms:.3f}",
                    f"{mib_per_second(len(data), best.parse_ms):.1f}",
                    f"{mib_per_second(len(data), read_ms + parser_ms):.1f}",
                    str(rss_kb),
                    str(hwm_kb),
                    str(best.counts.elements),
                    str(best.counts.attributes),
                    str(best.counts.nodes),
                )
            )
        )


if __name__ == "__main__":
    main()
