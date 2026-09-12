//! Local CPU inference. No runtime or model is downloaded by the server.
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
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
        if (!self.sampled_timestamps_ms.is_empty()
            && self.sampled_timestamps_ms.len() != super::video::SAMPLE_COUNT)
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
struct ModelInput {
    data: Vec<f32>,
    width: u32,
    height: u32,
}
fn input(bytes: &[u8]) -> Result<ModelInput, String> {
    let rgb = decode(bytes)?;
    let (width, height) = rgb.dimensions();
    let side = width.max(height);
    // Match NudeNet's OpenCV path: top-left square padding, INTER_LINEAR
    // resize, BGR planar channels, and [0,1] normalization. The reference
    // converts OpenCV BGR to RGB and blobFromImage swaps it back to BGR.
    // Sample the virtual padded square directly to avoid a second full image.
    let scale = f64::from(side) / 320.0;
    let mut data = vec![0.0; 3 * 320 * 320];
    if side == 320 {
        for (x, y, pixel) in rgb.enumerate_pixels() {
            for c in 0..3 {
                data[c * 320 * 320 + y as usize * 320 + x as usize] =
                    f32::from(pixel[2 - c]) / 255.0;
            }
        }
        return Ok(ModelInput {
            data,
            width,
            height,
        });
    }
    for y in 0..320 {
        let sy = ((y as f64 + 0.5) * scale - 0.5).clamp(0.0, f64::from(side - 1));
        let y0 = sy.floor() as u32;
        let y1 = (y0 + 1).min(side - 1);
        let fy = sy - f64::from(y0);
        for x in 0..320 {
            let sx = ((x as f64 + 0.5) * scale - 0.5).clamp(0.0, f64::from(side - 1));
            let x0 = sx.floor() as u32;
            let x1 = (x0 + 1).min(side - 1);
            let fx = sx - f64::from(x0);
            for c in 0..3 {
                let pixel = |px, py| {
                    if px < rgb.width() && py < rgb.height() {
                        f64::from(rgb.get_pixel(px, py)[2 - c])
                    } else {
                        0.0
                    }
                };
                let upper = pixel(x0, y0) * (1.0 - fx) + pixel(x1, y0) * fx;
                let lower = pixel(x0, y1) * (1.0 - fx) + pixel(x1, y1) * fx;
                data[c * 320 * 320 + y * 320 + x] =
                    ((upper * (1.0 - fy) + lower * fy).round() / 255.0) as f32;
            }
        }
    }
    Ok(ModelInput {
        data,
        width,
        height,
    })
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
            "nudenet320n:{}:bgr-bilinear-clipped-nms-v2",
            crate::storage::content_hash(&weights)
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
                let input = input(&bytes)?;
                let tensor = ort::value::Tensor::from_array(([1usize, 3, 320, 320], input.data))
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
                let scores = postprocess(data, 2100, input.width, input.height)?;
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
fn postprocess(
    data: &[f32],
    n: usize,
    width: u32,
    height: u32,
) -> Result<BTreeMap<String, f32>, String> {
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
            let bound_x = width as f32 / width.max(height) as f32 * 320.0;
            let bound_y = height as f32 / width.max(height) as f32 * 320.0;
            let x = (data[i] - data[2 * n + i] / 2.0).clamp(0.0, bound_x);
            let y = (data[n + i] - data[3 * n + i] / 2.0).clamp(0.0, bound_y);
            boxes.push((
                class,
                score,
                [
                    x,
                    y,
                    data[2 * n + i].max(0.0).min(bound_x - x),
                    data[3 * n + i].max(0.0).min(bound_y - y),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Deserialize)]
    struct Reference {
        width: u32,
        height: u32,
        samples: Vec<(usize, usize, [f32; 3])>,
    }
    #[test]
    fn preprocessing_matches_opencv_reference_channels_padding_and_resize() {
        // Generated with NudeNet v3's OpenCV 4.10 preprocessing. Differences
        // up to one 8-bit level account for OpenCV's fixed-point rounding.
        let fixtures: Vec<Reference> = serde_json::from_str(include_str!(
            "../../tests/fixtures/nudenet_preprocessing.json"
        ))
        .unwrap();
        for reference in fixtures {
            let rgb = image::RgbImage::from_fn(reference.width, reference.height, |x, y| {
                image::Rgb([
                    ((x * 13 + y * 3) % 256) as u8,
                    ((x * 7 + y * 11) % 256) as u8,
                    ((x * 5 + y * 17) % 256) as u8,
                ])
            });
            let mut bytes = Cursor::new(Vec::new());
            rgb.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            let actual = input(bytes.get_ref()).unwrap().data;
            for (x, y, expected) in reference.samples {
                for c in 0..3 {
                    assert!(
                        (actual[c * 320 * 320 + y * 320 + x] - expected[c]).abs()
                            <= 1.0 / 255.0 + 1e-6,
                        "{}x{} ({x},{y}) channel {c}",
                        reference.width,
                        reference.height
                    );
                }
            }
        }
    }
}
