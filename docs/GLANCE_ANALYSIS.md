# Glance 11.19 解包分析与岛仔功能映射

> 2026-11-11 · 只读分析，未安装、未运行 Glance 及其驱动。
> 目标：把成熟产品的**行为逻辑**移植到轻量岛仔，而不是移植它的体积与依赖。

## 一、解包范围

| 项目 | 数据 |
| --- | --- |
| `02离线安装包11.19版本软件.Msix` | 667,910,946 字节 |
| 解包后 | 973 个文件 / 约 787 MB |
| `01先安装驱动.exe` | 154,915,216 字节（外接摄像头私有驱动） |
| 应用二进制 | `Glance.exe` 4.6 MB、`NISDK.dll` 5.0 MB、`CameraController.dll` 255 KB、`Calibration.dll` 58 KB |
| 文本资源 | 529 个（Qt QML / HTML / XML），应用自有逻辑不在明文里 |
| 模型与配置 | 多个约 18 MB 的加密 `.date` 分类模型、加密 `.xme`（头部为随机字节，无明文头） |
| 离线帮助 | 33 种语言，均为「设备未连接，请访问 Mirametrix 网站」跳转页，无功能说明 |

**技术栈**：Qt6 QML + OpenCV 4.8.1 + TensorFlow Lite + libopenblas + 私有 `NISDK.dll`
+ 私有摄像头库（`FSCameraLibrary` / `SharedMemoryCameraLibrary` / `UVCCameraLibrary`）
+ 加密神经网络模型。对比岛仔：单个 Rust + egui 可执行文件约 19 MB，零外部模型。

**硬件前提**：设置 PDF 与驱动安装包共同表明，Glance 依赖**外接 RGB-IR 摄像头 + 私有增强驱动**
（PDF 原文提到驱动「只能和 Windows 系统驱动握手、不能和 Glance 软件握手」）。
这解释了它为什么能测真实深度（厘米）并在红外通道做多人分类——普通 Windows Hello 摄像头做不到。

## 二、分析方法

中文 UI 文案全部加密在 `.xme` / `.date` 里（尝试 UTF-16LE 提取得到的都是随机字节解码的乱码），
但两个主二进制的**英文配置键、说明文字、C++ 符号名是明文**，共提取 6.3 万条字符串：

- `glance/msix-strings/`：逐个二进制的 ASCII / UTF-16 字符串
- `glance/analysis/feature_map.txt`：按主题归类的配置键与检测流水线符号
- `glance/extract_strings.py`、`analyze_keywords.py`、`dump_features.py`：可复现的提取脚本

## 三、还原出的实现方法

### 1. 身后身影（Shoulder Surfing）——多人检测，不是背景差分

```
FEATURE_SHOULDER_SURFING_LEFT / FEATURE_SHOULDER_SURFING_RIGHT
"Filtering of low confidence shoulder surfers (with an IR cam) under that value (0-1 for 0-100%)"
"Filtering of low confidence shoulder surfers (with an RGB cam) under that value (0-1 for 0-100%)"
Surfer IR Threshold / Surfer RGB Threshold / surferIRThreshold / surferRGBThreshold
NIKROITracker::SetSurferConfidenceThreshold
NIKMaseInterface::SendShoulderSurfing(bool, bool)      ← 左右两个结果
SetMultiUsersRange / SetMultiUsersFramingState / RegisterMultiUsersCallback
GENERAL_SETTINGS_YELLOW_STATE_RANGE_MIN / MAX           ← 分级状态区间
```

结论：以**多用户人脸/人体分类器的置信度**为主，IR 与 RGB 各一套阈值；
检测范围限定为主人**左右两侧 ROI**（LEFT / RIGHT 两个独立事件），并按区间分级提示。

### 2. 坐姿提醒（Posture Reminder）——真实深度 + 持续时间

```
FEATURE_POSTURE_REMINDER
FEATURE_POSTURE_REMINDER_DEPTH_THRESHOLD_CM     ← 厘米
FEATURE_POSTURE_REMINDER_TIME_TO_WARNING        ← 持续多久才告警
FEATURE_CURRENT_DISTANCE_MANUAL_EVENT           ← 显示当前距离
日志字符串："Digital Wellness Posture Warning"
```

结论：靠 IR/ToF 深度测距，**必须持续超过阈值时间**才提示，并给出当前距离；归类在 Digital Wellness 下。

### 3. 离席（Presence Detection）——三信号 + 双延迟 + 渐暗警告

