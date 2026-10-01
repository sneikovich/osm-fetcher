use crate::element::{Coord, ElementKind};
use std::fmt::Write;

pub const DEFAULT_TIMEOUT: u32 = 25;

/// Bounding box in Overpass order: south, west, north, east.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bbox {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Area {
    Bbox(Bbox),
    Around { center: Coord, radius_m: f64 },
}

#[derive(Debug, Clone, PartialEq)]
enum Filter {
    Eq(String, String),
    Exists(String),
}

/// Minimal builder for a single-statement Overpass QL query with `out center`.
#[derive(Debug, Clone)]
pub struct Query {
    kind: Option<ElementKind>,
    filters: Vec<Filter>,
    area: Option<Area>,
    timeout: u32,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            kind: None,
            filters: Vec::new(),
            area: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl Query {
    pub fn new() -> Self {
        Self::default()
    }

    /// Restrict to one element kind; by default nodes, ways and relations (`nwr`).
    pub fn kind(mut self, kind: ElementKind) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn tag(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.filters.push(Filter::Eq(key.into(), value.into()));
        self
    }

    pub fn tag_exists(mut self, key: impl Into<String>) -> Self {
        self.filters.push(Filter::Exists(key.into()));
        self
    }

    /// Parse a CLI-style filter: `key=value` → [`Self::tag`], bare `key` → [`Self::tag_exists`].
    pub fn tag_expr(self, expr: &str) -> Self {
        match expr.split_once('=') {
            Some((k, v)) => self.tag(k, v),
            None => self.tag_exists(expr),
        }
    }

    pub fn within(mut self, area: Area) -> Self {
        self.area = Some(area);
        self
    }

    /// Server-side timeout in seconds (`[timeout:N]`).
    pub fn timeout(mut self, secs: u32) -> Self {
        self.timeout = secs;
        self
    }

    pub fn timeout_secs(&self) -> u32 {
        self.timeout
    }

    pub fn to_ql(&self) -> String {
        let mut ql = format!("[out:json][timeout:{}];", self.timeout);
        ql.push_str(match self.kind {
            None => "nwr",
            Some(ElementKind::Node) => "node",
            Some(ElementKind::Way) => "way",
            Some(ElementKind::Relation) => "relation",
        });
        for f in &self.filters {
            match f {
                Filter::Eq(k, v) => write!(ql, "[\"{}\"=\"{}\"]", escape(k), escape(v)),
                Filter::Exists(k) => write!(ql, "[\"{}\"]", escape(k)),
            }
            .unwrap();
        }
        match self.area {
            Some(Area::Bbox(b)) => {
                write!(ql, "({},{},{},{})", b.south, b.west, b.north, b.east).unwrap()
            }
            Some(Area::Around { center, radius_m }) => {
                write!(ql, "(around:{},{},{})", radius_m, center.lat, center.lon).unwrap()
            }
            None => {}
        }
        ql.push_str(";out center;");
        ql
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_query() {
        let q = Query::new().tag("amenity", "cafe").within(Area::Bbox(Bbox {
            south: 50.4,
            west: 30.4,
            north: 50.5,
            east: 30.6,
        }));
        assert_eq!(
            q.to_ql(),
            r#"[out:json][timeout:25];nwr["amenity"="cafe"](50.4,30.4,50.5,30.6);out center;"#
        );
    }

    #[test]
    fn around_query_with_kind_exists_and_timeout() {
        let q = Query::new()
            .kind(ElementKind::Node)
            .tag("shop", "bakery")
            .tag_exists("name")
            .timeout(60)
            .within(Area::Around {
                center: Coord {
                    lat: 50.45,
                    lon: 30.52,
                },
                radius_m: 500.0,
            });
        assert_eq!(
            q.to_ql(),
            r#"[out:json][timeout:60];node["shop"="bakery"]["name"](around:500,50.45,30.52);out center;"#
        );
    }

    #[test]
    fn escapes_quotes_and_backslashes() {
        let q = Query::new().tag("name", r#"Say "hi" \o/"#);
        assert!(q.to_ql().contains(r#"["name"="Say \"hi\" \\o/"]"#));
    }
}
