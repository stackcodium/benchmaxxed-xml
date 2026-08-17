use xml_parser::{
    parse_compact_document, parse_compact_document_bytes, parse_document_view,
    parse_document_view_with_source_offsets, parse_fragment, validate_document, XPathContext,
    XPathExpression, XPathVariables, XmlDeclarationMode, XmlDom, XmlDomError, XmlDomNodeSet,
    XmlElement, XmlErrorKind, XmlNode, XmlNodeKind, XmlOutputEncoding, XmlParser,
    XmlSerializeOptions, XmlValueError, XmlVersion, XmlWriteError,
};

#[test]
fn compact_document_moves_and_navigates() -> Result<(), Box<dyn std::error::Error>> {
    let source = "<?xml version='1.0'?><!--before--><!DOCTYPE r [<!ENTITY x 'value'>]><r a='&x;'><a>text</a><![CDATA[]]><?go now?></r><!--after-->";
    let document = parse_compact_document(source.to_owned())?;
    let moved = document;

    assert_eq!(moved.node_name(moved.root()), Some("r"));
    assert_eq!(moved.tree_stats().elements, 2);
    assert_eq!(moved.tree_stats().attributes, 1);
    assert!(moved
        .node_ids()
        .filter_map(|id| moved.node(id))
        .any(|node| node.kind() == XmlNodeKind::Cdata));
    Ok(())
}

#[test]
fn compact_document_keeps_strict_validation() {
    assert!(parse_compact_document("<r a='1' a='2'/>".to_owned()).is_err());
    assert!(parse_compact_document("<r>&missing;</r>".to_owned()).is_err());
    assert!(parse_compact_document("<r>\u{0}</r>".to_owned()).is_err());
    assert!(parse_compact_document("<r><a></r>".to_owned()).is_err());
}

#[test]
fn entity_graph_validation_memoizes_dags_without_changing_errors() {
    let dag = branching_entity_dag(28);
    assert_eq!(
        XmlDom::parse(dag)
            .unwrap()
            .root()
            .name()
            .unwrap()
            .as_deref(),
        Some("r")
    );

    let cycle = "<!DOCTYPE r [<!ENTITY a '&b;'><!ENTITY b '&a;'>]><r/>";
    let cycle_error = validate_document(cycle).unwrap_err();
    assert_eq!(
        cycle_error.kind,
        XmlErrorKind::EntityExpansionDepthLimitExceeded
    );
    assert_eq!(cycle_error.byte, cycle.find("<r/>").unwrap());

    let undeclared = "<!DOCTYPE r [<!ENTITY a '&missing;'>]><r/>";
    let undeclared_error = validate_document(undeclared).unwrap_err();
    assert_eq!(
        undeclared_error.kind,
        XmlErrorKind::UndeclaredEntity("missing".to_owned())
    );
    assert_eq!(undeclared_error.byte, undeclared.find("<r/>").unwrap());

    validate_document(&entity_chain(128)).unwrap();
    let excessive = entity_chain(129);
    let depth_error = validate_document(&excessive).unwrap_err();
    assert_eq!(
        depth_error.kind,
        XmlErrorKind::EntityExpansionDepthLimitExceeded
    );
    assert_eq!(depth_error.byte, excessive.find("<r/>").unwrap());

    validate_document("<!DOCTYPE r [<!ENTITY ext SYSTEM 'unused'>]><r/>").unwrap();
    let external = "<!DOCTYPE r [<!ENTITY ext SYSTEM 'unused'>]><r>&ext;</r>";
    let external_error = validate_document(external).unwrap_err();
    assert_eq!(
        external_error.kind,
        XmlErrorKind::ExternalEntityReference("ext".to_owned())
    );
    assert_eq!(external_error.byte, external.find("&ext;").unwrap());
}

#[test]
fn xpath_expression_depth_is_bounded_before_recursive_ast_work() {
    let document = XmlDom::parse("<r/>").unwrap();
    let control = nested_not(64);
    let compiled = XPathExpression::compile(&control).unwrap();
    assert!(document
        .evaluate_xpath_boolean_with_context(&compiled, &XPathContext::default())
        .unwrap());

    let deeply_parenthesized = format!("{}1{}", "(".repeat(20_000), ")".repeat(20_000));
    assert_xpath_depth_error(XPathExpression::compile(&deeply_parenthesized).unwrap_err());
    assert_xpath_depth_error(XPathExpression::compile(&nested_not(12_000)).unwrap_err());
    assert_xpath_depth_error(
        XPathExpression::compile(&format!("{}1", "-".repeat(20_000))).unwrap_err(),
    );
    assert_xpath_depth_error(
        XPathExpression::compile(&std::iter::repeat_n("1+", 20_000).collect::<String>())
            .unwrap_err(),
    );

    let nested_predicate = format!("//r[{}]", nested_not(12_000));
    assert_xpath_depth_error(XPathExpression::compile(&nested_predicate).unwrap_err());
    let inline_error = document
        .evaluate_xpath_boolean(&nested_not(12_000))
        .unwrap_err();
    assert!(inline_error
        .to_string()
        .contains("XPath expression depth limit exceeded"));
}

