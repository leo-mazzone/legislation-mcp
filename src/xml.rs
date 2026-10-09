//! Small namespace-aware XML tree retaining mixed content in document order.
use quick_xml::{events::Event, reader::NsReader};
use serde_json::{json, Value};

#[derive(Debug, Default)]
pub struct Node {
    pub name: String,
    pub ns: String,
    pub attrs: Vec<(String, String)>,
    pub parts: Vec<Part>,
}

#[derive(Debug)]
pub enum Part {
    Text(String),
    Child(Node),
}

impl Node {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
    pub fn children(&self) -> impl Iterator<Item = &Node> {
        self.parts.iter().filter_map(|part| match part {
            Part::Child(node) => Some(node),
            Part::Text(_) => None,
        })
    }
    pub fn find<'a>(&'a self, name: &str, out: &mut Vec<&'a Node>) {
        if self.name == name {
            out.push(self);
        }
        for child in self.children() {
            child.find(name, out);
        }
    }
    pub fn all(&self, name: &str) -> Vec<&Node> {
        let mut nodes = Vec::new();
        self.find(name, &mut nodes);
        nodes
    }
    pub fn text(&self) -> String {
        let mut out = String::new();
        self.render(&mut out);
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }
    pub fn readable(&self) -> String {
        let mut out = String::new();
        self.render(&mut out);
        out.lines()
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn render(&self, out: &mut String) {
        let block = matches!(
            self.name.as_str(),
            "Text"
                | "Title"
                | "Number"
                | "Pnumber"
                | "P"
                | "P1"
                | "P2"
                | "P3"
                | "P4"
                | "P5"
                | "Para"
                | "tr"
                | "Row"
        );
        if block {
            out.push('\n');
        }
        for part in &self.parts {
            match part {
                Part::Text(text) => out.push_str(text),
                Part::Child(child) => child.render(out),
            }
        }
        if matches!(self.name.as_str(), "td" | "th" | "Entry") {
            out.push_str(" | ");
        }
        if block {
            out.push('\n');
        }
    }
}

pub fn parse(xml: &str) -> Result<Node, String> {
    let mut reader = NsReader::from_str(xml);
    let mut stack = vec![Node::default()];
    loop {
        let (ns, event) = reader
            .read_resolved_event()
            .map_err(|_| "invalid_xml: malformed upstream XML".to_string())?;
        match event {
            Event::Start(ref tag) | Event::Empty(ref tag) => {
                let ns = match ns {
                    quick_xml::name::ResolveResult::Bound(ns) => {
                        String::from_utf8_lossy(ns.as_ref()).into_owned()
                    }
                    quick_xml::name::ResolveResult::Unbound => String::new(),
                    quick_xml::name::ResolveResult::Unknown(_) => {
                        return Err("invalid_xml: undeclared namespace".into())
                    }
                };
                let mut node = Node {
                    name: String::from_utf8_lossy(tag.local_name().as_ref()).into_owned(),
                    ns,
                    ..Default::default()
                };
                for attr in tag.attributes() {
                    let attr = attr.map_err(|_| "invalid_xml: invalid attribute".to_string())?;
                    node.attrs.push((
                        String::from_utf8_lossy(attr.key.local_name().as_ref()).into_owned(),
                        attr.decode_and_unescape_value(reader.decoder())
                            .map_err(|_| "invalid_xml: invalid attribute value".to_string())?
                            .into_owned(),
                    ));
                }
                if matches!(event, Event::Empty(_)) {
                    stack.last_mut().unwrap().parts.push(Part::Child(node));
                } else {
                    stack.push(node);
                }
            }
            Event::End(_) => {
                if stack.len() <= 1 {
                    return Err("invalid_xml: unexpected closing tag".into());
                }
                let node = stack.pop().unwrap();
                stack.last_mut().unwrap().parts.push(Part::Child(node));
            }
            Event::Text(text) => {
                let decoded = text
                    .decode()
                    .map_err(|_| "invalid_xml: unsupported encoding".to_string())?;
                let text = quick_xml::escape::unescape(&decoded)
                    .map_err(|_| "invalid_xml: invalid entity".to_string())?
                    .into_owned();
                stack.last_mut().unwrap().parts.push(Part::Text(text));
            }
            Event::CData(text) => {
                stack.last_mut().unwrap().parts.push(Part::Text(
                    text.decode()
                        .map_err(|_| "invalid_xml: invalid CDATA".to_string())?
                        .into_owned(),
                ));
            }
            Event::GeneralRef(reference) => {
                let reference = reference
                    .decode()
                    .map_err(|_| "invalid_xml: invalid reference".to_string())?;
                let escaped = format!("&{reference};");
                let text = quick_xml::escape::unescape(&escaped)
                    .map_err(|_| "invalid_xml: unsupported entity".to_string())?
                    .into_owned();
                stack.last_mut().unwrap().parts.push(Part::Text(text));
            }
            Event::DocType(_) => return Err("invalid_xml: DTDs are not supported".into()),
            Event::Eof => break,
            _ => {}
        }
        if stack.len() > 128 {
            return Err("invalid_xml: excessive nesting".into());
        }
    }
    if stack.len() != 1 {
        return Err("invalid_xml: incomplete XML".into());
    }
    let root = stack.pop().unwrap();
    if root.children().count() != 1 {
        return Err("invalid_xml: expected one root element".into());
    }
    Ok(root)
}

