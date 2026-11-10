//! 在场保护的设置卡片与岛上横幅绘制。
//!
//! 这个文件把新功能的 UI 集中在一个 `impl App` 里；上游 app.rs / extras.rs
//! 只保留少量挂点，合并新版本时冲突面最小。

use crate::app::App;
use crate::paint::{Card, H};
use crate::theme::{self, W};
use crate::presence::{Config, Tone};

impl App {
    /// 设置窗口「常规」页里的在场保护卡片。由 extras.rs::settings_ui 调用。
    pub(crate) fn presence_settings(&mut self, ui: &mut egui::Ui) {
        let before = self.presence.config.clone();
        let mut cfg: Config = before.clone();
        let mut enabled = cfg.enabled;

        crate::extras::setting_row(
            ui,
            "在场保护（摄像头）",
            "坐姿太近提示、离席自动锁屏、身后身影分级提示；图像只在本机内存中使用",
            &mut enabled,
        );
        ui.separator();
        crate::extras::setting_number(ui, "离席自动锁屏", &mut cfg.lock_after_secs, 10.0, 300.0, " 秒", 5.0);
        crate::extras::setting_number(ui, "倒计时提示起点", &mut cfg.countdown_after_secs, 1.0, 10.0, " 秒", 1.0);
        crate::extras::settings_hint(ui, "离开超过起点秒数后，灵动岛显示倒计时；在锁屏前回来立即解除。");
        ui.separator();
        crate::extras::setting_row(ui, "坐姿太近提示", "脸框明显大于校准时的正常距离就提示", &mut cfg.posture_enabled);
        crate::extras::setting_number(ui, "太近判定倍率", &mut cfg.posture_ratio, 1.05, 2.0, " ×", 0.05);
        ui.horizontal(|ui| {
            if ui.button("以当前坐姿校准正常距离").clicked() {
                let ok = self.presence.calibrate();
                self.say(if ok { "已记录正常坐姿距离" } else { "未识别到主人，请正对摄像头后重试" });
                cfg = self.presence.config.clone();
            }
            if ui.button("恢复默认基线").clicked() {
                cfg.posture_baseline_w = 0.16;
                cfg.posture_baseline_h = 0.32;
                cfg.posture_calibrated = false;
            }
        });
        ui.separator();
        crate::extras::setting_row(ui, "身后身影提示", "主座位区域以外出现持续身影：1 秒白字、3 秒黄字、5 秒红字", &mut cfg.behind_enabled);
        crate::extras::setting_number(ui, "身影检测阈值", &mut cfg.behind_motion, 6.0, 60.0, "", 2.0);
        crate::extras::settings_hint(ui, "阈值越小越灵敏。单个 RGB 摄像头无法区分人、宠物或光影变化；画面太暗、摄像头被占用或检测不可用时保护会暂停，不会误锁屏。提示可随时点击消除，直到本次事件结束。");
        ui.separator();
        ui.label(egui::RichText::new(format!("状态：{}", self.presence.status_line())).size(11.5));
        if self.presence.snapshot().and_then(|s| s.owner_box).is_some() {
            ui.label(egui::RichText::new("已识别到主座位的人，可以校准正常距离。").size(11.0).color(theme::GREEN));
        }

        cfg.enabled = enabled;
        if enabled != before.enabled {
            self.presence.set_enabled(enabled);
        }
        if cfg != before {
            self.presence.apply(cfg);
        }
    }

    /// 收起状态下绘制在场横幅。点击横幅任意位置即可消除本次提示。
    pub(crate) fn presence_draw(&mut self, c: &Card, panel: egui::Rect) {
        let Some(b) = self.presence.banner() else {
            self.presence.hit_rect = None;
            return;
        };
        let color = match b.tone {
            Tone::White => theme::WHITE,
            Tone::Yellow => theme::YELLOW,
            Tone::Red => theme::RED,
        };
        let w = panel.width().max(200.0);
        let cy = panel.height() * 0.5;
        let text_w = c.text_w(&b.text, 12.5, W::Regular);
        c.centered_text(14.0, cy, &b.text, 12.5, W::Regular, color, H::Left);
        c.centered_text(w - 20.0, cy, "×", 15.0, W::Regular, theme::GRAY, H::Center);
        // 实测宽度回写，供下一帧 base_size() 让岛体自动变宽。
        self.presence.width_hint = text_w + 96.0;
        self.presence.hit_rect = Some([panel.min.x, panel.min.y, panel.max.x, panel.max.y]);
        let response = c.hit(0.0, 0.0, w, panel.height(), "presence-dismiss");
        if response.clicked() {
            self.presence.dismiss(b.kind, b.epoch);
        }
    }
}
