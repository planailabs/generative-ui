//! The `crypto.*` host ops — a comprehensive, ring-backed crypto surface shared by every engine.
//!
//! Crypto is pure compute (no I/O, no ambient authority), so unlike `kv`/`fs`/`http` these ops
//! are **ungated** — always available to a handler. All bytes cross the JSON boundary as strings
//! with an `encoding` (`utf8` | `hex` | `base64` | `base64url`). Each op picks a sensible default
//! per field (text inputs default `utf8`, keys/sigs default `hex`, outputs default `hex`); a
//! caller overrides per-field via `<field>Encoding` and the output via `encoding`.

use std::num::NonZeroU32;

use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE as B64URL};
use base64::Engine as _;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{self, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use ring::{aead, digest, hmac, pbkdf2};
use serde_json::Value;
use subtle::ConstantTimeEq;

use crate::host::HostError;

// ponytail: fixed ceilings — these are single blocking calls the wall-clock interrupt can't
// preempt, so bound the inputs. Lift into `Limits` if a handler ever legitimately needs more.
const MAX_RANDOM_BYTES: usize = 1024 * 1024;
const MAX_PBKDF2_ITERS: u64 = 10_000_000;
const MAX_PBKDF2_LEN: usize = 1024;

/// Route a `crypto.*` op. Called from [`Host::dispatch`](crate::host::Host::dispatch).
pub fn dispatch(op: &str, args: &Value) -> Result<Value, HostError> {
    match op {
        "crypto.randomBytes" => random_bytes(args),
        "crypto.uuid" => Ok(Value::String(uuid_v4()?)),
        "crypto.hash" => hash(args),
        "crypto.hmac" => hmac_op(args),
        "crypto.verifyHmac" => verify_hmac(args),
        "crypto.encode" => encode_op(args),
        "crypto.constantTimeEqual" => ct_equal(args),
        "crypto.pbkdf2" => pbkdf2_op(args),
        "crypto.aead.seal" => aead_seal(args),
        "crypto.aead.open" => aead_open(args),
        "crypto.ed25519.keypair" => ed25519_keypair(args),
        "crypto.ed25519.sign" => ed25519_sign(args),
        "crypto.ed25519.verify" => ed25519_verify(args),
        other => Err(HostError::Other(format!("unknown crypto op {other:?}"))),
    }
}

// --- ops -----------------------------------------------------------------

fn random_bytes(args: &Value) -> Result<Value, HostError> {
    let n = args
        .get("bytes")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| HostError::Other("randomBytes: missing numeric `bytes`".into()))? as usize;
    if n > MAX_RANDOM_BYTES {
        return Err(HostError::Limit(format!("randomBytes {n} exceeds {MAX_RANDOM_BYTES}")));
    }
    let mut buf = vec![0u8; n];
    fill(&mut buf)?;
    Ok(Value::String(encode(&buf, out_enc(args))?))
}

/// RFC 4122 v4 UUID from 16 CSPRNG bytes.
fn uuid_v4() -> Result<String, HostError> {
    let mut b = [0u8; 16];
    fill(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    ))
}

fn hash(args: &Value) -> Result<Value, HostError> {
    let algo = digest_algo(req_str(args, "algo")?)?;
    let data = bytes_in(args, "data", "utf8")?;
    let d = digest::digest(algo, &data);
    Ok(Value::String(encode(d.as_ref(), out_enc(args))?))
}

fn hmac_op(args: &Value) -> Result<Value, HostError> {
    let algo = hmac_algo(opt_str(args, "algo").unwrap_or("sha256"))?;
    let key = bytes_in(args, "key", "utf8")?;
    let data = bytes_in(args, "data", "utf8")?;
    let tag = hmac::sign(&hmac::Key::new(algo, &key), &data);
    Ok(Value::String(encode(tag.as_ref(), out_enc(args))?))
}