#[test]
fn xpath_accepts_nested_predicates_without_weakening_depth_limits() {
    let document = XmlDom::parse(
        "<catalog><item><price currency='USD' note=']'>12</price></item><item><price currency='EUR' note='x'>9</price></item></catalog>",
    )
    .unwrap();

    let selected = document
        .select_elements("//item[price[@currency = 'USD'][@note = ']']]")
        .unwrap();
    assert_eq!(selected.len(), 1);

    let unterminated = XPathExpression::compile("//item[price[@currency = 'USD']").unwrap_err();
    assert_eq!(unterminated.message, "unterminated XPath predicate");

    let excessive = format!("//r{}", "[child".repeat(97));
    assert_xpath_depth_error(XPathExpression::compile(&excessive).unwrap_err());
}

#[test]
fn constructed_tree_stats_and_standalone_xpath_are_stack_safe() {
    const DEPTH: usize = 4_096;

    let document = XmlDom::new("r").unwrap();
    document
        .root()
        .append_node(XmlNode::Element(deep_element_chain(DEPTH)))
        .unwrap();
    let stats = document.tree_stats();
    assert_eq!(stats.elements, DEPTH + 1);
    assert_eq!(stats.nodes, DEPTH + 1);
    let document_stats = document.document_stats();
    assert_eq!(document_stats.elements, DEPTH + 1);
    assert_eq!(document_stats.nodes, DEPTH + 1);

    let element = deep_element_chain(DEPTH);
    assert_eq!(
        element
            .select_elements("descendant-or-self::*")
            .unwrap()
            .len(),
        DEPTH
    );
}

fn deep_element_chain(depth: usize) -> XmlElement {
    let mut root = XmlElement::new("n").unwrap();
    let mut current = &mut root;
    for _ in 1..depth {
        current = current.append_element("n").unwrap();
    }
    root
}

fn branching_entity_dag(depth: usize) -> String {
    let mut input = String::from("<!DOCTYPE r [<!ENTITY e0 'x'>");
    for level in 1..=depth {
        input.push_str(&format!(
            "<!ENTITY e{level} '&e{};&e{};'>",
            level - 1,
            level - 1
        ));
    }
    input.push_str("]><r>&e0;</r>");
    input
}

fn entity_chain(depth: usize) -> String {
    let mut input = String::from("<!DOCTYPE r [");
    for level in 0..depth {
        if level + 1 == depth {
            input.push_str(&format!("<!ENTITY e{level} 'x'>"));
        } else {
            input.push_str(&format!("<!ENTITY e{level} '&e{};'>", level + 1));
        }
    }
    input.push_str("]><r/>");
    input
}

fn nested_not(depth: usize) -> String {
    format!("{}true(){}", "not(".repeat(depth), ")".repeat(depth))
}

fn assert_xpath_depth_error(error: xml_parser::XPathError) {
    assert_eq!(error.message, "XPath expression depth limit exceeded");
    assert!(error.byte > 0);
}

#[test]
fn compact_document_decodes_byte_inputs() -> Result<(), Box<dyn std::error::Error>> {
    let utf16: Vec<u8> = [0xfe, 0xff]
        .into_iter()
        .chain(
            "<?xml version='1.0' encoding='UTF-16'?><r>ok</r>"
                .encode_utf16()
                .flat_map(u16::to_be_bytes),
        )
        .collect();
    let document = parse_compact_document_bytes(&utf16)?;
    assert_eq!(document.node_name(document.root()), Some("r"));
    assert!(document
        .node_ids()
        .any(|id| document.node_value(id) == Some("ok")));
    Ok(())
}