pub fn search(root: &Node) -> Result<Value, String> {
    let feed = root.children().next().unwrap();
    if feed.name != "feed" || feed.ns != "http://www.w3.org/2005/Atom" {
        return Err("invalid_xml: expected Atom feed".into());
    }
    let results: Vec<Value> = feed.children().filter(|node| node.name == "entry").map(|entry| {
        let value = |name| entry.children().find(|node| node.name == name).map(Node::text);
        let url = entry.children().find(|node| node.name == "link" && node.attr("rel") == Some("alternate"))
            .and_then(|node| node.attr("href")).map(str::to_owned).or_else(|| value("id"));
        let identifier = url.as_deref().and_then(identifier_from_url);
        json!({"title": value("title"), "identifier": identifier, "source_url": url, "updated": value("updated")})
    }).collect();
    let next = feed
        .children()
        .find(|node| node.name == "link" && node.attr("rel") == Some("next"))
        .and_then(|node| node.attr("href"));
    Ok(json!({"results": results, "next_page_url": next}))
}

pub fn identifier_from_url(url: &str) -> Option<String> {
    let url = reqwest::Url::parse(url).ok()?;
    if url.host_str()? != "www.legislation.gov.uk" {
        return None;
    }
    let parts: Vec<_> = url
        .path()
        .trim_matches('/')
        .split('/')
        .filter(|part| *part != "id")
        .collect();
    if parts.len() < 3 {
        return None;
    }
    let count = if parts[1].bytes().all(|b| b.is_ascii_digit()) {
        3
    } else {
        4
    };
    if parts.len() < count {
        return None;
    }
    Some(parts[..count].join("/"))
}

