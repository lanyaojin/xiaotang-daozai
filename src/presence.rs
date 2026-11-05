//! Glance 替代 M1：在场检测＋离开锁定原型（纯逻辑层）。
//!
//! 本模块只做「在场」这一件事，不碰岛上 UI、不碰 Glance、不碰电视：
//! - 输入是已经降到低分辨率的灰度帧（目标 320x240，见 `CAPTURE_WIDTH/HEIGHT`），
//!   真实摄像头后端只负责把彩色帧转成灰度、缩放到这个尺寸再交进来，
//!   帧只在内存里流转，用完即丢，绝不落盘存图；
//! - 不做身份识别、不存人脸模板、不存照片：输出只有在场/数量/大小/倒计时；
//! - 主人判定只用「最大、最稳」：坐得最近、框最大、最稳定的那张脸就是主人，
//!   用框重叠/中心点跨帧跟住，不认人，换个人坐下也不会被当成主人（见重获规则）；
//! - 低频轻量起步：默认约 1.5fps（`TICK_INTERVAL`），画面基本没变就跳过检测，
//!   先做再优化，不要高帧率、不要常驻大模型。
//!
//! 真实后端在 `presence_backend` 里（2026-11-05 ZH spike 验证后接线）：
//! - 摄像头：nokhwa（Windows 下即 MediaFoundation 封装）；
//! - 人脸检测：Windows 原生 FaceDetector（WinRT，系统自带、不用下模型）。
//! - M1 自带 `FaceDetector`/`CameraSource` 两个 trait，真实后端只要实现这两个 trait
//!   就能插进 `PresenceWatcher`，锁定逻辑不用改；单元测试用内存里的假源跑通逻辑。
//!
//! M2 / M3 预留（M1 只留状态位，不做 UI）：
//! - M2 偷窥提示：读 `PresenceSnapshot.disturbance`（除主人外的脸）＋ `other_faces`，
//!   再结合停留时长/稳定性分「偷窥」（持续朝屏）与「快速路过」（一闪而过）；
//!   M1 的 `disturbance` 只做到「有非主人脸」这一层，不分偷窥/路过，不做视线方向
//!   （2026-11-05 用户已定：偷窥判断用人脸数量＋停留时长/稳定性，不用视线，更轻量）；
//! - M3 坐姿距离：复用同一个主人框，用框大小相对基线判断「坐太近」，M1 不做，
//!   只保证 `owner_box`（归一化坐标＋面积）已输出，M3 不用再改检测。
//!
//! 失败安全（绝不乱锁）三条：
//! 1. 主人从没出现过不布防：刚启动、一次主人脸都没见过时，即使 30 秒没脸也不锁；
//! 2. 太黑/快门关了/摄像头拿不到帧时冻结倒计时，不涨也不清零，等画面恢复再继续；
//! 3. 主人短暂低头、侧脸、遮挡几秒不算：只要 30 秒连续缺席没满，主人一回来就清零。

use std::time::Duration;

/// Glance 在场阈值：连续 30 秒完全没有主人脸才锁（用户 2026-11-04 已确认 30 秒）。
pub const LOCK_AFTER_SECS: f64 = 30.0;
/// M1 采样间隔：约 1.5fps，落在任务要求的 1-2fps 内。
pub const TICK_INTERVAL: Duration = Duration::from_millis(667);
/// 摄像头目标分辨率：低分辨率以内，真实后端必须先缩放到这个尺寸再交进来。
pub const CAPTURE_WIDTH: u32 = 320;
pub const CAPTURE_HEIGHT: u32 = 240;
/// 太黑判定：灰度均值低于它就当快门关了/太黑，冻结倒计时（0..255）。
pub const DARK_MEAN_THRESHOLD: f32 = 12.0;
/// 帧差门限：平均绝对差低于它就当画面没变，跳过本次检测、复用上次框。
pub const MOTION_GATE: f32 = 0.01;
/// 主人框丢失后保留几拍用于重获（约 2 秒）：原主人回到原座位附近仍算主人，
/// 换个人坐到明显不同的位置不算主人（只算扰动）。
const OWNER_DROP_TICKS: u32 = 3;
/// 框匹配门限：IoU 或中心点距离（二者满足其一即算同一张脸）。
const MATCH_IOU: f32 = 0.2;
const MATCH_CENTER_DIST: f32 = 0.25;