#[test]
fn xml_dom_scan_borrows_compact_values_and_preserves_overlay_semantics(
) -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse("<r a='x&#x20;y'><item>A&amp;B</item><!--note--></r>")?;
    let mut observed = Vec::new();
    document.root().scan(|node| {
        let attributes = node
            .attributes()
            .map(|attribute| {
                let attribute = attribute?;
                Ok((attribute.name().to_owned(), attribute.value().to_owned()))
            })
            .collect::<Result<Vec<_>, XmlDomError>>()?;
        observed.push((
            node.kind(),
            node.name().map(str::to_owned),
            node.value()?.map(|value| value.into_owned()),
            attributes,
        ));
        Ok(())
    })?;
    assert_eq!(observed.len(), 4);
    assert_eq!(observed[0].3, [("a".to_owned(), "x y".to_owned())]);
    assert_eq!(observed[2].2.as_deref(), Some("A&B"));
    assert_eq!(observed[3].2.as_deref(), Some("note"));

    let element_names = document
        .root()
        .walk_elements()?
        .map(|node| node.name().unwrap().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(element_names, ["r", "item"]);
    let document_element_names = document
        .walk_elements()?
        .map(|node| node.name().unwrap().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(document_element_names, ["r", "item"]);

    document.root().set_attribute("a", "edited")?;
    let mut root_attribute = None;
    document.root().scan(|node| {
        if node.name() == Some("r") {
            root_attribute = node
                .attributes()
                .next()
                .transpose()?
                .map(|attribute| attribute.value().to_owned());
        }
        Ok(())
    })?;
    assert_eq!(root_attribute.as_deref(), Some("edited"));
    Ok(())
}

#[test]
fn xml_dom_scan_normalizes_literal_attribute_whitespace_in_compact_and_overlay_states(
) -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse("<r a='x\ny\tz'><item b='p\tq'/></r>")?;
    assert_eq!(document.root().attribute("a")?.as_deref(), Some("x y z"));

    let mut attributes = Vec::new();
    document.root().scan(|node| {
        for attribute in node.attributes() {
            let attribute = attribute?;
            attributes.push((attribute.name().to_owned(), attribute.value().to_owned()));
        }
        Ok(())
    })?;
    assert_eq!(
        attributes,
        [
            ("a".to_owned(), "x y z".to_owned()),
            ("b".to_owned(), "p q".to_owned())
        ]
    );

    document.root().set_attribute("a", "edited value")?;
    attributes.clear();
    document.root().scan(|node| {
        for attribute in node.attributes() {
            let attribute = attribute?;
            attributes.push((attribute.name().to_owned(), attribute.value().to_owned()));
        }
        Ok(())
    })?;
    assert_eq!(
        attributes,
        [
            ("a".to_owned(), "edited value".to_owned()),
            ("b".to_owned(), "p q".to_owned())
        ]
    );

    let large = XmlDom::parse(format!("<r a='large\nvalue'>{}</r>", "x".repeat(4096)))?;
    let mut large_attribute = None;
    large.root().scan(|node| {
        if node.name() == Some("r") {
            large_attribute = node
                .attributes()
                .next()
                .transpose()?
                .map(|attribute| attribute.value().to_owned());
        }
        Ok(())
    })?;
    assert_eq!(large_attribute.as_deref(), Some("large value"));
    Ok(())
}

#[test]
fn xml_dom_supports_shared_read_edit_query_and_write_workflow(
) -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse("<catalog><item id='42'>old</item></catalog>")?;
    let catalog = document.root();
    let item = catalog.child("item")?.ok_or("missing item")?;
    assert_eq!(item.parse_attribute::<u32>("id")?, Some(42));

    let added = catalog.append_element("item")?;
    added.set_attribute("id", "43")?;
    added.set_text("book")?;

    let values = catalog
        .children_named("item")?
        .map(|item| item.text().map(|value| value.unwrap_or_default()))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(values, ["old", "book"]);
    assert_eq!(document.select_elements("//item[@id='43']")?.len(), 1);
    assert_eq!(
        document.to_xml_string()?,
        "<catalog><item id=\"42\">old</item><item id=\"43\">book</item></catalog>"
    );
    Ok(())
}

#[test]
fn xml_dom_node_sets_support_borrowed_and_collected_workflows(
) -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse("<r><item id='1'/><item id='2'/></r>")?;
    let selected: XmlDomNodeSet = document.select_elements("//item[@id]")?;
    assert_eq!(selected[0].attribute("id")?.as_deref(), Some("1"));

    let cloned = selected.clone();
    let collected: Vec<_> = selected.into();
    assert_eq!(cloned.len(), 2);
    assert_eq!(collected[1].attribute("id")?.as_deref(), Some("2"));
    Ok(())
}

#[test]
fn xml_dom_handles_keep_identity_across_structural_edits() -> Result<(), Box<dyn std::error::Error>>
{
    let document = XmlDom::parse("<r><a/><b/></r>")?;
    let root = document.root();
    let a = root.child("a")?.expect("a");
    let b = root.child("b")?.expect("b");
    let b_id = b.id();
    root.prepend_element("first")?;

    assert_eq!(b.name()?.as_deref(), Some("b"));
    assert_eq!(b.id(), b_id);
    a.remove()?;
    assert_eq!(a.name(), Err(XmlDomError::DeletedHandle));
    assert_eq!(b.name()?.as_deref(), Some("b"));
    assert_eq!(root.children_named("first")?.count(), 1);
    Ok(())
}