pub fn document(root: &Node, url: &str, contents: bool) -> Result<Value, String> {
    if root.all("Legislation").is_empty() {
        return Err("pdf_only_or_unavailable: structured legislation XML is unavailable; open the source URL".into());
    }
    let metadata = root.all("Metadata");
    let title = metadata
        .iter()
        .flat_map(|node| node.all("title"))
        .next()
        .map(|node| node.text());
    let metadata_elements: Vec<_> = metadata.iter().flat_map(|node| node.children()).map(|node| {
        json!({"name": node.name, "namespace": node.ns, "attributes": node.attrs, "text": node.text()})
    }).collect();
    let extents: Vec<_> = root
        .all("Legislation")
        .iter()
        .flat_map(|node| collect_extents(node))
        .collect();
    let warnings: Vec<_> = root
        .all("UnappliedEffects")
        .iter()
        .chain(root.all("UnappliedEffect").iter())
        .map(|node| json!({"text": node.text(), "attributes": node.attrs}))
        .collect();
    let mut response = json!({
        "title": title, "source_url": url, "version": "latest available from legislation.gov.uk",
        "document_attributes": root.all("Legislation")[0].attrs,
        "metadata": metadata_elements, "extent": extents, "outstanding_amendments": warnings,
        "interpretation_note": "Latest available text is not a guarantee that every amendment has been applied or that every provision is in force. Absence of a warning is not confirmation of status."
    });
    if contents {
        let nodes = root.all("Contents");
        if nodes.is_empty() {
            return Err("contents_unavailable: no structured contents were returned".into());
        }
        response["contents"] = Value::Array(
            nodes
                .iter()
                .flat_map(|node| contents_items(node, 0))
                .collect(),
        );
    } else {
        let nodes: Vec<_> = root.all("Legislation")[0]
            .children()
            .filter(|node| {
                !matches!(
                    node.name.as_str(),
                    "Metadata" | "Contents" | "Commentaries" | "Versions" | "Resources"
                )
            })
            .collect();
        let text = nodes
            .iter()
            .map(|node| node.readable())
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            return Err("pdf_only_or_unavailable: no provision text was returned".into());
        }
        let truncated = text.chars().count() > 30_000;
        response["text"] = json!(text.chars().take(30_000).collect::<String>());
        response["truncated"] = json!(truncated);
    }
    Ok(response)
}

fn collect_extents(node: &Node) -> Vec<Value> {
    let mut values = Vec::new();
    if let Some(extent) = node.attr("RestrictExtent") {
        values.push(json!({"id": node.attr("id"), "extent": extent}));
    }
    for child in node.children() {
        values.extend(collect_extents(child));
    }
    values
}

fn contents_items(node: &Node, depth: usize) -> Vec<Value> {
    let mut out = Vec::new();
    for child in node.children() {
        if child.name.starts_with("Contents")
            && !matches!(child.name.as_str(), "ContentsTitle" | "ContentsNumber")
        {
            let title = child
                .children()
                .find(|node| node.name == "ContentsTitle")
                .map(Node::text);
            let number = child
                .children()
                .find(|node| node.name == "ContentsNumber")
                .map(Node::text);
            let reference = child.attr("ContentRef");
            if title.is_some() || reference.is_some() {
                out.push(
                    json!({"depth": depth, "title": title, "number": number, "reference": reference,
                    "provision": reference.map(|value| value.replace('-', "/"))}),
                );
            }
            out.extend(contents_items(child, depth + 1));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atom_pagination_and_identifier() {
        let root = parse(include_str!("../tests/fixtures/search.xml")).unwrap();
        let value = search(&root).unwrap();
        assert_eq!(value["results"][0]["identifier"], "ukpga/2010/15");
        assert!(value["next_page_url"].as_str().unwrap().contains("page=2"));
        assert_eq!(
            search(&parse("<feed xmlns='http://www.w3.org/2005/Atom'/>").unwrap()).unwrap()
                ["results"],
            json!([])
        );
    }
    #[test]
    fn mixed_content_tables_and_warnings() {
        let root = parse(include_str!("../tests/fixtures/provision.xml")).unwrap();
        let value = document(
            &root,
            "https://www.legislation.gov.uk/ukpga/2010/15/section/20",
            false,
        )
        .unwrap();
        let text = value["text"].as_str().unwrap();
        assert!(text.contains("A reasonable adjustment & assistance."));
        assert!(text.contains("Column A | Column B |"));
        assert!(!value["outstanding_amendments"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(value["extent"][0]["extent"], "E+W+S");
        assert!(!text.contains("Synthetic metadata"));
    }
    #[test]
    fn contents_and_malformed_xml() {
        let root = parse(include_str!("../tests/fixtures/provision.xml")).unwrap();
        assert_eq!(
            document(&root, "source", true).unwrap()["contents"][0]["provision"],
            "section/20"
        );
        assert!(parse("<a><b></a>").is_err());
        assert!(parse("<!DOCTYPE a><a/>").is_err());
        assert!(document(&parse("<html/>").unwrap(), "source", false).is_err());
    }
}
