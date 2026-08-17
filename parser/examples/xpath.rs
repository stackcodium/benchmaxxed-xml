use xml_parser::{XPathContext, XPathExpression, XmlDom};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse(
        "<catalog xmlns='urn:shop'><item price='12'>book</item><item price='3'>pen</item></catalog>",
    )?;
    let expression = XPathExpression::compile("/s:catalog/s:item[number(@price) >= $minimum]")?;
    let mut context = XPathContext::default();
    context.namespaces.bind("s", "urn:shop")?;
    context.variables.insert("minimum", 10.0)?;

    for item in document
        .select_elements_with_context(&expression, &context)?
        .into_vec()
    {
        println!("{}", item.text()?.unwrap_or_default());
    }
    Ok(())
}
