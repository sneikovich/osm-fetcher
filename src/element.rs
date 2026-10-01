use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Id(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct Coord {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Tags(HashMap<String, String>);

impl Tags {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ElementKind {
    Node,
    Way,
    Relation,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Member {
    #[serde(rename = "type")]
    pub kind: ElementKind,
    #[serde(rename = "ref")]
    pub id: Id,
    #[serde(default)]
    pub role: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Element {
    Node {
        id: Id,
        lat: f64,
        lon: f64,
        #[serde(default)]
        tags: Tags,
    },
    Way {
        id: Id,
        #[serde(default)]
        nodes: Vec<Id>,
        center: Option<Coord>,
        #[serde(default)]
        tags: Tags,
    },
    Relation {
        id: Id,
        #[serde(default)]
        members: Vec<Member>,
        center: Option<Coord>,
        #[serde(default)]
        tags: Tags,
    },
}

impl Element {
    pub fn kind(&self) -> ElementKind {
        match self {
            Element::Node { .. } => ElementKind::Node,
            Element::Way { .. } => ElementKind::Way,
            Element::Relation { .. } => ElementKind::Relation,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Response {
    pub elements: Vec<Element>,
    /// Overpass reports runtime errors (e.g. query timeout) here with HTTP 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remark: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{Located, Tagged};

    const SAMPLE: &str = r#"{
        "elements": [
            {"type": "node", "id": 1, "lat": 50.45, "lon": 30.52,
             "tags": {"amenity": "cafe", "name": "Кавуся"}},
            {"type": "way", "id": 2, "nodes": [10, 11],
             "center": {"lat": 50.44, "lon": 30.51},
             "tags": {"amenity": "cafe"}}
        ]
    }"#;

    #[test]
    fn parses_elements() {
        let r: Response = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(r.elements.len(), 2);

        let cafe = &r.elements[0];
        assert!(cafe.has_tag("amenity", "cafe"));
        assert_eq!(cafe.name(), Some("Кавуся"));
        assert!(r.elements[1].coord().is_some());
        assert!(r.remark.is_none());
    }

    #[test]
    fn parses_relation_members_and_remark() {
        let json = r#"{
            "remark": "runtime error: Query timed out",
            "elements": [
                {"type": "relation", "id": 3,
                 "members": [{"type": "way", "ref": 2, "role": "outer"}]}
            ]
        }"#;
        let r: Response = serde_json::from_str(json).unwrap();
        assert!(r.remark.unwrap().contains("timed out"));
        match &r.elements[0] {
            Element::Relation { members, .. } => {
                assert_eq!(members[0].kind, ElementKind::Way);
                assert_eq!(members[0].id, Id(2));
                assert_eq!(members[0].role, "outer");
            }
            other => panic!("expected relation, got {other:?}"),
        }
    }
}
