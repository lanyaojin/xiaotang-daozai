//! 在场保护的设置卡片与岛上横幅绘制。
//!
//! 这个文件把新功能的 UI 集中在一个 `impl App` 里；上游 app.rs / extras.rs
//! 只保留少量挂点，合并新版本时冲突面最小。

use crate::app::App;
use crate::paint::{Card, H};
use crate::theme::{self, W};
use crate::presence::{Config, Tone};
use eframe::egui;

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
        crate::extras::setting_row(ui, "到点自动锁屏", "关闭则只显示渐强倒计时、不锁屏（参考 Glance 的仅调暗动作）", &mut cfg.lock_enabled);
        crate::extras::settings_hint(ui, "离开超过起点秒数后，灵动岛显示倒计时并逐渐由白转黄；在锁屏前回来立即解除。");
        if ui.button("暂缓本次锁屏").clicked() { self.presence.snooze(); }
        crate::extras::settings_hint(ui, "暂缓只重置本次离席计时，布防不会关闭。");
        ui.separator();
        crate::extras::setting_row(ui, "坐姿太近提示", "脸框明显大于校准时的正常距离，且持续一段时间才提示", &mut cfg.posture_enabled);
        crate::extras::setting_number(ui, "太近判定倍率", &mut cfg.posture_ratio, 1.05, 2.0, " ×", 0.05);
        crate::extras::setting_number(ui, "太近持续多久提示", &mut cfg.posture_warning_secs, 0.5, 30.0, " 秒", 0.5);
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
        crate::extras::setting_row(ui, "身后身影提示", "识别到主人以外的第二张脸，且左右肩后有持续动静时分级提示", &mut cfg.behind_enabled);
        crate::extras::setting_number(ui, "身影检测阈值", &mut cfg.behind_motion, 6.0, 60.0, "", 2.0);
        crate::extras::settings_hint(ui, "模拟 Glance 的左右肩后 ROI + 多用户置信度。没有第二张脸时，打字、光影、宠物一律不提示（修复 11-11 误报）。画面太暗或检测不可用时保护暂停，不会误锁屏。");
        ui.separator();
        crate::extras::setting_row(ui, "用眼时长提醒（Digital Wellness）", "连续在座达到设定时长后提醒抬头远眺，离开一会儿自动重置", &mut cfg.wellness_enabled);
        crate::extras::setting_number(ui, "连续用眼提醒间隔", &mut cfg.wellness_break_secs, 60.0, 7200.0, " 秒", 60.0);
        crate::extras::setting_number(ui, "离席多久算休息", &mut cfg.wellness_rest_secs, 5.0, 300.0, " 秒", 5.0);
        crate::extras::setting_row(ui, "离席自动静音麦克风（Auto Mute）", "离席时静音默认麦克风，回来恢复；不会覆盖你原本的静音设置", &mut cfg.auto_mute_enabled);
        ui.separator();
        ui.label(egui::RichText::new(format!("状态：{}", self.presence.status_line())).size(11.5));
        if self.presence.snapshot().and_then(|s| s.owner_box).is_some() {
            ui.label(egui::RichText::new("已识别到主座位的人，可以校准正常距离。").size(11.0).color(theme::GREEN));
        }
        if let Some(s) = self.presence.snapshot() {
            let mut parts: Vec<String> = Vec::new();
            if cfg.posture_enabled && s.posture_closeness > 0.0 {
                parts.push(format!("当前距离 {:.2}× 基线", s.posture_closeness));
            }
            if cfg.wellness_enabled {
                parts.push(format!("连续用眼 {:.0} 分钟", (s.wellness_secs / 60.0).floor()));
            }
            if s.mic_mute { parts.push("麦克风已保护性静音".into()); }
            if !parts.is_empty() {
                crate::extras::settings_hint(ui, &parts.join(" · "));
            }
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