#[test]
fn xml_dom_tolerant_parse_is_explicit_and_strict_parse_stays_atomic(
) -> Result<(), Box<dyn std::error::Error>> {
    let source = "<r><metadata>useful</metadata><broken></wrong>";
    let strict = XmlDom::parse(source).unwrap_err();
    let outcome = XmlDom::parse_tolerant(source)?;
    assert_eq!(outcome.diagnostic.as_ref(), Some(&strict));
    assert_eq!(outcome.consumed_bytes, source.find("</wrong>").unwrap());
    assert_eq!(
        outcome
            .value
            .root()
            .child("metadata")?
            .expect("metadata")
            .text()?
            .as_deref(),
        Some("useful")
    );
    assert!(XmlDom::parse(source).is_err());
    Ok(())
}

#[test]
fn xml_dom_supports_fragments_and_compiled_xpath() -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse("<r><a/><b score='2'/></r>")?;
    let root = document.root();
    let b = root.child("b")?.expect("b");
    b.insert_before(XmlNode::Comment("marker".into()))?;
    document
        .root()
        .append_fragment(parse_fragment("text<c/>")?)?;

    let expression = XPathExpression::compile("//b[number(@score) >= $minimum]")?;
    let mut variables = XPathVariables::default();
    variables.insert("minimum", 2.0)?;
    assert_eq!(
        document
            .select_elements_with_variables(&expression, &variables)?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn xml_dom_node_relative_xpath_supports_compiled_context_and_scalars(
) -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse(
        "<r xmlns:s='urn:shop'><group><s:item score='2'>A</s:item><s:item score='4'>B</s:item><plain score='3'/><!--note--><?go yes?></group></r>",
    )?;
    let group = document.root().child("group")?.expect("group");
    let mut context = XPathContext::default();
    context.namespaces.bind("s", "urn:shop")?;
    context.variables.insert("minimum", 3.0)?;

    let selected = XPathExpression::compile(".//s:item[number(@score) >= $minimum]")?;
    assert_eq!(
        group
            .select_elements_with_context(&selected, &context)?
            .len(),
        1
    );
    assert!(group.evaluate_xpath_boolean_with_context(&selected, &context)?);
    assert_eq!(
        group.evaluate_xpath_number_with_context(
            &XPathExpression::compile("count(.//s:item)")?,
            &context,
        )?,
        2.0
    );
    assert_eq!(
        group.evaluate_xpath_string_with_context(
            &XPathExpression::compile("string(.//s:item[1])")?,
            &context,
        )?,
        "A"
    );

    let all_kinds = group.select_nodes_with_context(
        &XPathExpression::compile(".//@score | .//comment() | .//processing-instruction()")?,
        &context,
    )?;
    assert_eq!(all_kinds.len(), 5);

    let plain = XPathExpression::compile("./plain[number(@score) >= $minimum]")?;
    assert_eq!(
        group
            .select_elements_with_variables(&plain, &context.variables)?
            .len(),
        1
    );
    assert_eq!(
        group
            .select_nodes_with_variables(
                &XPathExpression::compile("./plain/@score")?,
                &context.variables,
            )?
            .len(),
        1
    );
    assert!(group.evaluate_xpath_boolean("count(./plain) = 1")?);
    assert_eq!(group.evaluate_xpath_number("count(./plain)")?, 1.0);
    assert_eq!(group.evaluate_xpath_string("string(./plain/@score)")?, "3");
    Ok(())
}

#[test]
fn xml_dom_supports_namespaces_and_non_utf8_output() -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse(
        "<catalog xmlns='urn:shop' xmlns:m='urn:meta'><item m:key='7' plain='yes'>é</item></catalog>",
    )?;
    let expression = XPathExpression::compile("/s:catalog/s:item")?;
    let mut context = XPathContext::default();
    context.namespaces.bind("s", "urn:shop")?;
    assert_eq!(
        document
            .select_elements_with_context(&expression, &context)?
            .len(),
        1
    );
    let root = document.root();
    let expanded = root.expanded_name()?.expect("element expanded name");
    assert_eq!(expanded.local, "catalog");
    assert_eq!(expanded.namespace_uri.as_deref(), Some("urn:shop"));
    let item = root
        .child_ns(Some("urn:shop"), "item")?
        .expect("namespaced item");
    assert_eq!(
        item.attribute_ns(Some("urn:meta"), "key")?.as_deref(),
        Some("7")
    );
    assert_eq!(item.attribute_ns(None, "plain")?.as_deref(), Some("yes"));

    let options = XmlSerializeOptions {
        declaration: XmlDeclarationMode::Always,
        encoding: XmlOutputEncoding::Utf16Le,
        write_bom: true,
        ..XmlSerializeOptions::default()
    };
    let mut bytes = Vec::new();
    document.write_xml_with_options(&mut bytes, &options)?;
    let reparsed = XmlDom::parse_bytes(&bytes)?;
    assert_eq!(
        reparsed
            .root()
            .child("item")?
            .expect("item")
            .text()?
            .as_deref(),
        Some("é")
    );
    Ok(())
}