/// 归一化人脸框（坐标 0.0..1.0，与分辨率无关，M3 坐姿可直接复用面积）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl FaceBox {
    pub fn area(&self) -> f32 {
        (self.w.max(0.0)) * (self.h.max(0.0))
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// 交并比（IoU），0 表示没交叠，1 表示完全重合。
    pub fn iou(&self, other: &FaceBox) -> f32 {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        let inter_w = (x1 - x0).max(0.0);
        let inter_h = (y1 - y0).max(0.0);
        let inter = inter_w * inter_h;
        if inter <= 0.0 {
            return 0.0;
        }
        let union = self.area() + other.area() - inter;
        if union <= 0.0 {
            0.0
        } else {
            inter / union
        }
    }

    pub fn center_distance(&self, other: &FaceBox) -> f32 {
        let (ax, ay) = self.center();
        let (bx, by) = other.center();
        ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt()
    }

    /// 是不是同一张脸（位置/大小都接近才算，避免把旁边的人认成主人）。
    pub fn matches(&self, other: &FaceBox) -> bool {
        if self.iou(other) >= MATCH_IOU {
            return true;
        }
        if self.center_distance(other) >= MATCH_CENTER_DIST {
            return false;
        }
        // 中心点接近时还要面积别差太多（坐远/坐近会变大小，但不会一拍差几倍）。
        let (a, b) = (self.area(), other.area());
        if a <= 0.0 || b <= 0.0 {
            return false;
        }
        let ratio = (a / b).max(b / a);
        ratio <= 2.0
    }
}

/// 内存里的灰度帧：只存一帧像素（320x240 约 76KB），不落盘、不缓存历史帧。
#[derive(Clone, Debug)]
pub struct GrayFrame {
    pub width: u32,
    pub height: u32,
    /// 行主序灰度像素，0=黑，255=白，长度应为 width*height。
    pub pixels: Vec<u8>,
}

impl GrayFrame {
    pub fn blank(width: u32, height: u32, value: u8) -> Self {
        Self {
            width,
            height,
            pixels: vec![value; (width as usize) * (height as usize)],
        }
    }

    pub fn usable(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.pixels.len() == (self.width as usize) * (self.height as usize)
    }

    pub fn mean_brightness(&self) -> f32 {
        if self.pixels.is_empty() {
            return 0.0;
        }
        let sum: u64 = self.pixels.iter().map(|p| *p as u64).sum();
        sum as f32 / self.pixels.len() as f32
    }

    pub fn is_dark(&self) -> bool {
        self.mean_brightness() < DARK_MEAN_THRESHOLD
    }

    /// 与上一帧的平均绝对差（0..1，已下采样到约 80x60 网格算，省 CPU）。
    /// 尺寸不一致、任一帧不可用时返回 1.0（当作画面变了，不跳过检测）。
    pub fn mean_abs_diff(&self, other: &GrayFrame) -> f32 {
        if !self.usable() || !other.usable() {
            return 1.0;
        }
        if self.width != other.width || self.height != other.height {
            return 1.0;
        }
        let step_x = (self.width / 80).max(1) as usize;
        let step_y = (self.height / 60).max(1) as usize;
        let mut sum: u64 = 0;
        let mut count: u64 = 0;
        for y in (0..self.height as usize).step_by(step_y) {
            for x in (0..self.width as usize).step_by(step_x) {
                let idx = y * self.width as usize + x;
                let a = self.pixels[idx] as i32;
                let b = other.pixels[idx] as i32;
                sum += (a - b).unsigned_abs() as u64;
                count += 1;
            }
        }
        if count == 0 {
            1.0
        } else {
            sum as f32 / count as f32 / 255.0
        }
    }
}

