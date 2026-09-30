use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(transparent)]
pub struct Id(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Coord {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct Tags(HashMap<String, String>);

impl Tags {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
}

#[derive(Debug, Clone, Deserialize)]
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
        center: Option<Coord>,
        #[serde(default)]
        tags: Tags,
    },
}

#[derive(Debug, Deserialize)]
pub struct Response {
    pub elements: Vec<Element>,
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
    }
}
