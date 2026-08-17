use xml_parser::XmlDom;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::build("catalog", |catalog| {
        catalog.comment("generated catalog");
        for (id, name) in [(1, "book"), (2, "pen")] {
            catalog.element("item", |item| {
                item.attribute_display("id", id)
                    .element("name", |name_element| {
                        name_element.text(name);
                    });
            });
        }
    })?;

    assert_eq!(document.select_elements("//item")?.len(), 2);
    println!("{}", document.to_xml_string()?);
    Ok(())
}
