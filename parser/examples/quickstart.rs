use xml_parser::XmlDom;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse("<catalog><item id='42'>old</item></catalog>")?;
    let catalog = document.root();
    let item = catalog.child("item")?.ok_or("missing item")?;
    assert_eq!(item.parse_attribute::<u32>("id")?, Some(42));

    item.set_text("updated")?;
    let added = catalog.append_element("item")?;
    added.set_attribute_typed("id", 43)?;
    added.set_text("book")?;

    assert_eq!(document.select_elements("//item")?.len(), 2);
    println!("{}", document.to_xml_string()?);
    Ok(())
}