```
FEATURE_PRESENCE_DETECTION_POI_USED_GAZE / HEAD / PRESENCE
FEATURE_PRESENCE_DETECTION_LOCK_ATTENTION_AVAILABLE_DELAY_SEC
FEATURE_PRESENCE_DETECTION_LOCK_ATTENTION_UNAVAILABLE_DELAY_SEC
FEATURE_PRESENCE_DETECTION_WARNING_DURATION_MS / FADE_DURATION_MS / FADE_TO_PERCENT
FEATURE_PRESENCE_DETECTION_LOCK_ACTION_DIM_DISPLAY / LOCK / SCREENSAVER
FEATURE_PRESENCE_SNOOZE_VALUE
FEATURE_ADAPTIVE_DIM_DELAY_TO_START / FADE_DURATION / SCREEN_OFF
```

结论：在场信号可选注视/头部/存在；「注意力可用 / 不可用」用**两套不同延迟**；
锁屏前先**渐变暗屏警告**；提供 snooze 暂缓与三种锁屏动作。

### 4. 其它可扩展功能（本次纳入两项）

```
FEATURE_DIGITAL_WELLNESS / FEATURE_DW_STATISTICS_DAILY_SCREEN_TIME_STR
                         / FEATURE_DW_STATISTICS_CURRENT_SESSION_STR
FEATURE_AUTO_MUTE / FEATURE_AUTO_MUTE_DEPTH_THRESHOLD_CM
FEATURE_MUTE_WARNING / FEATURE_WAKE_ON_APPROACH_SIMULATION
FEATURE_SMART_POINTER / FEATURE_SNAP_WINDOW / FEATURE_VIRTUAL_PRESENTER  ← 眼动/虚拟摄像头，不移植
```

## 四、岛仔 v5 的模拟映射（保持轻量）

| Glance 能力 | 岛仔实现 | 取舍 |
| --- | --- | --- |
| 左右 ROI + 多用户置信度 | WinRT FaceDetector 的**第二张脸**连续确认拍数 + 左右肩后 ROI 运动块辅助 | 无 IR / 无模型；**没有第二张脸一律不提示** |
| 1/3/5 秒分级 | 保留白 / 黄 / 红三级与可点击消除 | 对应 `YELLOW_STATE_RANGE` 思路 |
| 深度厘米阈值 | 脸框宽高相对校准基线的倍率 | 单 RGB 无深度，只能相对距离 |
| `TIME_TO_WARNING` | `posture_warning_secs`（可配置，默认 2 秒） | 防低头抬头闪烁 |
| `CURRENT_DISTANCE` | 设置页显示「当前距离 x.xx× 基线」 | — |
| 注视/头部/存在三信号 | 人脸存在与否（保守） | 不做眼动追踪 |
| 双延迟 | 单一 `lock_after_secs` + `countdown_after_secs` | 简化，避免误判 |
| 渐变暗屏警告 | 倒计时横幅由白渐黄 + `dim_progress` 0..1 | 不调用显示器亮度 API |
| `LOCK_ACTION_*` | `lock_enabled`：锁屏 / 仅提示两档 | 不做屏保动作 |
| `PRESENCE_SNOOZE_VALUE` | 「暂缓本次锁屏」按钮，只重置计时不撤防 | — |
| Digital Wellness 屏幕时间 | 连续在座用眼计时 + 远眺提醒 + 离席自动重置 | 不做跨日统计 |
| Auto Mute | 离席静音默认麦克风、回来恢复；**只恢复由我们静音的状态** | 复用项目已有的 Core Audio 端点代码 |
| 加密模型 / OpenCV / TFLite | **全部不引入** | exe 保持约 19 MB |

## 五、明确不做

- 不安装 Glance 或其私有驱动，不复用其任何二进制、模型或资源文件（仅供行为参考）。
- 不移植眼动控制、虚拟摄像头、虚拟演讲者、自动构图等与「在场保护」无关且高开销的功能。
- 不承诺与 Glance 同等准确率：单 RGB 摄像头在逆光、多人重叠、侧身无正脸时仍可能漏报；
  设计上**宁可漏报也不误报**（11-11 的 130 秒误报是反面教训）。

## 六、对 11-11 误报事故的解释

旧版代码 `evidence = (other_faces > 0 || blob) && !global_change`：
只要背景差分出现运动块就计为身后证据，主人打字、耸肩、光影变化都会被累计到红字。
Glance 从不做这种「纯运动块」判定——它以第二个人为中心。v5 已改为必须有非主人的第二张脸。
