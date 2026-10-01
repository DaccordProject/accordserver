use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use wasmparser::{ExternalKind, Operator, Parser, Payload, TypeRef, ValType, Validator};

pub const MAX_PACKAGE: usize = 100_000;
pub const MAX_MODULE: usize = 65_536;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub description: String,
    pub publisher: String,
    pub version: String,
    pub host_api: u32,
    pub runtime: String,
    pub authority: String,
    pub session_mode: String,
    pub min_players: u32,
    pub max_players: u32,
    pub max_spectators: u32,
    pub platforms: Vec<String>,
    pub capabilities: Vec<String>,
    pub module_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub manifest: Manifest,
    pub module: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub manifest: Manifest,
    pub payload: String,
    pub digest: String,
    pub key_id: String,
    pub signature: String,
    pub status: String,
    pub reviewed_at: i64,
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn slug(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_')
}

pub fn validate_package(bytes: &[u8]) -> Result<Package, String> {
    if bytes.len() > MAX_PACKAGE { return Err("Package exceeds 100000 bytes".into()); }
    let package: Package = serde_json::from_slice(bytes).map_err(|_| "Invalid package JSON")?;
    let m = &package.manifest;
    if !slug(&m.id) || !slug(&m.version) || m.name.is_empty() || m.name.len() > 100 || m.description.len() > 2000 || m.publisher.is_empty() || m.publisher.len() > 100 {
        return Err("Invalid manifest identity".into());
    }
    if m.host_api != 1 || m.runtime != "wasm-bounded-v1" || m.min_players != 2 || m.max_players != 2 || m.max_spectators > 16 || !matches!((m.authority.as_str(), m.session_mode.as_str()), ("chess", "turn_based") | ("pong", "real_time")) {
        return Err("Unsupported host, authority or participant limits".into());
    }
    if m.capabilities != ["session.read", "session.action", "draw"] || m.platforms.is_empty() || m.platforms.len() > 6 || m.platforms.iter().any(|p| !["linux", "windows", "macos", "android", "ios", "web"].contains(&p.as_str())) {
        return Err("Denied capability or platform".into());
    }
    let module = STANDARD.decode(&package.module).map_err(|_| "Invalid module base64")?;
    if digest(&module) != m.module_sha256 { return Err("Module digest mismatch".into()); }
    validate_module(&module)?;
    Ok(package)
}

pub fn verify_release(release: &Release, keys: &BTreeMap<String, String>) -> Result<Package, String> {
    if release.status != "approved" { return Err("Release is not approved".into()); }
    if release.payload.len() > MAX_PACKAGE * 2 { return Err("Release payload too large".into()); }
    let payload = STANDARD.decode(&release.payload).map_err(|_| "Invalid payload base64")?;
    let key = keys.get(&release.key_id).ok_or("Untrusted signing key")?;
    let key: [u8; 32] = decode_hex(key)?.try_into().map_err(|_| "Invalid public key")?;
    let signature = Signature::from_slice(&decode_hex(&release.signature)?).map_err(|_| "Invalid signature")?;
    VerifyingKey::from_bytes(&key).map_err(|_| "Invalid public key")?.verify_strict(&payload, &signature).map_err(|_| "Signature mismatch")?;
    if digest(&payload) != release.digest { return Err("Package digest mismatch".into()); }
    let package = validate_package(&payload)?;
    if package.manifest != release.manifest { return Err("Unsigned manifest mismatch".into()); }
    Ok(package)
}

pub fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
    if text.len() > 256 || text.len() % 2 != 0 { return Err("Invalid hex".into()); }
    (0..text.len()).step_by(2).map(|i| text.get(i..i+2).ok_or_else(|| "Invalid hex".to_string()).and_then(|s| u8::from_str_radix(s, 16).map_err(|_| "Invalid hex".into()))).collect()
}

/// Validate with the standard Wasm validator, then enforce the same bounded
/// profile as the portable client before publication or installation.
pub fn validate_module(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_MODULE { return Err("Module too large".into()); }
    Validator::new().validate_all(bytes).map_err(|_| "Invalid WebAssembly module")?;
    let mut types = Vec::new();
    let mut functions = Vec::new();
    let mut imports = 0;
    let mut bodies = 0;
    let mut exports = BTreeMap::new();
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.map_err(|_| "Malformed Wasm section")? {
            Payload::Version { num: 1, .. } | Payload::End(_) => {},
            Payload::TypeSection(reader) => {
                if reader.count() > 256 { return Err("Type limit".into()); }
                for ty in reader.into_iter_err_on_gc_types() {
                    let ty = ty.map_err(|_| "Only function types supported")?;
                    if ty.params().len() > 8 || ty.results().len() > 1 || ty.params().iter().chain(ty.results()).any(|t| *t != ValType::I32) { return Err("Only bounded i32 types supported".into()); }
                    types.push((ty.params().len(), ty.results().len()));
                }
            },
            Payload::ImportSection(reader) => {
                for import in reader {
                    let import = import.map_err(|_| "Invalid import")?;
                    let names = ["read_state", "draw", "action"];
                    let signatures = [(2, 1), (7, 0), (3, 0)];
                    if imports >= 3 || import.module != "daccord_v1" || import.name != names[imports] { return Err("Denied host import".into()); }
                    let TypeRef::Func(index) = import.ty else { return Err("Only function imports supported".into()); };
                    if types.get(index as usize) != Some(&signatures[imports]) { return Err("Invalid import signature".into()); }
                    functions.push(index as usize);
                    imports += 1;
                }
            },
            Payload::FunctionSection(reader) => {
                if reader.count() > 256 { return Err("Function limit".into()); }
                for index in reader { functions.push(index.map_err(|_| "Invalid function")? as usize); }
            },
            Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export.map_err(|_| "Invalid export")?;
                    if export.kind != ExternalKind::Func || export.index < 3 || !["render", "input"].contains(&export.name) { return Err("Invalid export".into()); }
                    let ty = functions.get(export.index as usize).and_then(|i| types.get(*i));
                    if ty != Some(&(if export.name == "render" {0} else {3}, 0)) { return Err("Invalid export signature".into()); }
                    exports.insert(export.name.to_string(), export.index);
                }
            },
            Payload::CodeSectionStart { count, .. } if count <= 256 => {},
            Payload::CodeSectionEntry(body) => {
                if body.get_locals_reader().map_err(|_| "Invalid locals")?.get_count() != 0 { return Err("Local allocation denied".into()); }
                let mut operators = body.get_operators_reader().map_err(|_| "Invalid code")?;
                let mut count = 0;
                let mut stack: i32 = 0;
                while !operators.eof() {
                    let op = operators.read().map_err(|_| "Invalid instruction")?;
                    count += 1;
                    if count > 4097 { return Err("Instruction limit".into()); }
                    match op {
                        Operator::I32Const { .. } | Operator::LocalGet { .. } => stack += 1,
                        Operator::Call { function_index } if function_index < bodies + 3 => {
                            let (p, r) = types[functions[function_index as usize]];
                            stack += r as i32 - p as i32;
                        },
                        Operator::Drop | Operator::I32Add | Operator::I32Sub | Operator::I32Mul => stack -= 1,
                        Operator::End => {},
                        _ => return Err("Instruction outside bounded v1 profile".into()),
                    }
                    if stack > 128 { return Err("Stack limit".into()); }
                }
                bodies += 1;
            },
            _ => return Err("Unsupported Wasm section".into()),
        }
    }
    if imports != 3 || exports.len() != 2 || functions.len() != bodies as usize + 3 { return Err("Incomplete module".into()); }
    Ok(())
}