/// Constant-time HMAC check — the webhook-signature primitive (GitHub/Stripe-style).
fn verify_hmac(args: &Value) -> Result<Value, HostError> {
    let algo = hmac_algo(opt_str(args, "algo").unwrap_or("sha256"))?;
    let key = bytes_in(args, "key", "utf8")?;
    let data = bytes_in(args, "data", "utf8")?;
    let sig = bytes_in(args, "signature", "hex")?;
    let ok = hmac::verify(&hmac::Key::new(algo, &key), &data, &sig).is_ok();
    Ok(Value::Bool(ok))
}

/// Transcode a string between encodings, e.g. `{ data, from: "utf8", to: "base64" }`.
fn encode_op(args: &Value) -> Result<Value, HostError> {
    let from = opt_str(args, "from").unwrap_or("utf8");
    let to = opt_str(args, "to").unwrap_or("hex");
    let bytes = decode(req_str(args, "data")?, from)?;
    Ok(Value::String(encode(&bytes, to)?))
}

fn ct_equal(args: &Value) -> Result<Value, HostError> {
    let enc = out_enc(args);
    let a = decode(req_str(args, "a")?, enc)?;
    let b = decode(req_str(args, "b")?, enc)?;
    // subtle's slice impl is length-aware (unequal lengths → not equal) and constant-time.
    Ok(Value::Bool(bool::from(a.ct_eq(&b))))
}

fn pbkdf2_op(args: &Value) -> Result<Value, HostError> {
    let algo = match opt_str(args, "algo").unwrap_or("sha256").to_ascii_lowercase().as_str() {
        "sha256" => pbkdf2::PBKDF2_HMAC_SHA256,
        "sha512" => pbkdf2::PBKDF2_HMAC_SHA512,
        other => return Err(HostError::Other(format!("unknown pbkdf2 algo {other:?}"))),
    };
    let iters = args.get("iterations").and_then(|v| v.as_u64()).unwrap_or(0);
    if iters == 0 || iters > MAX_PBKDF2_ITERS {
        return Err(HostError::Limit(format!("pbkdf2 iterations must be 1..={MAX_PBKDF2_ITERS}")));
    }
    let len = args.get("length").and_then(|v| v.as_u64()).unwrap_or(32) as usize;
    if len == 0 || len > MAX_PBKDF2_LEN {
        return Err(HostError::Limit(format!("pbkdf2 length must be 1..={MAX_PBKDF2_LEN}")));
    }
    let password = bytes_in(args, "password", "utf8")?;
    let salt = bytes_in(args, "salt", "hex")?;
    let mut out = vec![0u8; len];
    pbkdf2::derive(algo, NonZeroU32::new(iters as u32).unwrap(), &salt, &password, &mut out);
    Ok(Value::String(encode(&out, out_enc(args))?))
}

/// AEAD encrypt. Returns `{ ciphertext, nonce }` (ciphertext has the auth tag appended). A
/// missing `nonce` is generated (12 random bytes) and returned so the caller can store it.
fn aead_seal(args: &Value) -> Result<Value, HostError> {
    let key = aead_key(args)?;
    let nonce_bytes = match opt_bytes_in(args, "nonce", "hex")? {
        Some(n) => n,
        None => {
            let mut n = vec![0u8; aead::NONCE_LEN];
            fill(&mut n)?;
            n
        }
    };
    let nonce = nonce_from(&nonce_bytes)?;
    let aad = opt_bytes_in(args, "aad", "utf8")?.unwrap_or_default();
    let mut in_out = bytes_in(args, "plaintext", "utf8")?;
    key.seal_in_place_append_tag(nonce, aead::Aad::from(aad.as_slice()), &mut in_out)
        .map_err(|_| HostError::Other("aead seal failed".into()))?;
    let enc = out_enc(args);
    Ok(serde_json::json!({
        "ciphertext": encode(&in_out, enc)?,
        "nonce": encode(&nonce_bytes, enc)?,
    }))
}

