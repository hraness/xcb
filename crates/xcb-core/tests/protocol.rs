use serde::Deserialize;
use serde_json::json;
use xcb_core::protocol::{
    self, Capabilities, ErrorCode, Frame, ProtocolError, command_submit_request, decode_frame,
    encode_frame, initialize_request, negotiate_capabilities,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VectorFile {
    schema: String,
    max_frame_bytes: usize,
    golden: Vec<Vector>,
    negative: Vec<Vector>,
}

#[derive(Debug, Deserialize)]
struct Vector {
    name: String,
    frame: String,
}

fn vectors() -> VectorFile {
    serde_json::from_str(include_str!("../../../protocol/v1-vectors.json")).unwrap()
}

fn capabilities() -> Capabilities {
    Capabilities {
        versions: vec![protocol::SCHEMA.to_owned()],
        features: protocol::FEATURES
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect(),
    }
}

#[test]
fn shared_golden_vectors_are_byte_exact() {
    let vectors = vectors();
    assert_eq!(vectors.schema, protocol::SCHEMA);
    assert_eq!(vectors.max_frame_bytes, protocol::MAX_FRAME_BYTES);
    for vector in vectors.golden {
        let frame = decode_frame(vector.frame.as_bytes())
            .unwrap_or_else(|error| panic!("{}: {error}", vector.name));
        assert_eq!(
            encode_frame(&frame).unwrap(),
            vector.frame.as_bytes(),
            "{}",
            vector.name
        );
    }
}

#[test]
fn shared_negative_vectors_fail_closed() {
    for vector in vectors().negative {
        assert!(
            decode_frame(vector.frame.as_bytes()).is_err(),
            "{}",
            vector.name
        );
    }
}

#[test]
fn builders_keep_revision_and_idempotency_explicit() {
    let initialize = initialize_request("req_01", "test", "1", capabilities()).unwrap();
    let Frame::Request(initialize) = initialize else {
        panic!("request expected");
    };
    assert!(initialize.expected_revision.is_none());
    assert!(initialize.idempotency_key.is_none());

    let command = command_submit_request(
        "req_02",
        "task/run",
        json!({"dryRun":false}),
        Some("rev_abc".to_owned()),
        "idem_01",
    )
    .unwrap();
    let Frame::Request(command) = command else {
        panic!("request expected");
    };
    assert_eq!(command.expected_revision.as_deref(), Some("rev_abc"));
    assert_eq!(command.idempotency_key.as_deref(), Some("idem_01"));
}

#[test]
fn negotiation_is_a_version_and_capability_intersection() {
    let selected = negotiate_capabilities(&capabilities()).unwrap();
    assert_eq!(selected, capabilities());
    assert!(
        negotiate_capabilities(&Capabilities {
            versions: vec!["xcb.protocol.v2".to_owned()],
            features: Vec::new(),
        })
        .is_err()
    );
}

#[test]
fn bounded_error_has_no_provider_text_or_unbounded_details() {
    let error = ProtocolError {
        code: ErrorCode::RevisionConflict,
        message: "stale revision".to_owned(),
        retryable: true,
    };
    assert!(protocol::validate_error(&error).is_ok());
    assert!(
        protocol::validate_error(&ProtocolError {
            message: "x".repeat(protocol::MAX_ERROR_BYTES + 1),
            ..error
        })
        .is_err()
    );
}

proptest::proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(input in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..(protocol::MAX_FRAME_BYTES + 128))) {
        let _ = decode_frame(&input);
    }
}
