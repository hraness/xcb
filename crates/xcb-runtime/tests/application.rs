use serde_json::json;
use std::sync::Arc;
use tokio::sync::watch;
use xcb_core::Provider;
use xcb_runtime::{
    application::{self, FailureCode, GenerateFailure, GenerateRequest},
    store::Store,
};

fn request() -> serde_json::Value {
    json!({"version":1,"account":"a_synthetic","model":"claude/claude-test","prompt":"private synthetic prompt",
        "timeoutMs":1000,"maxOutputBytes":1024})
}

#[test]
fn request_is_closed_bounded_and_requires_explicit_selection() {
    assert!(GenerateRequest::parse(&serde_json::to_vec(&request()).unwrap()).is_ok());
    for extra in [
        "tools",
        "hooks",
        "cwd",
        "endpoint",
        "apiKey",
        "continuation",
        "judge",
    ] {
        let mut value = request();
        value[extra] = json!([]);
        assert!(GenerateRequest::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    for (field, value) in [
        ("version", json!(2)),
        ("account", json!("")),
        ("model", json!("")),
        ("prompt", json!("")),
        ("timeoutMs", json!(999)),
        ("timeoutMs", json!(120001)),
        ("maxOutputBytes", json!(0)),
        ("maxOutputBytes", json!(262145)),
    ] {
        let mut input = request();
        input[field] = value;
        assert!(GenerateRequest::parse(&serde_json::to_vec(&input).unwrap()).is_err());
    }
    assert!(GenerateRequest::parse(&vec![b' '; application::MAX_INPUT_BYTES + 1]).is_err());
    assert!(GenerateRequest::parse(br#"{"version":1,"version":1}"#).is_err());
    assert!(GenerateRequest::parse(b"\xff").is_err());
}

#[test]
fn capabilities_do_not_claim_qualification_or_expose_state_paths() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().canonicalize().unwrap().join("state")).unwrap();
    let account = store
        .add_account(Provider::Claude, "Synthetic account", "Synthetic", 1)
        .unwrap();
    // An unrelated unqualified catalog must not become actionable application
    // inventory or overflow a consumer's bounded discovery envelope.
    let catalog: Vec<_> = (0..4096)
        .map(|index| xcb_core::models::ModelChoice {
            provider: Provider::Claude,
            id: xcb_core::Id::new(format!("fixture-{index}")).unwrap(),
            label: "Synthetic".into(),
            mode: xcb_core::models::Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: xcb_runtime::now_ms(),
        })
        .collect();
    store.set_models(Provider::Claude, &catalog).unwrap();
    let value = serde_json::to_value(application::capabilities(&store).unwrap()).unwrap();
    assert_eq!(value["supported"], false);
    assert_eq!(value["zeroTools"], true);
    assert_eq!(value["zeroHooks"], true);
    assert_eq!(value["ephemeral"], true);
    assert_eq!(value["accounts"][0]["id"], account.id.as_str());
    assert_eq!(value["accounts"][0]["available"], false);
    assert_eq!(value["accounts"][0]["reason"], "application_not_qualified");
    assert!(value["accounts"][0].get("qualification").is_none());
    assert_eq!(value["accounts"][0]["models"], json!([]));
    assert!(!value.to_string().contains(root.path().to_str().unwrap()));
    assert!(store.unsettled_runs().unwrap().is_empty());
    assert!(store.sessions(10).unwrap().is_empty());
}

#[tokio::test]
async fn unqualified_request_has_no_provider_or_session_effects() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&root.path().canonicalize().unwrap().join("state")).unwrap());
    let account = store
        .add_account(Provider::Claude, "Synthetic", "Synthetic", 1)
        .unwrap();
    let mut value = request();
    value["account"] = json!(account.id);
    let request = GenerateRequest::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    let (_sender, cancel) = watch::channel(false);
    let failure = application::generate(store.clone(), request, cancel)
        .await
        .unwrap_err();
    assert_eq!(failure.code, FailureCode::Unavailable);
    assert_eq!(failure.joined, Some(true));
    assert_eq!(failure.effects, Some("none"));
    assert!(store.unsettled_runs().unwrap().is_empty());
    assert!(store.sessions(10).unwrap().is_empty());
    assert!(
        !serde_json::to_string(&failure)
            .unwrap()
            .contains("private synthetic")
    );
}

#[tokio::test]
async fn invalid_and_precancelled_requests_prove_request_local_non_start() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&root.path().canonicalize().unwrap().join("state")).unwrap());
    for (invalid, cancelled, code) in [
        (true, false, FailureCode::InvalidRequest),
        (false, true, FailureCode::Cancelled),
    ] {
        let mut request = GenerateRequest::parse(&serde_json::to_vec(&request()).unwrap()).unwrap();
        if invalid {
            request.prompt.clear();
        }
        let (_sender, cancel) = watch::channel(cancelled);
        let failure = application::generate(store.clone(), request, cancel)
            .await
            .unwrap_err();
        assert_eq!(failure.code, code);
        assert_eq!(failure.joined, Some(true));
        assert_eq!(failure.effects, Some("none"));
        assert!(store.unsettled_runs().unwrap().is_empty());
    }
}

#[test]
fn failure_does_not_invent_join_or_include_output() {
    let failure = serde_json::to_value(GenerateFailure::new(FailureCode::CustodyUnproven)).unwrap();
    assert_eq!(
        failure,
        json!({"version":1,"status":"failed","code":"custody_unproven"})
    );
}