#[test]
fn xml_dom_selected_nodes_expose_kinds_snapshots_and_subtree_output(
) -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse(
        "<?xml version='1.0'?><r><item id='1'>text<![CDATA[data]]><!--note--><?go now?></item></r>",
    )?;
    let item = document.root().child("item")?.expect("item");
    assert_eq!(item.kind()?, XmlNodeKind::Element);
    assert_eq!(
        item.children()?
            .map(|child| child.kind())
            .collect::<Result<Vec<_>, _>>()?,
        [
            XmlNodeKind::Text,
            XmlNodeKind::Cdata,
            XmlNodeKind::Comment,
            XmlNodeKind::ProcessingInstruction,
        ]
    );
    assert_eq!(
        item.to_inner_xml_string()?,
        "text<![CDATA[data]]><!--note--><?go now?>"
    );
    assert!(!item.to_xml_string()?.contains("<?xml"));
    let XmlNode::Element(snapshot) = item.snapshot()? else {
        return Err("selected element snapshot changed kind".into());
    };
    assert_eq!(snapshot.name(), "item");
    assert_eq!(snapshot.attribute("id").expect("id").value(), "1");
    Ok(())
}

#[test]
fn xml_dom_clone_share_and_send_ownership_are_explicit() -> Result<(), Box<dyn std::error::Error>> {
    fn assert_send<T: Send>() {}

    fn assert_send_sync<T: Send + Sync>() {}
    assert_send::<xml_parser::XmlDomSend>();
    assert_send_sync::<xml_parser::XmlCompactDocument>();

    let document = XmlDom::parse("<r><item>before</item></r>")?;
    let shared = document.share();
    shared.root().set_attribute("shared", "yes")?;
    assert_eq!(document.root().attribute("shared")?.as_deref(), Some("yes"));

    let independent = document.clone();
    independent.root().set_attribute("independent", "yes")?;
    assert_eq!(document.root().attribute("independent")?, None);

    drop(shared);
    let send = document
        .try_into_send()
        .map_err(|_| "unique document unexpectedly failed thread-transfer conversion")?;
    let send = std::thread::spawn(move || -> Result<_, String> {
        let document = send.into_local();
        if document
            .root()
            .attribute("shared")
            .map_err(|error| error.to_string())?
            .as_deref()
            != Some("yes")
        {
            return Err("worker lost parsed/edited document state".into());
        }
        document
            .root()
            .set_attribute("worker", "done")
            .map_err(|error| error.to_string())?;
        document
            .try_into_send()
            .map_err(|_| "worker retained an unexpected local alias".into())
    })
    .join()
    .map_err(|_| "worker panicked")??;
    let returned = send.into_local();
    assert_eq!(
        returned.root().attribute("worker")?.as_deref(),
        Some("done")
    );
    Ok(())
}

#[test]
fn borrowed_view_and_source_offsets_remain_available() -> Result<(), Box<dyn std::error::Error>> {
    let source = "<!--top--><root id = '7'><item>text</item></root>";
    let view = parse_document_view(source)?;
    assert_eq!(view.node_name(view.root()), Some("root"));
    let raw_attribute = *view.attributes().first().expect("raw attribute");
    assert_eq!(raw_attribute.name(view.raw_source()), Some("id"));
    assert_eq!(raw_attribute.value(view.raw_source()), Some("7"));
    let foreign = parse_document_view("<root id = '8'/>")?;
    assert_eq!(raw_attribute.name(foreign.raw_source()), None);
    assert_eq!(raw_attribute.value(foreign.raw_source()), None);

    let sourced = parse_document_view_with_source_offsets(source)?;
    let item = sourced
        .view
        .children(sourced.view.root())
        .next()
        .expect("item");
    assert_eq!(
        &source[sourced.node_span(item).expect("span").as_range()],
        "<item>text</item>"
    );
    Ok(())
}

