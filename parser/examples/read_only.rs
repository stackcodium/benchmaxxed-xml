use xml_parser::{count_document, parse_compact_document, parse_document_view, validate_document};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = "<catalog><item id='1'>book</item><item id='2'>pen</item></catalog>";

    validate_document(source)?;
    let counts = count_document(source)?;
    assert_eq!(counts.elements, 3);

    let view = parse_document_view(source)?;
    let first_item = view.children(view.root()).next().expect("first item");
    assert_eq!(view.node_name(first_item), Some("item"));

    let compact = parse_compact_document(source.to_string())?;
    let compact_item = compact
        .children(compact.root())
        .next()
        .expect("first compact item");
    assert_eq!(compact.node_name(compact_item), Some("item"));

    println!(
        "elements={} attributes={} nodes={}",
        counts.elements, counts.attributes, counts.nodes
    );
    Ok(())
}