/// AEAD decrypt+authenticate. Returns the plaintext (default `utf8`, override `plaintextEncoding`).
fn aead_open(args: &Value) -> Result<Value, HostError> {
    let key = aead_key(args)?;
    let nonce_bytes = bytes_in(args, "nonce", "hex")?;
    let nonce = nonce_from(&nonce_bytes)?;
    let aad = opt_bytes_in(args, "aad", "utf8")?.unwrap_or_default();
    let mut in_out = bytes_in(args, "ciphertext", "hex")?;
    let plaintext = key
        .open_in_place(nonce, aead::Aad::from(aad.as_slice()), &mut in_out)
        .map_err(|_| HostError::Other("aead open failed (bad key, nonce, or tag)".into()))?;
    let enc = opt_str(args, "plaintextEncoding").unwrap_or("utf8");
    Ok(Value::String(encode(plaintext, enc)?))
}

/// Generate an Ed25519 keypair: `{ privateKey (PKCS#8), publicKey (32 bytes) }`.
fn ed25519_keypair(args: &Value) -> Result<Value, HostError> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| HostError::Other("ed25519 keygen failed".into()))?;
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|_| HostError::Other("ed25519 keygen parse failed".into()))?;
    let enc = out_enc(args);
    Ok(serde_json::json!({
        "privateKey": encode(pkcs8.as_ref(), enc)?,
        "publicKey": encode(kp.public_key().as_ref(), enc)?,
    }))
}

fn ed25519_sign(args: &Value) -> Result<Value, HostError> {
    let pk = bytes_in(args, "privateKey", "hex")?;
    let kp = Ed25519KeyPair::from_pkcs8(&pk)
        .map_err(|_| HostError::Other("ed25519: invalid PKCS#8 private key".into()))?;
    let data = bytes_in(args, "data", "utf8")?;
    Ok(Value::String(encode(kp.sign(&data).as_ref(), out_enc(args))?))
}

fn ed25519_verify(args: &Value) -> Result<Value, HostError> {
    let pubkey = bytes_in(args, "publicKey", "hex")?;
    let data = bytes_in(args, "data", "utf8")?;
    let sig = bytes_in(args, "signature", "hex")?;
    let ok = UnparsedPublicKey::new(&signature::ED25519, &pubkey).verify(&data, &sig).is_ok();
    Ok(Value::Bool(ok))
}

// --- algorithm tables ----------------------------------------------------

fn digest_algo(name: &str) -> Result<&'static digest::Algorithm, HostError> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "sha256" => &digest::SHA256,
        "sha384" => &digest::SHA384,
        "sha512" => &digest::SHA512,
        "sha512_256" | "sha512-256" => &digest::SHA512_256,
        "sha1" => &digest::SHA1_FOR_LEGACY_USE_ONLY,
        other => return Err(HostError::Other(format!("unknown hash algo {other:?}"))),
    })
}

fn hmac_algo(name: &str) -> Result<hmac::Algorithm, HostError> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "sha256" => hmac::HMAC_SHA256,
        "sha384" => hmac::HMAC_SHA384,
        "sha512" => hmac::HMAC_SHA512,
        "sha1" => hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
        other => return Err(HostError::Other(format!("unknown hmac algo {other:?}"))),
    })
}

/// Build the AEAD key (`algo` defaults to `chacha20poly1305`). Returns a `LessSafeKey` — the
/// nonce is supplied per call, which is correct here since we never reuse a (key, nonce) pair.
fn aead_key(args: &Value) -> Result<aead::LessSafeKey, HostError> {
    let alg: &'static aead::Algorithm = match opt_str(args, "algo").unwrap_or("chacha20poly1305") {
        "aes256gcm" | "aes-256-gcm" => &aead::AES_256_GCM,
        "aes128gcm" | "aes-128-gcm" => &aead::AES_128_GCM,
        "chacha20poly1305" | "chacha20-poly1305" => &aead::CHACHA20_POLY1305,
        other => return Err(HostError::Other(format!("unknown aead algo {other:?}"))),
    };
    let key = bytes_in(args, "key", "hex")?;
    let unbound = aead::UnboundKey::new(alg, &key)
        .map_err(|_| HostError::Other(format!("aead: key must be {} bytes", alg.key_len())))?;
    Ok(aead::LessSafeKey::new(unbound))
}