#[test]
fn reusable_parser_exposes_one_coherent_input_and_representation_matrix(
) -> Result<(), Box<dyn std::error::Error>> {
    let parser = XmlParser::preserving_all();
    let source = "<!--before--><root><item>text</item></root>";

    assert_eq!(
        parser.parse(source)?.root().name()?.as_deref(),
        Some("root")
    );
    assert_eq!(
        parser
            .parse_bytes(source.as_bytes())?
            .root()
            .name()?
            .as_deref(),
        Some("root")
    );
    assert_eq!(
        parser
            .read(std::io::Cursor::new(source.as_bytes()))?
            .root()
            .name()?
            .as_deref(),
        Some("root")
    );
    assert_eq!(parser.parse_compact(source)?.tree_stats().elements, 2);
    assert_eq!(
        parser
            .parse_compact_bytes(source.as_bytes())?
            .tree_stats()
            .elements,
        2
    );
    assert_eq!(parser.parse_view(source)?.tree_stats().elements, 2);
    assert_eq!(
        parser
            .parse_view_with_source_offsets(source)?
            .view
            .tree_stats()
            .elements,
        2
    );
    assert_eq!(parser.parse_fragment("<a/><b/>")?.nodes().len(), 2);
    assert_eq!(parser.count(source)?.elements, 2);
    assert_eq!(parser.count_bytes(source.as_bytes())?.elements, 2);
    parser.validate(source)?;
    parser.validate_bytes(source.as_bytes())?;

    let tolerant = parser.parse_tolerant("<root><ok/><broken></wrong>")?;
    assert!(tolerant.diagnostic.is_some());
    assert_eq!(tolerant.value.root().name()?.as_deref(), Some("root"));
    assert!(parser
        .parse_compact_tolerant("<root><ok/><broken></wrong>")?
        .diagnostic
        .is_some());
    assert!(parser
        .parse_fragment_tolerant("<ok/><broken></wrong>")?
        .diagnostic
        .is_some());
    Ok(())
}

#[test]
fn facade_typed_values_preserve_the_target_parse_error() -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse("<root answer='forty-two'>NaN?</root>")?;
    let root = document.root();

    assert!(matches!(
        root.parse_attribute::<u32>("answer"),
        Err(XmlValueError::Parse(_))
    ));
    assert!(matches!(
        root.parse_text::<f64>(),
        Err(XmlValueError::Parse(_))
    ));
    Ok(())
}

#[test]
fn element_only_xpath_consistently_filters_non_element_nodes(
) -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse("<root id='7'><child/></root>")?;
    assert!(document.root().select_elements("@id")?.is_empty());
    assert_eq!(document.root().select_nodes("@id")?.len(), 1);

    let snapshot = document.root().snapshot()?;
    let element = snapshot.as_element().expect("root element snapshot");
    assert!(element.select_elements("@id")?.is_empty());
    assert_eq!(element.select_nodes("@id")?.len(), 1);
    Ok(())
}

#[test]
fn compact_and_borrowed_nodes_share_ergonomic_navigation_and_scalar_access(
) -> Result<(), Box<dyn std::error::Error>> {
    let source = "<root id='7'><!--note--><?go now?><item>text</item></root>";
    let compact = parse_compact_document(source.to_owned())?;
    let compact_root = compact.root_node();
    assert_eq!(compact_root.name(), Some("root"));
    assert_eq!(
        compact_root
            .attribute("id")
            .map(|attribute| attribute.raw_value()),
        Some("7")
    );
    assert_eq!(
        compact_root.child("item").and_then(|node| node.name()),
        Some("item")
    );
    assert_eq!(
        compact_root
            .children()
            .find(|node| node.kind() == XmlNodeKind::Comment)
            .and_then(|node| node.raw_value()),
        Some("note")
    );
    assert_eq!(
        compact_root
            .children()
            .find(|node| node.kind() == XmlNodeKind::ProcessingInstruction)
            .and_then(|node| node.raw_value()),
        Some("now")
    );

    let view = parse_document_view(source)?;
    let view_root = view.root_node();
    assert_eq!(view_root.name(), compact_root.name());
    assert_eq!(
        view_root
            .attribute("id")
            .map(|attribute| attribute.raw_value()),
        Some("7")
    );
    assert_eq!(
        view_root.children().count(),
        compact_root.children().count()
    );
    Ok(())
}

#[test]
fn compact_metadata_version_and_serialization_are_directly_accessible(
) -> Result<(), Box<dyn std::error::Error>> {
    let source = "<?xml version='1.1'?><!--before--><!DOCTYPE root><root/><!--after-->";
    let parser = XmlParser::preserving_all();
    let compact = parser.parse_compact(source)?;

    assert_eq!(compact.version(), XmlVersion::Xml11);
    assert_eq!(compact.declaration().map(|pi| pi.target()), Some("xml"));
    assert_eq!(
        compact.doctype().map(|doctype| doctype.name()),
        Some("root")
    );
    assert_eq!(compact.misc_before_root().len(), 1);
    assert_eq!(compact.misc_after_root().len(), 1);
    assert_eq!(compact.parse_options(), parser.options());
    assert!(compact.to_xml_string()?.contains("<!DOCTYPE root>"));

    let options = XmlSerializeOptions {
        declaration: XmlDeclarationMode::Always,
        version: XmlVersion::Xml11,
        ..XmlSerializeOptions::default()
    };
    let output = compact.to_xml_string_with_options(&options)?;
    assert!(output.starts_with("<?xml "));
    assert_eq!(parser.parse_view(&output)?.version(), XmlVersion::Xml11);

    assert_eq!(parser.parse_view(source)?.version(), XmlVersion::Xml11);
    Ok(())
}

