//! Cross-language golden vectors: fixtures emitted by
//! `hraness/relay/scripts/emit-vectors.ts` (WebCrypto reference
//! implementation). Every assertion proves the Rust crypto layer reproduces
//! the pinned wire contract byte-for-byte.

use serde_json::Value;
use xcb_runtime::cloud::canonical::canonicalize;
use xcb_runtime::cloud::crypto::{
    AccountKey, DeviceIdentity, PeerDevice, decode_base64url, device_id_of, import_signing_key,
    open_envelope, open_key_wrap, sign_canonical, verify_canonical,
};
use zeroize::Zeroizing;

fn vectors() -> Value {
    serde_json::from_str(include_str!("cloud_vectors.json")).expect("fixture is valid JSON")
}

fn b64(v: &Value, path: &str) -> Vec<u8> {
    let text = path
        .split('.')
        .fold(v.clone(), |acc, key| {
            acc.get(key).cloned().unwrap_or(Value::Null)
        })
        .as_str()
        .expect("vector field is a string")
        .to_string();
    decode_base64url(&text, 1 << 20).expect("vector field is base64url")
}

fn identity(v: &Value, which: &str) -> DeviceIdentity {
    let signing = p256::ecdsa::SigningKey::from_slice(&b64(v, &format!("{which}.signingScalar")))
        .expect("signing scalar");
    let agreement = p256::SecretKey::from_slice(&b64(v, &format!("{which}.agreementScalar")))
        .expect("agreement scalar");
    DeviceIdentity::from_scalars(signing, agreement).expect("identity")
}

fn peer(v: &Value, which: &str) -> PeerDevice {
    PeerDevice {
        device: v[which]["device"].as_str().unwrap().to_string(),
        verify_key_spki: b64(v, &format!("{which}.publicKeys.signing")),
        agreement_key_spki: b64(v, &format!("{which}.publicKeys.agreement")),
    }
}

#[test]
fn canonical_json_matches() {
    let v = vectors();
    assert_eq!(
        canonicalize(&v["message"]).expect("canonicalize"),
        v["canonical"].as_str().unwrap(),
    );
}

#[test]
fn device_ids_and_scalars_reconstruct() {
    let v = vectors();
    for which in ["sender", "receiver"] {
        let spki = b64(&v, &format!("{which}.publicKeys.signing"));
        assert_eq!(
            device_id_of(&spki).expect("spki"),
            v[which]["device"].as_str().unwrap(),
            "{which} device id",
        );
        // Scalar → SPKI → device id must land on the recorded identity.
        let id = identity(&v, which);
        assert_eq!(id.device, v[which]["device"].as_str().unwrap());
        assert_eq!(id.public.verify_key_spki, spki);
    }
}

#[test]
fn typescript_signature_verifies() {
    let v = vectors();
    let key = import_signing_key(&b64(&v, "sender.publicKeys.signing")).expect("spki");
    let signature = b64(&v, "signature");
    assert!(verify_canonical(&key, &v["message"], &signature));
}

#[test]
fn rust_signature_verifies_round_trip() {
    let v = vectors();
    let sender = identity(&v, "sender");
    let key = import_signing_key(&sender.public.verify_key_spki).expect("spki");
    let signature = sign_canonical(&sender.signing, &v["message"]).expect("sign");
    assert!(verify_canonical(&key, &v["message"], &signature));
    // A high-S twin must be rejected: s' = n - s over P-256's group order.
    const ORDER: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xbc, 0xe6, 0xfa, 0xad, 0xa7, 0x17, 0x9e, 0x84, 0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63,
        0x25, 0x51,
    ];
    let mut s = [0u8; 32];
    s.copy_from_slice(&signature[32..]);
    let flipped = sub_be_u256(&ORDER, &s);
    let mut high = signature.clone();
    high[32..].copy_from_slice(&flipped);
    assert!(!verify_canonical(&key, &v["message"], &high));
}

/// Big-endian 256-bit subtract for the high-S flip (n - s), valid because
/// every emitted signature has s < n / 2.
fn sub_be_u256(n: &[u8; 32], s: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut borrow = 0i32;
    for i in (0..32).rev() {
        let diff = i32::from(n[i]) - i32::from(s[i]) - borrow;
        if diff < 0 {
            out[i] = (diff + 256) as u8;
            borrow = 1;
        } else {
            out[i] = diff as u8;
            borrow = 0;
        }
    }
    out
}

#[test]
fn typescript_envelope_opens() {
    let v = vectors();
    let receiver = identity(&v, "receiver");
    let sender_peer = peer(&v, "sender");
    let key_bytes = b64(&v, "accountKey");
    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&key_bytes);
    let account_key = AccountKey(key);
    let plaintext = open_envelope(&v["envelope"], &receiver, &sender_peer, &account_key)
        .expect("envelope opens");
    assert_eq!(plaintext, b64(&v, "plaintext"));
}

#[test]
fn typescript_key_wrap_opens() {
    let v = vectors();
    let receiver = identity(&v, "receiver");
    let sender_peer = peer(&v, "sender");
    let opened = open_key_wrap(&v["keyWrap"], &receiver, &sender_peer).expect("wrap opens");
    assert_eq!(&opened.0[..], &b64(&v, "accountKey")[..]);
}
