//! Metered token/time payment streams.

use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::{Balance, Error, MintClient};

#[derive(Debug, Serialize)]
pub struct OpenStreamRequest<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub rate_micros: i64,
    pub cap_micros: i64,
    pub unit: &'a str,
    pub idem_key: &'a str,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct OpenStreamResponse {
    pub stream_id: String,
    pub first_seq: u64,
    pub holder_balance: Balance,
    pub idempotent_replay: bool,
}

#[derive(Debug, Serialize)]
pub struct TickStreamRequest<'a> {
    pub units: u64,
    pub seq: u64,
    pub idem_key: &'a str,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct StreamTickResponse {
    pub stream_id: String,
    pub tick_seq: u64,
    pub settled_total_micros: i64,
    pub from_balance: Balance,
    pub to_balance: Balance,
    pub idempotent_replay: bool,
}

#[derive(Debug, Serialize)]
pub struct CloseStreamRequest<'a> {
    pub units: u64,
    pub seq: u64,
    pub idem_key: &'a str,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct StreamCloseResponse {
    pub stream_id: String,
    pub settled_total_micros: i64,
    pub released_micros: i64,
    pub from_balance: Balance,
    pub to_balance: Balance,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Stream {
    pub id: String,
    pub from_id: String,
    pub to_id: String,
    pub rate_micros: i64,
    pub unit: String,
    pub cap_micros: i64,
    pub settled_micros: i64,
    pub last_seq: u64,
    pub status: String,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

impl MintClient {
    pub async fn open_stream(
        &self,
        request: &OpenStreamRequest<'_>,
    ) -> Result<OpenStreamResponse, Error> {
        self.send(Method::POST, "v1/streams", Some(request)).await
    }

    pub async fn stream(&self, stream_id: &str) -> Result<Stream, Error> {
        self.get(&format!("v1/streams/{stream_id}")).await
    }

    pub async fn tick_stream(
        &self,
        stream_id: &str,
        request: &TickStreamRequest<'_>,
    ) -> Result<StreamTickResponse, Error> {
        self.send(
            Method::POST,
            &format!("v1/streams/{stream_id}/tick"),
            Some(request),
        )
        .await
    }

    pub async fn close_stream(
        &self,
        stream_id: &str,
        request: &CloseStreamRequest<'_>,
    ) -> Result<StreamCloseResponse, Error> {
        self.send(
            Method::POST,
            &format!("v1/streams/{stream_id}/close"),
            Some(request),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_wire_uses_integer_money_and_ref_name() {
        let value = serde_json::to_value(OpenStreamRequest {
            from: "acct_sender",
            to: "acct_recipient",
            rate_micros: 25,
            cap_micros: 10_000,
            unit: "Unit",
            idem_key: "stream-1",
            reference: Some("generation:42"),
        })
        .unwrap();
        assert_eq!(value["rate_micros"], 25);
        assert_eq!(value["ref"], "generation:42");
        assert!(value.get("rate").is_none());
    }
}