/// 人脸检测器抽象：真实后端（WinRT FaceDetector 或轻量 ONNX）实现它即可插进来。
/// M1 不规定它怎么实现，只规定它吃灰度帧、吐归一化框，且绝不落盘、不认身份。
pub trait FaceDetector {
    fn detect(&mut self, frame: &GrayFrame) -> Vec<FaceBox>;
}

/// 摄像头源抽象：真实后端（nokhwa 优先）实现它即可插进来。
/// 返回 None 表示摄像头拿不到帧（被占用、没插、权限没给等），调用方当作暂停处理，
/// 绝不能把「拿不到帧」当成「主人不在」去倒计时。
pub trait CameraSource {
    fn next_frame(&mut self) -> Option<GrayFrame>;
}

/// 一拍的在场快照：M2 岛上提示直接读它，M1 不渲染它。
#[derive(Clone, Debug, PartialEq)]
pub struct PresenceSnapshot {
    /// 主人在不在（已跟住的主人框当前拍可见）。
    pub owner_present: bool,
    /// 主人框（归一化坐标；不在时为 None；M3 坐姿复用它的面积）。
    pub owner_box: Option<FaceBox>,
    /// 除主人外的脸数（0 表示没有别人）。
    pub other_faces: usize,
    /// 有没有除主人外的扰动（M1 定义：other_faces > 0；M2 再分偷窥/路过）。
    pub disturbance: bool,
    /// 连续缺席秒数（主人一回来就清零；太黑/拿不到帧时冻结不动）。
    pub away_secs: f64,
    /// 距离锁定还剩几秒（0 表示已到锁定线；没布防时就是满格 LOCK_AFTER_SECS）。
    pub countdown_secs: f64,
    /// 本拍是否触发锁定（调用方见它为 true 就调一次锁屏，随后调 `reset()`）。
    pub should_lock: bool,
    /// 太黑/快门关了（倒计时已冻结，绝不乱锁）。
    pub paused_dark: bool,
    /// 摄像头本拍有没有给帧（false 时倒计时同样冻结）。
    pub camera_available: bool,
    /// 本拍相对上拍的画面变化（0=没变，1=全变；用于诊断，不作锁定依据）。
    pub motion: f32,
    /// 本拍是否因画面没变而跳过检测（省 CPU 的证据）。
    pub detector_skipped: bool,
}

/// M1 可调参数：默认值就是任务定的值，改动时只改这里，不要散落在逻辑里。
#[derive(Clone, Copy, Debug)]
pub struct PresenceConfig {
    pub lock_after_secs: f64,
    pub dark_mean_threshold: f32,
    pub motion_gate: f32,
    pub tick_interval: Duration,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            lock_after_secs: LOCK_AFTER_SECS,
            dark_mean_threshold: DARK_MEAN_THRESHOLD,
            motion_gate: MOTION_GATE,
            tick_interval: TICK_INTERVAL,
        }
    }
}

/// 主人跟踪器：只跟「最大、最稳」那张脸，不认身份。
struct OwnerTracker {
    /// 当前跟住的主人框（丢失几拍内仍保留，用于原座位重获）。
    owner: Option<FaceBox>,
    /// 主人框连续丢失拍数（到 OWNER_DROP_TICKS 就把当前框丢掉，只留 last_owner 供重获比对）。
    missing_ticks: u32,
    /// 最近一次的主人框（丢掉后仍保留，用于判断回来的那张脸是不是原主人）。
    last_owner: Option<FaceBox>,
    /// 自启动以来有没有见过主人（没见过就不布防，见 should_lock 的注释）。
    owner_ever: bool,
}

struct TrackerUpdate {
    owner_present: bool,
    owner_box: Option<FaceBox>,
    other_faces: usize,
}

