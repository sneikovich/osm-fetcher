use crate::element::{Coord, Element, Id, Tags};

pub trait Identified {
    fn id(&self) -> Id;
}

pub trait Tagged {
    fn tags(&self) -> &Tags;
    fn tag(&self, key: &str) -> Option<&str> {
        self.tags().get(key)
    }
    fn has_tag(&self, key: &str, value: &str) -> bool {
        self.tag(key) == Some(value)
    }

    fn name(&self) -> Option<&str> {
        self.tag("name")
    }
}

pub trait Located {
    fn coord(&self) -> Option<Coord>;
}

impl Identified for Element {
    fn id(&self) -> Id {
        match self {
            Element::Node { id, .. } | Element::Way { id, .. } | Element::Relation { id, .. } => {
                *id
            }
        }
    }
}

impl Tagged for Element {
    fn tags(&self) -> &Tags {
        match self {
            Element::Node { tags, .. }
            | Element::Way { tags, .. }
            | Element::Relation { tags, .. } => tags,
        }
    }
}

impl Located for Element {
    fn coord(&self) -> Option<Coord> {
        match self {
            Element::Node { lat, lon, .. } => Some(Coord {
                lat: *lat,
                lon: *lon,
            }),
            Element::Way { center, .. } | Element::Relation { center, .. } => *center,
        }
    }
}
