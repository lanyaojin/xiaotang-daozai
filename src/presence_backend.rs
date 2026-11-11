//! 在场保护的 Windows 后端：nokhwa(Media Foundation) 采集 + WinRT 人脸检测。
//!
//! 与旧版的关键差异：
//! - 采集线程与检测线程都显式 `RoInitialize(MTA)`，WinRT 对象不再在 UI 线程创建；
//! - 摄像头枚举优先排除 IR/Windows Hello 摄像头（本机是 Integrated Camera +
//!   Integrated IR Camera 双设备，选错设备会拿到不可用画面）；
//! - 设备名、打开/失败、锁屏结果写入 presence.log，故障可见；
//! - 采集线程每拍检查 stop 标志，关闭功能后摄像头一定释放。

// ---------------------------------------------------------------------------
// Windows 实现
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub struct ComGuard {
    cleanup: bool,
}

#[cfg(windows)]
impl ComGuard {
    pub fn init() -> Self {
        use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
        let hr = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        // RPC_E_CHANGED_MODE：线程已被其它代码初始化（例如 STA），沿用即可，不重复清理。
        Self { cleanup: hr.is_ok() }
    }
}

#[cfg(windows)]
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.cleanup {
            unsafe { windows::Win32::System::WinRT::RoUninitialize(); }
        }
    }
}

#[cfg(windows)]
mod win {
    use crate::presence::{FaceBox, FaceDetector, FrameSource, GrayFrame, SourceStatus, FRAME_H, FRAME_W, TICK};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    /// 摄像头线程给检测线程的包：帧 + 当前设备状态。
    struct Packet {
        frame: GrayFrame,
        ok: bool,
        name: String,
    }

    pub struct FrameSourceImpl {
        rx: mpsc::Receiver<Packet>,
        stop: Arc<AtomicBool>,
        status: Arc<Mutex<SourceStatus>>,
    }

    impl FrameSourceImpl {
        pub fn new() -> Self {
            let (tx, rx) = mpsc::sync_channel::<Packet>(1);
            let stop = Arc::new(AtomicBool::new(false));
            let status = Arc::new(Mutex::new(SourceStatus { ok: false, name: String::new() }));
            let handle = std::thread::Builder::new()
                .name("daozai-camera".into())
                .spawn({
                    let stop = stop.clone();
                    let status = status.clone();
                    move || camera_loop(tx, stop, status)
                });
            if handle.is_err() {
                if let Ok(mut s) = status.lock() { s.name = "摄像头线程启动失败".into(); }
            }
            Self { rx, stop, status }
        }

        fn downsample(rgb: &[u8], w: usize, h: usize) -> Option<GrayFrame> {
            if w == 0 || h == 0 || rgb.len() < w * h * 3 { return None; }
            let tw = FRAME_W as usize;
            let th = FRAME_H as usize;
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
            Some(GrayFrame { width: FRAME_W, height: FRAME_H, pixels })
        }

        fn grab(camera: &mut nokhwa::Camera) -> Option<GrayFrame> {
            let buffer = camera.frame().ok()?;
            let (w, h) = {
                let res = buffer.resolution();
                (res.width() as usize, res.height() as usize)
            };
            let rgb = buffer.decode_image::<nokhwa::pixel_format::RgbFormat>().ok()?;
            Self::downsample(&rgb, w, h)
        }

        fn try_open(index: &nokhwa::utils::CameraIndex) -> Result<nokhwa::Camera, nokhwa::NokhwaError> {
            use nokhwa::pixel_format::RgbFormat;
            use nokhwa::utils::{RequestedFormat, RequestedFormatType};
            let requested = RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
            let mut camera = nokhwa::Camera::new(index.clone(), requested)?;
            camera.open_stream()?;
            Ok(camera)
        }
    }

    impl Default for FrameSourceImpl {
        fn default() -> Self { Self::new() }
    }

    impl Drop for FrameSourceImpl {
        fn drop(&mut self) { self.stop.store(true, Ordering::Relaxed); }
    }

