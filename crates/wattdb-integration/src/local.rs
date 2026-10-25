//! A DynamoDB Local for the tests, and fresh tables in it.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_sdk_dynamodb::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_dynamodb::types::{
    AttributeDefinition, BillingMode, GlobalSecondaryIndex, KeySchemaElement, KeyType, Projection,
    ProjectionType, ScalarAttributeType,
};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::sync::OnceCell;

/// The pinned DynamoDB Local image (decision D63).
pub const IMAGE: (&str, &str) = ("amazon/dynamodb-local", "3.3.1");

/// Overrides the container with a running DynamoDB Local.
pub const ENDPOINT_VAR: &str = "WATTDB_DYNAMODB_ENDPOINT";

/// The endpoint, and the container behind it, which lives as long as the
/// test binary.
static LOCAL: OnceCell<(String, Option<ContainerAsync<GenericImage>>)> = OnceCell::const_new();

/// The endpoint of DynamoDB Local, starting a container on first use
/// unless `WATTDB_DYNAMODB_ENDPOINT` is set.
pub async fn endpoint() -> &'static str {
    let (endpoint, _) = LOCAL
        .get_or_init(|| async {
            match std::env::var(ENDPOINT_VAR) {
                Ok(e) => (e, None),
                Err(_) => start().await,
            }
        })
        .await;
    endpoint
}

async fn start() -> (String, Option<ContainerAsync<GenericImage>>) {
    let c = GenericImage::new(IMAGE.0, IMAGE.1)
        .with_exposed_port(8000.tcp())
        .with_wait_for(WaitFor::message_on_stdout("CorsParams"))
        .with_cmd(["-jar", "DynamoDBLocal.jar", "-inMemory", "-sharedDb"])
        .start()
        .await
        .expect("starting DynamoDB Local (is Docker running? or set WATTDB_DYNAMODB_ENDPOINT)");
    let host = c.get_host().await.expect("container host");
    let port = c.get_host_port_ipv4(8000).await.expect("container port");
    // The log line can come just before the port accepts connections.
    let addr = format!("{host}:{port}");
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(&addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    (format!("http://{addr}"), Some(c))
}

/// A client for DynamoDB Local. Each test makes its own, since a client's
/// connections belong to the runtime that opened them.
pub async fn client() -> aws_sdk_dynamodb::Client {
    let conf = aws_sdk_dynamodb::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new("local", "local", None, None, "local"))
        .endpoint_url(endpoint().await)
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
        Gsi {
            name,
            pk,
            sk,
            projection: ProjectionType::All,
            include: Vec::new(),
        }
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
    let run = RUN.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    });
    format!(
        "t{run}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

fn key(name: &str, kt: KeyType) -> KeySchemaElement {
    KeySchemaElement::builder()
        .attribute_name(name)
        .key_type(kt)
        .build()
        .unwrap()
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
