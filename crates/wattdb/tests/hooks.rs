//! Hooks see, change and short-circuit requests.
#![cfg(feature = "tracing")]

mod common;
mod fixtures;

use std::io::Write;
use std::sync::{Arc, Mutex};

use fixtures::member::{MemberClient, MemberKey};
use fixtures::taskapp::{TeamClient, TeamKey};
use wattdb::{Call, Config, Error, Next, Request};

/// A tracing writer appending to a shared buffer.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

fn key() -> MemberKey {
    MemberKey {
        org_id: "O1".into(),
        member_id: "M1".into(),
    }
}

#[tokio::test]
async fn hooks() -> Result<(), Box<dyn std::error::Error>> {
    let logs = Logs::default();
    let w = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || w.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let (client, fake) = common::client(&[]);
    type Seen = (&'static str, Option<String>, Option<String>);
    let calls: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let seen = calls.clone();
    let cfg = Config::new(client.clone())
        .with_hook(move |mut call: Call, next: Next| {
            seen.lock().unwrap().push((
                call.operation(),
                call.entity.clone(),
                call.table().map(String::from),
            ));
            if let Request::GetItem(input) = &mut call.request {
                input.consistent_read = Some(true); // hooks may change the request
            }
            next.run(call)
        })
        .with_tracing();

    let m = MemberClient::new(&cfg, "table")?;
    assert_eq!(m.get(key()).exec().await?, None);
    TeamClient::new(&cfg, "table")?
        .query(TeamKey {
            org_id: "O1".into(),
        })
        .exec()
        .await?;
    m.batch_get([key()]).exec().await?;

    let want = [
        (
            "GetItem",
            Some("member".to_string()),
            Some("table".to_string()),
        ),
        ("Query", None, Some("table".to_string())),
        ("BatchGetItem", Some("member".to_string()), None),
    ];
    assert_eq!(*calls.lock().unwrap(), want);
    let sent = fake.sent();
    assert_eq!(
        sent[0].body["ConsistentRead"], true,
        "hook change not sent: {}",
        sent[0].body
    );
    let text = logs.text();
    assert_eq!(text.matches("wattdb request").count(), 3, "logs:\n{text}");
    assert!(
        text.contains("operation=\"GetItem\" entity=\"member\""),
        "logs:\n{text}"
    );

    // A hook can answer without sending.
    let blocked = Config::new(client).with_hook(|_: Call, _: Next| async {
        Err::<wattdb::Response, _>(Error::Hook("blocked".into()))
    });
    let before = fake.sent().len();
    let err = MemberClient::new(&blocked, "table")?
        .get(key())
        .exec()
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "blocked");
    assert_eq!(fake.sent().len(), before, "blocked request was sent");
    Ok(())
}
