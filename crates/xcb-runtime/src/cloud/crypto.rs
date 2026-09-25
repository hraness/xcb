//! The pinned relay envelope scheme, byte-compatible with
//! `hraness/relay`'s `crypto/`: ECDSA P-256 low-S signatures over
//! canonical JSON, ECDH-P256 device pairing, HKDF-SHA-256, and
//! AES-GCM-256 with 96-bit IVs and 128-bit tags.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use p256::ecdh::diffie_hellman;
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::elliptic_curve::scalar::IsHigh;
use p256::pkcs8::{DecodePublicKey, EncodePublicKey};
use p256::{PublicKey, SecretKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::{Error, Result};

use super::canonical::canonicalize;

pub const SIGNATURE_BYTES: usize = 64;
pub const IV_BYTES: usize = 12;
pub const ACCOUNT_KEY_BYTES: usize = 32;

pub const RELAY_ENVELOPE_CONTRACT: &str = "relay.envelope.v1";
pub const RELAY_KEYWRAP_CONTRACT: &str = "relay.keywrap.v1";

/// DER prefix of an uncompressed P-256 public key in SPKI form, through the
/// 0x04 point marker. Mirrors `P256_SPKI_PREFIX` in the TypeScript contract.
const P256_SPKI_PREFIX: [u8; 27] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
    0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00, 0x04,
];
pub const P256_SPKI_BYTES: usize = 91;

fn invalid(what: &'static str) -> Error {
    Error::from(xcb_core::Error::Invalid(what))
}

fn cloud(what: &'static str) -> Error {
    Error::Protocol(what)
}

/// `true` when `bytes` is a P-256 public key in the exact SPKI form
/// WebCrypto exports (and the wire contract pins).
pub fn is_p256_spki(bytes: &[u8]) -> bool {
    bytes.len() == P256_SPKI_BYTES && bytes[..27] == P256_SPKI_PREFIX
}

/// A 32-byte AES-256-GCM account key. The wire never sees it; custody holds
/// it in a `Zeroizing` buffer.
#[derive(Clone)]
pub struct AccountKey(pub Zeroizing<[u8; ACCOUNT_KEY_BYTES]>);

impl AccountKey {
    pub fn generate() -> Self {
        let mut bytes = Zeroizing::new([0u8; ACCOUNT_KEY_BYTES]);
        getrandom::fill(&mut bytes[..]).expect("OS entropy");
        Self(bytes)
    }

    fn cipher(&self) -> Aes256Gcm {
        // `new_from_slice` is the non-deprecated `KeyInit` constructor.
        Aes256Gcm::new_from_slice(&self.0[..]).expect("account key is 32 bytes")
    }
}

// Base64url (no padding) --------------------------------------------------------

pub fn encode_base64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn decode_base64url(text: &str, max_chars: usize) -> Result<Vec<u8>> {
    use base64::Engine;
    if text.len() > max_chars {
        return Err(invalid("base64url length bound"));
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.as_bytes())
        .map_err(|_| invalid("base64url decode"))
}

// Device identity ----------------------------------------------------------------

/// The public half of a device, all a peer needs to verify and wrap.
#[derive(Clone, Debug)]
pub struct PeerDevice {
    pub device: String,
    pub verify_key_spki: Vec<u8>,
    pub agreement_key_spki: Vec<u8>,
}

/// A full device identity: signing + agreement private keys and the derived
/// device id. Private material lives only in custody, never on the wire.
pub struct DeviceIdentity {
    pub device: String,
    pub signing: SigningKey,
    pub agreement: SecretKey,
    pub public: PeerDevice,
}

impl DeviceIdentity {
    pub fn generate() -> Result<Self> {
        // Rejection-sampled scalars; `from_slice` refuses zero and out-of-range
        // values, so a retry loop is the honest constructor.
        let signing = loop {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|_| cloud("OS entropy unavailable"))?;
            if let Ok(key) = SigningKey::from_slice(&bytes) {
                break key;
            }
        };
        let agreement = loop {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|_| cloud("OS entropy unavailable"))?;
            if let Ok(key) = SecretKey::from_slice(&bytes) {
                break key;
            }
        };
        Self::from_scalars(signing, agreement)
    }

    /// Rebuild an identity from stored scalar bytes (custody format).
    pub fn from_scalars(signing: SigningKey, agreement: SecretKey) -> Result<Self> {
        let verify_key_spki = spki_bytes(&PublicKey::from(signing.verifying_key()))?;
        let agreement_key_spki = spki_bytes(&agreement.public_key())?;
        let device = device_id_of(&verify_key_spki)?;
        Ok(Self {
            device: device.clone(),
            signing,
            agreement,
            public: PeerDevice {
                device: device.clone(),
                verify_key_spki,
                agreement_key_spki,
            },
        })
    }
}

