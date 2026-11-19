//! Every case in shared/webhook-vectors.json, unchanged.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use vojo::webhook::Verifier;
use vojo::Error;

fn vectors() -> Vec<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = ["shared", "../shared"]
        .iter()
        .map(|dir| root.join(dir).join("webhook-vectors.json"))
        .find(|p| p.exists())
        .expect("no shared/webhook-vectors.json");
    let file: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    file["vectors"].as_array().unwrap().clone()
}

#[test]
fn every_vector_passes() {
    let vectors = vectors();
    assert!(vectors.len() >= 30, "only {} vectors", vectors.len());
    for vector in &vectors {
        let name = vector["name"].as_str().unwrap();
        let secrets: Vec<&str> =
            vector["secrets"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
        let mut verifier = Verifier::new(secrets[0])
            .tolerance(Duration::from_secs(vector["tolerance_seconds"].as_u64().unwrap()))
            .now(UNIX_EPOCH + Duration::from_secs(vector["now"].as_u64().unwrap()));
        for secret in &secrets[1..] {
            verifier = verifier.secret(*secret);
        }
        let result =
            verifier.construct_event(vector["body"].as_str().unwrap().as_bytes(), vector["header"].as_str());
        let expect = &vector["expect"];
        if expect["ok"].as_bool().unwrap() {
            let event = result.unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(event.event_type, expect["event"]["type"].as_str().unwrap(), "{name}");
            assert_eq!(event.id, expect["event"]["id"].as_str().unwrap(), "{name}");
            assert_eq!(event.attempt_id, expect["event"]["attempt_id"].as_str().unwrap(), "{name}");
        } else {
            match result {
                Err(Error::WebhookVerification(e)) => {
                    assert_eq!(e.kind().as_str(), expect["error"].as_str().unwrap(), "{name}: {e}")
                }
                other => panic!("{name}: expected {}, got {other:?}", expect["error"]),
            }
        }
    }
}

#[test]
fn missing_header_says_why() {
    let error = vojo::webhook::construct_event(b"{}", None, "vwsec_1").unwrap_err();
    assert!(error.to_string().contains("predates signing"), "{error}");
}

#[test]
fn events_decode_into_their_types() {
    use vojo::webhook::{event_type, events};

    let body = br#"{"eventType":"verification.ping","eventId":"evt_1","attemptId":"att_1","playId":"play_1","newField":1}"#;
    let header = vojo::webhook::generate_test_header(body, "vwsec_1", SystemTime::now());
    let event = vojo::webhook::construct_event(body, Some(&header), "vwsec_1").unwrap();
    assert_eq!(event.event_type, event_type::VERIFICATION_PING);
    let ping: events::VerificationPingEvent = event.decode().unwrap();
    assert_eq!(ping.play_id, "play_1");
}

#[test]
fn works_from_the_client_too() {
    let client = vojo::Client::new(["vojo_", "live_", "x"].concat()).unwrap();
    let body = br#"{"event_type":"wrapper.ready","event_id":"e","attempt_id":"a"}"#;
    let header = client.webhooks().generate_test_header(body, "vwsec_1", SystemTime::now());
    assert_eq!(client.webhooks().construct_event(body, Some(&header), "vwsec_1").unwrap().id, "e");
}