impl OwnerTracker {
    fn new() -> Self {
        Self {
            owner: None,
            missing_ticks: 0,
            last_owner: None,
            owner_ever: false,
        }
    }

    fn reset(&mut self) {
        self.owner = None;
        self.missing_ticks = 0;
        self.last_owner = None;
        self.owner_ever = false;
    }

    fn update(&mut self, faces: &[FaceBox]) -> TrackerUpdate {
        // 从没见过主人：第一张最稳的脸（最大那张）就是主人候选。
        if self.owner.is_none() && !self.owner_ever {
            if let Some(best) = largest(faces) {
                self.owner = Some(best);
                self.last_owner = Some(best);
                self.owner_ever = true;
                self.missing_ticks = 0;
                return TrackerUpdate {
                    owner_present: true,
                    owner_box: Some(best),
                    other_faces: faces.len().saturating_sub(1),
                };
            }
            return TrackerUpdate {
                owner_present: false,
                owner_box: None,
                other_faces: 0,
            };
        }

        // 已经有主人（或丢过主人）：先看当前拍有没有跟住主人。
        if let Some(current) = self.owner {
            if let Some(matched) = faces.iter().find(|f| current.matches(f)) {
                let matched = *matched;
                self.owner = Some(matched);
                self.last_owner = Some(matched);
                self.missing_ticks = 0;
                return TrackerUpdate {
                    owner_present: true,
                    owner_box: Some(matched),
                    other_faces: faces.len().saturating_sub(1),
                };
            }
            // 主人本拍没出现：记丢失拍数，别人都算扰动，不提拔新人当主人。
            self.missing_ticks += 1;
            if self.missing_ticks >= OWNER_DROP_TICKS {
                self.owner = None;
                self.missing_ticks = 0;
            }
            return TrackerUpdate {
                owner_present: false,
                owner_box: None,
                other_faces: faces.len(),
            };
        }

        // 主人丢过（last_owner 还在）：回来的脸只有位置/大小都像原主人才算重获，
        // 坐到明显不同位置的新脸只算扰动，不当主人（防陌生人坐下就接管）。
        if let Some(last) = self.last_owner {
            if let Some(matched) = faces.iter().find(|f| last.matches(f)) {
                let matched = *matched;
                self.owner = Some(matched);
                self.last_owner = Some(matched);
                self.missing_ticks = 0;
                return TrackerUpdate {
                    owner_present: true,
                    owner_box: Some(matched),
                    other_faces: faces.len().saturating_sub(1),
                };
            }
        }
        TrackerUpdate {
            owner_present: false,
            owner_box: None,
            other_faces: faces.len(),
        }
    }
}

fn largest(faces: &[FaceBox]) -> Option<FaceBox> {
    faces
        .iter()
        .max_by(|a, b| a.area().partial_cmp(&b.area()).unwrap_or(std::cmp::Ordering::Equal))
        .copied()
}

/// 在场状态机：纯逻辑，不碰摄像头、不碰 Win32（锁屏动作在 `lock_workstation()` 里单点做）。
pub struct PresenceWatcher {
    config: PresenceConfig,
    tracker: OwnerTracker,
    last_frame: Option<GrayFrame>,
    last_boxes: Vec<FaceBox>,
    away_secs: f64,
    /// 已触发过锁定、等调用方 reset（防一拍触发多次锁屏调用）。
    locked: bool,
    /// 累计因画面没变而省掉的检测次数（诊断用，证明低频门限在起作用）。
    pub detector_skipped_total: u64,
}

impl PresenceWatcher {
    pub fn new(config: PresenceConfig) -> Self {
        Self {
            config,
            tracker: OwnerTracker::new(),
            last_frame: None,
            last_boxes: Vec::new(),
            away_secs: 0.0,
            locked: false,
            detector_skipped_total: 0,
        }
    }