/// SPKI DER bytes of a P-256 public key, shape-checked.
fn spki_bytes(public: &PublicKey) -> Result<Vec<u8>> {
    let document = public
        .to_public_key_der()
        .map_err(|_| cloud("public key export failed"))?;
    let bytes = document.as_bytes().to_vec();
    if !is_p256_spki(&bytes) {
        return Err(cloud("unexpected SPKI shape"));
    }
    Ok(bytes)
}

/// Import a peer's signing key from SPKI bytes.
pub fn import_signing_key(spki: &[u8]) -> Result<VerifyingKey> {
    if !is_p256_spki(spki) {
        return Err(invalid("not a P-256 SPKI public key"));
    }
    VerifyingKey::from_public_key_der(spki).map_err(|_| invalid("bad SPKI"))
}

fn peer_agreement_key(spki: &[u8]) -> Result<PublicKey> {
    if !is_p256_spki(spki) {
        return Err(invalid("not a P-256 SPKI public key"));
    }
    PublicKey::from_public_key_der(spki).map_err(|_| invalid("bad SPKI"))
}

/// The first 128 bits of SHA-256 over the signing public key's SPKI bytes,
/// as 32 lowercase hex characters.
pub fn device_id_of(spki: &[u8]) -> Result<String> {
    if !is_p256_spki(spki) {
        return Err(invalid("not a P-256 SPKI public key"));
    }
    let digest = Sha256::digest(spki);
    Ok(hex::encode(&digest[..16]))
}

// Signatures ----------------------------------------------------------------------

/// ECDSA P-256 over the canonical JSON of `value`, normalized to low-S.
pub fn sign_canonical(signing: &SigningKey, value: &Value) -> Result<Vec<u8>> {
    let message = canonicalize(value)?;
    let signature: Signature = signing.sign(message.as_bytes());
    // p256 already emits low-S; normalize defensively for parity. In ecdsa
    // 0.17 `normalize_s` returns the normalized signature unconditionally.
    Ok(signature.normalize_s().to_bytes().to_vec())
}

/// Verify a low-S ECDSA P-256 signature over the canonical JSON of `value`.
/// High-S encodings are rejected before the cryptographic check.
pub fn verify_canonical(verify_key: &VerifyingKey, value: &Value, signature: &[u8]) -> bool {
    let Ok(parsed) = Signature::from_slice(signature) else {
        return false;
    };
    // High-S encodings are rejected before the cryptographic check.
    if bool::from(parsed.s().is_high()) {
        return false;
    }
    let Ok(message) = canonicalize(value) else {
        return false;
    };
    verify_key.verify(message.as_bytes(), &parsed).is_ok()
}

// AES-GCM ---------------------------------------------------------------------------

/// `Nonce` construction from a checked-length slice; `TryFrom` is the
/// non-deprecated path on the hybrid-array alias.
fn nonce_of(iv: &[u8]) -> Result<Nonce<aes_gcm::aead::consts::U12>> {
    if iv.len() != IV_BYTES {
        return Err(invalid("iv length"));
    }
    Nonce::try_from(iv).map_err(|_| invalid("iv length"))
}

