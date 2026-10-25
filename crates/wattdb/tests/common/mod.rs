//! A real SDK client whose HTTP layer answers from recorded responses and
//! records what it was sent, for replaying vectors through generated code.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use aws_sdk_dynamodb::config::{BehaviorVersion, Credentials, Region};
use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::client::runtime_components::RuntimeComponents;
use aws_smithy_types::body::SdkBody;
use serde_json::{Value as Json, json};

/// One request sent to DynamoDB.
#[derive(Clone, Debug)]
pub struct Sent {
    pub target: String,
    pub body: Json,
}

#[derive(Default, Debug)]
struct State {
    responses: Vec<Json>,
    sent: Vec<Sent>,
}

/// An HTTP client answering DynamoDB calls from scripted responses.
#[derive(Clone, Debug, Default)]
pub struct Fake(Arc<Mutex<State>>);

impl Fake {
    pub fn sent(&self) -> Vec<Sent> {
        self.0.lock().unwrap().sent.clone()
    }
}

/// Answers requests with no scripted response, as the recorder's transport
/// does.
fn default_response(target: &str) -> Json {
    match target {
        "BatchGetItem" => json!({ "Responses": {}, "UnprocessedKeys": {} }),
        "BatchWriteItem" => json!({ "UnprocessedItems": {} }),
        "Query" | "Scan" => json!({ "Items": [], "Count": 0, "ScannedCount": 0 }),
        _ => json!({}),
    }
}

impl HttpConnector for Fake {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        let target = request
            .headers()
            .get("x-amz-target")
            .unwrap_or_default()
            .trim_start_matches("DynamoDB_20120810.")
            .to_string();
        let body: Json = serde_json::from_slice(request.body().bytes().unwrap_or_default())
            .unwrap_or(Json::Null);
        let resp = {
            let mut st = self.0.lock().unwrap();
            st.sent.push(Sent {
                target: target.clone(),
                body,
            });
            if st.responses.is_empty() {
                default_response(&target)
            } else {
                st.responses.remove(0)
            }
        };
        let status: u16 = if resp.get("__type").is_some() {
            400
        } else {
            200
        };
        let mut r = HttpResponse::new(
            status.try_into().unwrap(),
            SdkBody::from(serde_json::to_vec(&resp).unwrap()),
        );
        r.headers_mut()
            .insert("content-type", "application/x-amz-json-1.0");
        HttpConnectorFuture::ready(Ok(r))
    }
}

impl HttpClient for Fake {
    fn http_connector(
        &self,
        _: &HttpConnectorSettings,
        _: &RuntimeComponents,
    ) -> SharedHttpConnector {
        SharedHttpConnector::new(self.clone())
    }
}

/// A DynamoDB client sending its requests to a Fake answering `responses`.
pub fn client(responses: &[Json]) -> (aws_sdk_dynamodb::Client, Fake) {
    let fake = Fake::default();
    fake.0.lock().unwrap().responses = responses.to_vec();
    let conf = aws_sdk_dynamodb::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new("test", "test", None, None, "test"))
        .endpoint_url("http://dynamodb.test")
        .http_client(fake.clone())
        .retry_config(aws_sdk_dynamodb::config::retry::RetryConfig::disabled())
        .build();
    (aws_sdk_dynamodb::Client::from_conf(conf), fake)
}

/// A recorded case: the responses to script and what ElectroDB sent.
pub struct Recorded {
    pub responses: Vec<Json>,
    pub requests: Vec<wattdb_vectors::Request>,
    pub result: Option<Json>,
}

pub fn recorded(vector: &str, case: &str) -> Recorded {
    let v = wattdb_vectors::load_named(vector);
    let c = v
        .cases
        .iter()
        .find(|c| c.name == case)
        .unwrap_or_else(|| panic!("no case {case:?} in {vector}"));
    Recorded {
        responses: c.responses.clone(),
        requests: c.requests().to_vec(),
        result: c.go.as_ref().and_then(|g| g.result.clone()),
    }
}

/// A Config answering `rc`'s responses, and its fake.
pub fn config(rc: &Recorded) -> (wattdb::Config, Fake) {
    let (c, fake) = client(&rc.responses);
    (wattdb::Config::new(c), fake)
}

/// Compares the requests sent with the recorded ones, as JSON. `ignore`
/// names top-level fields to skip (such as a random token).
pub fn check_requests(got: &[Sent], want: &[wattdb_vectors::Request], ignore: &[&str]) {
    assert_eq!(
        got.len(),
        want.len(),
        "sent {} requests, ElectroDB sent {}: {got:#?}",
        got.len(),
        want.len()
    );
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        let strip = |j: &Json| {
            let mut j = j.clone();
            if let Some(o) = j.as_object_mut() {
                for k in ignore {
                    o.remove(*k);
                }
            }
            j
        };
        let (gb, wb) = (strip(&g.body), strip(&w.request));
        assert!(
            g.target == w.target && gb == wb,
            "request {i} ({}, want {}) differs\n got: {}\nwant: {}",
            g.target,
            w.target,
            serde_json::to_string_pretty(&gb).unwrap(),
            serde_json::to_string_pretty(&wb).unwrap()
        );
    }
}

/// The recorded result's `M` fields, as engine values.
pub fn result(rc: &Recorded) -> wattdb::Attrs {
    match wattdb_vectors::attribute_value(rc.result.as_ref().expect("a recorded result")) {
        wattdb::Value::Map(m) => m,
        v => panic!("result isn't a map: {v:?}"),
    }
}

/// Decodes a recorded list of items.
pub fn list<E: wattdb::Entity>(v: &wattdb::Value) -> Vec<E> {
    match v {
        wattdb::Value::List(items) => items
            .iter()
            .map(|i| match i {
                wattdb::Value::Map(m) => E::decode(m).unwrap(),
                v => panic!("not an item: {v:?}"),
            })
            .collect(),
        v => panic!("not a list: {v:?}"),
    }
}