#[test]
fn constructed_element_serialization_is_stack_safe_and_preserves_depth_limits(
) -> Result<(), Box<dyn std::error::Error>> {
    const DEPTH: usize = 20_000;
    let output = std::thread::Builder::new()
        .stack_size(1_024 * 1_024)
        .spawn(|| -> Result<String, XmlWriteError> {
            let mut root = XmlElement::new("n").expect("valid element name");
            let mut current = &mut root;
            for _ in 1..DEPTH {
                current = current.append_element("n").expect("valid element name");
            }
            current.set_attribute("a", "<&").expect("valid attribute");
            current
                .append_child(XmlNode::Text("<&".to_owned()))
                .expect("valid text");
            root.to_xml_string_with_options(&XmlSerializeOptions {
                max_depth: DEPTH,
                ..XmlSerializeOptions::default()
            })
        })?
        .join()
        .map_err(|_| "deep serialization thread panicked")??;
    assert!(output.starts_with("<n><n>"));
    assert!(output.contains("<n a=\"&lt;&amp;\">&lt;&amp;</n>"));
    assert!(output.ends_with("</n></n>"));

    let mut limited = XmlElement::new("n")?;
    let mut current = &mut limited;
    for _ in 0..140 {
        current = current.append_element("n")?;
    }
    assert!(matches!(
        limited.to_xml_string_with_options(&XmlSerializeOptions {
            max_depth: 100,
            ..XmlSerializeOptions::default()
        }),
        Err(XmlWriteError::DepthLimitExceeded)
    ));
    Ok(())
}

#[test]
fn constructed_element_debug_is_stack_safe_and_usefully_bounded(
) -> Result<(), Box<dyn std::error::Error>> {
    let shallow = XmlElement::with_text("n", "x")?;
    assert_eq!(
        format!("{shallow:?}"),
        "XmlElement { name: \"n\", attributes: [], children: [Text(\"x\")] }"
    );

    let (compact, pretty) = std::thread::Builder::new()
        .stack_size(512 * 1_024)
        .spawn(|| {
            let mut root = XmlElement::new("n").expect("valid element name");
            let mut current = &mut root;
            for _ in 1..20_000 {
                current = current.append_element("n").expect("valid element name");
            }
            (format!("{root:?}"), format!("{root:#?}"))
        })?
        .join()
        .map_err(|_| "deep debug-formatting thread panicked")?;
    for rendered in [&compact, &pretty] {
        assert!(rendered.starts_with("XmlElement"));
        assert!(rendered.contains("children"));
        assert!(rendered.contains(".."));
        assert!(rendered.len() < 1_000_000);
    }
    Ok(())
}

#[test]
fn sparse_root_relocation_uses_the_structural_root_boundary(
) -> Result<(), Box<dyn std::error::Error>> {
    let sources = [
        "<catalog><item id='1'/><item id='2'/></catalog><!-- </catalog> -->",
        "<?before x?><catalog><item id='1'/><item id='2'/></catalog><?after </catalog>?><!-- </catalog> -->",
        "<catalog note='&lt;/catalog>'><item id='1'/><item id='2'/><![CDATA[</catalog>]]></catalog><!--tail-->",
    ];
    for source in sources {
        let document = XmlDom::parse(source)?;
        let root = document.root();
        let first = root.children()?.next().ok_or("missing first root child")?;
        let child_count = root.children()?.count();
        first.move_to(&root, child_count)?;

        let serialized = document.to_xml_string()?;
        let reparsed = XmlDom::parse(&serialized)?;
        assert_eq!(reparsed.select_elements("//item")?.len(), 2, "{serialized}");
        assert_eq!(
            reparsed
                .select_elements("//item")?
                .iter()
                .map(|item| item.attribute("id").unwrap().unwrap())
                .collect::<Vec<_>>(),
            ["2", "1"],
            "{serialized}"
        );

        let next = root
            .children()?
            .next()
            .ok_or("missing relocated root child")?;
        let child_count = root.children()?.count();
        next.move_to(&root, child_count)?;
        let repeated = document.to_xml_string()?;
        assert_eq!(
            XmlDom::parse(&repeated)?
                .select_elements("//item")?
                .iter()
                .map(|item| item.attribute("id").unwrap().unwrap())
                .collect::<Vec<_>>(),
            ["1", "2"],
            "{repeated}"
        );
    }
    Ok(())
}