fn seal_bytes(cipher: &Aes256Gcm, iv: &[u8], aad: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
    cipher
        .encrypt(
            &nonce_of(iv)?,
            Payload {
                msg: plaintext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| cloud("seal failed"))
}

fn open_bytes(cipher: &Aes256Gcm, iv: &[u8], aad: &str, ciphertext: &[u8]) -> Result<Vec<u8>> {
    cipher
        .decrypt(
            &nonce_of(iv)?,
            Payload {
                msg: ciphertext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| cloud("open failed"))
}

/// The AES-GCM-256 wrapping key two devices share via static ECDH, bound to
/// `info` through HKDF and to direction through additional data at the call
/// site. Salt is the recipient device id, UTF-8.
fn derive_wrap_key(
    private_key: &SecretKey,
    peer_spki: &[u8],
    salt: &str,
    info: &str,
) -> Result<Aes256Gcm> {
    let peer = peer_agreement_key(peer_spki)?;
    let secret = diffie_hellman(private_key.to_nonzero_scalar(), peer.as_affine());
    let hkdf = Hkdf::<Sha256>::new(Some(salt.as_bytes()), secret.raw_secret_bytes().as_slice());
    let mut key = Zeroizing::new([0u8; 32]);
    hkdf.expand(info.as_bytes(), &mut key[..])
        .map_err(|_| cloud("wrap key derivation failed"))?;
    Ok(Aes256Gcm::new_from_slice(&key[..]).expect("hkdf output is 32 bytes"))
}

// Envelopes ----------------------------------------------------------------------------

/// `relay.envelope.v1` additional data: `contract|scope|sender|recipient|keyVersion`.
pub fn envelope_additional_data(
    scope: &str,
    sender: &str,
    recipient: &str,
    key_version: u64,
) -> String {
    format!("{RELAY_ENVELOPE_CONTRACT}|{scope}|{sender}|{recipient}|{key_version}")
}

fn envelope_signed_fields(envelope: &Value) -> Result<Value> {
    let object = envelope.as_object().ok_or(invalid("envelope"))?;
    Ok(json!({
        "ciphertext": object.get("ciphertext").ok_or(invalid("envelope.ciphertext"))?,
        "contract": object.get("contract").ok_or(invalid("envelope.contract"))?,
        "iv": object.get("iv").ok_or(invalid("envelope.iv"))?,
        "keyVersion": object.get("keyVersion").ok_or(invalid("envelope.keyVersion"))?,
        "recipient": object.get("recipient").ok_or(invalid("envelope.recipient"))?,
        "scope": object.get("scope").ok_or(invalid("envelope.scope"))?,
        "sender": object.get("sender").ok_or(invalid("envelope.sender"))?,
    }))
}

/// Seal `plaintext` under `account_key`, addressed to `recipient`
/// (`"account"` for fleet-visible content) within `scope`. Returns the wire
/// envelope as a `serde_json` object.
pub fn seal_envelope(
    sender: &DeviceIdentity,
    account_key: &AccountKey,
    scope: &str,
    key_version: u64,
    plaintext: &[u8],
    recipient: Option<&str>,
) -> Result<Value> {
    let recipient = recipient.unwrap_or("account");
    let aad = envelope_additional_data(scope, &sender.device, recipient, key_version);
    let mut iv = [0u8; IV_BYTES];
    getrandom::fill(&mut iv[..]).map_err(|_| cloud("OS entropy unavailable"))?;
    let ciphertext = seal_bytes(&account_key.cipher(), &iv, &aad, plaintext)?;
    let unsigned = json!({
        "ciphertext": encode_base64url(&ciphertext),
        "contract": RELAY_ENVELOPE_CONTRACT,
        "iv": encode_base64url(&iv),
        "keyVersion": key_version,
        "recipient": recipient,
        "scope": scope,
        "sender": sender.device,
    });
    let signature = sign_canonical(&sender.signing, &envelope_signed_fields(&unsigned)?)?;
    let mut envelope = unsigned;
    envelope["signature"] = json!(encode_base64url(&signature));
    Ok(envelope)
}

pub type EnvelopeRejection = &'static str;

/// Open a signed envelope: check the sender's signature, then decrypt under
/// the account key with the full bound additional data.
pub fn open_envelope(
    envelope: &Value,
    recipient: &DeviceIdentity,
    sender: &PeerDevice,
    account_key: &AccountKey,
) -> std::result::Result<Vec<u8>, EnvelopeRejection> {
    let fields = envelope_signed_fields(envelope).map_err(|_| "malformed-envelope")?;
    let object = envelope.as_object().ok_or("malformed-envelope")?;
    let get_str = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .ok_or("malformed-envelope")
    };
    let scope = get_str("scope")?;
    let iv = decode_base64url(get_str("iv")?, 32).map_err(|_| "malformed-envelope")?;
    if iv.len() != IV_BYTES {
        return Err("malformed-envelope");
    }
    let ciphertext = decode_base64url(get_str("ciphertext")?, 2 * (48 * 1024 + 16))
        .map_err(|_| "malformed-envelope")?;
    let signature =
        decode_base64url(get_str("signature")?, 128).map_err(|_| "malformed-envelope")?;
    if signature.len() != SIGNATURE_BYTES {
        return Err("malformed-envelope");
    }
    let key_version = object
        .get("keyVersion")
        .and_then(Value::as_u64)
        .ok_or("malformed-envelope")?;
    let declared_recipient = get_str("recipient")?;
    let declared_sender = get_str("sender")?;
    if declared_recipient != "account" && declared_recipient != recipient.device {
        return Err("recipient-mismatch");
    }
    if declared_sender != sender.device {
        return Err("sender-mismatch");
    }
    let verify_key =
        import_signing_key(&sender.verify_key_spki).map_err(|_| "malformed-envelope")?;
    if !verify_canonical(&verify_key, &fields, &signature) {
        return Err("bad-signature");
    }
    let aad = envelope_additional_data(scope, declared_sender, declared_recipient, key_version);
    open_bytes(&account_key.cipher(), &iv, &aad, &ciphertext).map_err(|_| "decrypt-failed")
}

/// `relay.keywrap.v1` additional data: `contract|sender|recipient|keyVersion`.
pub fn key_wrap_additional_data(sender: &str, recipient: &str, key_version: u64) -> String {
    format!("{RELAY_KEYWRAP_CONTRACT}|{sender}|{recipient}|{key_version}")
}

fn key_wrap_signed_fields(envelope: &Value) -> Result<Value> {
    let object = envelope.as_object().ok_or(invalid("envelope"))?;
    Ok(json!({
        "contract": object.get("contract").ok_or(invalid("envelope.contract"))?,
        "iv": object.get("iv").ok_or(invalid("envelope.iv"))?,
        "keyVersion": object.get("keyVersion").ok_or(invalid("envelope.keyVersion"))?,
        "recipient": object.get("recipient").ok_or(invalid("envelope.recipient"))?,
        "sender": object.get("sender").ok_or(invalid("envelope.sender"))?,
        "wrapped": object.get("wrapped").ok_or(invalid("envelope.wrapped"))?,
    }))
}

/// Wrap `account_key` for `recipient` — a device already enrolled whose
/// public keys this device trusts.
pub fn seal_key_wrap(
    sender: &DeviceIdentity,
    recipient: &PeerDevice,
    account_key: &AccountKey,
    key_version: u64,
) -> Result<Value> {
    if recipient.device == sender.device {
        return Err(invalid("a device does not wrap the account key for itself"));
    }
    let sealing = derive_wrap_key(
        &sender.agreement,
        &recipient.agreement_key_spki,
        &recipient.device,
        RELAY_KEYWRAP_CONTRACT,
    )?;
    let mut iv = [0u8; IV_BYTES];
    getrandom::fill(&mut iv[..]).map_err(|_| cloud("OS entropy unavailable"))?;
    let aad = key_wrap_additional_data(&sender.device, &recipient.device, key_version);
    let wrapped = seal_bytes(&sealing, &iv, &aad, &account_key.0[..])?;
    let unsigned = json!({
        "contract": RELAY_KEYWRAP_CONTRACT,
        "iv": encode_base64url(&iv),
        "keyVersion": key_version,
        "recipient": recipient.device,
        "sender": sender.device,
        "wrapped": encode_base64url(&wrapped),
    });
    let signature = sign_canonical(&sender.signing, &key_wrap_signed_fields(&unsigned)?)?;
    let mut envelope = unsigned;
    envelope["signature"] = json!(encode_base64url(&signature));
    Ok(envelope)
}

/// Open a key-wrap addressed to `recipient` from `sender`. The signature is
/// checked before the key is derived.
pub fn open_key_wrap(
    envelope: &Value,
    recipient: &DeviceIdentity,
    sender: &PeerDevice,
) -> std::result::Result<AccountKey, EnvelopeRejection> {
    let fields = key_wrap_signed_fields(envelope).map_err(|_| "malformed-envelope")?;
    let object = envelope.as_object().ok_or("malformed-envelope")?;
    let get_str = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .ok_or("malformed-envelope")
    };
    let iv = decode_base64url(get_str("iv")?, 32).map_err(|_| "malformed-envelope")?;
    if iv.len() != IV_BYTES {
        return Err("malformed-envelope");
    }
    let wrapped = decode_base64url(get_str("wrapped")?, 256).map_err(|_| "malformed-envelope")?;
    let signature =
        decode_base64url(get_str("signature")?, 128).map_err(|_| "malformed-envelope")?;
    if signature.len() != SIGNATURE_BYTES {
        return Err("malformed-envelope");
    }
    let key_version = object
        .get("keyVersion")
        .and_then(Value::as_u64)
        .ok_or("malformed-envelope")?;
    let declared_recipient = get_str("recipient")?;
    let declared_sender = get_str("sender")?;
    if declared_recipient != recipient.device {
        return Err("recipient-mismatch");
    }
    if declared_sender != sender.device {
        return Err("sender-mismatch");
    }
    let verify_key =
        import_signing_key(&sender.verify_key_spki).map_err(|_| "malformed-envelope")?;
    if !verify_canonical(&verify_key, &fields, &signature) {
        return Err("bad-signature");
    }
    let sealing = derive_wrap_key(
        &recipient.agreement,
        &sender.agreement_key_spki,
        &recipient.device,
        RELAY_KEYWRAP_CONTRACT,
    )
    .map_err(|_| "unwrap-failed")?;
    let aad = key_wrap_additional_data(declared_sender, declared_recipient, key_version);
    let raw = open_bytes(&sealing, &iv, &aad, &wrapped).map_err(|_| "unwrap-failed")?;
    if raw.len() != ACCOUNT_KEY_BYTES {
        return Err("unwrap-failed");
    }
    let mut bytes = Zeroizing::new([0u8; ACCOUNT_KEY_BYTES]);
    bytes.copy_from_slice(&raw);
    Ok(AccountKey(bytes))
}

// Random bytes ----------------------------------------------------------------------------

pub fn random_bytes(length: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![0u8; length];
    getrandom::fill(&mut bytes[..]).map_err(|_| cloud("OS entropy unavailable"))?;
    Ok(bytes)
}
