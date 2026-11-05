//! M1 真实后端：nokhwa 摄像头 + WinRT FaceDetector。
//!
//! 由 2026-11-05 ZH spike 验证过的管线改写而来（spike 只测检测、不锁屏）：
//! 摄像头 0..3 依次试、RGB 帧软件缩放到 320x240 灰度、Gray8 SoftwareBitmap、
//! DetectFacesAsync、像素框归一化。失败安全：摄像头忙/无设备/检测出错一律
//! 返回 None / 空框，绝不 panic、绝不把错误当成「主人不在」去锁屏。
//! 帧只在内存里流转，用完即丢，绝不落盘、不存人脸模板、不认身份。

use crate::presence::{
    CameraSource, FaceBox, FaceDetector, GrayFrame, CAPTURE_HEIGHT, CAPTURE_WIDTH, TICK_INTERVAL,
};

#[cfg(windows)]
pub struct NokhwaCamera {
    /// 摄像头独占线程发来的最新帧（sync_channel(1)，只留最新一帧）。
    /// nokhwa::Camera 本身不是 Send，不能放进 watcher 线程的 CameraSource，
    /// 所以它只活在下面 camera_loop 那个线程里，帧（纯 Vec<u8>，Send）
    /// 经通道交给 watcher 线程。这样摄像头只开一次、常开不关：
    /// 指示灯常亮不再闪烁，且 watcher 每拍不再等开摄像头，
    /// 计秒（dt = tick_interval）才和真实时间对得上，不再 30 秒计成 60 秒。
    /// 摄像头拿不到帧时通道里没新帧，next_frame 返回 None 当暂停，绝不发假黑帧。
    rx: std::sync::mpsc::Receiver<GrayFrame>,
    /// 停线程标志：CameraSource 被丢掉（presence_stop / watcher 退出）时置位，
    /// camera_loop 一拍内看到就关摄像头退出，指示灯不至于一直亮着。
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(windows)]
impl NokhwaCamera {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_thread = stop.clone();
        std::thread::Builder::new()
            .name("daozai-camera".into())
            .spawn(move || Self::camera_loop(tx, stop_thread))
            .expect("camera thread");
        Self { rx, stop }
    }

    /// 摄像头独占线程：只开一次、常开抓帧，Camera 绝不跨线程。
    /// 抓一帧 → 通道 try_send（满了就丢掉这帧，下拍发更新的，不排旧帧）→
    /// 按 TICK_INTERVAL 补齐一拍间隔再抓（抓帧耗时计入间隔内，周期约 1.5fps，
    /// 略快于 watcher 的 667ms+检测耗时，watcher 每拍基本都有新帧，不会无故暂停）。
    /// 抓失败就关掉重开（先试上次成功的索引）；全索引都开不了就每 500ms 重试，
    /// 期间不发帧，watcher 当暂停冻结倒计时。stop 置位或接收端断开就退出并关摄像头。
    fn camera_loop(
        tx: std::sync::mpsc::SyncSender<GrayFrame>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};
        let mut start_index: u32 = 0;
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            // 先试上次成功的索引，失败再轮 1..3，四个都开不了就 500ms 后重试。
            let mut opened: Option<nokhwa::Camera> = None;
            for offset in 0..4 {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let idx = (start_index + offset) % 4;
                if let Some(camera) = Self::try_open(idx) {
                    start_index = idx;
                    opened = Some(camera);
                    break;
                }
            }
            let Some(mut camera) = opened else {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            };
            loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let tick_start = Instant::now();
                let Some(frame) = Self::grab(&mut camera) else {
                    break; // 抓失败：关掉，回到外层重开（先试同一索引）。
                };
                match tx.try_send(frame) {
                    Ok(()) => {}
                    Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return,
                }
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let elapsed = tick_start.elapsed();
                if elapsed < TICK_INTERVAL {
                    std::thread::sleep(TICK_INTERVAL - elapsed);
                }
            }
            // camera 在此被 drop 关掉，200ms 后重开，避免忙循环反复开设备。
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn downsample(rgb: &[u8], w: usize, h: usize) -> Option<GrayFrame> {
        if w == 0 || h == 0 || rgb.len() < w * h * 3 {
            return None;
        }
        let tw = CAPTURE_WIDTH as usize;
        let th = CAPTURE_HEIGHT as usize;
        let mut pixels = vec![0u8; tw * th];
        for y in 0..th {
            let sy = y * h / th;
            for x in 0..tw {
                let sx = x * w / tw;
                let i = (sy * w + sx) * 3;
                let r = rgb[i] as u32;
                let g = rgb[i + 1] as u32;
                let b = rgb[i + 2] as u32;
                pixels[y * tw + x] = ((r * 299 + g * 587 + b * 114) / 1000) as u8;
            }
        }
        Some(GrayFrame { width: CAPTURE_WIDTH, height: CAPTURE_HEIGHT, pixels })
    }

    fn grab(camera: &mut nokhwa::Camera) -> Option<GrayFrame> {
        let frame = camera.frame().ok()?;
        let rgb = frame.decode_image::<nokhwa::pixel_format::RgbFormat>().ok()?;
        let res = camera.resolution();
        Self::downsample(&rgb, res.width() as usize, res.height() as usize)
    }

    fn try_open(index: u32) -> Option<nokhwa::Camera> {
        use nokhwa::pixel_format::RgbFormat;
        use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
        // spike 验证过的请求方式：最高帧率格式，软件再缩放到 320x240。
        let requested =
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
        let mut camera = nokhwa::Camera::new(CameraIndex::Index(index), requested).ok()?;
        camera.open_stream().ok()?;
        Some(camera)
    }
}

