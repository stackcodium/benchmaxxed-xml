use std::{hint::black_box, time::Instant};

use xml_parser::{count_document, parse_document_view, validate_document, XmlDom};

const CASES: [(usize, usize); 5] = [
    (1_000, 64),
    (2_000, 32),
    (4_000, 16),
    (8_000, 8),
    (16_000, 4),
];

#[derive(Clone, Copy)]
enum Mode {
    XmlDom,
    View,
    Validate,
    Count,
}

impl Mode {
    const ALL: [(Self, &'static str); 4] = [
        (Self::XmlDom, "xml-dom"),
        (Self::View, "view"),
        (Self::Validate, "validate"),
        (Self::Count, "count"),
    ];

    fn run(self, input: &str, expected_attributes: usize) {
        let attributes = match self {
            Self::XmlDom => XmlDom::parse(input).unwrap().tree_stats().attributes,
            Self::View => parse_document_view(input).unwrap().tree_stats().attributes,
            Self::Validate => {
                validate_document(input).unwrap();
                expected_attributes
            }
            Self::Count => count_document(input).unwrap().attributes,
        };
        assert_eq!(black_box(attributes), expected_attributes);
    }
}

fn main() {
    println!("mode\tattributes\titerations\tns_per_parse\tgrowth_from_previous");
    for (mode, label) in Mode::ALL {
        let mut previous = None;
        for (attributes, iterations) in CASES {
            let input = unique_attributes(attributes);
            mode.run(black_box(&input), attributes);

            let started = Instant::now();
            for _ in 0..iterations {
                mode.run(black_box(&input), attributes);
            }
            let nanos = started.elapsed().as_nanos() / iterations as u128;
            let growth = previous.map_or(0.0, |value| nanos as f64 / value as f64);
            println!("{label}\t{attributes}\t{iterations}\t{nanos}\t{growth:.3}");
            previous = Some(nanos);
        }
    }
}

fn unique_attributes(count: usize) -> String {
    let mut input = String::with_capacity(count * 16 + 4);
    input.push_str("<r");
    for index in 0..count {
        input.push_str(" a");
        input.push_str(&index.to_string());
        input.push_str("=\"v\"");
    }
    input.push_str("/>");
    input
}
