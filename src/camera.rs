//! Webcam capture through `nokhwa`: Media Foundation on Windows, Video4Linux
//! on Linux, AVFoundation on macOS.
//!
//! The Insta360 Link 2 Pro presents itself as a standard UVC camera, so no
//! vendor SDK is needed. Frames are decoded here rather than in `nokhwa` so we
//! avoid pulling in a native JPEG library.

use anyhow::{Context, Result, anyhow, bail};
use image::RgbImage;
use nokhwa::Camera;
use nokhwa::utils::{
    ApiBackend, CameraFormat, CameraIndex, CameraInfo, FrameFormat, RequestedFormat,
    RequestedFormatType,
};

/// Substring used to auto-select the Insta360 camera when no index is given.
const PREFERRED_CAMERA: &str = "insta360";

#[cfg(windows)]
const BACKEND: ApiBackend = ApiBackend::MediaFoundation;
#[cfg(target_os = "linux")]
const BACKEND: ApiBackend = ApiBackend::Video4Linux;
#[cfg(target_os = "macos")]
const BACKEND: ApiBackend = ApiBackend::AVFoundation;

pub fn list_cameras() -> Result<Vec<CameraInfo>> {
    nokhwa::query(BACKEND).context("failed to enumerate cameras")
}

pub struct Webcam {
    camera: Camera,
    pub name: String,
    /// The mode we selected. The Media Foundation backend misreports the
    /// active frame rate (often as 1 fps), so remember what we asked for.
    chosen: Option<CameraFormat>,
}

impl Webcam {
    /// Open a camera by index, or the first Insta360 device (falling back to
    /// camera 0) when `index` is `None`.
    pub fn open(index: Option<u32>, width: u32, height: u32) -> Result<Self> {
        let cameras = list_cameras()?;
        if cameras.is_empty() {
            bail!("no cameras found");
        }
        let info = match index {
            Some(i) => cameras
                .iter()
                .find(|c| matches!(c.index(), CameraIndex::Index(n) if *n == i))
                .ok_or_else(|| anyhow!("camera index {i} not found (run `mtg-scan cameras`)"))?,
            None => cameras
                .iter()
                .find(|c| c.human_name().to_lowercase().contains(PREFERRED_CAMERA))
                .unwrap_or(&cameras[0]),
        };

        let decodable = [FrameFormat::MJPEG, FrameFormat::NV12, FrameFormat::YUYV];
        let request = RequestedFormat::with_formats(RequestedFormatType::None, &decodable);
        let mut camera = Camera::with_backend(info.index().clone(), request, BACKEND)
            .with_context(|| format!("failed to open camera '{}'", info.human_name()))?;

        // nokhwa's "closest" matching can land on a 1 fps mode, so choose
        // the format ourselves.
        let formats = camera.compatible_camera_formats().unwrap_or_default();
        let chosen = choose_format(&formats, width, height);
        if let Some(best) = chosen {
            camera
                .set_camera_requset(RequestedFormat::with_formats(
                    RequestedFormatType::Exact(best),
                    &decodable,
                ))
                .with_context(|| format!("failed to select {best}"))?;
        }
        camera
            .open_stream()
            .context("failed to start camera stream")?;

        Ok(Self {
            camera,
            name: info.human_name(),
            chosen,
        })
    }

    pub fn format(&self) -> CameraFormat {
        self.chosen.unwrap_or_else(|| self.camera.camera_format())
    }

    /// Grab and decode the next frame.
    pub fn frame(&mut self) -> Result<RgbImage> {
        let buffer = self.camera.frame().context("failed to read frame")?;
        let res = buffer.resolution();
        decode(
            buffer.buffer(),
            buffer.source_frame_format(),
            res.width(),
            res.height(),
        )
    }
}

impl Drop for Webcam {
    fn drop(&mut self) {
        let _ = self.camera.stop_stream();
    }
}

