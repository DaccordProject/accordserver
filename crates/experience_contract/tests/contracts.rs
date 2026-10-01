use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signer, SigningKey};
use experience_contract::{digest, validate_module, validate_package, verify_release, Release};
use std::collections::BTreeMap;

#[test]
fn packages_bind_exact_manifest_module_and_signature() {
    for payload in [include_bytes!("fixtures/chess.json").as_slice(), include_bytes!("fixtures/pong.json").as_slice()] {
        let package=validate_package(payload).unwrap();
        let signer=SigningKey::from_bytes(&[1;32]);
        let public=signer.verifying_key().to_bytes().iter().map(|b|format!("{b:02x}")).collect();
        let keys=BTreeMap::from([("test".into(),public)]);
        let mut release=Release{manifest:package.manifest,payload:STANDARD.encode(payload),digest:digest(payload),key_id:"test".into(),signature:signer.sign(payload).to_bytes().iter().map(|b|format!("{b:02x}")).collect(),status:"approved".into(),reviewed_at:1};
        assert!(verify_release(&release,&keys).is_ok());
        release.manifest.name="Forged".into();assert!(verify_release(&release,&keys).is_err());
        release.manifest=validate_package(payload).unwrap().manifest;
        release.status="revoked".into();assert!(verify_release(&release,&keys).is_err());
        release.status="approved".into();release.key_id="unknown".into();assert!(verify_release(&release,&keys).is_err());
    }
}

#[test]
fn malformed_modules_and_denied_capabilities_are_rejected() {
    let package=validate_package(include_bytes!("fixtures/chess.json")).unwrap();
    let module=STANDARD.decode(&package.module).unwrap();
    for length in 0..module.len() {assert!(validate_module(&module[..length]).is_err());}
    let mut tampered=serde_json::from_slice::<serde_json::Value>(include_bytes!("fixtures/chess.json")).unwrap();
    tampered["manifest"]["capabilities"]=serde_json::json!(["network"]);
    assert!(validate_package(tampered.to_string().as_bytes()).is_err());
    assert!(validate_module(&vec![0;65537]).is_err());
}
