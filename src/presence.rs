//! 在场保护 v2：摄像头感知的坐姿、离席与身后身影三项功能。
//!
//! 设计原则（针对旧版「能锁屏但不稳定、另外两项不工作」的根因）：
//! 1. 计时一律用 `Instant` 墙钟差值，不用「拍数 × 固定周期」，30 秒就是 30 秒。
//! 2. 每拍都重新检测人脸，绝不因为帧差小就复用上一帧的主人框——旧版恰恰
//!    在「人起身离开、画面几乎不变」时复用了旧框，导致离席永不触发。
//! 3. 摄像头/检测失败、画面太黑时冻结所有计时，绝不误锁屏。
//! 4. 后台线程用显式 COM(MTA) 初始化，WinRT 人脸检测不占用 UI 线程。
//! 5. 线程停止用显式 stop 标志，每拍无条件检查；不依赖 channel 断连。
//! 6. 所有开关与阈值持久化；错误状态可见、可排查。
//! 7. 身后身影以「非主人的第二张人脸」为主证据（2026-11-11 误报事故后修正），
//!    左右肩后 ROI 的运动块只作辅助；按 1/3/5 秒分级，提示可点击消除。
//! 8. 行为逻辑参考 Glance 11.19 的明文配置项模拟（见 docs/GLANCE_ANALYSIS.md），
//!    但不用 IR 深度摄像头 / 神经网络模型：坐姿用脸框比例代替厘米深度，
//!    离席用渐强警告 + snooze 代替注视追踪，Digital Wellness / Auto Mute
//!    复用同一套在场信号，保持单文件、零额外依赖、exe 约 19MB。
//!
//! 本模块不依赖 egui / Windows API，纯逻辑可用假帧与假检测器单测。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const FRAME_W: u32 = 320;
pub const FRAME_H: u32 = 240;
pub const TICK: Duration = Duration::from_millis(250);
pub const GRID_W: usize = 20;
pub const GRID_H: usize = 15;
pub const GRID_CELLS: usize = GRID_W * GRID_H;
pub const DARK_LEVEL: f32 = 12.0;

/// 身后身影分级阈值（秒）。
pub const BEHIND_WHITE: f64 = 1.0;
pub const BEHIND_YELLOW: f64 = 3.0;
pub const BEHIND_RED: f64 = 5.0;
/// 身影证据短暂消失的宽限，避免因压缩噪声/自动曝光把 5 秒计时清零。
pub const BEHIND_GRACE: f64 = 0.6;
/// 坐姿解除的持续时间（秒）；进入时长已改为可配置的
/// `Config::posture_warning_secs`（模拟 Glance TIME_TO_WARNING）。
pub const POSTURE_CLEAR: f64 = 2.0;
/// 人脸短暂漏检的容忍拍数（250ms/拍）。
pub const OWNER_DROP_TICKS: u32 = 3;

// ---------------------------------------------------------------------------
// 配置：独立文件 %APPDATA%/Daozai/presence.txt，完全不改上游 model.rs
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// 总开关（三项功能统一开关）。默认关闭，开启后持久化。
    pub enabled: bool,
    /// 离席多久自动锁屏，默认 30 秒。
    pub lock_after_secs: f64,
    /// 离席多久开始显示倒计时，默认 5 秒。
    pub countdown_after_secs: f64,
    /// 到点是否真正锁屏；false = 只提示不锁（模拟 Glance LOCK_ACTION_* 的轻量版）。
    pub lock_enabled: bool,
    pub posture_enabled: bool,
    /// 脸框超过基线多少倍判定太近，默认 1.30。
    pub posture_ratio: f32,
    /// 校准得到的「正常距离」脸框宽/高（归一化 0..1）。
    pub posture_baseline_w: f32,
    pub posture_baseline_h: f32,
    pub posture_calibrated: bool,
    /// 太近需持续多久才提示（模拟 Glance FEATURE_POSTURE_REMINDER_TIME_TO_WARNING）。
    pub posture_warning_secs: f64,
    pub behind_enabled: bool,
    /// 背景差分显著变化的灰阶阈值（越大越不敏感）。
    pub behind_motion: f32,
    /// 判定身影所需的最小变化网格数。
    pub behind_min_cells: usize,
    /// 「第二张脸」需连续确认的拍数（模拟 Glance Surfer IR/RGB Threshold）。
    pub behind_face_confirm: u32,
    /// Digital Wellness：连续用眼提醒开关。
    pub wellness_enabled: bool,
    /// 连续在座多久提醒远眺，默认 20 分钟。
    pub wellness_break_secs: f64,
    /// 视线离开多久算完成一次休息，默认 20 秒。
    pub wellness_rest_secs: f64,
    /// 用眼判定的「关注点」信号（模拟 Glance FEATURE_PRESENCE_DETECTION_POI_USED_*）：
    /// 0 = PRESENCE（人在座即算用眼），1 = HEAD（本拍检测到正脸才算看屏幕）。
    /// GAZE 档需专用眼动仪与私有 SDK，本机不提供，故不实现。
    pub wellness_poi: u8,
    /// Auto Mute：离席自动静音麦克风，回来自动恢复。
    pub auto_mute_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            lock_after_secs: 30.0,
            countdown_after_secs: 5.0,
            lock_enabled: true,
            posture_enabled: true,
            posture_ratio: 1.30,
            posture_baseline_w: 0.16,
            posture_baseline_h: 0.32,
            posture_calibrated: false,
            posture_warning_secs: 2.0,
            behind_enabled: true,
            behind_motion: 18.0,
            behind_min_cells: 6,
            behind_face_confirm: 2,
            wellness_enabled: false,
            wellness_break_secs: 1200.0,
            wellness_rest_secs: 20.0,
            wellness_poi: 1,
            auto_mute_enabled: false,
        }
    }
}

impl Config {
    pub fn sanitize(&mut self) {
        self.lock_after_secs = self.lock_after_secs.clamp(10.0, 300.0);
        self.countdown_after_secs = self.countdown_after_secs.clamp(1.0, self.lock_after_secs - 1.0);
        self.posture_ratio = self.posture_ratio.clamp(1.05, 2.0);
        self.posture_baseline_w = self.posture_baseline_w.clamp(0.04, 0.70);
        self.posture_baseline_h = self.posture_baseline_h.clamp(0.08, 0.95);
        self.posture_warning_secs = self.posture_warning_secs.clamp(0.5, 30.0);
        self.behind_motion = self.behind_motion.clamp(6.0, 60.0);
        self.behind_min_cells = self.behind_min_cells.clamp(2, 60);
        self.behind_face_confirm = self.behind_face_confirm.clamp(1, 12);
        self.wellness_break_secs = self.wellness_break_secs.clamp(60.0, 7200.0);
        self.wellness_rest_secs = self.wellness_rest_secs.clamp(5.0, 300.0);
        // 2 = GAZE 需要外接眼动仪，本机不支持，回退到 HEAD 档。
        self.wellness_poi = self.wellness_poi.min(1);
    }

    fn path() -> PathBuf {
        crate::model::config_dir().join("presence.txt")
    }

