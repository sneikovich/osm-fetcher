use clap::{ArgGroup, Parser, ValueEnum};
use overpass::client::{DEFAULT_ENDPOINT, DEFAULT_RETRIES};
use overpass::query::DEFAULT_TIMEOUT;
use overpass::{Area, Bbox, Client, Coord, ElementKind, Error, Identified, Located, Query, Tagged};
use std::process::ExitCode;
use std::time::Duration;

/// Fetch OpenStreetMap elements from the Overpass API.
#[derive(Parser)]
#[command(version, group(ArgGroup::new("source").required(true).multiple(true).args(["tag", "ql"])))]
struct Cli {
    /// Tag filter: `key=value` or just `key` (repeatable)
    #[arg(long, short)]
    tag: Vec<String>,

    /// Bounding box: south,west,north,east
    #[arg(long, value_parser = parse_bbox, conflicts_with = "around")]
    bbox: Option<Bbox>,

    /// Circle: lat,lon,radius_m
    #[arg(long, value_parser = parse_around)]
    around: Option<Area>,

    /// Only this element kind (default: all)
    #[arg(long, short)]
    kind: Option<Kind>,

    /// Server-side timeout in seconds
    #[arg(long, default_value_t = DEFAULT_TIMEOUT)]
    timeout: u32,

    /// Raw Overpass QL query (must use [out:json]); excludes other filters
    #[arg(long, conflicts_with_all = ["tag", "bbox", "around", "kind"])]
    ql: Option<String>,

    #[arg(long, default_value = DEFAULT_ENDPOINT)]
    endpoint: String,

    /// Retries when the server is busy (429/504)
    #[arg(long, default_value_t = DEFAULT_RETRIES)]
    retries: u32,

    /// Print the JSON response instead of a table
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    Node,
    Way,
    Relation,
}

impl From<Kind> for ElementKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Node => ElementKind::Node,
            Kind::Way => ElementKind::Way,
            Kind::Relation => ElementKind::Relation,
        }
    }
}

fn parse_floats<const N: usize>(s: &str) -> Result<[f64; N], String> {
    let parts: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse::<f64>().map_err(|e| format!("{p:?}: {e}")))
        .collect::<Result<_, _>>()?;
    parts
        .try_into()
        .map_err(|_| format!("expected {N} comma-separated numbers"))
}

fn parse_bbox(s: &str) -> Result<Bbox, String> {
    let [south, west, north, east] = parse_floats(s)?;
    Ok(Bbox {
        south,
        west,
        north,
        east,
    })
}

fn parse_around(s: &str) -> Result<Area, String> {
    let [lat, lon, radius_m] = parse_floats(s)?;
    Ok(Area::Around {
        center: Coord { lat, lon },
        radius_m,
    })
}

fn report_retry(err: &Error, delay: Duration) {
    eprintln!("server busy ({err}), retrying in {}s…", delay.as_secs());
}

fn build_query(cli: &Cli) -> Query {
    let mut q = Query::new().timeout(cli.timeout);
    if let Some(k) = cli.kind {
        q = q.kind(k.into());
    }
    for t in &cli.tag {
        q = match t.split_once('=') {
            Some((k, v)) => q.tag(k, v),
            None => q.tag_exists(t.as_str()),
        };
    }
    if let Some(b) = cli.bbox {
        q = q.within(Area::Bbox(b));
    } else if let Some(a) = cli.around {
        q = q.within(a);
    }
    q
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let client = match Client::with_endpoint(&cli.endpoint) {
        Ok(c) => c.retries(cli.retries).on_retry(report_retry),
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result = match &cli.ql {
        Some(ql) => client.raw(ql).await,
        None => client.fetch(&build_query(&cli)).await,
    };
    let resp = match result {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            if matches!(e, Error::RateLimited | Error::Timeout) {
                eprintln!(
                    "hint: the public server is overloaded; try again later or use \
                     --endpoint https://overpass.private.coffee/api/interpreter"
                );
            }
            return ExitCode::FAILURE;
        }
    };

    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&resp).expect("response serializes")
        );
        return ExitCode::SUCCESS;
    }

    for el in &resp.elements {
        let kind = format!("{:?}", el.kind()).to_lowercase();
        let (lat, lon) = el.coord().map_or((String::new(), String::new()), |c| {
            (format!("{:.6}", c.lat), format!("{:.6}", c.lon))
        });
        println!(
            "{kind:<8} {:<12} {:>10} {:>10}  {}",
            el.id().0,
            lat,
            lon,
            el.name().unwrap_or("-")
        );
    }
    eprintln!("{} elements", resp.elements.len());
    ExitCode::SUCCESS
}