fn nonce_from(bytes: &[u8]) -> Result<aead::Nonce, HostError> {
    aead::Nonce::try_assume_unique_for_key(bytes)
        .map_err(|_| HostError::Other(format!("aead nonce must be {} bytes", aead::NONCE_LEN)))
}

fn fill(buf: &mut [u8]) -> Result<(), HostError> {
    SystemRandom::new().fill(buf).map_err(|_| HostError::Other("system RNG failure".into()))
}

// --- arg + encoding helpers ----------------------------------------------

fn req_str<'a>(args: &'a Value, k: &str) -> Result<&'a str, HostError> {
    args.get(k)
        .and_then(|v| v.as_str())
        .ok_or_else(|| HostError::Other(format!("missing string arg {k:?}")))
}

fn opt_str<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(|v| v.as_str())
}

fn out_enc(args: &Value) -> &str {
    opt_str(args, "encoding").unwrap_or("hex")
}

/// Decode a required string field per its `<field>Encoding` (or `default_enc`).
fn bytes_in(args: &Value, field: &str, default_enc: &str) -> Result<Vec<u8>, HostError> {
    decode(req_str(args, field)?, field_enc(args, field, default_enc))
}

/// Like [`bytes_in`] but `None` when the field is absent.
fn opt_bytes_in(args: &Value, field: &str, default_enc: &str) -> Result<Option<Vec<u8>>, HostError> {
    match opt_str(args, field) {
        None => Ok(None),
        Some(s) => decode(s, field_enc(args, field, default_enc)).map(Some),
    }
}

fn field_enc<'a>(args: &'a Value, field: &str, default_enc: &'a str) -> &'a str {
    opt_str(args, &format!("{field}Encoding")).unwrap_or(default_enc)
}

fn decode(s: &str, enc: &str) -> Result<Vec<u8>, HostError> {
    match enc {
        "utf8" | "utf-8" => Ok(s.as_bytes().to_vec()),
        "hex" => hex_decode(s),
        "base64" => B64.decode(s.trim()).map_err(|e| HostError::Other(format!("base64 decode: {e}"))),
        "base64url" => B64URL.decode(s.trim()).map_err(|e| HostError::Other(format!("base64url decode: {e}"))),
        other => Err(HostError::Other(format!("unknown encoding {other:?}"))),
    }
}

fn encode(b: &[u8], enc: &str) -> Result<String, HostError> {
    match enc {
        "utf8" | "utf-8" => String::from_utf8(b.to_vec()).map_err(|_| HostError::Other("output is not valid utf8".into())),
        "hex" => Ok(hex_encode(b)),
        "base64" => Ok(B64.encode(b)),
        "base64url" => Ok(B64URL.encode(b)),
        other => Err(HostError::Other(format!("unknown encoding {other:?}"))),
    }
}

fn hex_encode(b: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        let _ = write!(s, "{byte:02x}");
    }
    s
}

fn hex_decode(s: &str) -> Result<Vec<u8>, HostError> {
    let s = s.trim().as_bytes();
    if !s.len().is_multiple_of(2) {
        return Err(HostError::Other("hex: odd-length string".into()));
    }
    s.chunks(2).map(|p| Ok((hexval(p[0])? << 4) | hexval(p[1])?)).collect()
}