    /// 锁屏后调用：清掉主人/倒计时/锁定态，下一登录后的第一张脸重新当主人。
    /// 注意：不清除 detector_skipped_total（它是累计诊断数）。
    pub fn reset(&mut self) {
        self.tracker.reset();
        self.last_frame = None;
        self.last_boxes.clear();
        self.away_secs = 0.0;
        self.locked = false;
    }

    /// 推进一拍：dt_secs 是本拍距上拍的秒数（正常就是 tick_interval，测试可传任意值）。
    /// 摄像头给 None、帧不可用、太黑时都冻结倒计时，绝不把「看不见」当成「人走了」。
    pub fn tick(
        &mut self,
        frame: Option<GrayFrame>,
        dt_secs: f64,
        detector: &mut dyn FaceDetector,
    ) -> PresenceSnapshot {
        let Some(frame) = frame else {
            return self.snapshot_paused(false, 0.0, false);
        };
        if !frame.usable() {
            return self.snapshot_paused(true, 0.0, false);
        }
        if frame.mean_brightness() < self.config.dark_mean_threshold {
            // 太黑：不检测、不涨倒计时、不清零，上一帧也别留（黑帧不能当基准）。
            return self.snapshot_paused(true, frame.mean_brightness(), true);
        }

        let motion = match &self.last_frame {
            Some(prev) => prev.mean_abs_diff(&frame),
            None => 1.0,
        };
        // 画面基本没变且上拍已有检测结果：复用上次框，省一次检测。
        // 第一拍（last_frame 为 None）一定走检测，先立一个基准。
        let (boxes, skipped) = if self.last_frame.is_some() && motion < self.config.motion_gate {
            self.detector_skipped_total += 1;
            (self.last_boxes.clone(), true)
        } else {
            let boxes = detector.detect(&frame);
            self.last_boxes = boxes.clone();
            (boxes, false)
        };
        self.last_frame = Some(frame);

        let update = self.tracker.update(&boxes);
        if update.owner_present {
            self.away_secs = 0.0;
        } else if !self.locked {
            // 只有「已见过主人、没锁过、画面正常」才涨倒计时；太黑/拿不到帧在上面已提前返回。
            self.away_secs += dt_secs.max(0.0);
        }
        // 没布防（一次主人脸都没见过）时不锁：刚启动、桌前没人不是「离开」。
        let should_lock = !self.locked
            && self.tracker.owner_ever
            && update.owner_present == false
            && self.away_secs >= self.config.lock_after_secs;
        if should_lock {
            self.locked = true;
        }
        PresenceSnapshot {
            owner_present: update.owner_present,
            owner_box: update.owner_box,
            other_faces: update.other_faces,
            disturbance: update.other_faces > 0,
            away_secs: self.away_secs,
            countdown_secs: (self.config.lock_after_secs - self.away_secs).max(0.0),
            should_lock,
            paused_dark: false,
            camera_available: true,
            motion,
            detector_skipped: skipped,
        }
    }

    /// 暂停快照：倒计时冻结（away_secs 不动），owner_present 置 false 但附带暂停位，
    /// 调用方（以后 M2 的岛上提示）必须先看 paused 位，别把「暂停」读成「人走了」。
    fn snapshot_paused(
        &mut self,
        camera_available: bool,
        _mean: f32,
        dark: bool,
    ) -> PresenceSnapshot {
        PresenceSnapshot {
            owner_present: false,
            owner_box: None,
            other_faces: 0,
            disturbance: false,
            away_secs: self.away_secs,
            countdown_secs: (self.config.lock_after_secs - self.away_secs).max(0.0),
            should_lock: false,
            paused_dark: dark,
            camera_available,
            motion: 0.0,
            detector_skipped: false,
        }
    }
}

/// 调一次 Windows 锁屏（LockWorkStation，windows crate
/// Win32_System_Shutdown 特性）。
/// 非 Windows 下是空操作桩（返回 false），只用于逻辑测试，绝不假装锁过。
#[cfg(windows)]
pub fn lock_workstation() -> bool {
    unsafe { windows::Win32::System::Shutdown::LockWorkStation().is_ok() }
}