#[test]
fn xpath_following_axis_preserves_union_and_predicate_semantics(
) -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse(
        "<r><section id='outer'><section id='inner'/><inside/></section><middle/><section id='last'><n/></section><tail/></r>",
    )?;
    let names = document
        .select_elements("//section/following::*")?
        .iter()
        .map(|node| node.name().unwrap().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["inside", "middle", "section", "n", "tail"]);

    let first_following = document
        .select_elements("//section/following::*[position() = 1]")?
        .iter()
        .map(|node| node.name().unwrap().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(first_following, ["inside", "middle", "tail"]);

    let mixed =
        XmlDom::parse("<r xmlns:p='urn:test'><section id='s'>text<child/></section><after/></r>")?;
    for query in [
        "//@id/following::*",
        "//section/text()/following::*",
        "//section/namespace::p/following::*",
    ] {
        assert_eq!(
            mixed
                .select_elements(query)?
                .iter()
                .map(|node| node.name().unwrap().unwrap())
                .collect::<Vec<_>>(),
            ["child", "after"],
            "{query}"
        );
    }
    mixed.root().set_attribute("changed", "yes")?;
    assert_eq!(
        mixed
            .select_elements("//section/following::*")?
            .iter()
            .map(|node| node.name().unwrap().unwrap())
            .collect::<Vec<_>>(),
        ["after"]
    );

    let mut standalone = XmlElement::new("r")?;
    let first = standalone.append_element("section")?;
    first.append_element("child")?;
    standalone.append_element("after")?;
    assert_eq!(
        standalone
            .select_elements("descendant::section/following::*")?
            .iter()
            .map(|element| element.name())
            .collect::<Vec<_>>(),
        ["after"]
    );
    Ok(())
}

#[test]
fn xpath_preceding_axis_preserves_union_and_predicate_semantics(
) -> Result<(), Box<dyn std::error::Error>> {
    let document = XmlDom::parse(
        "<r><before/><section id='outer'><lead/><section id='inner'/></section><middle/><section id='last'><n/></section><tail/></r>",
    )?;
    let names = document
        .select_elements("//section/preceding::*")?
        .iter()
        .map(|node| node.name().unwrap().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["before", "section", "lead", "section", "middle"]);

    let nearest_preceding = document
        .select_elements("//section/preceding::*[position() = 1]")?
        .iter()
        .map(|node| node.name().unwrap().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(nearest_preceding, ["before", "lead", "middle"]);

    let mixed = XmlDom::parse(
        "<r xmlns:p='urn:test'><before/><section id='s'>text<child/></section><after/></r>",
    )?;
    for query in [
        "//@id/preceding::*",
        "//section/text()/preceding::*",
        "//section/namespace::p/preceding::*",
    ] {
        assert_eq!(
            mixed
                .select_elements(query)?
                .iter()
                .map(|node| node.name().unwrap().unwrap())
                .collect::<Vec<_>>(),
            ["before"],
            "{query}"
        );
    }
    mixed.root().set_attribute("changed", "yes")?;
    assert_eq!(
        mixed
            .select_elements("//section/preceding::*")?
            .iter()
            .map(|node| node.name().unwrap().unwrap())
            .collect::<Vec<_>>(),
        ["before"]
    );

    let mut standalone = XmlElement::new("r")?;
    standalone.append_element("before")?;
    let section = standalone.append_element("section")?;
    section.append_element("child")?;
    assert_eq!(
        standalone
            .select_elements("descendant::section/preceding::*")?
            .iter()
            .map(|element| element.name())
            .collect::<Vec<_>>(),
        ["before"]
    );
    Ok(())
}

#[test]
fn xpath_nodeset_comparisons_preserve_existential_rules() -> Result<(), Box<dyn std::error::Error>>
{
    let document = XmlDom::parse(
        "<r><a>x</a><a>y</a><a>bad</a><a>2</a><b>x</b><b>3</b><same>z</same><same>z</same></r>",
    )?;
    assert!(document.evaluate_xpath_boolean("//a = //b")?);
    assert!(document.evaluate_xpath_boolean("//a != //b")?);
    assert!(!document.evaluate_xpath_boolean("//same != //same")?);
    assert!(!document.evaluate_xpath_boolean("//missing = //a")?);
    assert!(!document.evaluate_xpath_boolean("//missing != //a")?);
    assert!(document.evaluate_xpath_boolean("//a < //b")?);
    assert!(document.evaluate_xpath_boolean("//b > //a")?);
    assert!(document.evaluate_xpath_boolean("//a <= //b")?);
    assert!(document.evaluate_xpath_boolean("//b >= //a")?);

    let strings = XmlDom::parse(
        "<r><left>one<!--ignored--><nested>two</nested>three</left><right>onetwothree</right><number>NaN</number><number>-2</number><limit>-1</limit></r>",
    )?;
    assert!(strings.evaluate_xpath_boolean("//left = //right")?);
    assert!(!strings.evaluate_xpath_boolean("//number > //limit")?);
    assert!(strings.evaluate_xpath_boolean("//number < //limit")?);
    assert!(strings.evaluate_xpath_boolean("//number = '-2'")?);
    Ok(())
}