    impl FrameSource for FrameSourceImpl {
        fn next_frame(&mut self) -> Option<GrayFrame> {
            let mut latest: Option<Packet> = None;
            loop {
                match self.rx.try_recv() {
                    Ok(p) => latest = Some(p),
                    Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected) => break,
                }
            }
            let packet = latest?;
            if let Ok(mut s) = self.status.lock() {
                s.ok = packet.ok;
                s.name = packet.name;
            }
            Some(packet.frame)
        }

        fn status(&self) -> SourceStatus {
            self.status.lock().map(|s| s.clone()).unwrap_or_default()
        }
    }

    /// 设备是否属于 IR / Windows Hello 摄像头（优先排除）。
    fn is_ir(name: &str) -> bool {
        let n = name.to_ascii_lowercase();
        n.contains("infrared")
            || n.contains("ir camera")
            || n.split_whitespace().any(|w| w == "ir")
            || n.contains("hello")
    }

    /// 枚举摄像头，非 IR 的排在前面；枚举失败时退回索引 0..3。
    fn candidates() -> Vec<(nokhwa::utils::CameraIndex, String)> {
        use nokhwa::utils::ApiBackend;
        let mut found: Vec<(bool, nokhwa::utils::CameraIndex, String)> = Vec::new();
        match nokhwa::query(ApiBackend::Auto) {
            Ok(infos) => {
                for info in infos {
                    let name = info.human_name();
                    found.push((is_ir(&name), info.index().clone(), name));
                }
            }
            Err(e) => {
                crate::presence_backend::log(&format!("camera query failed, fallback to index 0..4: {e}"));
            }
        }
        found.sort_by_key(|(ir, _, _)| *ir);
        let mut out: Vec<_> = found.into_iter().map(|(_, idx, name)| (idx, name)).collect();
        if out.is_empty() {
            for i in 0..4 { out.push((nokhwa::utils::CameraIndex::Index(i), format!("摄像头 {i}"))); }
        }
        out
    }

    fn camera_loop(
        tx: mpsc::SyncSender<Packet>,
        stop: Arc<AtomicBool>,
        status: Arc<Mutex<SourceStatus>>,
    ) {
        // 不要在本线程做任何 COM/WinRT 初始化：nokhwa 的 Windows 绑定会自己调用
        // CoInitializeEx(STA)；若先 RoInitialize(MTA)，STA 初始化得到 RPC_E_CHANGED_MODE，
        // Media Foundation 的枚举与打开会全部失败（2026-11-10 本机实测）。
        // WinRT 人脸检测线程（presence::run）不受影响，仍用 MTA。
        let mut devices = candidates();
        crate::presence_backend::log(&format!(
            "camera devices: {} (non-IR preferred)",
            devices.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(" | ")
        ));
        let mut last_index = 0usize;
        let mut fail_cycles: u32 = 0;
        let mut last_fail_detail = String::new();
        loop {
            if stop.load(Ordering::Relaxed) { return; }
            let mut opened: Option<(nokhwa::Camera, String)> = None;
            for offset in 0..devices.len() {
                if stop.load(Ordering::Relaxed) { return; }
                let idx = (last_index + offset) % devices.len();
                let (index, name) = &devices[idx];
                match FrameSourceImpl::try_open(index) {
                    Ok(camera) => {
                        last_index = idx;
                        if fail_cycles > 0 {
                            crate::presence_backend::log(&format!(
                                "camera opened after {fail_cycles} failed cycles: {name}"
                            ));
                        } else {
                            crate::presence_backend::log(&format!("camera opened: {name}"));
                        }
                        fail_cycles = 0;
                        last_fail_detail.clear();
                        opened = Some((camera, name.clone()));
                        break;
                    }
                    Err(e) => {
                        // 失败原因只在变化时记一次，避免每秒重试刷爆日志。
                        let detail = format!("camera open failed: {name}: {e}");
                        if detail != last_fail_detail {
                            crate::presence_backend::log(&detail);
                            last_fail_detail = detail;
                        }
                    }
                }
            }
            let Some((mut camera, name)) = opened else {
                if let Ok(mut s) = status.lock() { s.ok = false; s.name = "摄像头无法打开".into(); }
                fail_cycles += 1;
                // 每秒重试，但每 30 轮才记一条汇总；每 5 轮重新枚举一次设备。
                if fail_cycles == 1 || fail_cycles % 30 == 0 {
                    crate::presence_backend::log(&format!("all cameras failed, retrying (cycle {fail_cycles})"));
                }
                std::thread::sleep(Duration::from_secs(1));
                if fail_cycles % 5 == 0 { devices = candidates(); }
                continue;
            };
            loop {
                if stop.load(Ordering::Relaxed) { return; }
                let started = Instant::now();
                let Some(frame) = FrameSourceImpl::grab(&mut camera) else {
                    crate::presence_backend::log("camera frame failed, reopening");
                    break;
                };
                let packet = Packet { frame, ok: true, name: name.clone() };
                match tx.try_send(packet) {
                    Ok(()) => {}
                    Err(mpsc::TrySendError::Full(_)) => {}
                    Err(mpsc::TrySendError::Disconnected(_)) => return,
                }
                if stop.load(Ordering::Relaxed) { return; }
                std::thread::sleep(TICK.saturating_sub(started.elapsed()));
            }
            if let Ok(mut s) = status.lock() { s.ok = false; }
            // 关闭设备后稍等再重开，避免忙循环把摄像头锁死。
            drop(camera);
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    pub struct DetectorImpl {
        detector: Option<windows::Media::FaceAnalysis::FaceDetector>,
    }

    impl DetectorImpl {
        pub fn new() -> Self {
            use windows::Media::FaceAnalysis::FaceDetector as WinRtFaceDetector;
            let detector = match WinRtFaceDetector::IsSupported() {
                Ok(true) => match WinRtFaceDetector::CreateAsync() {
                    Ok(op) => op.get().ok(),
                    Err(e) => { crate::presence_backend::log(&format!("FaceDetector::CreateAsync failed: {e}")); None }
                },
                Ok(false) => { crate::presence_backend::log("FaceDetector not supported"); None }
                Err(e) => { crate::presence_backend::log(&format!("FaceDetector::IsSupported failed: {e}")); None }
            };
            if detector.is_some() { crate::presence_backend::log("FaceDetector ready (MTA)"); }
            Self { detector }
        }
    }

    impl Default for DetectorImpl {
        fn default() -> Self { Self::new() }
    }

    impl FaceDetector for DetectorImpl {
        fn available(&self) -> bool { self.detector.is_some() }

        fn detect(&mut self, frame: &GrayFrame) -> Vec<FaceBox> {
            use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
            use windows::Storage::Streams::DataWriter;
            let Some(detector) = self.detector.as_ref() else { return Vec::new() };
            if !frame.usable() { return Vec::new() }
            let bitmap = (|| -> windows::core::Result<SoftwareBitmap> {
                let writer = DataWriter::new()?;
                writer.WriteBytes(&frame.pixels)?;
                let buffer = writer.DetachBuffer()?;
                SoftwareBitmap::CreateCopyFromBuffer(
                    &buffer, BitmapPixelFormat::Gray8, frame.width as i32, frame.height as i32,
                )
            })();
            let Ok(bitmap) = bitmap else { return Vec::new() };
            let Ok(op) = detector.DetectFacesAsync(&bitmap) else { return Vec::new() };
            let Ok(faces) = op.get() else { return Vec::new() };
            let Ok(count) = faces.Size() else { return Vec::new() };
            let mut out = Vec::new();
            for i in 0..count {
                let Ok(face) = faces.GetAt(i) else { continue };
                let Ok(b) = face.FaceBox() else { continue };
                let fw = frame.width as f32;
                let fh = frame.height as f32;
                if fw <= 0.0 || fh <= 0.0 { continue }
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

    pub fn lock_workstation() -> bool {
        unsafe { windows::Win32::System::Shutdown::LockWorkStation().is_ok() }
    }
}

#[cfg(windows)]
pub use win::{DetectorImpl, FrameSourceImpl};

#[cfg(windows)]
pub fn lock_workstation() -> bool { win::lock_workstation() }

// ---------------------------------------------------------------------------
// 诊断日志：只写设备名与状态，不写图像、不写人脸数据
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub fn log(message: &str) {
    use std::io::Write;
    let path = crate::model::config_dir().join("presence.log");
    if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
    // 超过 64KB 时轮转，避免长期运行把AppData撑大。
    if std::fs::metadata(&path).map(|m| m.len() > 64 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{secs}] {message}");
    }
}

// ---------------------------------------------------------------------------
// 非 Windows 桩：功能恒暂停，但库与测试可编译、不会误锁屏
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
pub struct ComGuard;
#[cfg(not(windows))]
impl ComGuard { pub fn init() -> Self { Self } }

#[cfg(not(windows))]
pub struct FrameSourceImpl;
#[cfg(not(windows))]
impl FrameSourceImpl { pub fn new() -> Self { Self } }
#[cfg(not(windows))]
impl Default for FrameSourceImpl { fn default() -> Self { Self::new() } }
#[cfg(not(windows))]
impl crate::presence::FrameSource for FrameSourceImpl {
    fn next_frame(&mut self) -> Option<crate::presence::GrayFrame> { None }
    fn status(&self) -> crate::presence::SourceStatus {
        crate::presence::SourceStatus { ok: false, name: "非 Windows 平台".into() }
    }
}

#[cfg(not(windows))]
pub struct DetectorImpl;
#[cfg(not(windows))]
impl DetectorImpl { pub fn new() -> Self { Self } }
#[cfg(not(windows))]
impl Default for DetectorImpl { fn default() -> Self { Self::new() } }
#[cfg(not(windows))]
impl crate::presence::FaceDetector for DetectorImpl {
    fn detect(&mut self, _frame: &crate::presence::GrayFrame) -> Vec<crate::presence::FaceBox> { Vec::new() }
    fn available(&self) -> bool { false }
}

#[cfg(not(windows))]
pub fn lock_workstation() -> bool { false }
#[cfg(not(windows))]
pub fn log(_message: &str) {}

// ---------------------------------------------------------------------------
// Auto Mute：默认麦克风端点静音（模拟 Glance FEATURE_AUTO_MUTE_*）
//
// 复用 system.rs 的 IMMDeviceEnumerator 模式，但取 eCapture 而非 eRender。
// 关键：调用方是已初始化 COM(MTA) 的 presence 采集线程，这里绝不能再调
// CoInitializeEx(STA)——那正是 2026-11-10 摄像头打不开的根因。
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod mic {
    use std::cell::RefCell;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{eCapture, eConsole, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

    fn endpoint() -> Option<IAudioEndpointVolume> {
        thread_local! {
            static ENUM: RefCell<Option<IMMDeviceEnumerator>> = const { RefCell::new(None) };
        }
        unsafe {
            ENUM.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot.is_none() {
                    *slot = Some(CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?);
                }
                let dev = slot.as_ref()?.GetDefaultAudioEndpoint(eCapture, eConsole).ok()?;
                dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok()
            })
        }
    }

    pub fn muted() -> Option<bool> {
        unsafe { Some(endpoint()?.GetMute().ok()?.as_bool()) }
    }

    pub fn set(mute: bool) -> bool {
        unsafe { endpoint().is_some_and(|a| a.SetMute(mute, std::ptr::null()).is_ok()) }
    }
}

#[cfg(windows)]
pub fn mic_muted() -> Option<bool> { mic::muted() }
#[cfg(windows)]
pub fn set_mic_mute(mute: bool) -> bool { mic::set(mute) }
#[cfg(not(windows))]
pub fn mic_muted() -> Option<bool> { None }
#[cfg(not(windows))]
pub fn set_mic_mute(_mute: bool) -> bool { false }
