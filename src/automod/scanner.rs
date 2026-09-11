//! Local CPU inference. No runtime or model is downloaded by the server.
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Cursor,
    path::Path,
    sync::{Arc, Mutex},
};

pub const LABELS: [&str; 18] = [
    "FEMALE_GENITALIA_COVERED",
    "FACE_FEMALE",
    "BUTTOCKS_EXPOSED",
    "FEMALE_BREAST_EXPOSED",
    "FEMALE_GENITALIA_EXPOSED",
    "MALE_BREAST_EXPOSED",
    "ANUS_EXPOSED",
    "FEET_EXPOSED",
    "BELLY_COVERED",
    "FEET_COVERED",
    "ARMPITS_COVERED",
    "ARMPITS_EXPOSED",
    "FACE_MALE",
    "BELLY_EXPOSED",
    "MALE_GENITALIA_EXPOSED",
    "ANUS_COVERED",
    "FEMALE_BREAST_COVERED",
    "BUTTOCKS_COVERED",
];
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sampled_timestamps_ms: Vec<u64>,
    pub model_version: String,
    pub scores: BTreeMap<String, f32>,
}
impl ScanResult {
    pub fn validate(&self) -> Result<(), String> {
        if (!self.sampled_timestamps_ms.is_empty() && self.sampled_timestamps_ms.len() != 5)
            || self.model_version.is_empty()
            || self.model_version.len() > 256
            || self.scores.is_empty()
            || self.scores.len() > 64
            || self.scores.iter().any(|(k, v)| {
                k.is_empty() || k.len() > 64 || !v.is_finite() || !(0.0..=1.0).contains(v)
            })
        {
            return Err("invalid scanner response".into());
        }
        Ok(())
    }
}
pub trait Scanner: Send + Sync {
    fn version(&self) -> &str;
    fn status(&self) -> String {
        "ready".into()
    }
    fn scan(&self, bytes: Vec<u8>) -> BoxFuture<'_, Result<ScanResult, String>>;
}
pub struct Unavailable(pub String);
impl Scanner for Unavailable {
    fn version(&self) -> &str {
        "unavailable"
    }
    fn status(&self) -> String {
        self.0.clone()
    }
    fn scan(&self, _: Vec<u8>) -> BoxFuture<'_, Result<ScanResult, String>> {
        Box::pin(async { Err(self.0.clone()) })
    }
}