#[cfg(windows)]
impl Default for NokhwaCamera {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
impl Drop for NokhwaCamera {
    fn drop(&mut self) {
        self.stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(windows)]
impl CameraSource for NokhwaCamera {
    fn next_frame(&mut self) -> Option<GrayFrame> {
        // 把通道里攒的帧全取出来，只留最新一帧，旧帧不排队、不回放。
        // 通道空（摄像头线程还没发下一帧 / 摄像头打不开）就返回 None，
        // watcher 当暂停冻结倒计时；绝不返回上一帧冒充新帧，
        // 否则摄像头一死、画面一停，watcher 会永远以为主人在、永远不锁。
        let mut latest: Option<GrayFrame> = None;
        loop {
            match self.rx.try_recv() {
                Ok(frame) => latest = Some(frame),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        latest
    }
}

#[cfg(windows)]
pub struct WinRTFaceDetector {
    detector: Option<windows::Media::FaceAnalysis::FaceDetector>,
}

#[cfg(windows)]
impl WinRTFaceDetector {
    pub fn new() -> Self {
        let detector = match windows::Media::FaceAnalysis::FaceDetector::IsSupported() {
            Ok(true) => match windows::Media::FaceAnalysis::FaceDetector::CreateAsync() {
                Ok(op) => op.get().ok(),
                Err(_) => None,
            },
            _ => None,
        };
        Self { detector }
    }
}

#[cfg(windows)]
impl Default for WinRTFaceDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
impl FaceDetector for WinRTFaceDetector {
    fn detect(&mut self, frame: &GrayFrame) -> Vec<FaceBox> {
        use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
        use windows::Storage::Streams::DataWriter;
        let Some(detector) = self.detector.as_ref() else {
            return Vec::new();
        };
        if !frame.usable() {
            return Vec::new();
        }
        let w = frame.width as i32;
        let h = frame.height as i32;
        let bitmap = (|| -> windows::core::Result<SoftwareBitmap> {
            let writer = DataWriter::new()?;
            writer.WriteBytes(&frame.pixels)?;
            let buffer = writer.DetachBuffer()?;
            SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Gray8, w, h)
        })();
        let bitmap = match bitmap {
            Ok(b) => b,
            Err(_) => return Vec::new(),
        };
        let faces = match detector.DetectFacesAsync(&bitmap) {
            Ok(op) => match op.get() {
                Ok(f) => f,
                Err(_) => return Vec::new(),
            },
            Err(_) => return Vec::new(),
        };
        let count = match faces.Size() {
            Ok(n) => n,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        for i in 0..count {
            let Ok(face) = faces.GetAt(i) else { continue };
            let Ok(b) = face.FaceBox() else { continue };
            let fw = frame.width as f32;
            let fh = frame.height as f32;
            if fw <= 0.0 || fh <= 0.0 {
                continue;
            }
            out.push(FaceBox {
                x: (b.X as f32 / fw).clamp(0.0, 1.0),
                y: (b.Y as f32 / fh).clamp(0.0, 1.0),
                w: (b.Width as f32 / fw).clamp(0.0, 1.0),
                h: (b.Height as f32 / fh).clamp(0.0, 1.0),
            });
        }
        out
    }
}

/// 非 Windows 桩：永远拿不到帧、永远检不到脸，Linux 开发构建仍能编译，
/// 且因拿不到帧会被 PresenceWatcher 当作暂停，绝不会误锁。
#[cfg(not(windows))]
pub struct NokhwaCamera;

#[cfg(not(windows))]
impl NokhwaCamera {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(windows))]
impl Default for NokhwaCamera {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(windows))]
impl CameraSource for NokhwaCamera {
    fn next_frame(&mut self) -> Option<GrayFrame> {
        None
    }
}

#[cfg(not(windows))]
pub struct WinRTFaceDetector;

#[cfg(not(windows))]
impl WinRTFaceDetector {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(windows))]
impl Default for WinRTFaceDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(windows))]
impl FaceDetector for WinRTFaceDetector {
    fn detect(&mut self, _frame: &GrayFrame) -> Vec<FaceBox> {
        Vec::new()
    }
}