/// Pick the mode closest to the requested size, preferring MJPEG (the only
/// format UVC cameras can push at 4K over USB) and a frame rate near 30.
fn choose_format(formats: &[CameraFormat], width: u32, height: u32) -> Option<CameraFormat> {
    let wanted_px = (width * height) as i64;
    formats
        .iter()
        .filter(|f| {
            matches!(
                f.format(),
                FrameFormat::MJPEG | FrameFormat::NV12 | FrameFormat::YUYV
            )
        })
        .min_by_key(|f| {
            let size_off = ((f.width() * f.height()) as i64 - wanted_px).abs();
            let exact = !(f.width() == width && f.height() == height);
            let not_mjpeg = f.format() != FrameFormat::MJPEG;
            let fps_off = (f.frame_rate() as i64 - 30).abs();
            (exact, size_off, not_mjpeg, fps_off)
        })
        .copied()
}

fn decode(data: &[u8], format: FrameFormat, width: u32, height: u32) -> Result<RgbImage> {
    match format {
        FrameFormat::MJPEG => Ok(image::load_from_memory_with_format(
            data,
            image::ImageFormat::Jpeg,
        )
        .context("failed to decode MJPEG frame")?
        .to_rgb8()),
        FrameFormat::YUYV => Ok(yuyv_to_rgb(data, width, height)),
        FrameFormat::NV12 => Ok(nv12_to_rgb(data, width, height)),
        FrameFormat::RAWRGB => RgbImage::from_raw(width, height, data.to_vec())
            .ok_or_else(|| anyhow!("short RGB frame")),
        FrameFormat::RAWBGR => {
            let mut rgb = data.to_vec();
            rgb.as_chunks_mut::<3>()
                .0
                .iter_mut()
                .for_each(|px| px.swap(0, 2));
            RgbImage::from_raw(width, height, rgb).ok_or_else(|| anyhow!("short BGR frame"))
        }
        FrameFormat::GRAY => {
            let rgb = data.iter().flat_map(|&g| [g, g, g]).collect();
            RgbImage::from_raw(width, height, rgb).ok_or_else(|| anyhow!("short GRAY frame"))
        }
    }
}

fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let c = y as f32 - 16.0;
    let d = u as f32 - 128.0;
    let e = v as f32 - 128.0;
    let clamp = |x: f32| x.clamp(0.0, 255.0) as u8;
    [
        clamp(1.164 * c + 1.596 * e),
        clamp(1.164 * c - 0.392 * d - 0.813 * e),
        clamp(1.164 * c + 2.017 * d),
    ]
}

fn yuyv_to_rgb(data: &[u8], width: u32, height: u32) -> RgbImage {
    let mut out = RgbImage::new(width, height);
    for (i, chunk) in data.as_chunks::<4>().0.iter().enumerate() {
        let px = (i * 2) as u32;
        let (x, y) = (px % width, px / width);
        if y >= height {
            break;
        }
        let [y0, u, y1, v] = [chunk[0], chunk[1], chunk[2], chunk[3]];
        out.put_pixel(x, y, image::Rgb(yuv_to_rgb(y0, u, v)));
        if x + 1 < width {
            out.put_pixel(x + 1, y, image::Rgb(yuv_to_rgb(y1, u, v)));
        }
    }
    out
}

fn nv12_to_rgb(data: &[u8], width: u32, height: u32) -> RgbImage {
    let (w, h) = (width as usize, height as usize);
    let (luma, chroma) = data.split_at((w * h).min(data.len()));
    RgbImage::from_fn(width, height, |x, y| {
        let (x, y) = (x as usize, y as usize);
        let yv = luma.get(y * w + x).copied().unwrap_or(0);
        let ci = (y / 2) * w + (x & !1);
        let u = chroma.get(ci).copied().unwrap_or(128);
        let v = chroma.get(ci + 1).copied().unwrap_or(128);
        image::Rgb(yuv_to_rgb(yv, u, v))
    })
}

/// Discard the first frames so auto-exposure and auto-focus can settle.
pub fn warm_frame(cam: &mut Webcam) -> Result<RgbImage> {
    let start = std::time::Instant::now();
    let mut frame = cam.frame()?;
    while start.elapsed() < std::time::Duration::from_millis(1500) {
        frame = cam.frame()?;
    }
    Ok(frame)
}
