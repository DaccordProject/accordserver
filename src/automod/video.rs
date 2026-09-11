//! Five evenly spaced video samples, using bounded FFmpeg child processes.
//! Only self-contained MP4/MOV, WebM/Matroska and AVI containers are accepted.
use futures_util::future::BoxFuture;
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};

pub const SAMPLE_COUNT: usize = 5;
pub const CACHE_VERSION: &str = "video5-midpoints-320-v1";
#[derive(Clone, Copy)]
pub enum Container {
    Mov,
    Matroska,
    Avi,
}
impl Container {
    fn demuxer(self) -> &'static str {
        match self {
            Self::Mov => "mov",
            Self::Matroska => "matroska",
            Self::Avi => "avi",
        }
    }
}
pub fn container(bytes: &[u8]) -> Option<Container> {
    if bytes.len() >= 12 && matches!(&bytes[4..8], b"ftyp" | b"moov" | b"mdat" | b"wide") {
        Some(Container::Mov)
    } else if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        Some(Container::Matroska)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"AVI " {
        Some(Container::Avi)
    } else {
        None
    }
}
pub struct Samples {
    pub frames: Vec<Vec<u8>>,
    pub timestamps_ms: Vec<u64>,
}
pub trait VideoSampler: Send + Sync {
    fn status(&self) -> &'static str {
        "configured"
    }
    fn sample(&self, path: &Path, container: Container) -> BoxFuture<'_, Result<Samples, String>>;
}
pub struct Ffmpeg {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}
impl Default for Ffmpeg {
    fn default() -> Self {
        Self::new(PathBuf::from("ffmpeg"), PathBuf::from("ffprobe"))
    }
}
impl Ffmpeg {
    pub fn new(ffmpeg: PathBuf, ffprobe: PathBuf) -> Self {
        Self { ffmpeg, ffprobe }
    }
    pub fn from_env() -> Self {
        Self::new(
            std::env::var_os("ACCORD_AUTOMOD_FFMPEG_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| "ffmpeg".into()),
            std::env::var_os("ACCORD_AUTOMOD_FFPROBE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| "ffprobe".into()),
        )
    }
}
#[derive(Deserialize)]
struct Probe {
    streams: Vec<Stream>,
    format: Option<Format>,
}
#[derive(Deserialize)]
struct Stream {
    width: Option<u32>,
    height: Option<u32>,
    duration: Option<String>,
}
#[derive(Deserialize)]
struct Format {
    duration: Option<String>,
}
fn duration(probe: &Probe) -> Result<f64, String> {
    if probe.streams.len() != 1 {
        return Err("video requires exactly one video stream".into());
    }
    let stream = &probe.streams[0];
    let (width, height) = (stream.width.unwrap_or(0), stream.height.unwrap_or(0));
    if width == 0
        || height == 0
        || width > 4096
        || height > 4096
        || u64::from(width) * u64::from(height) > 16_000_000
    {
        return Err("video exceeds automod dimension limit".into());
    }
    let value = stream
        .duration
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| {
            probe
                .format
                .as_ref()?
                .duration
                .as_deref()?
                .parse::<f64>()
                .ok()
        })
        .ok_or("video duration unavailable")?;
    if !value.is_finite() || value <= 0.0 || value > 600.0 {
        return Err("video exceeds automod duration limit (10 minutes)".into());
    }
    Ok(value)
}
/// Midpoints of five equal intervals avoid an EOF seek at the exact duration.
pub fn timestamps(duration: f64) -> [f64; SAMPLE_COUNT] {
    std::array::from_fn(|i| duration * (i as f64 + 0.5) / SAMPLE_COUNT as f64)
}
async fn output(command: &mut Command, limit: u64) -> Result<Vec<u8>, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|_| "video decoder unavailable; install ffmpeg and ffprobe")?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or("video decoder output unavailable")?
        .take(limit + 1);
    let operation = async {
        let mut bytes = Vec::new();
        stdout
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "video decoder read failed")?;
        if bytes.len() as u64 > limit {
            return Err("video decoder output exceeds limit".into());
        }
        if !child
            .wait()
            .await
            .map_err(|_| "video decoder wait failed")?
            .success()
        {
            return Err("video decoding failed".into());
        }
        Ok(bytes)
    };
    let result = tokio::time::timeout(Duration::from_secs(10), operation)
        .await
        .map_err(|_| "video decoder timed out".to_string())?;
    if result.is_err() {
        let _ = child.start_kill();
    }
    result
}
impl VideoSampler for Ffmpeg {
    fn status(&self) -> &'static str {
        let exists = |binary: &Path| {
            if binary.is_absolute() || binary.components().count() > 1 {
                return binary.is_file();
            }
            std::env::var_os("PATH").is_some_and(|paths| {
                std::env::split_paths(&paths).any(|path| path.join(binary).is_file())
            })
        };
        if exists(&self.ffmpeg) && exists(&self.ffprobe) {
            "available"
        } else {
            "unavailable; install ffmpeg and ffprobe"
        }
    }

    fn sample(&self, path: &Path, container: Container) -> BoxFuture<'_, Result<Samples, String>> {
        let path = path.to_path_buf();
        Box::pin(async move {
            let mut probe = Command::new(&self.ffprobe);
            probe
                .args([
                    "-v",
                    "error",
                    "-max_alloc",
                    "67108864",
                    "-threads",
                    "1",
                    "-protocol_whitelist",
                    "file,pipe",
                    "-probesize",
                    "5000000",
                    "-analyzeduration",
                    "5000000",
                    "-f",
                    container.demuxer(),
                    "-select_streams",
                    "v",
                    "-show_entries",
                    "stream=width,height,duration:format=duration",
                    "-of",
                    "json",
                    "-i",
                ])
                .arg(&path);
            let metadata: Probe = serde_json::from_slice(&output(&mut probe, 65536).await?)
                .map_err(|_| "invalid video metadata")?;
            let duration = duration(&metadata)?;
            let mut frames = Vec::with_capacity(SAMPLE_COUNT);
            let mut timestamps_ms = Vec::with_capacity(SAMPLE_COUNT);
            for timestamp in timestamps(duration) {
                let mut decoder = Command::new(&self.ffmpeg);
                decoder.args(["-nostdin","-v","error","-max_alloc","67108864","-threads","1","-filter_threads","1","-protocol_whitelist","file,pipe","-probesize","5000000","-analyzeduration","5000000","-ss",&format!("{timestamp:.6}"),"-f",container.demuxer(),"-i"]).arg(&path)
                    .args(["-map","0:v:0","-frames:v","1","-an","-sn","-dn","-vf","scale=320:320:force_original_aspect_ratio=decrease:flags=bilinear,pad=320:320:0:0:black,setsar=1","-threads","1","-c:v","png","-f","image2pipe","pipe:1"]);
                let png = output(&mut decoder, 512 * 1024).await?;
                if png.is_empty() {
                    return Err("video sample unavailable; moderator review required".into());
                }
                frames.push(png);
                timestamps_ms.push((timestamp * 1000.0).round() as u64);
            }
            Ok(Samples {
                frames,
                timestamps_ms,
            })
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sample_positions_and_metadata_limits() {
        assert_eq!(timestamps(10.0), [1.0, 3.0, 5.0, 7.0, 9.0]);
        let probe = |duration| Probe {
            streams: vec![Stream {
                width: Some(1920),
                height: Some(1080),
                duration: Some(duration),
            }],
            format: None,
        };
        assert_eq!(duration(&probe("10".into())).unwrap(), 10.0);
        for value in ["NaN", "inf", "0", "601"] {
            assert!(duration(&probe(value.into())).is_err());
        }
        assert!(container(b"#EXTM3U\nhttps://example.com/video").is_none());
        assert!(container(b"\0\0\0\x18ftypmp42").is_some());
    }
}
