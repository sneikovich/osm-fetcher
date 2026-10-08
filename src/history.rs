//! Fire-and-forget reporting of served queries to the `history` service through RabbitMQ.

use crate::element::ElementKind;
use crate::server::QueryRequest;
use lapin::options::{BasicPublishOptions, ConfirmSelectOptions, QueueDeclareOptions};
use lapin::types::{AMQPValue, FieldTable};
use lapin::{BasicProperties, Channel, Confirmation, Connection, ConnectionProperties};
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

const SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// JSON body of a message on [`QUEUE`].
#[derive(Debug, Serialize)]
pub struct Event {
    pub tags: Vec<String>,
    pub area_type: &'static str,
    pub coords: Option<Vec<f64>>,
    pub kind: Option<ElementKind>,
    pub status: u16,
    pub element_count: Option<usize>,
    pub duration_ms: u128,
    pub user_agent: Option<String>,
}

impl Event {
    pub fn new(
        req: &QueryRequest,
        status: u16,
        element_count: Option<usize>,
        took: Duration,
        user_agent: Option<String>,
    ) -> Self {
        let (area_type, coords) = match (req.bbox, req.around) {
            (Some(b), _) => ("bbox", Some(b.to_vec())),
            (None, Some(a)) => ("around", Some(a.to_vec())),
            (None, None) => ("none", None),
        };
        Self {
            tags: req
                .tags
                .iter()
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
                .map(String::from)
                .collect(),
            area_type,
            coords,
            kind: req.kind,
            status,
            element_count,
            duration_ms: took.as_millis(),
            user_agent,
        }
    }
}

/// Durable queue consumed by the `history` service (default exchange, routing key = queue name).
pub const QUEUE: &str = "history.events";
/// Malformed messages are dead-lettered here by `history`.
pub const DEAD_QUEUE: &str = "history.events.dead";

#[derive(Clone)]
pub struct History {
    inner: Arc<Inner>,
}

struct Inner {
    url: String,
    channel: Mutex<Option<Channel>>,
}

impl History {
    /// `url` is an AMQP URL, e.g. `amqp://app:app@rabbitmq:5672/%2f`.
    /// The broker is contacted lazily, so it may come up after the fetcher.
    pub fn new(url: &str) -> Self {
        Self {
            inner: Arc::new(Inner {
                url: url.to_owned(),
                channel: Mutex::new(None),
            }),
        }
    }

    /// Publish in the background; failures are logged and never reach the caller.
    pub fn record(&self, event: Event) {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let body = match serde_json::to_vec(&event) {
                Ok(b) => b,
                Err(e) => return eprintln!("history: {e}"),
            };
            match tokio::time::timeout(SEND_TIMEOUT, inner.publish(&body)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!("history: {e}"),
                Err(_) => eprintln!("history: publish timed out"),
            }
        });
    }
}

impl Inner {
    async fn publish(&self, body: &[u8]) -> lapin::Result<()> {
        let channel = self.channel().await?;
        let res = async {
            channel
                .basic_publish(
                    "".into(),
                    QUEUE.into(),
                    BasicPublishOptions::default(),
                    body,
                    BasicProperties::default()
                        .with_content_type("application/json".into())
                        .with_delivery_mode(2),
                )
                .await?
                .await
        }
        .await;
        match res {
            Ok(Confirmation::Nack(_)) => {
                Err(lapin::Error::from(lapin::ErrorKind::InvalidChannelState(
                    lapin::ChannelState::Error,
                    "broker nacked the message",
                )))
            }
            Ok(_) => Ok(()),
            Err(e) => {
                // Drop the cached channel so the next event reconnects.
                *self.channel.lock().await = None;
                Err(e)
            }
        }
    }

    /// The cached channel, or a fresh connection if there is none or it died.
    async fn channel(&self) -> lapin::Result<Channel> {
        let mut slot = self.channel.lock().await;
        if let Some(ch) = slot.as_ref().filter(|c| c.status().connected()) {
            return Ok(ch.clone());
        }
        let conn = Connection::connect(&self.url, ConnectionProperties::default()).await?;
        let ch = conn.create_channel().await?;
        declare_queues(&ch).await?;
        ch.confirm_select(ConfirmSelectOptions::default()).await?;
        *slot = Some(ch.clone());
        Ok(ch)
    }
}

/// Must match `history::consumer::declare_queues`: RabbitMQ rejects a redeclare with other arguments.
async fn declare_queues(channel: &Channel) -> lapin::Result<()> {
    channel
        .queue_declare(
            DEAD_QUEUE.into(),
            QueueDeclareOptions::durable(),
            FieldTable::default(),
        )
        .await?;
    let mut args = FieldTable::default();
    args.insert(
        "x-dead-letter-exchange".into(),
        AMQPValue::LongString("".into()),
    );
    args.insert(
        "x-dead-letter-routing-key".into(),
        AMQPValue::LongString(DEAD_QUEUE.into()),
    );
    channel
        .queue_declare(QUEUE.into(), QueueDeclareOptions::durable(), args)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_the_history_wire_format() {
        let req = QueryRequest {
            tags: vec![" amenity=cafe ".into(), "".into()],
            around: Some([50.45, 30.52, 300.0]),
            bbox: None,
            kind: None,
            timeout: None,
        };
        let ev = Event::new(
            &req,
            200,
            Some(18),
            Duration::from_millis(812),
            Some("curl/8".into()),
        );
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "tags": ["amenity=cafe"], "area_type": "around", "coords": [50.45, 30.52, 300.0],
                "kind": null, "status": 200, "element_count": 18, "duration_ms": 812,
                "user_agent": "curl/8"
            })
        );
    }
}