#[cfg(not(windows))]
pub fn lock_workstation() -> bool {
    false
}

/// 后台采样循环（M1 原型入口，app.rs 以后接它）：按 tick_interval 采样，
/// 只在状态变化时发快照（主人/扰动/暂停/锁定/倒计时整秒变化/主人框变化（M3 坐姿用）），不满 1 个的旧快照直接丢掉，
/// 界面不会被每拍刷屏；接收端断开就停线程。
/// 锁屏成功后自动 reset，等下次登录重新认主人。
pub fn spawn_watcher(
    config: PresenceConfig,
    mut camera: Box<dyn CameraSource + Send>,
    mut detector: Box<dyn FaceDetector + Send>,
) -> (
    std::sync::mpsc::Receiver<PresenceSnapshot>,
    std::thread::JoinHandle<()>,
) {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let handle = std::thread::Builder::new()
        .name("daozai-presence".into())
        .spawn(move || {
            let mut watcher = PresenceWatcher::new(config);
            let mut last_sent: Option<PresenceSnapshot> = None;
            let dt = config.tick_interval.as_secs_f64();
            loop {
                let frame = camera.next_frame();
                let snap = watcher.tick(frame, dt, detector.as_mut());
                let changed = match &last_sent {
                    None => true,
                    Some(prev) => {
                        prev.owner_present != snap.owner_present
                            || prev.other_faces != snap.other_faces
                            || prev.disturbance != snap.disturbance
                            || prev.paused_dark != snap.paused_dark
                            || prev.camera_available != snap.camera_available
                            || prev.should_lock != snap.should_lock
                            || prev.owner_box != snap.owner_box
                            || (prev.away_secs as u64) != (snap.away_secs as u64)
                    }
                };
                if snap.should_lock {
                    // 先发锁定快照（接收端断开也别漏），再调锁屏、再 reset。
                    let _ = tx.try_send(snap.clone());
                    if lock_workstation() {
                        watcher.reset();
                        last_sent = None;
                    }
                } else if changed {
                    match tx.try_send(snap.clone()) {
                        Ok(()) => last_sent = Some(snap),
                        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return,
                        Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                    }
                }
                std::thread::sleep(config.tick_interval);
            }
        })
        .expect("presence thread");
    (rx, handle)
}

