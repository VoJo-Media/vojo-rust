//! The client against a stub: requests, retries, idempotency, errors, paging and redaction.

mod common;

use std::time::Duration;

use common::*;
use tokio::time::Instant;
use vojo::http::TransportError;
use vojo::models::{CreateSiteRequest, ScreenOrientation, UpdateScreenRequest};
use vojo::params::{PlaysListParams, ScreensListParams, StatsPlaysRetrieveParams};
use vojo::{ApiErrorKind, Client, Error, RequestOptions};

fn api(error: &Error) -> &vojo::ApiError {
    error.as_api().unwrap_or_else(|| panic!("not an API error: {error:?}"))
}

// Requests

#[tokio::test]
async fn sends_the_standard_headers() {
    let (client, stub) = stub_client(vec![ok()]);
    client.screens().retrieve("scr_1").await.unwrap();
    let sent = &stub.requests()[0];
    assert_eq!(sent.method, "GET");
    assert_eq!(sent.url, "https://stub.example/v1/screens/scr_1");
    assert_eq!(sent.headers.get("authorization"), Some(format!("Bearer {}", key()).as_str()));
    assert_eq!(sent.headers.get("Vojo-Version"), Some(vojo::API_VERSION));
    assert_eq!(sent.headers.get("User-Agent"), Some(format!("vojo-rust/{}", vojo::VERSION).as_str()));
    assert_eq!(sent.headers.get("Accept"), Some("application/json"));
    assert_eq!(sent.headers.get("Vojo-Organisation"), None);
    assert_eq!(sent.headers.get("Content-Type"), None);
    assert_eq!(sent.headers.get("Idempotency-Key"), None);
}

#[tokio::test]
async fn client_and_call_options_apply() {
    let (client, stub) = stub_client_with(vec![ok(), ok()], |b| {
        b.organisation("org_1").version("2026-01-01").base_url("https://stub.example/v1/")
    });
    client.screens().retrieve("scr_1").await.unwrap();
    let sent = &stub.requests()[0];
    assert_eq!(sent.url, "https://stub.example/v1/screens/scr_1");
    assert_eq!(sent.headers.get("Vojo-Organisation"), Some("org_1"));
    assert_eq!(sent.headers.get("Vojo-Version"), Some("2026-01-01"));

    let scoped = client.with_options(RequestOptions::new().version("2027-01-01").organisation("org_2"));
    scoped.screens().retrieve("scr_1").await.unwrap();
    let sent = &stub.requests()[1];
    assert_eq!(sent.headers.get("Vojo-Organisation"), Some("org_2"));
    assert_eq!(sent.headers.get("Vojo-Version"), Some("2027-01-01"));
}