    pub fn load() -> Self {
        let mut c = Self::default();
        let text = std::fs::read_to_string(Self::path()).unwrap_or_default();
        for line in text.lines() {
            let line = line.trim_start_matches('\u{feff}').trim();
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "enabled" => c.enabled = v == "1",
                "lock_after_secs" => c.lock_after_secs = v.parse().unwrap_or(c.lock_after_secs),
                "countdown_after_secs" => c.countdown_after_secs = v.parse().unwrap_or(c.countdown_after_secs),
                "lock_enabled" => c.lock_enabled = v == "1",
                "posture_enabled" => c.posture_enabled = v == "1",
                "posture_warning_secs" => c.posture_warning_secs = v.parse().unwrap_or(c.posture_warning_secs),
                "behind_face_confirm" => c.behind_face_confirm = v.parse().unwrap_or(c.behind_face_confirm),
                "wellness_enabled" => c.wellness_enabled = v == "1",
                "wellness_break_secs" => c.wellness_break_secs = v.parse().unwrap_or(c.wellness_break_secs),
                "wellness_rest_secs" => c.wellness_rest_secs = v.parse().unwrap_or(c.wellness_rest_secs),
                "wellness_poi" => c.wellness_poi = v.parse().unwrap_or(c.wellness_poi),
                "auto_mute_enabled" => c.auto_mute_enabled = v == "1",
                "posture_ratio" => c.posture_ratio = v.parse().unwrap_or(c.posture_ratio),
                "posture_baseline_w" => c.posture_baseline_w = v.parse().unwrap_or(c.posture_baseline_w),
                "posture_baseline_h" => c.posture_baseline_h = v.parse().unwrap_or(c.posture_baseline_h),
                "posture_calibrated" => c.posture_calibrated = v == "1",
                "behind_enabled" => c.behind_enabled = v == "1",
                "behind_motion" => c.behind_motion = v.parse().unwrap_or(c.behind_motion),
                "behind_min_cells" => c.behind_min_cells = v.parse().unwrap_or(c.behind_min_cells),
                _ => {}
            }
        }
        c.sanitize();
        c
    }

    pub fn save(&self) {
        let c = {
            let mut c = self.clone();
            c.sanitize();
            c
        };
        let text = format!(
            "enabled={}\nlock_after_secs={}\ncountdown_after_secs={}\nlock_enabled={}\nposture_enabled={}\nposture_ratio={}\nposture_baseline_w={}\nposture_baseline_h={}\nposture_calibrated={}\nposture_warning_secs={}\nbehind_enabled={}\nbehind_motion={}\nbehind_min_cells={}\nbehind_face_confirm={}\nwellness_enabled={}\nwellness_break_secs={}\nwellness_rest_secs={}\nwellness_poi={}\nauto_mute_enabled={}\n",
            c.enabled as u8,
            c.lock_after_secs,
            c.countdown_after_secs,
            c.lock_enabled as u8,
            c.posture_enabled as u8,
            c.posture_ratio,
            c.posture_baseline_w,
            c.posture_baseline_h,
            c.posture_calibrated as u8,
            c.posture_warning_secs,
            c.behind_enabled as u8,
            c.behind_motion,
            c.behind_min_cells,
            c.behind_face_confirm,
            c.wellness_enabled as u8,
            c.wellness_break_secs,
            c.wellness_rest_secs,
            c.wellness_poi,
            c.auto_mute_enabled as u8,
        );
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

// ---------------------------------------------------------------------------
// 帧与几何
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct GrayFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl GrayFrame {
    pub fn blank(width: u32, height: u32, value: u8) -> Self {
        Self { width, height, pixels: vec![value; (width * height) as usize] }
    }

    pub fn usable(&self) -> bool {
        self.width > 0 && self.height > 0 && self.pixels.len() >= (self.width * self.height) as usize
    }

    pub fn brightness(&self) -> f32 {
        if !self.usable() { return 0.0; }
        let n = (self.width * self.height) as usize;
        let mut sum: u64 = 0;
        // 稀疏采样足够估计整体亮度，避免每拍做 76800 次加法。
        let step = (n / 4096).max(1);
        let mut count = 0usize;
        let mut i = 0usize;
        while i < n {
            sum += self.pixels[i] as u64;
            count += 1;
            i += step;
        }
        if count == 0 { 0.0 } else { sum as f32 / count as f32 }
    }

    pub fn is_dark(&self) -> bool {
        !self.usable() || self.brightness() < DARK_LEVEL
    }

    /// GRID_W × GRID_H 网格均值，用于背景差分。
    pub fn cells(&self) -> Vec<f32> {
        let mut sums = vec![0u64; GRID_CELLS];
        let mut counts = vec![0u32; GRID_CELLS];
        for y in 0..self.height as usize {
            let gy = (y * GRID_H / self.height as usize).min(GRID_H - 1);
            let row = y * self.width as usize;
            for x in 0..self.width as usize {
                let gx = (x * GRID_W / self.width as usize).min(GRID_W - 1);
                let i = gy * GRID_W + gx;
                sums[i] += self.pixels[row + x] as u64;
                counts[i] += 1;
            }
        }
        sums.iter().zip(counts.iter()).map(|(s, c)| if *c == 0 { 0.0 } else { *s as f32 / *c as f32 }).collect()
    }

    /// 测试辅助：把一块矩形区域改成指定亮度（模拟有人影经过）。
    #[cfg(test)]
    pub fn with_block(mut self, x0: usize, y0: usize, w: usize, h: usize, value: u8) -> Self {
        for y in y0..(y0 + h).min(self.height as usize) {
            for x in x0..(x0 + w).min(self.width as usize) {
                self.pixels[y * self.width as usize + x] = value;
            }
        }
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl FaceBox {
    pub fn area(&self) -> f32 { self.w.max(0.0) * self.h.max(0.0) }
    pub fn center(&self) -> (f32, f32) { (self.x + self.w * 0.5, self.y + self.h * 0.5) }
    pub fn iou(&self, o: &FaceBox) -> f32 {
        let (x0, y0) = (self.x.max(o.x), self.y.max(o.y));
        let (x1, y1) = ((self.x + self.w).min(o.x + o.w), (self.y + self.h).min(o.y + o.h));
        let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
        let union = self.area() + o.area() - inter;
        if union <= 0.0 { 0.0 } else { inter / union }
    }
    pub fn matches(&self, o: &FaceBox) -> bool {
        if self.iou(o) >= 0.20 { return true; }
        let (ax, ay) = self.center();
        let (bx, by) = o.center();
        let dist = ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt();
        let ratio = (self.area().max(o.area()) / self.area().min(o.area()).max(1e-6)).max(1.0);
        dist <= 0.25 && ratio <= 2.0
    }
}

// ---------------------------------------------------------------------------
// 硬件抽象（Windows 实现在 presence_backend.rs）
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct SourceStatus {
    pub ok: bool,
    pub name: String,
}

pub trait FrameSource {
    fn next_frame(&mut self) -> Option<GrayFrame>;
    fn status(&self) -> SourceStatus;
}

pub trait FaceDetector {
    fn detect(&mut self, frame: &GrayFrame) -> Vec<FaceBox>;
    fn available(&self) -> bool;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    #[default]
    None,
    White,
    Yellow,
    Red,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind { Away, Posture, Behind, Diag, Wellness }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone { White, Yellow, Red }

// ---------------------------------------------------------------------------
// 快照 / 提示
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub seq: u64,
    pub camera: String,
    pub camera_ok: bool,
    pub detector_ok: bool,
    pub paused: bool,
    pub dark: bool,
    pub diag: String,
    pub diag_epoch: u64,
    pub owner_present: bool,
    pub owner_ever: bool,
    pub owner_box: Option<FaceBox>,
    pub away_secs: f64,
    pub away_epoch: u64,
    pub countdown: bool,
    pub lock_remaining: f64,
    pub locked: bool,
    pub posture: bool,
    pub posture_epoch: u64,
    pub behind: Level,
    pub behind_secs: f64,
    pub behind_epoch: u64,
    pub other_faces: usize,
    /// 当前脸框相对校准基线的倍率（模拟 Glance CURRENT_DISTANCE 显示）。
    pub posture_closeness: f32,
    /// 离席警告渐强进度 0..1（模拟 Glance FADE_TO_PERCENT / ADAPTIVE_DIM）。
    pub dim_progress: f32,
    /// 连续用眼时长（秒）。
    pub wellness_secs: f64,
    pub wellness_due: bool,
    pub wellness_epoch: u64,
    /// 本拍是否判定为「正在看屏幕」（模拟 Glance POI 的注意力状态）。
    pub attending: bool,
    /// 当前应处于「保护性静音」状态（模拟 Glance AUTO_MUTE）。
    pub mic_mute: bool,
}

#[derive(Clone, Debug)]
pub struct Banner {
    pub kind: Kind,
    pub epoch: u64,
    pub tone: Tone,
    pub text: String,
    pub hint: String,
}

#[derive(Clone, Debug, Default)]
pub struct Control {
    pub config: Config,
    pub dismissed: HashMap<Kind, u64>,
    /// UI 点「暂缓锁屏」时自增；Watcher 看到 epoch 变化就重置离席计时。
    pub snooze_epoch: u64,
}

impl Control {
    fn hidden(&self, kind: Kind, epoch: u64) -> bool {
        self.dismissed.get(&kind).copied() == Some(epoch)
    }
}

// ---------------------------------------------------------------------------
// 状态机
// ---------------------------------------------------------------------------

pub struct Watcher {
    config: Config,
    last_owner: Option<FaceBox>,
    owner_lost: u32,
    owner_present: bool,
    owner_ever: bool,
    away_accum: f64,
    away_active: bool,
    away_epoch: u64,
    locked: bool,
    lock_failed: bool,
    posture_accum: f64,
    posture_clear: f64,
    posture_active: bool,
    posture_epoch: u64,
    background: Option<Vec<f32>>,
    /// 连续看到「非主人侧脸」的拍数：低拍数视为低置信度，不单独触发提示。
    face_streak: u32,
    behind_accum: f64,
    behind_missing: f64,
    behind_active: bool,
    behind_epoch: u64,
    last_snooze_epoch: u64,
    wellness_accum: f64,
    wellness_rest: f64,
    wellness_due: bool,
    wellness_epoch: u64,
    diag_epoch: u64,
    last_diag: String,
    seq: u64,
    last_tick: Option<Instant>,
}

impl Watcher {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            last_owner: None,
            owner_lost: 0,
            owner_present: false,
            owner_ever: false,
            away_accum: 0.0,
            away_active: false,
            away_epoch: 0,
            locked: false,
            lock_failed: false,
            posture_accum: 0.0,
            posture_clear: 0.0,
            posture_active: false,
            posture_epoch: 0,
            background: None,
            face_streak: 0,
            behind_accum: 0.0,
            behind_missing: 0.0,
            behind_active: false,
            behind_epoch: 0,
            last_snooze_epoch: 0,
            wellness_accum: 0.0,
            wellness_rest: 0.0,
            wellness_due: false,
            wellness_epoch: 0,
            diag_epoch: 0,
            last_diag: String::new(),
            seq: 0,
            last_tick: None,
        }
    }

    /// UI 侧反馈锁屏结果：成功则停止重试并保持已锁状态直到用户回来；
    /// 失败则记录错误，由服务每 2 秒重试一次，绝不静默放弃。
    pub fn note_lock(&mut self, ok: bool) {
        self.locked = ok;
        self.lock_failed = !ok;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        frame: Option<GrayFrame>,
        now: Instant,
        detector: &mut dyn FaceDetector,
        source: &SourceStatus,
        ctrl: &Control,
    ) -> (Snapshot, bool) {
        self.config = ctrl.config.clone();
        let dt = self.last_tick
            .map(|t| now.saturating_duration_since(t).as_secs_f64().min(1.0))
            .unwrap_or(0.0);
        self.last_tick = Some(now);
        self.seq += 1;

        let detector_ok = detector.available();
        let has_frame = frame.as_ref().is_some_and(|f| f.usable() && !f.is_dark());
        let dark = frame.as_ref().is_some_and(|f| f.is_dark());
        let paused = !source.ok || !detector_ok || !has_frame;

        // 每拍都检测，不用帧差门控复用旧框（旧版离席失效的根因）。
        let faces: Vec<FaceBox> = if paused { Vec::new() } else { detector.detect(frame.as_ref().unwrap()) };
        let (other_faces, owner_box) = if paused {
            (0usize, self.last_owner)
        } else {
            let (present, b, others) = self.track(&faces);
            self.owner_present = present;
            (others, b)
        };

        // ---- 暂缓锁屏：模拟 Glance FEATURE_PRESENCE_SNOOZE_VALUE ----
        if ctrl.snooze_epoch != self.last_snooze_epoch {
            self.last_snooze_epoch = ctrl.snooze_epoch;
            if self.away_active { self.away_accum = 0.0; }
        }

        // ---- 离席与锁屏 ----
        if !paused {
            if self.owner_present {
                // 用户返回：倒计时立即解除，重新布防。
                self.away_accum = 0.0;
                self.away_active = false;
                self.locked = false;
                self.lock_failed = false;
            } else if self.owner_ever {
                if !self.away_active {
                    self.away_active = true;
                    self.away_epoch += 1;
                    self.away_accum = 0.0;
                }
                self.away_accum += dt;
            }
        }
        let lock_now = self.owner_ever
            && !self.owner_present
            && !paused
            && !self.locked
            && self.config.lock_enabled
            && self.away_accum >= self.config.lock_after_secs;
        let countdown = self.owner_ever
            && !self.owner_present
            && !paused
            && !self.locked
            && self.away_accum >= self.config.countdown_after_secs;
        // 渐强警告进度：模拟 Glance FADE_TO_PERCENT / FEATURE_ADAPTIVE_DIM_*。
        // 单 RGB 轻量版不改显示器亮度，只让横幅由白渐黄并给出 0..1 进度。
        let dim_progress = if countdown {
            let span = (self.config.lock_after_secs - self.config.countdown_after_secs).max(1.0);
            ((self.away_accum - self.config.countdown_after_secs) / span).clamp(0.0, 1.0) as f32
        } else { 0.0 };
        // ---- Auto Mute：离席后要求保护性静音，回来恢复（模拟 FEATURE_AUTO_MUTE_*）----
        let mic_mute = self.config.auto_mute_enabled
            && self.owner_ever
            && !self.owner_present
            && !paused;

        // ---- 坐姿：模拟 FEATURE_POSTURE_REMINDER_TIME_TO_WARNING + CURRENT_DISTANCE ----
        let bw = self.config.posture_baseline_w;
        let bh = self.config.posture_baseline_h;
        let ratio = self.config.posture_ratio;
        let clear_ratio = (ratio - 0.15).max(1.02);
        let posture_closeness = owner_box
            .map(|b| (b.w / bw.max(1e-6)).max(b.h / bh.max(1e-6)))
            .unwrap_or(0.0);
        let too_close_now = self.config.posture_enabled
            && self.owner_present
            && owner_box.is_some_and(|b| b.w > bw * ratio || b.h > bh * ratio);
        let clear_now = owner_box.is_none()
            || owner_box.is_some_and(|b| b.w < bw * clear_ratio && b.h < bh * clear_ratio);
        if !paused {
            if too_close_now {
                self.posture_accum += dt;
                self.posture_clear = 0.0;
            } else {
                self.posture_accum = 0.0;
                if self.posture_active && clear_now {
                    self.posture_clear += dt;
                }
            }
            if !self.posture_active && self.posture_accum >= self.config.posture_warning_secs {
                self.posture_active = true;
                self.posture_epoch += 1;
            } else if self.posture_active && clear_now && self.posture_clear >= POSTURE_CLEAR {
                self.posture_active = false;
                self.posture_accum = 0.0;
                self.posture_clear = 0.0;
            }
        }

        // ---- Digital Wellness：模拟 Glance POI_USED_PRESENCE / POI_USED_HEAD ----
        // 真 POI_USED_GAZE 需外接 RGB-IR 眼动仪 + 私有驱动/模型：本机虽有
        // Integrated IR Camera，但标准 Media Foundation 只枚举到 RGB 通道，
        // WinRT FaceDetector 也只返回脸框（无视线/头部姿态），故用两档近似：
        //   PRESENCE(0)：人在座即算用眼（旧行为）；
        //   HEAD(1)    ：本拍必须检测到主人正脸，转头远眺/低头即视为视线离开。
        let owner_face_seen = !paused && faces.iter().any(|f| {
            owner_box.is_some_and(|o| o.matches(f))
        });
        let attending = if self.config.wellness_poi == 1 {
            owner_face_seen
        } else {
            self.owner_present
        };
        if self.wellness_due && ctrl.hidden(Kind::Wellness, self.wellness_epoch) {
            self.wellness_due = false;
            self.wellness_accum = 0.0;
        }
        if !paused {
            if attending {
                self.wellness_accum += dt;
                self.wellness_rest = 0.0;
            } else if self.owner_ever {
                // 视线离开（转头远眺）与离席一样，都累计为休息时间。
                self.wellness_rest += dt;
                if self.wellness_rest >= self.config.wellness_rest_secs {
                    self.wellness_accum = 0.0;
                    self.wellness_rest = 0.0;
                    self.wellness_due = false;
                }
            }
            if self.config.wellness_enabled
                && !self.wellness_due
                && self.wellness_accum >= self.config.wellness_break_secs
            {
                self.wellness_due = true;
                self.wellness_epoch += 1;
            }
        }

        // ---- 身后身影：模拟 Glance FEATURE_SHOULDER_SURFING_LEFT/RIGHT ----
        // Glance 用多用户人脸检测 + IR/RGB 分类器置信度阈值；我们没有 IR 与模型，
        // 因此把「第二张脸连续出现的拍数」当作置信度，左右肩后 ROI 的运动块只作辅助。
        // 2026-11-11 事故根因：旧版 `evidence = other_faces > 0 || blob`，
        // 打字/耸肩的运动块单独就把提示刷到红字 130 秒。现在没有第二张脸一律不计数。
        let mut evidence_gain = 0.0;
        if self.config.behind_enabled && self.owner_present && !paused {
            let cells = frame.as_ref().unwrap().cells();
            let (side_blob, global_change) = self.motion(&cells, owner_box);
            let side_faces = faces.iter().filter(|f| {
                if owner_box.is_some_and(|o| o.matches(f)) { return false; }
                let (cx, cy) = f.center();
                owner_box.is_some_and(|o| {
                    cy >= o.y - 0.10 && cy <= o.y + o.h * 1.5
                        && (cx < o.x - o.w * 0.10 || cx > o.x + o.w * 1.10)
                        // 尺寸相近才算「站在身后的人」，排除远景小脸/海报。
                        && f.w >= o.w * 0.30 && f.w <= o.w * 3.0
                })
            }).count();
            // 单帧误检不算人；置信度记忆上限很低，漏检时快速衰减，
            // 避免一次误检把提示长期钉在屏幕上。
            let streak_cap = self.config.behind_face_confirm.saturating_mul(2).max(4);
            self.face_streak = if side_faces > 0 {
                (self.face_streak + 1).min(streak_cap)
            } else {
                self.face_streak.saturating_sub(2)
            };
            let strong = self.face_streak >= self.config.behind_face_confirm;
            if self.face_streak > 0 && (strong || (side_blob && !global_change)) {
                evidence_gain = dt;
            }
        } else {
            self.face_streak = 0;
        }
        if !paused {
            if evidence_gain > 0.0 {
                self.behind_missing = 0.0;
                if !self.behind_active {
                    self.behind_active = true;
                    self.behind_epoch += 1;
                    self.behind_accum = 0.0;
                }
                self.behind_accum += evidence_gain;
            } else if self.behind_active {
                self.behind_missing += dt;
                if self.behind_missing >= BEHIND_GRACE {
                    self.behind_active = false;
                    self.behind_accum = 0.0;
                    self.behind_missing = 0.0;
                    // 事件结束：换新 epoch，让「点击消除」只在本次事件内生效。
                    self.behind_epoch += 1;
                }
            }
        }
        let behind_level = if self.behind_accum >= BEHIND_RED { Level::Red }
            else if self.behind_accum >= BEHIND_YELLOW { Level::Yellow }
            else if self.behind_accum >= BEHIND_WHITE { Level::White }
            else { Level::None };

        // ---- 诊断状态（可见、可按事件消除） ----
        let diag = if !source.ok {
            "摄像头不可用，保护已暂停".to_string()
        } else if !detector_ok {
            "人脸检测不可用，保护已暂停".to_string()
        } else if dark {
            "画面太暗，保护已暂停".to_string()
        } else if self.lock_failed {
            "锁屏失败，请按 Win+L 手动锁定".to_string()
        } else {
            String::new()
        };
        if diag != self.last_diag {
            if !diag.is_empty() { self.diag_epoch += 1; }
            self.last_diag = diag.clone();
        }

        let mut snap = Snapshot {
            seq: self.seq,
            camera: source.name.clone(),
            camera_ok: source.ok,
            detector_ok,
            paused,
            dark,
            diag,
            diag_epoch: self.diag_epoch,
            owner_present: self.owner_present,
            owner_ever: self.owner_ever,
            owner_box,
            away_secs: self.away_accum,
            away_epoch: self.away_epoch,
            countdown,
            lock_remaining: (self.config.lock_after_secs - self.away_accum).max(0.0),
            locked: self.locked,
            posture: self.posture_active,
            posture_epoch: self.posture_epoch,
            behind: behind_level,
            behind_secs: self.behind_accum,
            behind_epoch: self.behind_epoch,
            other_faces,
            posture_closeness,
            dim_progress,
            wellness_secs: self.wellness_accum,
            wellness_due: self.wellness_due,
            wellness_epoch: self.wellness_epoch,
            attending,
            mic_mute,
        };
        // 点击消除：按事件 epoch 抑制，事件结束后自动恢复。
        if ctrl.hidden(Kind::Diag, snap.diag_epoch) { snap.diag.clear(); }
        if ctrl.hidden(Kind::Away, snap.away_epoch) { snap.countdown = false; }
        if ctrl.hidden(Kind::Posture, snap.posture_epoch) { snap.posture = false; }
        if ctrl.hidden(Kind::Behind, snap.behind_epoch) { snap.behind = Level::None; }
        if ctrl.hidden(Kind::Wellness, snap.wellness_epoch) { snap.wellness_due = false; }
        (snap, lock_now)
    }

    fn track(&mut self, faces: &[FaceBox]) -> (bool, Option<FaceBox>, usize) {
        if faces.is_empty() {
            self.owner_lost += 1;
            if self.owner_lost > OWNER_DROP_TICKS {
                return (false, None, 0);
            }
            return (true, self.last_owner, 0);
        }
        let mut matched = None;
        if let Some(last) = self.last_owner {
            for (i, f) in faces.iter().enumerate() {
                if last.matches(f) { matched = Some(i); break; }
            }
        }
        // 主人刚丢失的几拍内，出现一张完全对不上的脸也先不当成主人回来，
        // 避免旁人坐到相近位置就解除布防。
        if matched.is_none() && self.last_owner.is_some() && self.owner_lost <= OWNER_DROP_TICKS && faces.len() == 1 {
            self.owner_lost += 1;
            return (true, self.last_owner, 0);
        }
        let idx = matched.unwrap_or_else(|| {
            faces.iter().enumerate().max_by(|a, b| a.1.area().total_cmp(&b.1.area())).map(|(i, _)| i).unwrap_or(0)
        });
        let b = faces[idx];
        self.last_owner = Some(b);
        self.owner_lost = 0;
        self.owner_ever = true;
        (true, Some(b), faces.len().saturating_sub(1))
    }

    /// 左右肩后 ROI 背景差分：返回 (侧翼显著变化, 全局光照突变)。
    fn motion(&mut self, cells: &[f32], owner: Option<FaceBox>) -> (bool, bool) {
        if cells.len() != GRID_CELLS { return (false, false); }
        if self.background.is_none() {
            self.background = Some(cells.to_vec());
            return (false, false);
        }
        let bg = self.background.as_mut().unwrap();
        let threshold = self.config.behind_motion;
        let mut changed = 0usize;
        let mut counted = 0usize;
        let mut diffs = vec![0f32; GRID_CELLS];
        for gy in 0..GRID_H {
            for gx in 0..GRID_W {
                let i = gy * GRID_W + gx;
                let d = (cells[i] - bg[i]).abs();
                diffs[i] = d;
                let cx = (gx as f32 + 0.5) / GRID_W as f32;
                let cy = (gy as f32 + 0.5) / GRID_H as f32;
                // 主人排除区：1) 人脸框附近；2) 脸框下方的「身体锥形」——从脸往下延伸到
                // 画面底部、左右放宽到肩/手范围。单 RGB 摄像头无法真正区分「身后」，
                // 若不排除主人自己的躯干与手部动作，打字、耸肩会被误判成身后人影
                // （2026-11-11 本机实测误报 130 秒红字）。
                let in_owner = owner.is_some_and(|b| {
                    let pad = 0.12;
                    let body_pad = (b.w * 0.8).max(0.08);
                    (cx >= b.x - pad && cx <= b.x + b.w + pad
                        && cy >= b.y - pad && cy <= b.y + b.h + pad)
                        || (cy >= b.y + b.h * 0.5
                            && cx >= b.x - body_pad && cx <= b.x + b.w + body_pad)
                });
                // 模拟 FEATURE_SHOULDER_SURFING_LEFT/RIGHT：只统计主人左右两侧、
                // 与头部高度相当的「肩后带」，忽略头顶/脚下/画面边缘的光影变化。
                let in_side = owner.is_some_and(|b| {
                    let top = (b.y - 0.08).max(0.0);
                    let bottom = (b.y + b.h * 1.5).min(0.95);
                    cy >= top && cy <= bottom
                        && ((cx > 0.02 && cx < b.x - b.w * 0.10)
                            || (cx > b.x + b.w * 1.10 && cx < 0.98))
                });
                if in_side && !in_owner {
                    counted += 1;
                    if d > threshold { changed += 1; }
                }
            }
        }
        let global = counted > 0 && changed as f32 / counted as f32 > 0.70;
        // 背景模型慢速自适应：变化区域学得慢，静止区域学得快。
        for i in 0..GRID_CELLS {
            let alpha = if diffs[i] > threshold { 0.005 } else { 0.05 };
            bg[i] = bg[i] * (1.0 - alpha) + cells[i] * alpha;
        }
        if global {
            bg.copy_from_slice(cells);
            return (false, true);
        }
        (changed >= self.config.behind_min_cells, false)
    }
}

// ---------------------------------------------------------------------------
// 后台服务
// ---------------------------------------------------------------------------

pub struct Service {
    stop: Arc<AtomicBool>,
    control: Arc<Mutex<Control>>,
    shared: Arc<Mutex<Option<Snapshot>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Service {
    pub fn start(config: Config) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let control = Arc::new(Mutex::new(Control {
            config, dismissed: HashMap::new(), snooze_epoch: 0,
        }));
        let shared = Arc::new(Mutex::new(None));
        let handle = std::thread::Builder::new()
            .name("daozai-presence".into())
            .spawn({
                let stop = stop.clone();
                let control = control.clone();
                let shared = shared.clone();
                move || run(stop, control, shared)
            })
            .ok();
        Self { stop, control, shared, handle }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // 不 join：线程每拍（250ms）检查一次 stop，自行退出并释放摄像头。
        self.handle.take();
    }

    pub fn set_config(&self, config: Config) {
        if let Ok(mut c) = self.control.lock() { c.config = config; }
    }

    pub fn dismiss(&self, kind: Kind, epoch: u64) {
        if let Ok(mut c) = self.control.lock() { c.dismissed.insert(kind, epoch); }
    }

    /// UI 点「暂缓锁屏」：只重置离席计时，布防状态不变。
    pub fn snooze(&self) {
        if let Ok(mut c) = self.control.lock() {
            c.snooze_epoch = c.snooze_epoch.wrapping_add(1);
        }
    }

    pub fn poll(&self) -> Option<Snapshot> {
        self.shared.lock().ok().and_then(|s| s.clone())
    }
}

impl Drop for Service {
    fn drop(&mut self) { self.stop(); }
}

fn run(stop: Arc<AtomicBool>, control: Arc<Mutex<Control>>, shared: Arc<Mutex<Option<Snapshot>>>) {
    #[cfg(windows)]
    let _com = crate::presence_backend::ComGuard::init();

    let mut source = crate::presence_backend::FrameSourceImpl::new();
    let mut detector = crate::presence_backend::DetectorImpl::new();
    let initial = control.lock().map(|c| c.config.clone()).unwrap_or_default();
    let mut watcher = Watcher::new(initial);
    let mut last_lock_attempt: Option<Instant> = None;
    // Auto Mute：只记录「由我们静音」的状态，绝不覆盖用户原本的静音设置。
    let mut we_muted_mic = false;

    loop {
        if stop.load(Ordering::Relaxed) { break; }
        let started = Instant::now();
        let frame = source.next_frame();
        let status = source.status();
        let ctrl = control.lock().map(|c| c.clone()).unwrap_or_default();
        let (snap, lock_now) = watcher.tick(frame, started, &mut detector, &status, &ctrl);
        // Auto Mute：离席时静音麦克风（仅当用户本来没静音），回来时恢复。
        // 调用发生在已初始化 COM(MTA) 的采集线程，后端不再做任何 COM 初始化。
        if snap.mic_mute && !we_muted_mic {
            if matches!(crate::presence_backend::mic_muted(), Some(false))
                && crate::presence_backend::set_mic_mute(true)
            {
                we_muted_mic = true;
                crate::presence_backend::log("auto mute: mic muted while away");
            }
        } else if !snap.mic_mute && we_muted_mic {
            if crate::presence_backend::set_mic_mute(false) {
                we_muted_mic = false;
                crate::presence_backend::log("auto mute: mic restored on return");
            }
        }
        if lock_now && last_lock_attempt.map_or(true, |t| started.duration_since(t) >= Duration::from_secs(2)) {
            last_lock_attempt = Some(started);
            let ok = crate::presence_backend::lock_workstation();
            watcher.note_lock(ok);
            crate::presence_backend::log(&format!("lock attempt ok={ok}"));
        }
        if let Ok(mut slot) = shared.lock() { *slot = Some(snap); }
        std::thread::sleep(TICK.saturating_sub(started.elapsed()));
    }
    // 退出前一定把麦克风恢复，避免服务停止后用户一直处于静音。
    if we_muted_mic { let _ = crate::presence_backend::set_mic_mute(false); }
    crate::presence_backend::log("presence watcher stopped");
}

// ---------------------------------------------------------------------------
// UI 状态（挂进 App 的单个聚合字段）
// ---------------------------------------------------------------------------

pub struct PresenceUi {
    pub config: Config,
    service: Option<Service>,
    snapshot: Option<Snapshot>,
    /// 上一帧实测的横幅文字宽度，供 base_size() 使用。
    pub width_hint: f32,
    /// 本帧横幅的屏幕坐标 [minx,miny,maxx,maxy]，并入点击命中区域。
    pub hit_rect: Option<[f32; 4]>,
}

impl Default for PresenceUi {
    fn default() -> Self { Self::new() }
}

impl PresenceUi {
    pub fn new() -> Self {
        let config = Config::load();
        // 自动化测试会设置 DAOZAI_DATA_DIR；此时不要打开真实摄像头。
        let tests = std::env::var_os("DAOZAI_DATA_DIR").is_some();
        let service = if config.enabled && !tests { Some(Service::start(config.clone())) } else { None };
        Self { config, service, snapshot: None, width_hint: 0.0, hit_rect: None }
    }

    pub fn poll(&mut self) {
        if let Some(s) = &self.service {
            if let Some(snap) = s.poll() { self.snapshot = Some(snap); }
        }
    }

    pub fn snapshot(&self) -> Option<&Snapshot> { self.snapshot.as_ref() }

    pub fn banner(&self) -> Option<Banner> {
        let s = self.snapshot.as_ref()?;
        if !self.config.enabled { return None; }
        if !s.diag.is_empty() {
            return Some(Banner {
                kind: Kind::Diag, epoch: s.diag_epoch, tone: Tone::Yellow,
                text: s.diag.clone(), hint: "点击可隐藏".into(),
            });
        }
        if s.behind != Level::None {
            let (tone, label) = match s.behind {
                Level::White => (Tone::White, "身后有人影"),
                Level::Yellow => (Tone::Yellow, "身后有人驻足"),
                Level::Red => (Tone::Red, "身后有人长时间停留"),
                Level::None => unreachable!(),
            };
            return Some(Banner {
                kind: Kind::Behind, epoch: s.behind_epoch, tone,
                text: format!("{} · {:.0} 秒", label, s.behind_secs),
                hint: "点击消除".into(),
            });
        }
        if s.posture {
            return Some(Banner {
                kind: Kind::Posture, epoch: s.posture_epoch, tone: Tone::Yellow,
                text: "离屏幕太近了，坐远一点".into(), hint: "点击消除".into(),
            });
        }
        if s.wellness_due {
            return Some(Banner {
                kind: Kind::Wellness, epoch: s.wellness_epoch, tone: Tone::White,
                text: format!("已连续用眼 {:.0} 分钟，抬头远眺一会儿",
                    (s.wellness_secs / 60.0).floor()),
                hint: "点击消除".into(),
            });
        }
        if s.countdown {
            // 渐强警告：模拟 Glance FADE_TO_PERCENT，越接近锁屏由白转黄。
            let tone = if s.dim_progress > 0.6 { Tone::Yellow } else { Tone::White };
            return Some(Banner {
                kind: Kind::Away, epoch: s.away_epoch, tone,
                text: format!("离开中，还剩 {} 秒锁屏", s.lock_remaining.ceil() as u64),
                hint: "点击隐藏".into(),
            });
        }
        None
    }

    /// 收起状态下岛上横幅需要的宽度；首帧用字符估算，之后用实测值。
    pub fn banner_width(&self) -> Option<f32> {
        let b = self.banner()?;
        let est = b.text.chars().map(|c| if c.is_ascii() { 7.5 } else { 13.5 }).sum::<f32>() + 96.0;
        Some(self.width_hint.max(est).clamp(200.0, 520.0))
    }

    pub fn dismiss(&mut self, kind: Kind, epoch: u64) {
        if let Some(s) = &self.service { s.dismiss(kind, epoch); }
        // 本地立即隐藏，不等下一拍（250ms）。
        if let Some(s) = &mut self.snapshot {
            match kind {
                Kind::Away => s.countdown = false,
                Kind::Posture => s.posture = false,
                Kind::Behind => s.behind = Level::None,
                Kind::Diag => s.diag.clear(),
                Kind::Wellness => s.wellness_due = false,
            }
        }
    }

    /// 倒计时期间「暂缓锁屏」（模拟 Glance FEATURE_PRESENCE_SNOOZE_VALUE）。
    pub fn snooze(&mut self) {
        if let Some(s) = &self.service { s.snooze(); }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.config.enabled = enabled;
        self.config.save();
        if enabled {
            if self.service.is_none() { self.service = Some(Service::start(self.config.clone())); }
        } else if let Some(mut s) = self.service.take() {
            s.stop();
            self.snapshot = None;
        }
    }

    pub fn apply(&mut self, mut config: Config) {
        config.sanitize();
        self.config = config.clone();
        self.config.save();
        if let Some(s) = &self.service { s.set_config(config); }
    }

    /// 用当前主人脸框校准「正常距离」，作为坐姿判定基线。
    pub fn calibrate(&mut self) -> bool {
        let Some(b) = self.snapshot().and_then(|s| s.owner_box) else { return false };
        let mut c = self.config.clone();
        c.posture_baseline_w = b.w.clamp(0.05, 0.60);
        c.posture_baseline_h = b.h.clamp(0.10, 0.90);
        c.posture_calibrated = true;
        self.apply(c);
        true
    }

    pub fn status_line(&self) -> String {
        if !self.config.enabled { return "未开启".into(); }
        let Some(s) = self.snapshot() else { return "正在启动摄像头…".into(); };
        if !s.diag.is_empty() { return s.diag.clone(); }
        if s.paused { return "保护已暂停".into(); }
        if s.locked { return "已锁定，等待你回来".into(); }
        if s.owner_present {
            let camera = if s.camera.is_empty() { "已连接" } else { &s.camera };
            format!("主人在座 · 摄像头 {} · 人脸检测{}", camera, if s.detector_ok { "正常" } else { "异常" })
        } else if s.owner_ever {
            format!("离开 {:.0} 秒 · {} 秒后锁屏", s.away_secs, self.config.lock_after_secs)
        } else {
            "等待识别主座位的人…".into()
        }
    }
}

// ---------------------------------------------------------------------------
// 单测：全部是纯逻辑，不碰摄像头、不碰 WinRT、不锁屏
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeDetector { script: Vec<Vec<FaceBox>>, calls: usize }
    impl FakeDetector {
        fn new(script: Vec<Vec<FaceBox>>) -> Self { Self { script, calls: 0 } }
    }
    impl FaceDetector for FakeDetector {
        fn detect(&mut self, _frame: &GrayFrame) -> Vec<FaceBox> {
            let i = self.calls.min(self.script.len().saturating_sub(1));
            self.calls += 1;
            self.script.get(i).cloned().unwrap_or_default()
        }
        fn available(&self) -> bool { true }
    }

    fn owner() -> FaceBox { FaceBox { x: 0.35, y: 0.25, w: 0.16, h: 0.32 } }
    /// 站在主人右后方的第二个人（Glance 的多用户检测场景）。
    fn stranger() -> FaceBox { FaceBox { x: 0.76, y: 0.10, w: 0.14, h: 0.28 } }
    fn ok_source() -> SourceStatus { SourceStatus { ok: true, name: "Integrated Camera".into() } }
    fn ctrl_with(config: Config) -> Control {
        Control { config, dismissed: HashMap::new(), snooze_epoch: 0 }
    }
    fn ctrl() -> Control { ctrl_with(Config::default()) }
    fn at(t0: Instant, s: f64) -> Instant { t0 + Duration::from_secs_f64(s) }
    fn bright() -> GrayFrame { GrayFrame::blank(FRAME_W, FRAME_H, 128) }

    fn step(w: &mut Watcher, det: &mut FakeDetector, t0: Instant, t: f64, frame: Option<GrayFrame>, c: &Control) -> (Snapshot, bool) {
        w.tick(frame, at(t0, t), det, &ok_source(), c)
    }

    #[test]
    fn away_countdown_starts_at_5s_and_locks_at_30s_and_return_cancels() {
        let t0 = Instant::now();
        let mut w = Watcher::new(Config::default());
        let mut d = FakeDetector::new(vec![vec![owner()], vec![], vec![]]);
        let c = ctrl();
        // 先建立主人
        step(&mut w, &mut d, t0, 0.0, Some(bright()), &c);
        // 之后每拍 0.5 秒推进，模拟 30 秒离席
        let mut last = step(&mut w, &mut d, t0, 0.5, Some(bright()), &c);
        assert!(!last.0.countdown, "5 秒前不显示倒计时");
        for i in 2..=20 { last = step(&mut w, &mut d, t0, 0.5 * i as f64, Some(bright()), &c); }
        assert!(last.0.countdown, "5 秒后应显示倒计时");
        assert!(!last.1, "未到 30 秒不锁屏");
        let mut triggered = false;
        for i in 21..=90 {
            last = step(&mut w, &mut d, t0, 0.5 * i as f64, Some(bright()), &c);
            if last.1 { triggered = true; break; }
        }
        assert!(triggered, "30 秒应触发锁屏");
        assert!(last.0.lock_remaining < 0.01);
        w.note_lock(true);
        // 用户返回：倒计时解除、重新布防
        let mut back = FakeDetector::new(vec![vec![owner()]]);
        let (s, lock) = step(&mut w, &mut back, t0, 60.0, Some(bright()), &c);
        assert!(!s.countdown && !lock && s.away_secs == 0.0, "返回应立即解除倒计时");
    }

    #[test]
    fn paused_freezes_countdown_and_never_locks() {
        let t0 = Instant::now();
        let mut w = Watcher::new(Config::default());
        let mut d = FakeDetector::new(vec![vec![owner()], vec![]]);
        let c = ctrl();
        step(&mut w, &mut d, t0, 0.0, Some(bright()), &c);
        let bad = SourceStatus { ok: false, name: String::new() };
        let mut last = (Snapshot::default(), false);
        for i in 1..=80 {
            last = w.tick(Some(bright()), at(t0, i as f64), &mut d, &bad, &c);
        }
        assert!(!last.1, "摄像头不可用绝不误锁");
        assert!(last.0.away_secs < 0.01, "暂停时倒计时冻结");
    }

    #[test]
    fn dark_frame_is_paused() {
        let t0 = Instant::now();
        let mut w = Watcher::new(Config::default());
        let mut d = FakeDetector::new(vec![vec![owner()]]);
        let (s, lock) = w.tick(Some(GrayFrame::blank(FRAME_W, FRAME_H, 2)), at(t0, 0.0), &mut d, &ok_source(), &ctrl());
        assert!(s.paused && !lock);
    }

    #[test]
    fn behind_levels_are_1_3_5_seconds_and_dismiss_is_per_event() {
        let t0 = Instant::now();
        let mut w = Watcher::new(Config::default());
        // 第一拍只有主人（建立背景），之后主人 + 右后方的第二张脸。
        let mut d = FakeDetector::new(vec![vec![owner()], vec![owner(), stranger()]]);
        let mut c = ctrl();
        step(&mut w, &mut d, t0, 0.0, Some(bright()), &c);
        // 之后每 0.5 秒一帧：右上角出现一块人影（主座位区域之外）
        let blob = || Some(bright().with_block(250, 10, 60, 90, 210));
        let mut s = Snapshot::default();
        for i in 1..=2 { s = step(&mut w, &mut d, t0, 0.5 * i as f64, blob(), &c).0; }
        assert_eq!(s.behind, Level::White, "1 秒：白字");
        for i in 3..=6 { s = step(&mut w, &mut d, t0, 0.5 * i as f64, blob(), &c).0; }
        assert_eq!(s.behind, Level::Yellow, "3 秒：黄字");
        for i in 7..=10 { s = step(&mut w, &mut d, t0, 0.5 * i as f64, blob(), &c).0; }
        assert_eq!(s.behind, Level::Red, "5 秒：红字");
        // 点击消除：本次事件不再提示
        let epoch = s.behind_epoch;
        c.dismissed.insert(Kind::Behind, epoch);
        s = step(&mut w, &mut d, t0, 5.5, blob(), &c).0;
        assert_eq!(s.behind, Level::None, "点击后应立即消除");
        // 身影与第二张脸一起消失 → 置信度衰减 → 事件结束。
        let mut gone = FakeDetector::new(vec![vec![owner()]]);
        for i in 12..=16 { s = step(&mut w, &mut gone, t0, 0.5 * i as f64, Some(bright()), &c).0; }
        assert_eq!(s.behind, Level::None);
        // 新事件：人影与第二张脸重新出现，恢复提示。
        let mut again = FakeDetector::new(vec![vec![owner(), stranger()]]);
        for i in 17..=20 { s = step(&mut w, &mut again, t0, 0.5 * i as f64, blob(), &c).0; }
        assert_ne!(s.behind_epoch, epoch, "新事件应换 epoch");
        assert_eq!(s.behind, Level::White, "新事件重新提示");
    }

    #[test]
    fn global_light_change_does_not_count_as_behind() {
        let t0 = Instant::now();
        let mut w = Watcher::new(Config::default());
        let mut d = FakeDetector::new(vec![vec![owner()]]);
        let c = ctrl();
        step(&mut w, &mut d, t0, 0.0, Some(bright()), &c);
        let mut s = Snapshot::default();
        for i in 1..=8 {
            s = step(&mut w, &mut d, t0, 0.5 * i as f64, Some(GrayFrame::blank(FRAME_W, FRAME_H, 220)), &c).0;
        }
        assert_eq!(s.behind, Level::None, "整屏变亮是光照变化，不是人影");
    }

    #[test]
    fn owner_body_motion_below_face_is_not_behind_evidence() {
        // 主人脸框正下方（躯干/手）持续晃动 12 秒：打字、耸肩不得被当成身后人影。
        // 2026-11-11 本机实测：旧版只排除脸框附近，误报红字 130 秒。
        let t0 = Instant::now();
        let mut w = Watcher::new(Config::default());
        let mut d = FakeDetector::new(vec![vec![owner()]]);
        let c = ctrl();
        step(&mut w, &mut d, t0, 0.0, Some(bright()), &c);
        let body = || Some(bright().with_block(90, 130, 100, 100, 210));
        let mut s = Snapshot::default();
        for i in 1..=24 { s = step(&mut w, &mut d, t0, 0.5 * i as f64, body(), &c).0; }
        assert_eq!(s.behind, Level::None, "主人自己的身体动作不算身后人影");
    }

    #[test]
    fn motion_alone_never_triggers_behind() {
        // 2026-11-11 误报回归：主人在座、左右侧翼有持续运动块（打字/光影/宠物），
        // 但画面里从来没有第二张脸 —— 20 秒内不得出现任何身后提示。
        let t0 = Instant::now();
        let mut w = Watcher::new(Config::default());
        let mut d = FakeDetector::new(vec![vec![owner()]]);
        let c = ctrl();
        step(&mut w, &mut d, t0, 0.0, Some(bright()), &c);
        let side = || Some(bright().with_block(260, 40, 55, 120, 215));
        let mut s = Snapshot::default();
        for i in 1..=40 { s = step(&mut w, &mut d, t0, 0.5 * i as f64, side(), &c).0; }
        assert_eq!(s.behind, Level::None, "没有第二张脸，运动块再久也不算身后人影");
        assert_eq!(s.behind_secs, 0.0);
    }

    #[test]
    fn snooze_resets_away_timer_without_disarming() {
        let t0 = Instant::now();
        let mut w = Watcher::new(Config::default());
        let mut d = FakeDetector::new(vec![vec![owner()], vec![]]);
        let mut c = ctrl();
        step(&mut w, &mut d, t0, 0.0, Some(bright()), &c);
        let mut s = Snapshot::default();
        for i in 1..=20 { s = step(&mut w, &mut d, t0, 0.5 * i as f64, Some(bright()), &c).0; }
        assert!(s.countdown, "离席应进入倒计时");
        c.snooze_epoch += 1;
        s = step(&mut w, &mut d, t0, 10.5, Some(bright()), &c).0;
        assert!(s.away_secs < 1.0, "暂缓应重置离席计时");
        let mut locked = false;
        for i in 22..=90 {
            if step(&mut w, &mut d, t0, 0.5 * i as f64, Some(bright()), &c).1 {
                locked = true;
                break;
            }
        }
        assert!(locked, "暂缓只是延后，布防仍然有效");
    }

    #[test]
    fn lock_disabled_only_warns() {
        let t0 = Instant::now();
        let mut cfg = Config::default();
        cfg.lock_enabled = false;
        let mut w = Watcher::new(cfg.clone());
        let mut d = FakeDetector::new(vec![vec![owner()], vec![]]);
        let c = ctrl_with(cfg);
        step(&mut w, &mut d, t0, 0.0, Some(bright()), &c);
        let mut warned = false;
        let mut locked = false;
        for i in 1..=90 {
            let (s, l) = step(&mut w, &mut d, t0, 0.5 * i as f64, Some(bright()), &c);
            warned |= s.countdown;
            locked |= l;
        }
        assert!(warned, "只提示模式仍应显示倒计时");
        assert!(!locked, "只提示模式绝不锁屏");
    }

    #[test]
    fn posture_warning_secs_is_configurable_and_reports_distance() {
        let t0 = Instant::now();
        let mut cfg = Config::default();
        cfg.posture_warning_secs = 4.0;
        let mut w = Watcher::new(cfg.clone());
        let close = FaceBox { x: 0.30, y: 0.20, w: 0.16 * 1.5, h: 0.32 * 1.5 };
        let mut d = FakeDetector::new(vec![vec![close]]);
        let c = ctrl_with(cfg);
        let mut s = Snapshot::default();
        for i in 0..=4 { s = step(&mut w, &mut d, t0, 0.5 * i as f64, Some(bright()), &c).0; }
        assert!(!s.posture, "2 秒不足 4 秒阈值，不应闪烁提示");
        assert!(s.posture_closeness > 1.4, "应给出当前距离倍率");
        for i in 5..=10 { s = step(&mut w, &mut d, t0, 0.5 * i as f64, Some(bright()), &c).0; }
        assert!(s.posture, "持续 4 秒后提示");
    }

    #[test]
    fn wellness_reminds_after_break_and_restarts_when_dismissed() {
        let t0 = Instant::now();
        let mut cfg = Config::default();
        cfg.wellness_enabled = true;
        cfg.wellness_break_secs = 6.0;
        cfg.wellness_rest_secs = 2.0;
        let mut w = Watcher::new(cfg.clone());
        let mut d = FakeDetector::new(vec![vec![owner()]]);
        let c = ctrl_with(cfg);
        step(&mut w, &mut d, t0, 0.0, Some(bright()), &c);
        let mut s = Snapshot::default();
        for i in 1..=14 { s = step(&mut w, &mut d, t0, 0.5 * i as f64, Some(bright()), &c).0; }
        assert!(s.wellness_due, "连续在座达到阈值应提醒远眺");
        let mut c2 = c.clone();
        c2.dismissed.insert(Kind::Wellness, s.wellness_epoch);
        s = step(&mut w, &mut d, t0, 7.5, Some(bright()), &c2).0;
        assert!(!s.wellness_due, "点击消除后不再打扰");
        assert!(s.wellness_secs < 1.0, "消除后重新累计用眼时长");
    }

    #[test]
    fn wellness_head_poi_counts_look_away_as_rest() {
        // 坐在座位上转头远眺：人没离席，但正脸消失 → 应算休息并重置用眼计时。
        let t0 = Instant::now();
        let mut cfg = Config::default();
        cfg.wellness_enabled = true;
        cfg.wellness_break_secs = 6.0;
        cfg.wellness_rest_secs = 2.0;
        cfg.wellness_poi = 1;
        let mut w = Watcher::new(cfg.clone());
        let mut d = FakeDetector::new(vec![vec![owner()], vec![]]);
        let c = ctrl_with(cfg);
        step(&mut w, &mut d, t0, 0.0, Some(bright()), &c);
        let mut s = Snapshot::default();
        for i in 1..=12 { s = step(&mut w, &mut d, t0, 0.5 * i as f64, Some(bright()), &c).0; }
        assert!(!s.attending, "主人正脸消失应判定为视线离开");
        assert_eq!(s.wellness_secs, 0.0, "持续视线离开应重置用眼计时，而不是继续累加");
    }

    #[test]
    fn wellness_presence_poi_ignores_brief_look_away() {
        let t0 = Instant::now();
        let mut cfg = Config::default();
        cfg.wellness_enabled = true;
        cfg.wellness_break_secs = 100.0;
        cfg.wellness_rest_secs = 2.0;
        cfg.wellness_poi = 0;
        let mut w = Watcher::new(cfg.clone());
        let mut d = FakeDetector::new(vec![vec![owner()], vec![]]);
        let c = ctrl_with(cfg);
        step(&mut w, &mut d, t0, 0.0, Some(bright()), &c);
        let s = step(&mut w, &mut d, t0, 0.5, Some(bright()), &c).0;
        assert!(s.attending, "PRESENCE 档在人脸漏检容错期内仍算在用眼");
        assert!(s.wellness_secs > 0.0);
    }

    #[test]
    fn gaze_poi_falls_back_to_head_without_eye_tracker() {
        let mut cfg = Config::default();
        cfg.wellness_poi = 2;
        cfg.sanitize();
        assert_eq!(cfg.wellness_poi, 1, "无眼动仪时 GAZE 档回退到 HEAD 档");
    }

    #[test]
    fn auto_mute_and_dim_track_away_state() {
        let t0 = Instant::now();
        let mut cfg = Config::default();
        cfg.auto_mute_enabled = true;
        cfg.lock_after_secs = 20.0;
        cfg.countdown_after_secs = 5.0;
        let mut w = Watcher::new(cfg.clone());
        let mut d = FakeDetector::new(vec![vec![owner()], vec![]]);
        let c = ctrl_with(cfg);
        let s0 = step(&mut w, &mut d, t0, 0.0, Some(bright()), &c).0;
        assert!(!s0.mic_mute && s0.dim_progress == 0.0);
        let mut s = Snapshot::default();
        for i in 1..=20 { s = step(&mut w, &mut d, t0, 0.5 * i as f64, Some(bright()), &c).0; }
        assert!(s.mic_mute, "离席应要求保护性静音");
        assert!(s.dim_progress > 0.0 && s.dim_progress <= 1.0, "渐强进度应增长");
        let mut back = FakeDetector::new(vec![vec![owner()]]);
        let s2 = step(&mut w, &mut back, t0, 10.5, Some(bright()), &c).0;
        assert!(!s2.mic_mute, "回来应解除静音");
        assert_eq!(s2.dim_progress, 0.0);
    }

    #[test]
    fn posture_enters_and_clears_with_hysteresis() {
        let t0 = Instant::now();
        let mut w = Watcher::new(Config::default());
        let close = FaceBox { x: 0.30, y: 0.20, w: 0.16 * 1.5, h: 0.32 * 1.5 };
        let mut d = FakeDetector::new(vec![vec![close]]);
        let c = ctrl();
        let mut s = Snapshot::default();
        for i in 0..=10 { s = step(&mut w, &mut d, t0, 0.5 * i as f64, Some(bright()), &c).0; }
        assert!(s.posture, "持续太近 2 秒后提示");
        let mut back = FakeDetector::new(vec![vec![owner()]]);
        for i in 11..=20 { s = step(&mut w, &mut back, t0, 0.5 * i as f64, Some(bright()), &c).0; }
        assert!(!s.posture, "恢复并保持 2 秒后解除");
    }

    #[test]
    fn config_roundtrip() {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _g = LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("daozai-presence-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let old = std::env::var_os("DAOZAI_DATA_DIR");
        std::env::set_var("DAOZAI_DATA_DIR", &dir);
        let mut c = Config::default();
        c.enabled = true;
        c.lock_after_secs = 45.0;
        c.posture_baseline_w = 0.184375;
        c.posture_calibrated = true;
        c.posture_warning_secs = 4.5;
        c.lock_enabled = false;
        c.behind_face_confirm = 3;
        c.wellness_enabled = true;
        c.wellness_poi = 0;
        c.auto_mute_enabled = true;
        c.save();
        let loaded = Config::load();
        assert!(loaded.enabled);
        assert_eq!(loaded.lock_after_secs, 45.0);
        assert!((loaded.posture_baseline_w - 0.184375).abs() < 1e-6);
        assert!(loaded.posture_calibrated);
        assert_eq!(loaded.posture_warning_secs, 4.5);
        assert!(!loaded.lock_enabled);
        assert_eq!(loaded.behind_face_confirm, 3);
        assert!(loaded.wellness_enabled);
        assert_eq!(loaded.wellness_poi, 0);
        assert!(loaded.auto_mute_enabled);
        match old { Some(v) => std::env::set_var("DAOZAI_DATA_DIR", v), None => std::env::remove_var("DAOZAI_DATA_DIR") }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn face_box_matching_is_stable() {
        let a = owner();
        let mut b = a;
        b.x += 0.02;
        assert!(a.matches(&b));
        let far = FaceBox { x: 0.8, y: 0.1, w: 0.1, h: 0.2 };
        assert!(!a.matches(&far));
    }
}