// 真实后端（2026-11-05 ZH spike 已验证，后接线到 presence_backend.rs）：
// - 摄像头：nokhwa 0.10 input-native，索引 0 1920x1080 出帧，软件缩放到 320x240 灰度；
// - 检测：WinRT FaceDetector，Gray8 SoftwareBitmap，DetectFacesAsync，
//   合成图 faces=0、真机一帧 faces=1 约 7ms；SSH 与 Lijin 会话下摄像头均通。
// - 备选（仅当 WinRT 走不通时单独立项）：YuNet 级轻量 ONNX，注意运行时体积与分发成本。
// - app.rs 接线：presence_start / presence_stop / presence_poll，只存快照、不再调锁屏
//   （spawn_watcher 里 should_lock 时已经调过一次锁屏并 reset）。

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用检测器：按预设表一拍一拍吐框，并计数（验证帧差门限省了几次检测）。
    struct FakeDetector {
        script: Vec<Vec<FaceBox>>,
        calls: usize,
    }

    impl FakeDetector {
        fn new(script: Vec<Vec<FaceBox>>) -> Self {
            Self { script, calls: 0 }
        }
    }

    impl FaceDetector for FakeDetector {
        fn detect(&mut self, _frame: &GrayFrame) -> Vec<FaceBox> {
            self.calls += 1;
            let idx = (self.calls - 1).min(self.script.len().saturating_sub(1));
            self.script.get(idx).cloned().unwrap_or_default()
        }
    }

    fn owner_box() -> FaceBox {
        FaceBox {
            x: 0.3,
            y: 0.2,
            w: 0.3,
            h: 0.4,
        }
    }

    fn stranger_box() -> FaceBox {
        FaceBox {
            x: 0.7,
            y: 0.2,
            w: 0.2,
            h: 0.3,
        }
    }

    fn bright_frame() -> GrayFrame {
        GrayFrame::blank(CAPTURE_WIDTH, CAPTURE_HEIGHT, 128)
    }

    fn frame_value(value: u8) -> GrayFrame {
        GrayFrame::blank(CAPTURE_WIDTH, CAPTURE_HEIGHT, value)
    }

    #[test]
    fn frame_diff_identical_is_zero_and_changed_is_high() {
        let a = frame_value(0);
        let b = frame_value(0);
        assert!(a.mean_abs_diff(&b) < 0.001);
        let c = frame_value(255);
        assert!(a.mean_abs_diff(&c) > 0.9);
    }

    #[test]
    fn dark_frame_is_dark_and_freezes_countdown() {
        let mut watcher = PresenceWatcher::new(PresenceConfig::default());
        let mut detector = FakeDetector::new(vec![vec![owner_box()]]);
        let snap = watcher.tick(Some(bright_frame()), 1.0, &mut detector);
        assert!(snap.owner_present);
        // 主人走开 10 秒。
        let mut detector = FakeDetector::new(vec![vec![]]);
        let snap = watcher.tick(Some(frame_value(200)), 10.0, &mut detector);
        assert!(!snap.owner_present);
        assert!((snap.away_secs - 10.0).abs() < 1e-6);
        // 太黑一拍：倒计时冻结，不涨也不清零。
        let dark = GrayFrame::blank(CAPTURE_WIDTH, CAPTURE_HEIGHT, 5);
        let snap = watcher.tick(Some(dark), 10.0, &mut detector);
        assert!(snap.paused_dark);
        assert!(!snap.should_lock);
        assert!((snap.away_secs - 10.0).abs() < 1e-6);
    }

    #[test]
    fn thirty_seconds_continuous_absence_locks_and_return_resets() {
        let mut watcher = PresenceWatcher::new(PresenceConfig::default());
        let mut detector = FakeDetector::new(vec![vec![owner_box()]]);
        assert!(watcher
            .tick(Some(bright_frame()), 1.0, &mut detector)
            .owner_present);
        // 29 秒缺席：还没到线。
        let mut detector = FakeDetector::new(vec![vec![]]);
        let snap = watcher.tick(Some(frame_value(200)), 29.0, &mut detector);
        assert!(!snap.should_lock);
        assert!((snap.countdown_secs - 1.0).abs() < 1e-6);
        // 再 1 秒：到线，触发一次。
        let snap = watcher.tick(Some(bright_frame()), 1.0, &mut detector);
        assert!(snap.should_lock);
        // 没 reset 前不再重复触发。
        let snap = watcher.tick(Some(bright_frame()), 1.0, &mut detector);
        assert!(!snap.should_lock);
        // reset 后主人回来：重新布防，不立即锁。
        watcher.reset();
        let mut detector = FakeDetector::new(vec![vec![owner_box()]]);
        let snap = watcher.tick(Some(bright_frame()), 1.0, &mut detector);
        assert!(snap.owner_present);
        assert!(!snap.should_lock);
    }

    #[test]
    fn brief_absence_resets_on_return() {
        let mut watcher = PresenceWatcher::new(PresenceConfig::default());
        let mut detector = FakeDetector::new(vec![vec![owner_box()]]);
        assert!(watcher
            .tick(Some(bright_frame()), 1.0, &mut detector)
            .owner_present);
        // 低头/侧脸 5 秒（超过 OWNER_DROP_TICKS 但远不到 30 秒），回来仍是主人。
        let mut detector = FakeDetector::new(vec![vec![]]);
        let snap = watcher.tick(Some(frame_value(200)), 5.0, &mut detector);
        assert!(!snap.owner_present);
        let mut detector = FakeDetector::new(vec![vec![owner_box()]]);
        let snap = watcher.tick(Some(bright_frame()), 1.0, &mut detector);
        assert!(snap.owner_present);
        assert!((snap.away_secs).abs() < 1e-6);
        assert!(!snap.should_lock);
    }

    #[test]
    fn never_seen_owner_does_not_arm_lock() {
        let mut watcher = PresenceWatcher::new(PresenceConfig::default());
        let mut detector = FakeDetector::new(vec![vec![]]);
        // 桌前从没人：即使 60 秒没脸也不锁（不是「离开」，是「一直没来」）。
        let snap = watcher.tick(Some(bright_frame()), 60.0, &mut detector);
        assert!(!snap.should_lock);
    }

    #[test]
    fn second_face_is_disturbance_not_new_owner() {
        let mut watcher = PresenceWatcher::new(PresenceConfig::default());
        let mut detector = FakeDetector::new(vec![vec![owner_box()]]);
        assert!(watcher
            .tick(Some(bright_frame()), 1.0, &mut detector)
            .owner_present);
        // 主人还在，旁边多一张脸：扰动 true，主人还是原来那张。
        let mut detector = FakeDetector::new(vec![vec![owner_box(), stranger_box()]]);
        let snap = watcher.tick(Some(frame_value(200)), 1.0, &mut detector);
        assert!(snap.owner_present);
        assert_eq!(snap.other_faces, 1);
        assert!(snap.disturbance);
        assert_eq!(snap.owner_box, Some(owner_box()));
    }

    #[test]
    fn stranger_in_different_seat_does_not_become_owner() {
        let mut watcher = PresenceWatcher::new(PresenceConfig::default());
        let mut detector = FakeDetector::new(vec![vec![owner_box()]]);
        assert!(watcher
            .tick(Some(bright_frame()), 1.0, &mut detector)
            .owner_present);
        // 主人走开，陌生人坐到明显不同的位置：只算扰动，不当主人。
        let mut detector = FakeDetector::new(vec![vec![stranger_box()]]);
        for v in [200u8, 128, 200, 128, 200] {
            let snap = watcher.tick(Some(frame_value(v)), 1.0, &mut detector);
            assert!(!snap.owner_present);
            assert!(snap.disturbance);
        }
    }

    #[test]
    fn unchanged_frame_skips_detector() {
        let mut watcher = PresenceWatcher::new(PresenceConfig::default());
        let mut detector = FakeDetector::new(vec![vec![owner_box()]]);
        let first = watcher.tick(Some(bright_frame()), 1.0, &mut detector);
        assert!(!first.detector_skipped);
        assert_eq!(detector.calls, 1);
        // 画面一模一样：第二拍跳过检测，复用上次框。
        let second = watcher.tick(Some(bright_frame()), 1.0, &mut detector);
        assert!(second.detector_skipped);
        assert_eq!(detector.calls, 1);
        assert!(second.owner_present);
        assert!(watcher.detector_skipped_total >= 1);
    }

    #[test]
    fn camera_none_freezes_countdown_instead_of_counting_away() {
        let mut watcher = PresenceWatcher::new(PresenceConfig::default());
        let mut detector = FakeDetector::new(vec![vec![owner_box()]]);
        assert!(watcher
            .tick(Some(bright_frame()), 1.0, &mut detector)
            .owner_present);
        let mut detector = FakeDetector::new(vec![vec![]]);
        let snap = watcher.tick(Some(frame_value(200)), 10.0, &mut detector);
        assert!((snap.away_secs - 10.0).abs() < 1e-6);
        // 摄像头拿不到帧：冻结，不涨。
        let snap = watcher.tick(None, 10.0, &mut detector);
        assert!(!snap.camera_available);
        assert!(!snap.should_lock);
        assert!((snap.away_secs - 10.0).abs() < 1e-6);
    }
}