#[tokio::test]
async fn query_is_encoded_and_unset_fields_left_out() {
    let (client, stub) = stub_client(vec![respond(200, r#"{"data":[],"meta":{"has_more":false}}"#)]);
    let params = ScreensListParams::new().q("a b&c").has_harvester(true).limit(5);
    client.screens().list(params).first_page().await.unwrap();
    assert_eq!(query_of(&stub.requests()[0].url), "has_harvester=true&q=a%20b%26c&limit=5");
}

#[tokio::test]
async fn path_ids_are_escaped_and_empty_ids_refused() {
    let (client, stub) = stub_client(vec![ok()]);
    client.screens().retrieve("a/b").await.unwrap();
    assert_eq!(stub.requests()[0].url, "https://stub.example/v1/screens/a%2Fb");

    for bad in ["", ".", ".."] {
        let error = client.screens().retrieve(bad).await.unwrap_err();
        assert!(matches!(error, Error::InvalidArgument(_)), "{error:?}");
        let mut list = client.plays().events().list("", Default::default());
        assert!(matches!(list.next().await, Some(Err(Error::InvalidArgument(_)))));
        assert!(list.next().await.is_none());
    }
    assert_eq!(stub.count(), 1);
}

#[tokio::test]
async fn body_leaves_out_unset_fields_and_sends_null() {
    let (client, stub) = stub_client(vec![ok()]);
    let update = UpdateScreenRequest {
        name: Some("Platform 1".into()),
        notes: Some(None),
        orientation: Some(ScreenOrientation::Portrait),
        ..Default::default()
    };
    client.screens().update("scr_1", &update).await.unwrap();
    let sent = &stub.requests()[0];
    assert_eq!(sent.headers.get("Content-Type"), Some("application/json"));
    let body: serde_json::Value = serde_json::from_slice(sent.body.as_ref().unwrap()).unwrap();
    assert_eq!(body, serde_json::json!({"name": "Platform 1", "notes": null, "orientation": "portrait"}));
}

// Responses

#[tokio::test]
async fn response_details_are_exposed() {
    let (client, _) = stub_client(vec![respond_with(
        200,
        &[("Vojo-Request-Id", "req_9"), ("Idempotent-Replayed", "true")],
        r#"{"data":{"id":"site_1","name":"Kings Cross"},"meta":{"note":1}}"#,
    )]);
    let site = client
        .sites()
        .create(&CreateSiteRequest { name: "Kings Cross".into(), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(site.name, "Kings Cross");
    let info = site.info();
    assert_eq!(
        (info.status, info.request_id.as_deref(), info.idempotent_replayed),
        (200, Some("req_9"), true)
    );
    assert_eq!(site.meta()["note"], 1);
}

#[tokio::test]
async fn unknown_fields_and_values_are_tolerated() {
    let (client, _) = stub_client(vec![respond(
        200,
        r#"{"data":{"id":"scr_1","name":null,"orientation":"diagonal","brand_new_field":{"x":1}},"meta":{}}"#,
    )]);
    let screen = client.screens().retrieve("scr_1").await.unwrap();
    assert_eq!(screen.name, "");
    assert_eq!(screen.orientation, ScreenOrientation::Unknown("diagonal".into()));
    assert_eq!(screen.orientation.as_str(), "diagonal");
}

#[tokio::test]
async fn statistics_keep_their_meta() {
    let (client, stub) =
        stub_client(vec![respond(200, r#"{"data":{},"meta":{"range":"7d","granularity":"day"}}"#)]);
    let stats = client.stats().plays().retrieve(StatsPlaysRetrieveParams::new().range("7d")).await.unwrap();
    assert_eq!(stats.meta()["range"], "7d");
    #[derive(serde::Deserialize)]
    struct Meta {
        granularity: String,
    }
    assert_eq!(stats.meta_as::<Meta>().unwrap().granularity, "day");
    assert_eq!(query_of(&stub.requests()[0].url), "range=7d");
}

#[tokio::test]
async fn a_success_that_is_not_json_is_a_decode_error() {
    let (client, _) = stub_client(vec![respond(200, "<html>")]);
    let error = client.screens().retrieve("scr_1").await.unwrap_err();
    assert!(matches!(error, Error::Decode { status: 200, .. }), "{error:?}");
}

// Errors

#[tokio::test]
async fn statuses_map_to_kinds() {
    let cases = [
        (400, ApiErrorKind::Validation),
        (401, ApiErrorKind::Authentication),
        (402, ApiErrorKind::Permission),
        (403, ApiErrorKind::Permission),
        (404, ApiErrorKind::NotFound),
        (409, ApiErrorKind::Conflict),
        (413, ApiErrorKind::Validation),
        (418, ApiErrorKind::Other),
        (422, ApiErrorKind::Validation),
        (429, ApiErrorKind::RateLimit),
        (500, ApiErrorKind::Server),
        (503, ApiErrorKind::Server),
    ];
    for (status, kind) in cases {
        let (client, _) = stub_client_with(vec![problem(status, "some_new_code")], |b| b.max_retries(0));
        let error = client.screens().retrieve("scr_1").await.unwrap_err();
        let e = api(&error);
        assert_eq!((e.status, e.kind()), (status, kind));
        assert_eq!(e.code.as_deref(), Some("some_new_code"));
        assert_eq!(e.message, "why");
        assert_eq!(e.request_id.as_deref(), Some("req_1"));
        assert_eq!(error.status(), Some(status));
        assert_eq!(error.to_string(), format!("vojo: {status} some_new_code: why (request id: req_1)"));
    }
}

#[tokio::test]
async fn validation_errors_carry_the_fields() {
    let body = r#"{"title":"Invalid","code":"validation_failed","errors":[{"pointer":"/name","code":"required","detail":"name is required"}]}"#;
    let (client, _) = stub_client(vec![respond(422, body)]);
    let error = client.sites().update("site_1", &Default::default()).await.unwrap_err();
    let e = api(&error);
    assert_eq!(e.message, "Invalid /name (required): name is required");
    assert_eq!(e.errors.len(), 1);
    assert_eq!((e.errors[0].pointer.as_str(), e.errors[0].code.as_str()), ("/name", "required"));
}

#[tokio::test]
async fn plan_limits_keep_the_whole_problem() {
    let body =
        r#"{"title":"Limit","code":"entitlement_limit_reached","feature_key":"screens","limit":5,"used":5}"#;
    let (client, _) = stub_client(vec![respond(402, body)]);
    let error = client.screens().create(&Default::default()).await.unwrap_err();
    let e = api(&error);
    assert_eq!(e.kind(), ApiErrorKind::Permission);
    assert_eq!((e.problem["feature_key"].as_str(), e.problem["limit"].as_i64()), (Some("screens"), Some(5)));
}

#[tokio::test]
async fn a_proxy_page_maps_by_status() {
    let (client, _) = stub_client_with(
        vec![respond_with(502, &[("Vojo-Request-Id", "req_p")], "<html>Bad gateway</html>")],
        |b| b.max_retries(0),
    );
    let error = client.screens().retrieve("scr_1").await.unwrap_err();
    let e = api(&error);
    assert_eq!((e.kind(), e.code.as_deref()), (ApiErrorKind::Server, None));
    assert_eq!(e.message, "the API returned HTTP 502");
    assert_eq!(e.request_id.as_deref(), Some("req_p"));
    assert!(e.problem.is_null());
}

// The API answers a wrong-mode key like any invalid one, so the SDK adds nothing.
#[tokio::test]
async fn a_test_key_401_adds_no_hint() {
    let stub = Stub::new(vec![problem(401, "invalid_token")]);
    let client = Client::builder(["vojo_", "test_", "abc"].concat()).http_client(stub).build().unwrap();
    let error = client.screens().retrieve("scr_1").await.unwrap_err();
    assert_eq!(api(&error).message, "why");
}

#[tokio::test]
async fn an_unserved_operation_404_says_so() {
    let (client, _) = stub_client(vec![problem(404, "not_found"), problem(404, "not_found")]);
    let unserved = client.me().retrieve().await.unwrap_err();
    assert!(unserved.to_string().contains("not be available yet during early access"), "{unserved}");
    let served = client.screens().retrieve("scr_1").await.unwrap_err();
    assert!(!served.to_string().contains("early access"), "{served}");
}

// Retries

#[tokio::test(start_paused = true)]
async fn safe_calls_retry_5xx_with_backoff() {
    let (client, stub) = stub_client(vec![problem(503, "x"), problem(500, "x"), ok()]);
    let start = Instant::now();
    client.screens().retrieve("scr_1").await.unwrap();
    assert_eq!(stub.count(), 3);
    // 0.25-0.5s, then 0.5-1s.
    let waited = start.elapsed();
    assert!(waited >= Duration::from_millis(750) && waited < Duration::from_millis(1500), "{waited:?}");
}

#[tokio::test(start_paused = true)]
async fn retries_stop_at_max_retries() {
    let (client, stub) = stub_client_with(
        vec![problem(503, "x"), problem(503, "x"), problem(503, "x"), problem(503, "x")],
        |b| b.max_retries(2),
    );
    let error = client.screens().retrieve("scr_1").await.unwrap_err();
    assert_eq!((stub.count(), error.status()), (3, Some(503)));

    let (client, stub) = stub_client(vec![problem(503, "x"), ok()]);
    let error = client.with_options(RequestOptions::new().max_retries(0)).screens().retrieve("scr_1").await;
    assert!(error.is_err());
    assert_eq!(stub.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn unsafe_calls_do_not_retry_5xx() {
    let (client, stub) = stub_client(vec![problem(503, "x"), ok()]);
    let error = client.screens().update("scr_1", &Default::default()).await.unwrap_err();
    assert_eq!((stub.count(), error.status()), (1, Some(503)));
}

#[tokio::test(start_paused = true)]
async fn rate_limits_retry_on_any_method_and_honour_retry_after() {
    let (client, stub) = stub_client(vec![respond_with(429, &[("Retry-After", "7")], "{}"), ok()]);
    let start = Instant::now();
    client.screens().update("scr_1", &Default::default()).await.unwrap();
    assert_eq!(stub.count(), 2);
    assert_eq!(start.elapsed(), Duration::from_secs(7));
}

#[tokio::test(start_paused = true)]
async fn a_long_retry_after_goes_to_the_caller() {
    let (client, stub) = stub_client(vec![respond_with(429, &[("Retry-After", "61")], "{}"), ok()]);
    let error = client.screens().retrieve("scr_1").await.unwrap_err();
    assert_eq!(stub.count(), 1);
    assert_eq!(api(&error).retry_after(), Some(Duration::from_secs(61)));

    let (client, stub) = stub_client(vec![respond_with(503, &[("Retry-After", "120")], "{}"), ok()]);
    client.screens().retrieve("scr_1").await.unwrap_err();
    assert_eq!(stub.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn other_statuses_never_retry() {
    for status in [400, 401, 403, 404, 409, 422] {
        let (client, stub) = stub_client(vec![problem(status, "x"), ok()]);
        client.screens().retrieve("scr_1").await.unwrap_err();
        assert_eq!(stub.count(), 1, "{status}");
    }
}

#[tokio::test(start_paused = true)]
async fn connection_failures_follow_the_table() {
    // Never opened: safe to resend anything.
    let (client, stub) = stub_client(vec![Step::Fail(TransportError::connect("refused")), ok()]);
    client.screens().update("scr_1", &Default::default()).await.unwrap();
    assert_eq!(stub.count(), 2);

    // Lost after sending: only a safe call is resent.
    let (client, stub) = stub_client(vec![Step::Fail(TransportError::other("reset")), ok()]);
    client.screens().retrieve("scr_1").await.unwrap();
    assert_eq!(stub.count(), 2);
    let (client, stub) = stub_client(vec![Step::Fail(TransportError::other("reset")), ok()]);
    let error = client.screens().update("scr_1", &Default::default()).await.unwrap_err();
    assert_eq!(stub.count(), 1);
    assert!(matches!(&error, Error::Connection(e) if !e.timeout), "{error:?}");
}

#[tokio::test(start_paused = true)]
async fn timeouts_apply_per_attempt() {
    let (client, stub) = stub_client_with(vec![Step::Hang, ok()], |b| b.timeout(Duration::from_secs(2)));
    let start = Instant::now();
    client.screens().retrieve("scr_1").await.unwrap();
    assert_eq!(stub.count(), 2);
    assert!(start.elapsed() >= Duration::from_secs(2));

    let (client, stub) = stub_client(vec![Step::Hang, ok()]);
    let error = client
        .with_options(RequestOptions::new().timeout(Duration::from_secs(1)))
        .screens()
        .update("scr_1", &Default::default())
        .await
        .unwrap_err();
    assert!(matches!(&error, Error::Connection(e) if e.timeout), "{error:?}");
    assert_eq!(stub.count(), 1);
    assert_eq!(stub.requests()[0].timeout, Some(Duration::from_secs(1)));
}

// Idempotency

#[tokio::test(start_paused = true)]
async fn a_create_reuses_one_key_across_retries() {
    let (client, stub) =
        stub_client(vec![problem(503, "x"), problem(409, "idempotency_request_in_progress"), ok()]);
    client.sites().create(&Default::default()).await.unwrap();
    let keys: Vec<String> =
        stub.requests().iter().map(|r| r.headers.get("Idempotency-Key").unwrap().to_owned()).collect();
    assert_eq!(keys.len(), 3);
    assert!(keys.iter().all(|k| *k == keys[0]));
    assert_eq!(keys[0].len(), 36);
    assert_eq!(&keys[0][14..15], "4");

    let (client, stub) = stub_client(vec![ok(), ok()]);
    client.sites().create(&Default::default()).await.unwrap();
    client.sites().create(&Default::default()).await.unwrap();
    let sent = stub.requests();
    assert_ne!(sent[0].headers.get("Idempotency-Key"), sent[1].headers.get("Idempotency-Key"));
}

#[tokio::test]
async fn a_caller_key_replaces_the_generated_one() {
    let (client, stub) = stub_client(vec![ok(), ok()]);
    let keyed = client.with_options(RequestOptions::new().idempotency_key("my-key-1"));
    keyed.sites().create(&Default::default()).await.unwrap();
    keyed.screens().retrieve("scr_1").await.unwrap();
    let sent = stub.requests();
    assert_eq!(sent[0].headers.get("Idempotency-Key"), Some("my-key-1"));
    assert_eq!(sent[1].headers.get("Idempotency-Key"), None);
}

#[tokio::test(start_paused = true)]
async fn other_conflicts_are_not_retried() {
    let (client, stub) = stub_client(vec![problem(409, "idempotency_key_reused"), ok()]);
    let error = client.sites().create(&Default::default()).await.unwrap_err();
    assert_eq!((stub.count(), api(&error).kind()), (1, ApiErrorKind::Conflict));

    // Without a key, in_progress means nothing.
    let (client, stub) = stub_client(vec![problem(409, "idempotency_request_in_progress"), ok()]);
    client.screens().update("scr_1", &Default::default()).await.unwrap_err();
    assert_eq!(stub.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_retried_delete_surfaces_its_404() {
    let (client, stub) =
        stub_client(vec![Step::Fail(TransportError::other("reset")), problem(404, "not_found")]);
    let error = client.sites().delete("site_1").await.unwrap_err();
    assert_eq!((stub.count(), api(&error).kind()), (2, ApiErrorKind::NotFound));
}

// Paging

fn page(ids: &[&str], has_more: Option<bool>, cursor: Option<&str>) -> Step {
    let data: Vec<String> = ids.iter().map(|id| format!(r#"{{"id":"{id}"}}"#)).collect();
    let mut meta = Vec::new();
    if let Some(more) = has_more {
        meta.push(format!(r#""has_more":{more}"#));
    }
    if let Some(cursor) = cursor {
        meta.push(format!(r#""next_cursor":"{cursor}""#));
    }
    respond(200, &format!(r#"{{"data":[{}],"meta":{{{}}}}}"#, data.join(","), meta.join(",")))
}

#[tokio::test]
async fn iterates_every_item_across_pages() {
    let (client, stub) = stub_client(vec![
        page(&["p1", "p2"], Some(true), Some("c1")),
        page(&["p3"], Some(true), Some("c2")),
        page(&[], Some(false), None),
    ]);
    let plays =
        client.plays().list(PlaysListParams::new().screen_id("scr_1").limit(2)).collect().await.unwrap();
    let ids: Vec<&str> = plays.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["p1", "p2", "p3"]);
    let queries: Vec<String> = stub.requests().iter().map(|r| query_of(&r.url).to_owned()).collect();
    assert_eq!(
        queries,
        ["screen_id=scr_1&limit=2", "screen_id=scr_1&limit=2&cursor=c1", "screen_id=scr_1&limit=2&cursor=c2"]
    );
}

#[tokio::test]
async fn has_more_false_wins_over_a_cursor() {
    let (client, stub) = stub_client(vec![page(&["p1"], Some(false), Some("c1"))]);
    assert_eq!(client.plays().list(Default::default()).collect().await.unwrap().len(), 1);
    assert_eq!(stub.count(), 1);
}

#[tokio::test]
async fn an_empty_cursor_is_no_cursor() {
    let (client, stub) = stub_client(vec![page(&["p1"], None, Some(""))]);
    assert_eq!(client.plays().list(Default::default()).collect().await.unwrap().len(), 1);
    assert_eq!(stub.count(), 1);
}

#[tokio::test]
async fn has_more_without_a_cursor_is_an_error() {
    let (client, _) = stub_client(vec![page(&["p1"], Some(true), None)]);
    let mut plays = client.plays().list(Default::default());
    assert_eq!(plays.next().await.unwrap().unwrap().id, "p1");
    assert!(matches!(plays.next().await, Some(Err(Error::Pagination(_)))));
    assert!(plays.next().await.is_none());
}

#[tokio::test]
async fn a_repeated_cursor_is_an_error() {
    let (client, stub) =
        stub_client(vec![page(&["p1"], Some(true), Some("c1")), page(&["p2"], Some(true), Some("c1"))]);
    let error = client.plays().list(Default::default()).collect().await.unwrap_err();
    assert!(matches!(&error, Error::Pagination(m) if m.contains("repeated")), "{error:?}");
    assert_eq!(stub.count(), 2);
}

#[tokio::test]
async fn the_callers_cursor_counts_as_followed() {
    let (client, stub) = stub_client(vec![page(&["p1"], Some(true), Some("start"))]);
    let mut pages = client.plays().list(PlaysListParams::new().cursor("start"));
    assert!(pages.next_page().await.unwrap().is_ok());
    assert!(matches!(pages.next_page().await, Some(Err(Error::Pagination(_)))));
    assert_eq!(query_of(&stub.requests()[0].url), "cursor=start");
}

#[tokio::test]
async fn first_page_fetches_one_page() {
    let (client, stub) = stub_client(vec![page(&["p1", "p2"], Some(true), Some("c1"))]);
    let first = client.plays().list(Default::default()).first_page().await.unwrap();
    assert_eq!((first.data.len(), first.has_more, first.next_cursor.as_deref()), (2, true, Some("c1")));
    assert_eq!(stub.count(), 1);
}

#[tokio::test]
async fn a_page_error_ends_the_list() {
    let (client, _) = stub_client_with(vec![page(&["p1"], Some(true), Some("c1")), problem(500, "x")], |b| {
        b.max_retries(0)
    });
    let mut plays = client.plays().list(Default::default());
    assert!(plays.next().await.unwrap().is_ok());
    assert_eq!(plays.next().await.unwrap().unwrap_err().status(), Some(500));
    assert!(plays.next().await.is_none());
}

// Keys

#[test]
fn a_missing_key_names_the_convention() {
    let error = Client::new("").unwrap_err();
    assert!(error.to_string().contains("VOJO_SECRET_KEY"), "{error}");
    let error = Client::new("vojo_live_ab cd").unwrap_err();
    assert!(error.to_string().contains("spaces or control characters"), "{error}");
    assert!(!error.to_string().contains("ab cd"));
}

#[tokio::test]
async fn keys_never_show() {
    let echoed = format!(r#"{{"title":"bad","detail":"key {} is wrong","code":"invalid_token"}}"#, key());
    let (client, _) = stub_client(vec![
        respond(401, &echoed),
        Step::Fail(TransportError::other(format!("could not send {}", key()))),
    ]);
    assert!(!format!("{client:?}").contains(&key()));

    let error = client.screens().retrieve("scr_1").await.unwrap_err();
    let e = api(&error);
    for text in [error.to_string(), format!("{error:?}"), e.problem.to_string()] {
        assert!(!text.contains(&key()), "{text}");
        assert!(!text.contains("0123456789abcdef"), "{text}");
    }
    assert!(e.message.contains("<redacted>"));

    let error = client
        .with_options(RequestOptions::new().max_retries(0))
        .screens()
        .update("scr_1", &Default::default())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Connection(_)));
    assert!(!error.to_string().contains(&key()), "{error}");
}

#[tokio::test]
async fn request_debug_hides_the_key() {
    let (client, stub) = stub_client(vec![ok()]);
    client.screens().retrieve("scr_1").await.unwrap();
    let shown = format!("{:?}", stub.requests()[0]);
    assert!(!shown.contains(&key()), "{shown}");
    assert!(shown.contains("<redacted>"));
}

// Rules from the shared conventions on status, messages, Retry-After and cancellation

#[tokio::test]
async fn a_redirect_is_an_error_not_a_success() {
    for status in [301, 302, 303, 307, 308] {
        let (client, stub) =
            stub_client(vec![respond_with(status, &[("Location", "https://elsewhere.example/")], "")]);
        let error = client.sites().create(&Default::default()).await.unwrap_err();
        let e = api(&error);
        assert_eq!((e.status, e.kind()), (status, ApiErrorKind::Other));
        assert!(e.message.contains("check the base URL"), "{}", e.message);
        assert_eq!(stub.count(), 1);
    }
}

#[tokio::test]
async fn only_2xx_is_a_success() {
    let (client, _) = stub_client(vec![respond(204, ""), respond(299, r#"{"data":{"id":"x"}}"#)]);
    client.sites().delete("site_1").await.unwrap();
    assert_eq!(client.sites().retrieve("site_1").await.unwrap().id, "x");
}

#[tokio::test]
async fn validation_messages_name_the_fields() {
    let errors = r#"[
        {"pointer":"/name","code":"required","detail":"Name is required"},
        {"pointer":"/limit","code":"too_large","detail":"Too large"},
        {"pointer":"/site_id","code":"not_found","detail":"No such site"},
        {"pointer":"/starts_at","code":"invalid","detail":"Not a date"},
        {"pointer":"/tags/0","code":7,"detail":"Too long"},
        {"pointer":"","code":null,"detail":{}},
        {"pointer":"/a","code":"x"},
        {"detail":"Only a detail"}
    ]"#;
    let body = format!(
        r#"{{"title":"t","detail":"7 fields failed validation.","code":"validation_failed","errors":{errors}}}"#
    );
    let (client, _) = stub_client(vec![respond(422, &body)]);
    let error = client.sites().update("site_1", &Default::default()).await.unwrap_err();
    assert_eq!(
        api(&error).message,
        "7 fields failed validation. /name (required): Name is required; /limit (too_large): Too large; \
         /site_id (not_found): No such site; /starts_at (invalid): Not a date; /tags/0: Too long; and 2 more"
    );
    assert_eq!(api(&error).errors.len(), 8);

    // Other kinds append nothing, and no usable entry leaves the message alone.
    let body = r#"{"detail":"Nope","errors":[{"pointer":"/x","code":"y","detail":"z"}]}"#;
    let (client, _) =
        stub_client(vec![respond(403, body), respond(400, r#"{"detail":"Bad","errors":[{"code":1}]}"#)]);
    assert_eq!(api(&client.sites().retrieve("site_1").await.unwrap_err()).message, "Nope");
    assert_eq!(api(&client.sites().retrieve("site_1").await.unwrap_err()).message, "Bad");
}

#[tokio::test(start_paused = true)]
async fn retry_after_values_follow_the_table() {
    // (header, waited, attempts)
    let cases: [(&str, Option<u64>, usize); 12] = [
        ("3", Some(3000), 2),
        ("2.5", Some(2500), 2),
        (" 4 ", Some(4000), 2),
        ("0", Some(0), 2),
        ("60", Some(60_000), 2),
        ("60.5", None, 1),
        ("3600", None, 1),
        ("99999999999999999999999999", None, 1),
        ("1e3", Some(u64::MAX), 2),
        ("-1", Some(u64::MAX), 2),
        ("inf", Some(u64::MAX), 2),
        ("Wed, 21 Oct 2026 07:28:00 GMT", Some(u64::MAX), 2),
    ];
    for (header, waited, attempts) in cases {
        let (client, stub) = stub_client(vec![respond_with(503, &[("Retry-After", header)], "{}"), ok()]);
        let start = Instant::now();
        let result = client.screens().retrieve("scr_1").await;
        assert_eq!(stub.count(), attempts, "{header:?}");
        match waited {
            // An invalid value means the computed backoff, 0.25-0.5s.
            Some(u64::MAX) => {
                let ms = start.elapsed().as_millis() as u64;
                assert!((250..500).contains(&ms), "{header:?} waited {ms}ms");
            }
            Some(ms) => assert_eq!(start.elapsed(), Duration::from_millis(ms), "{header:?}"),
            None => assert!(result.is_err(), "{header:?}"),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_long_wait_cap_applies_to_a_keyed_conflict() {
    let busy = r#"{"title":"busy","code":"idempotency_request_in_progress"}"#;
    let (client, stub) = stub_client(vec![respond_with(409, &[("Retry-After", "61")], busy), ok()]);
    client.sites().create(&Default::default()).await.unwrap_err();
    assert_eq!(stub.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn dropping_a_call_cancels_it() {
    let (client, stub) = stub_client(vec![problem(503, "x"), ok()]);
    // The first retry waits at least 0.25s; give up before that.
    let call = client.screens().retrieve("scr_1");
    assert!(tokio::time::timeout(Duration::from_millis(100), call).await.is_err());
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(stub.count(), 1);
}

#[tokio::test]
async fn one_idempotency_key_serves_one_create() {
    let (client, stub) = stub_client(vec![ok(), ok(), ok()]);
    let keyed = client.with_options(RequestOptions::new().idempotency_key("my-key-2"));
    keyed.sites().create(&Default::default()).await.unwrap();
    let error = keyed.sites().create(&Default::default()).await.unwrap_err();
    assert!(matches!(&error, Error::InvalidArgument(m) if m.contains("already used")), "{error:?}");
    // Calls that take no key are unaffected.
    keyed.screens().retrieve("scr_1").await.unwrap();
    assert_eq!(stub.count(), 2);
}
