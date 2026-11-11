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

**2026-11-11 17:00 真机验收**：用户报告「随意移动、打字耸肩没出现身后有人提示」，
随后「有人来找签字，确实显示身后有人并做了提醒」。误报与正报双向通过，
身后检测方案定稿，后续不再改动。

## 七、眼动追踪（POI_USED_GAZE）为何没有移植

用户指出本机 Windows Hello 摄像头支持眼动追踪、Glance 曾在此机使用。实测核查：

| 检查 | 结果 |
| --- | --- |
| 设备管理器 | 存在 `Integrated Camera`（`USB\VID_174F&PID_1812&MI_00`）与 **`Integrated IR Camera`**（同设备 `MI_02`）——确实是 RGB+IR 双通道 |
| 标准 Media Foundation 枚举 | 只返回 `Integrated Camera`；`presence.log` 全历史均为 `camera devices: Integrated Camera (non-IR preferred)`，**IR 通道不对标准采集 API 暴露** |
| Windows Eye Control | `HKCU\...\EyeControl` 不存在；无 Tobii / Mirametrix / Glance 相关服务或驱动在运行 |
| WinRT `FaceDetector` | 官方 API 只有 `CreateAsync` / `DetectFacesAsync` / 格式查询，返回 `DetectedFace.FaceBox`，**无视线方向、无头部姿态、无瞳孔数据** |
| Windows Hello IR | 仅用于登录时的人脸认证（±15° 正脸检查），不对外提供 gaze 数据流 |

Glance 的注视能力来自它自己的链路：154 MB 私有增强驱动 +
`NISDK.dll` + 加密分类模型 + `SharedMemoryCameraLibrary` / `FSCameraLibrary` /
`UVCCameraLibrary`，从 IR 帧计算注视点（`NIKROITracker`、`NIKFixationHandler`、
`NIKGazeArea::PogType::PT_FIXATION`），并通过 `FEATURE_PRESENCE_DETECTION_POI_USED_GAZE`
把注视点作为在场/休息判据。**这条链路不通过任何 Windows 公开 API 暴露**。

因此岛仔的取舍：

- 不移植真 gaze：需要私有驱动 + 150 MB 以上依赖 + 模型，与单文件 19 MB 的轻量目标直接冲突，
  且涉及第三方闭源组件，合规与稳定性都不可控。
- 复刻 Glance 的**降级档位**：`POI_USED_PRESENCE`（人在座即算用眼）与
  `POI_USED_HEAD`（本拍检测到主人正脸才算看屏幕，转头远眺即算休息）。
  配置项 `wellness_poi`，默认 HEAD 档；`Snapshot.attending` 可在设置页看到当前注意力状态。
- 由此，Digital Wellness 的「是否休息」不再只看离席：坐在座位上转头远眺也能重置用眼计时，
  符合 20-20-20 的本意。
- 保留的可选路线：若将来愿意安装那个私有驱动，可评估其共享内存接口能否只读取得
  IR 帧或注视点；届时需重新评估体量、稳定性与合规性，本版不做。

## 八、复刻私有增强驱动的可行性评估与最终决定（2026-11-11 17:27）

用户提问「能否实现私有增强驱动的功能、完成眼动追踪」，随后明确决策：
**若实现太困难就按「转头远眺算休息」轻量化落地，exe 控制在 30 MB 以下，
核心功能实现即可，眼动追踪暂时不上、后续需要再上。** 分层评估如下。

### 1. 驱动层（不可行 / 不建议）

| 障碍 | 说明 |
| --- | --- |
| 签名与分发 | 内核或 KS/UVC 过滤驱动需要 EV 代码签名 + WHQL/HLK，第三方无法复用 Glance 的签名 |
| 硬件资料 | IR 传感器寄存器、暗瞳/亮瞳 LED 同步时序、双通道配准参数均无公开数据手册 |
| 设备独占 | Windows Hello 人脸认证会独占 IR 流；第三方采集需与其共存或抢占，可能破坏登录 |
| 稳定性 | 闭源驱动与系统相机栈耦合，蓝屏/摄像头不可用风险不可控 |

即「复刻私有增强驱动」本质上是在没有资料与签名条件的情况下重写一个闭源驱动，
不是应用侧工作量，且会直接损伤本机 Windows Hello。

### 2. 算法层（即使拿到 IR 帧仍需要的完整链路）

PCCR（瞳孔中心-角膜反射）特征提取 → 5/9 点标定 → 头动补偿 →
视线向量到屏幕坐标映射 → 注视/扫视/眨眼滤波（`NIKFixationHandler` 的 `PT_FIXATION`）
→ 与在场状态机双延迟联动。Glance 用加密 `.date` 模型 + `NIKROITracker` 完成，
任一环节缺失都无法给出可信注视点。

### 3. 不碰内核的替代路径（可行但与轻量目标冲突）

用公开 RGB 人脸关键点 / 轻量视线估计模型在普通摄像头上估算注视方向。
代价：引入 ONNX/MediaPipe 类运行时 + 模型文件，exe 体量与常驻 CPU 明显上升，
且单 RGB 无 IR 补光，逆光、戴眼镜、低头场景精度远低于专用眼动仪。
与「单文件、零模型、30 MB 以下」的约束直接冲突。

### 4. 决定与体量对照

**不安装第三方私有驱动、不编写内核驱动、不擅自引入模型**；
保留已实现的 `POI_USED_PRESENCE / POI_USED_HEAD` 两档降级判据，默认 HEAD 档
（本拍检测到主人正脸才算看屏幕，转头远眺/低头累计休息时间并重置用眼计时）。

| 项目 | Glance 11.19 | 岛仔 v6 |
| --- | --- | --- |
| 分发体量 | MSIX 667,910,946 B + 驱动 154,915,216 B；解包约 787 MB / 973 文件 | 单文件 `daozai.exe` **19,063,296 B（19.06 MB）** |
| 运行时 | Qt6 QML + OpenCV 4.8.1 + TensorFlow Lite + libopenblas + NISDK + 加密模型 | 无外部依赖，零模型 |
| 眼动 | 私有驱动 + IR + 加密模型，`POI_USED_GAZE` | 暂不实现；HEAD 档近似 |
| 用户上限 | — | 30 MB，**实测 19.06 MB，余量充足** |

后续若确需真 gaze，接入顺序建议：先确认私有驱动能否以只读方式共享注视点
（不改驱动、不写内核），再评估是否为一个可选插件单独引入模型；
主线程序继续保持单文件轻量，不因可选能力牺牲体量。