/// Decode based on bytes, never a caller-provided MIME type. Animation and other
/// formats remain held until a scanner capable of covering them is implemented.
pub fn decode(bytes: &[u8]) -> Result<image::RgbImage, String> {
    let format = image::guess_format(bytes).map_err(|_| "unsupported media format")?;
    match format {
        image::ImageFormat::Jpeg => {}
        image::ImageFormat::Png => {
            let mut offset = 8usize;
            while offset + 12 <= bytes.len() {
                let len =
                    u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
                if &bytes[offset + 4..offset + 8] == b"acTL" {
                    return Err("animated PNG requires moderator review".into());
                }
                offset = offset
                    .checked_add(len)
                    .and_then(|v| v.checked_add(12))
                    .ok_or("invalid PNG")?;
            }
        }
        image::ImageFormat::WebP => {
            if bytes.len() >= 21 && &bytes[12..16] == b"VP8X" && bytes[20] & 2 != 0 {
                return Err("animated WebP requires moderator review".into());
            }
        }
        _ => return Err("unsupported media format; moderator review required".into()),
    }
    let dimensions = image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    let (w, h) = dimensions;
    if w == 0
        || h == 0
        || w > 8192
        || h > 8192
        || u64::from(w) * u64::from(h) > 16_000_000
        || u64::from(w.max(h)).pow(2) > 16_000_000
    {
        return Err("image exceeds automod decoded pixel limit".into());
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    reader
        .decode()
        .map(|i| i.into_rgb8())
        .map_err(|e| e.to_string())
}
fn input(bytes: &[u8]) -> Result<Vec<f32>, String> {
    let rgb = decode(bytes)?;
    let side = rgb.width().max(rgb.height());
    let mut padded = image::RgbImage::new(side, side);
    image::imageops::replace(&mut padded, &rgb, 0, 0);
    let resized = image::imageops::resize(&padded, 320, 320, image::imageops::FilterType::Triangle);
    let mut data = vec![0.0; 3 * 320 * 320];
    for (i, p) in resized.pixels().enumerate() {
        for c in 0..3 {
            data[c * 320 * 320 + i] = f32::from(p[c]) / 255.0;
        }
    }
    Ok(data)
}
pub struct Local {
    version: String,
    session: Arc<Mutex<ort::session::Session>>,
    permit: Arc<tokio::sync::Semaphore>,
}
impl Local {
    pub fn load(model: &Path, runtime: &Path, threads: usize) -> Result<Self, String> {
        if !runtime.is_file() {
            return Err("ONNX Runtime library not found; set ACCORD_AUTOMOD_RUNTIME_PATH".into());
        }
        let weights =
            std::fs::read(model).map_err(|e| format!("cannot read automod model: {e}"))?;
        let version = format!(
            "nudenet320n:{:x}:rgb-triangle-nms-v1",
            Sha256::digest(&weights)
        );
        // ort 2.0 rc10 panics on a missing/incompatible dynamic library.
        let session = std::panic::catch_unwind(|| -> Result<_, String> {
            ort::init_from(runtime.to_string_lossy())
                .with_telemetry(false)
                .commit()
                .map_err(|e| e.to_string())?;
            ort::session::Session::builder()
                .map_err(|e| e.to_string())?
                .with_intra_threads(threads.clamp(1, 4))
                .map_err(|e| e.to_string())?
                .with_inter_threads(1)
                .map_err(|e| e.to_string())?
                .commit_from_memory(&weights)
                .map_err(|e| e.to_string())
        })
        .map_err(|_| "unable to load ONNX Runtime".to_string())??;
        Ok(Self {
            version,
            session: Arc::new(Mutex::new(session)),
            permit: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }
}
impl Scanner for Local {
    fn version(&self) -> &str {
        &self.version
    }
    fn scan(&self, bytes: Vec<u8>) -> BoxFuture<'_, Result<ScanResult, String>> {
        let session = self.session.clone();
        let version = self.version.clone();
        let permit = self.permit.clone();
        Box::pin(async move {
            let permit = permit
                .try_acquire_owned()
                .map_err(|_| "local scanner is busy")?;
            tokio::task::spawn_blocking(move || {
                // This permit stays inside the blocking task even if the caller times out.
                let _permit = permit;
                let tensor =
                    ort::value::Tensor::from_array(([1usize, 3, 320, 320], input(&bytes)?))
                        .map_err(|e| e.to_string())?;
                let mut session = session.lock().map_err(|e| e.to_string())?;
                let outputs = session
                    .run(ort::inputs![tensor])
                    .map_err(|e| e.to_string())?;
                let (shape, data) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| e.to_string())?;
                if shape.len() != 3 || shape[0] != 1 || shape[1] != 22 || shape[2] != 2100 {
                    return Err("expected NudeNet 320n output [1,22,2100]".into());
                }
                let scores = postprocess(data, 2100)?;
                Ok(ScanResult {
                    sampled_timestamps_ms: vec![],
                    model_version: version,
                    scores,
                })
            })
            .await
            .map_err(|e| e.to_string())?
        })
    }
}
/// NudeNet's top class per box, confidence cutoff and class-agnostic NMS.
fn postprocess(data: &[f32], n: usize) -> Result<BTreeMap<String, f32>, String> {
    if data.len() != 22 * n || data.iter().any(|v| !v.is_finite()) {
        return Err("invalid model output".into());
    }
    let mut boxes = Vec::new();
    for i in 0..n {
        let (class, score) = (0..18)
            .map(|c| (c, data[(4 + c) * n + i]))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        if !(0.0..=1.0).contains(&score) {
            return Err("invalid model score".into());
        }
        if score > 0.25 {
            boxes.push((
                class,
                score,
                [
                    data[i] - data[2 * n + i] / 2.0,
                    data[n + i] - data[3 * n + i] / 2.0,
                    data[2 * n + i].max(0.0),
                    data[3 * n + i].max(0.0),
                ],
            ));
        }
    }
    boxes.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut selected: Vec<[f32; 4]> = Vec::new();
    let mut scores: BTreeMap<String, f32> = LABELS.iter().map(|s| (s.to_string(), 0.0)).collect();
    for (class, score, b) in boxes {
        if selected.iter().any(|a| {
            let intersection = ((a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0])).max(0.0)
                * ((a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1])).max(0.0);
            intersection / (a[2] * a[3] + b[2] * b[3] - intersection).max(f32::EPSILON) > 0.45
        }) {
            continue;
        }
        selected.push(b);
        let entry = scores.get_mut(LABELS[class]).unwrap();
        *entry = entry.max(score);
    }
    Ok(scores)
}
pub struct Http {
    client: reqwest::Client,
    url: String,
    secret: String,
    version: String,
}
impl Http {
    pub fn new(url: String, secret: String, version: String) -> Result<Self, String> {
        let parsed = reqwest::Url::parse(&url).map_err(|e| e.to_string())?;
        if !matches!(parsed.scheme(), "http" | "https")
            || secret.is_empty()
            || version.is_empty()
            || version.len() > 128
        {
            return Err("HTTP scanner requires URL, secret and model version".into());
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            url,
            secret,
            version,
        })
    }
}
impl Scanner for Http {
    fn version(&self) -> &str {
        &self.version
    }
    fn scan(&self, bytes: Vec<u8>) -> BoxFuture<'_, Result<ScanResult, String>> {
        Box::pin(async move {
            let bytes = tokio::task::spawn_blocking(move || {
                decode(&bytes)?;
                Ok::<_, String>(bytes)
            })
            .await
            .map_err(|e| e.to_string())??;
            let mut response = self
                .client
                .post(&self.url)
                .bearer_auth(&self.secret)
                .header("Content-Type", "application/octet-stream")
                .body(bytes)
                .send()
                .await
                .map_err(|_| "HTTP scanner unavailable")?
                .error_for_status()
                .map_err(|_| "HTTP scanner returned an error")?;
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| "invalid scanner response")?
            {
                if body.len() + chunk.len() > 65536 {
                    return Err("scanner response too large".into());
                }
                body.extend_from_slice(&chunk);
            }
            let mut result: ScanResult =
                serde_json::from_slice(&body).map_err(|_| "invalid scanner JSON")?;
            result.sampled_timestamps_ms.clear();
            result.validate()?;
            if result.model_version != self.version {
                return Err("HTTP scanner model version does not match configuration".into());
            }
            Ok(result)
        })
    }
}
