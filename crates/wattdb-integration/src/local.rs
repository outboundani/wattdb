//! A DynamoDB Local for the tests, and fresh tables in it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use aws_sdk_dynamodb::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_dynamodb::types::{
    AttributeDefinition, BillingMode, GlobalSecondaryIndex, KeySchemaElement, KeyType, Projection,
    ProjectionType, ScalarAttributeType,
};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::SyncRunner;
use testcontainers::{Container, GenericImage, ImageExt};

/// The pinned DynamoDB Local image (decision D63).
pub const IMAGE: (&str, &str) = ("amazon/dynamodb-local", "3.3.1");

/// Overrides the container with a running DynamoDB Local.
pub const ENDPOINT_VAR: &str = "WATTDB_DYNAMODB_ENDPOINT";

static ENDPOINT: OnceLock<String> = OnceLock::new();
/// The container lives as long as the test binary.
static CONTAINER: Mutex<Option<Container<GenericImage>>> = Mutex::new(None);

/// The endpoint of DynamoDB Local, starting a container on first use
/// unless `WATTDB_DYNAMODB_ENDPOINT` is set.
pub fn endpoint() -> &'static str {
    ENDPOINT.get_or_init(|| {
        if let Ok(e) = std::env::var(ENDPOINT_VAR) {
            return e;
        }
        // The blocking runner can't start inside the test's runtime.
        std::thread::spawn(start).join().expect("starting DynamoDB Local")
    })
}

fn start() -> String {
    let c = GenericImage::new(IMAGE.0, IMAGE.1)
        .with_exposed_port(8000.tcp())
        .with_wait_for(WaitFor::message_on_stdout("CorsParams"))
        .with_cmd(["-jar", "DynamoDBLocal.jar", "-inMemory", "-sharedDb"])
        .start()
        .expect("starting DynamoDB Local (is Docker running? or set WATTDB_DYNAMODB_ENDPOINT)");
    let host = c.get_host().expect("container host");
    let port = c.get_host_port_ipv4(8000).expect("container port");
    *CONTAINER.lock().unwrap() = Some(c);
    format!("http://{host}:{port}")
}

/// A client for DynamoDB Local. Each test makes its own, since a client's
/// connections belong to the runtime that opened them.
pub fn client() -> aws_sdk_dynamodb::Client {
    let conf = aws_sdk_dynamodb::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new("local", "local", None, None, "local"))
        .endpoint_url(endpoint())
        .build();
    aws_sdk_dynamodb::Client::from_conf(conf)
}

/// A GSI of a test table.
#[derive(Clone, Debug)]
pub struct Gsi {
    pub name: &'static str,
    pub pk: &'static str,
    pub sk: &'static str,
    pub projection: ProjectionType,
    pub include: Vec<&'static str>,
}

impl Gsi {
    /// A GSI projecting every attribute.
    pub fn new(name: &'static str, pk: &'static str, sk: &'static str) -> Self {
        Gsi { name, pk, sk, projection: ProjectionType::All, include: Vec::new() }
    }

    pub fn projection(mut self, p: ProjectionType, include: &[&'static str]) -> Self {
        self.projection = p;
        self.include = include.to_vec();
        self
    }
}

/// A table name unique to this process: test binaries can share one
/// DynamoDB Local.
pub fn table_name() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    static RUN: OnceLock<u128> = OnceLock::new();
    let run = RUN.get_or_init(|| SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos());
    format!("t{run}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

fn key(name: &str, kt: KeyType) -> KeySchemaElement {
    KeySchemaElement::builder().attribute_name(name).key_type(kt).build().unwrap()
}

/// Creates a fresh table keyed by `pk`/`sk` with the given GSIs.
pub async fn new_table(c: &aws_sdk_dynamodb::Client, gsis: &[Gsi]) -> String {
    let name = table_name();
    let mut defs = vec!["pk", "sk"];
    let mut req = c
        .create_table()
        .table_name(&name)
        .billing_mode(BillingMode::PayPerRequest)
        .key_schema(key("pk", KeyType::Hash))
        .key_schema(key("sk", KeyType::Range));
    for g in gsis {
        for f in [g.pk, g.sk] {
            if !defs.contains(&f) {
                defs.push(f);
            }
        }
        let mut p = Projection::builder().projection_type(g.projection.clone());
        for a in &g.include {
            p = p.non_key_attributes(*a);
        }
        req = req.global_secondary_indexes(
            GlobalSecondaryIndex::builder()
                .index_name(g.name)
                .key_schema(key(g.pk, KeyType::Hash))
                .key_schema(key(g.sk, KeyType::Range))
                .projection(p.build())
                .build()
                .unwrap(),
        );
    }
    for d in defs {
        req = req.attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name(d)
                .attribute_type(ScalarAttributeType::S)
                .build()
                .unwrap(),
        );
    }
    req.send().await.expect("creating a table");
    name
}