fn hexval(c: u8) -> Result<u8, HostError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(HostError::Other(format!("hex: invalid char {:?}", c as char))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(op: &str, args: Value) -> Value {
        dispatch(op, &args).unwrap_or_else(|e| panic!("{op}: {e}"))
    }

    #[test]
    fn hash_known_answer() {
        // SHA-256("abc") — FIPS 180-2 example.
        assert_eq!(
            d("crypto.hash", json!({ "algo": "sha256", "data": "abc" })),
            json!("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn hmac_known_answer_and_verify() {
        // The canonical HMAC-SHA256 test vector.
        let key = "key";
        let msg = "The quick brown fox jumps over the lazy dog";
        let mac = d("crypto.hmac", json!({ "algo": "sha256", "key": key, "data": msg }));
        assert_eq!(mac, json!("f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"));
        assert_eq!(
            d("crypto.verifyHmac", json!({ "key": key, "data": msg, "signature": mac })),
            json!(true)
        );
        assert_eq!(
            d("crypto.verifyHmac", json!({ "key": "wrong", "data": msg, "signature": mac })),
            json!(false)
        );
    }

    #[test]
    fn encode_transcodes() {
        assert_eq!(d("crypto.encode", json!({ "data": "hello", "to": "base64" })), json!("aGVsbG8="));
        assert_eq!(d("crypto.encode", json!({ "data": "aGVsbG8=", "from": "base64", "to": "utf8" })), json!("hello"));
        assert_eq!(d("crypto.encode", json!({ "data": "hello", "to": "hex" })), json!("68656c6c6f"));
    }

    #[test]
    fn random_and_uuid_shape() {
        let hex = d("crypto.randomBytes", json!({ "bytes": 16 }));
        assert_eq!(hex.as_str().unwrap().len(), 32, "16 bytes → 32 hex chars");
        let u = d("crypto.uuid", json!({}));
        let s = u.as_str().unwrap();
        assert_eq!(s.len(), 36);
        assert_eq!(&s[14..15], "4", "version nibble");
        assert!(matches!(&s[19..20], "8" | "9" | "a" | "b"), "variant nibble");
    }

    #[test]
    fn aead_roundtrip_and_tamper() {
        let key = d("crypto.randomBytes", json!({ "bytes": 32 }));
        let sealed = d(
            "crypto.aead.seal",
            json!({ "algo": "chacha20poly1305", "key": key, "plaintext": "secret", "aad": "ctx" }),
        );
        let (ct, nonce) = (&sealed["ciphertext"], &sealed["nonce"]);
        let opened = d(
            "crypto.aead.open",
            json!({ "key": key, "ciphertext": ct, "nonce": nonce, "aad": "ctx" }),
        );
        assert_eq!(opened, json!("secret"));
        // Wrong AAD fails authentication.
        assert!(dispatch(
            "crypto.aead.open",
            &json!({ "key": key, "ciphertext": ct, "nonce": nonce, "aad": "other" })
        )
        .is_err());
    }

    #[test]
    fn ed25519_sign_verify() {
        let kp = d("crypto.ed25519.keypair", json!({}));
        let (priv_k, pub_k) = (&kp["privateKey"], &kp["publicKey"]);
        let sig = d("crypto.ed25519.sign", json!({ "privateKey": priv_k, "data": "msg" }));
        assert_eq!(
            d("crypto.ed25519.verify", json!({ "publicKey": pub_k, "data": "msg", "signature": sig })),
            json!(true)
        );
        assert_eq!(
            d("crypto.ed25519.verify", json!({ "publicKey": pub_k, "data": "tampered", "signature": sig })),
            json!(false)
        );
    }

    #[test]
    fn pbkdf2_is_deterministic_and_bounded() {
        let a = d("crypto.pbkdf2", json!({ "password": "pw", "salt": "abcd", "iterations": 4096, "length": 32 }));
        let b = d("crypto.pbkdf2", json!({ "password": "pw", "salt": "abcd", "iterations": 4096, "length": 32 }));
        assert_eq!(a, b);
        assert_eq!(a.as_str().unwrap().len(), 64, "32 bytes → 64 hex chars");
        assert!(matches!(
            dispatch("crypto.pbkdf2", &json!({ "password": "p", "salt": "ab", "iterations": 0 })),
            Err(HostError::Limit(_))
        ));
    }

    #[test]
    fn limits_and_unknown_op() {
        assert!(matches!(
            dispatch("crypto.randomBytes", &json!({ "bytes": MAX_RANDOM_BYTES + 1 })),
            Err(HostError::Limit(_))
        ));
        assert!(matches!(dispatch("crypto.nope", &json!({})), Err(HostError::Other(_))));
    }
}
