# 在场保护（摄像头）功能说明

适用于 `presence-v2` 定制分支，基于上游 `Renly1994/xiaotang-daozai` v0.6.63 重新开发。

## 一、三个目标

### 1. 坐姿太近提示

- 在「设置 → 常规 → 在场保护」点击「以当前坐姿校准正常距离」，程序记录此刻人脸框大小作为基线，写入 `%APPDATA%\Daozai\presence.txt`。
- 之后当人脸框宽或高超过基线 ×「太近判定倍率」（默认 1.30）并持续 2 秒，灵动岛显示黄色提示「离屏幕太近了，坐远一点」。
- 距离回落到 1.15 倍以内并持续 2 秒后自动解除（滞回，避免低头抬头闪烁）。
- 只判断主座位的人；旁人靠近不会触发。

### 2. 离席自动锁屏

- 默认 30 秒（可在 10–300 秒间调整）。
- 主座位的人离开后，计时用系统单调时钟（`Instant`）精确累计；达到「倒计时提示起点」（默认 5 秒）后，灵动岛显示「离开中，还剩 N 秒锁屏」。
- 在锁屏前返回座位，倒计时立即解除并重新布防。
- 到设定时间后调用 `LockWorkStation` 锁定工作站；失败会每 2 秒重试并在岛上显示错误，不会静默放弃。
- **安全底线**：摄像头不可用、人脸检测不可用、画面太暗、从未识别到主人时，暂停计时且不锁屏，避免误锁。

### 3. 身后身影分级提示

- 主人在座时，对「主座位区域以外」的画面做背景差分，并结合「出现非主人的人脸」两种证据判断是否有人影。
- 身影持续 1 秒：白字；3 秒：黄字；5 秒：红字。
- 点击横幅任意位置即可消除本次提示；身影消失、事件结束后再出现，会重新提示。
- 整屏光照突变（开灯、窗帘）自动识别并忽略；人影短暂被遮挡有 0.6 秒宽限，计时不重置。

## 二、开启与配置

1. 设置 → 常规 → 在场保护 → 打开「在场保护（摄像头）」总开关。
2. 摄像头枚举会优先选择非 IR 摄像头（本机为 `Integrated Camera` + `Integrated IR Camera` 双设备，选错会拿到 Windows Hello 的红外画面）。
3. 开关、锁屏秒数、倒计时起点、坐姿基线、身后灵敏度全部持久化，重启后保持。
4. 设置页底部实时显示状态：摄像头设备名、主人在座/离开、检测是否可用。

## 三、旧版问题与本次修复

| 旧版问题 | 本次修复 |
|---|---|
| 帧差门控在「人离开、画面几乎不变」时复用旧主人框，离席永不触发 | 每拍重新检测人脸，删除门控复用 |
| 计时用「拍数 × 667ms」，30 秒可能变成 60 秒 | 一律用 `Instant` 墙钟差值 |
| WinRT 人脸检测在 UI 线程创建、后台线程无 COM 初始化，错误被吞掉 | 检测线程显式 `RoInitialize(MTA)`，检测对象在 MTA 线程创建，错误写日志并在岛上可见；采集线程**不做** COM 初始化（nokhwa 的 Windows 绑定会自行 `CoInitializeEx(STA)`，预先 MTA 初始化会因 `RPC_E_CHANGED_MODE` 让 Media Foundation 枚举/打开全部失败，2026-11-10 实测修复） |
| 关闭开关后线程靠 channel 断连退出，摄像头常亮 | 显式 stop 标志，每拍无条件检查 |
| 身后提示只是「出现第二张脸」，无分级、不能点击消除 | 背景差分 + 非主人脸双证据，1/3/5 秒分级，点击按事件消除 |
| 开关与锁屏秒数不持久化，重启即失效 | 独立 `presence.txt` 持久化全部配置 |
| 锁屏失败后永久静默失效 | 失败重试 + 岛上有提示 |
| 每帧重推 Alert 污染通知历史、与全局通知开关耦合 | 独立横幅渲染，不走 Alert 队列 |

## 四、已知限制（重要）

1. 单个 RGB 摄像头无法真正判断「身后」的空间位置。本实现检测的是「主座位区域以外出现的持续运动块或非主人人脸」，会把宠物、晃动的窗帘、光影变化误判为人影；也可能漏检完全静止、镜头外或只拍到身体局部的人。可在设置里提高「身影检测阈值」降低误报。
2. 坐姿「距离」是人脸框大小的代理量，不是物理测距；换摄像头、改变座位或戴眼镜都会影响基线，建议重新校准。
3. WinRT `FaceDetector` 主要识别正脸，低头、侧脸、遮挡会漏检。系统用 3 拍（约 0.75 秒）容忍，长时间彻底遮住脸可能被判定为离席。
4. 摄像头被其它程序独占（Zoom、相机 App）时保护会暂停，不会误锁屏。
5. 帧只在内存中处理，不落盘、不保存人脸模板、不做身份识别。日志仅记录设备名与状态，写入 `%APPDATA%\Daozai\presence.log`（超过 64 KB 自动轮转）。

## 五、与 Glance 的关系及下一步（本次不实施）

用户提供的 `11.19版本Glance.zip` 是这套需求的参考原型：压缩包约 825 MB，内含
MSIX 安装包约 652 MB、驱动约 151 MB，属于完整商业产品。本次实现**不依赖 Glance 的任何文件**：
三个目标用岛仔自带的 Rust/egui 管线重新实现，只做轻量近似。

轻量性对比（本次实现）：

- 采样 4 Hz、每帧 320×240 灰度约 75 KB，两名后台线程（采集 + 状态机），无模型文件、无驱动、不落盘图像；
- 新增依赖仅 `nokhwa`（含 MJPEG 解码）与 Windows 自带 WinRT 人脸检测，无 OpenCV / ONNX / 运行时；
- 设置与状态数据在 `%APPDATA%\Daozai\presence.txt`，与上游 settings 分离，便于合并。

关于 Windows Hello 摄像头：本机枚举到 `Integrated Camera`（RGB）与 `Integrated IR Camera`
（Windows Hello 红外）两个设备。当前策略是**优先 RGB**：WinRT `FaceDetector` 稳定支持
Gray8/Nv12，RGB 转灰度链路已在旧版验证可用，而 IR 设备常被 Windows 生物识别服务独占、
且红外画面中的人脸特征与可见光模型不匹配。所有设备仍是回退候选，RGB 打不开时会自动尝试
IR；设置页「状态」会显示实际打开的设备名。若后续决定以 IR 为主，只需调整
`presence_backend.rs::candidates()` 的排序，并把 `behind_motion` 阈值按红外画面重新标定。

下一步移植建议（需另行安排，不在本次范围）：

1. 只读分析 MSIX 内资源与设置说明 PDF，提取三项功能的判定阈值、状态机与交互细节，用于校准本实现的阈值与文案；
2. 结合 Windows Hello 的红外/深度能力评估「身后」几何判定，改进单 RGB 摄像头无法区分人与光影的固有限制；
3. 若体积需要进一步压缩，可评估用 Windows 原生 Media Foundation 直接采集替换 `nokhwa`，
   去掉 MJPEG 解码依赖，但需重新验证设备兼容性与帧格式。

## 六、故障排查

- 岛上出现「摄像头不可用」/「人脸检测不可用」/「画面太暗」：查看 `presence.log` 最后一行的设备打开与 FaceDetector 初始化结果；确认摄像头未被独占、光线充足、系统支持 WinRT 人脸检测。摄像头打开失败时，日志会记录**每个设备的具体失败原因**（nokhwa 错误），同一原因只记一次，不会刷屏。
- 锁屏未触发：确认设置页状态显示「主人在座」曾经出现（没识别到主人不会布防），再离开座位观察倒计时。
- 提示误报频繁：提高「身影检测阈值」，或在不需要时单独关闭「身后身影提示」。

## 七、本机安装与实测清单

构建产物来自 GitHub Actions「构建在场保护版本」，release 目标为
`x86_64-pc-windows-msvc`；本机缺 MSVC 链接器，因此不要指望本地 `cargo build` 成功，
以 CI 产物为准。

安装（覆盖前必须备份，且先退出正在运行的岛仔）：

```powershell
$dest = "$env:LOCALAPPDATA\Programs\Daozai\daozai.exe"
Copy-Item $dest "${dest}.bak-presence-v2" -Force
Copy-Item "F:\Software Engineering\dist\daozai.exe" $dest -Force
```

岛仔是单实例程序（互斥体 `Local\Daozai.SingleInstance`），新旧 exe 不能同时运行，
替换前需结束现有 `daozai` 进程再启动。也可设 `DAOZAI_DATA_DIR` 指向临时目录，
用隔离配置做冒烟测试，不影响正式设置。

实测顺序（先在设置 → 常规 → 在场保护打开总开关，确认状态栏显示实际摄像头名）：

1. **摄像头链路**：状态应显示设备名（本机预期 `Integrated Camera`）与「主人在座」；
   若显示诊断横幅，先看 `%APPDATA%\Daozai\presence.log`。
2. **坐姿**：以正常坐姿点「以当前坐姿校准正常距离」→ 刻意靠近镜头并停留 2 秒 →
   岛上出现黄色「离屏幕太近了，坐远一点」→ 坐回原位 2 秒后自动消失。
   注意旧版把基线存在 `settings.txt`，新版存在 `presence.txt`，**需重新校准一次**。
3. **离席**：保持被识别后离座 → 约 5 秒出现「离开中，还剩 N 秒锁屏」→
   中途回座，倒计时应立即消失且之后不锁屏；再次离座且不返回 → 约 30 秒锁屏。
   测试时建议先把「锁屏秒数」调到 10–15 秒，避免等待过久。
4. **身后身影**：保持主人在座，让另一人从身后经过 → 1 秒白字、3 秒黄字、
   5 秒红字；各阶段点击横幅应立刻消除，人影消失后再次出现会重新提示。
5. **安全底线**：用手完全遮住摄像头或关灯 → 岛上出现「画面太暗」类诊断，
   离席计时冻结，不应锁屏。
6. **资源**：任务管理器观察 `daozai` 的 CPU / 内存；关闭总开关后摄像头指示灯应熄灭。

回滚：结束进程，把 `daozai.exe.bak-presence-v2` 复制回 `daozai.exe` 即可；
`presence.txt` / `presence.log` 是独立文件，删除不影响原有岛仔设置。

## 八、2026-11-10 首次本机实测记录（已暂停，后续继续）

### 时间线与证据

| 时间 | 事件 | 证据 |
|---|---|---|
| 16:46 | 安装首个 CI 产物（19,048,960 B，v0.6.63），开启总开关 | 进程 PID 16452 |
| 16:46–17:06 | 摄像头枚举退化为「摄像头 0..3」，4 个索引每秒重试全部打开失败；`FaceDetector ready (MTA)` 正常 | `%APPDATA%\Daozai\presence.log` |
| 16:52 | 定位根因：`nokhwa-bindings-windows 0.4.6::initialize_mf()` 在采集线程调用 `CoInitializeEx(STA)`，而 `camera_loop` 先 `RoInitialize(MTA)`，STA 初始化返回 `RPC_E_CHANGED_MODE` | 绑定库源码 `lib.rs:179`；提交 `f9c643e` |
| 17:07 | 修复版 CI 通过 | Actions run `42786053956`：逻辑测试 ✅ / release 构建 ✅ / 上传 ✅ |
| 17:08 | 安装修复版（19,055,616 B，SHA256 `e9202f71…936e`），摄像头链路**成功** | 日志：`camera devices: Integrated Camera (non-IR preferred)` → `FaceDetector ready (MTA)` → `camera opened: Integrated Camera`；打开后 0 次帧失败、0 次重开、0 次打开失败 |
| 约 17:10 | 进程静默退出：**无 WER 崩溃事件、无系统重启记录、日志无错误行** | `Get-WinEvent`（Application / System，近 30 分钟）为空 |
| 17:13 | 带 stderr 重定向复现：存活 50 秒、stderr/stdout 均为空；但该启动方式使进程成为工具会话子进程，会话结束会被清理，**不能作为崩溃证据** | `dist\daozai-stderr.log` 0 字节 |
| 17:15 | 回滚到已知稳定版并拉起 | 安装目录 `daozai.exe` = 19,263,488 B，SHA256 `2ccfbd67…7eb7`，与备份一致；进程 PID 23672 稳定运行 |

### 结论与未决问题

- **已验证成功**：COM 单元冲突修复有效。设备枚举恢复真实名称、RGB 摄像头成功打开、WinRT 人脸检测器在 MTA 线程初始化成功、打开后无帧错误。
- **未决问题**：修复版在「成功采集帧之后的路径」出现一次进程静默退出，原因未定位。嫌疑方向：① `[profile.release] panic = "abort"` 使任意线程 panic 直接终止 GUI 进程且不留 WER 记录；② `DetectorImpl::detect()` 的 WinRT `DetectFacesAsync().get()` 或帧几何索引在真实帧数据下触发异常；③ 采集线程 STA 单元与 WinRT MTA 线程的交互。**也可能是外部看门狗 / 启动方式导致，尚未证实为崩溃**。
- 旧版在同一台机器上带摄像头稳定运行 20 分钟以上，因此该问题与本次「成功打开摄像头」的新路径相关。

### 当前机器状态（下班前）

- `C:\Users\Lijin\AppData\Local\Programs\Daozai\daozai.exe` = **旧稳定版**（自动锁定功能可用），备份 `daozai.exe.bak-presence-v2` 同哈希。
- 修复版 exe 保留在 `F:\Software Engineering\dist\daozai-v3.exe`，CI 产物 `daozai-presence-v3.zip`；旧 CI 版 `dist\daozai.exe`。
- `%APPDATA%\Daozai\presence.txt` 的 `enabled=0`（防止修复版被误启动后自动开启摄像头）。**恢复开发时需改回 `enabled=1`**。
- upstream remote 的 push 地址已禁用；所有提交只推送到自己的 fork `lanyaojin/xiaotang-daozai:presence-v2`。

### 后续开发待办（按优先级）

1. 抓退出证据：用 `Start-Process -PassThru` 启动修复版，轮询 `HasExited` / `ExitCode`；启用 WER `LocalDumps` 或检查 `diagnostics\` 目录，确认是 Rust panic、原生访问冲突还是正常退出。
2. 因 `panic = "abort"`，`catch_unwind` **无法**兜底；需消除 panic 源，或评估将 presence 逻辑放到独立子进程，或为该版本改用 `panic = "unwind"`（会增加体积，与轻量化目标冲突，需权衡）。
3. 重点审查 `presence_backend.rs::detect()` 的 `DataWriter` / `SoftwareBitmap(Gray8)` 尺寸、`faces.GetAt` 边界，以及 `presence.rs` 中 `GrayFrame::cells()`、`Watcher::motion()` 的网格索引（真实帧尺寸来自 `buffer.resolution()`，与解码 RGB 长度需一致）。
4. 修复后重新走 CI（run 模板 `42786053956`）+ 本机至少 30 分钟稳定性观察，再按第七节清单由用户配合完成三项功能实测。
5. 三项功能实测完成后，把结果补写进本节并同步到自己的 fork。

## 九、2026-11-11 下午：Glance 解包分析、误报修复与 v5 功能完善

### Glance 11.19 解包分析

完成只读解包与逆向（未安装、未运行）：787 MB / 973 文件，Qt6 + OpenCV 4.8.1 +
TensorFlow Lite + 私有 NISDK + 加密神经网络模型，依赖外接 RGB-IR 摄像头与私有驱动。
中文 UI 文案加密在 `.xme`/`.date` 中，但**英文配置键、说明文字与 C++ 符号是明文**，
据此还原出三项功能的实现方法。完整证据与映射表见 [GLANCE_ANALYSIS.md](GLANCE_ANALYSIS.md)：

- **身后**：`FEATURE_SHOULDER_SURFING_LEFT/RIGHT` 左右 ROI + 多用户检测 +
  `Surfer IR/RGB Threshold` 分类器置信度阈值 → **不是背景差分**。
- **坐姿**：`..._DEPTH_THRESHOLD_CM` 真实深度 + `..._TIME_TO_WARNING` 持续时间。
- **离席**：`POI_USED_GAZE/HEAD/PRESENCE` + 注意力可用/不可用双延迟 +
  渐变暗屏警告 + snooze + 三种锁屏动作。
- 另有 `FEATURE_DIGITAL_WELLNESS_*`、`FEATURE_AUTO_MUTE_*`、`FEATURE_ADAPTIVE_DIM_*`。

### 两个历史问题的定性

1. **11-10「崩溃退出」**：资源采样日志显示全部 `abnormal=false`，`resources.rs` 无杀进程路径；
   计划任务（svchost 父进程）启动的 v3 连续存活，而工具会话 `Start-Process` 启动的实例
   13–33 秒内死亡。11-11 由计划任务启动的 PID 3032 从 15:07 起**稳定存活超过 1 小时**。
   结论：高置信度为工具会话的子进程生命周期导致，非代码缺陷。
2. **身后误报 130 秒**：旧代码 `evidence = (other_faces > 0 || blob) && !global_change`，
   只要背景差分有运动块就计数，主人打字、耸肩被当成身后人影。已按 Glance 思路改为
   **第二张脸为主证据**，见下。

### v5 改动（提交 62516f2）

| 功能 | 改动 | 模拟的 Glance 能力 |
| --- | --- | --- |
| 身后 | 第二张脸连续确认拍数 + 左右肩后 ROI + 尺寸过滤；无第二张脸一律不提示 | 多用户置信度 + LEFT/RIGHT ROI |
| 坐姿 | `posture_warning_secs` 可配置持续时长、设置页显示当前距离倍率 | TIME_TO_WARNING / CURRENT_DISTANCE |
| 离席 | `snooze` 暂缓（只重置计时不撤防）、`lock_enabled` 仅提示模式、渐强警告进度 | PRESENCE_SNOOZE / LOCK_ACTION / FADE_TO_PERCENT |
| 新增 | Digital Wellness 连续用眼提醒（离席自动重置） | DW_STATISTICS / POSTURE_REMINDER |
| 新增 | Auto Mute 离席静音默认麦克风、回来恢复；只恢复由本程序设置的静音 | FEATURE_AUTO_MUTE |

配置向后兼容旧 `presence.txt`，新增项均有默认值。所有新增逻辑为纯计时 / 现有
WinRT 人脸检测 / 项目已有的 Core Audio 端点封装，**未引入任何新依赖或模型**，
exe 体积保持在约 19 MB。

### 本机实测证据（v3 运行期）

- 稳定性：PID 3032（计划任务于 15:07:17 启动）持续存活，`resources.log` 每 30 秒采样，
  内存约 78 MB、线程 34–37、句柄约 979，全部 `abnormal=false`。
- 离席锁屏：`presence.log` 出现 `[1794384020] lock attempt ok=true`，
  证明摄像头 → 人脸丢失 → 计时 → `LockWorkStation()` 全链路在真机成功执行。
- 身后检测：`behind_enabled=0` 期间无误报；修复版待 v5 安装后重新启用验证。

### 待办

1. v5 CI（run 42984513687）通过 → 下载产物 → 安装 → 重新启用 `behind_enabled=1`。
2. 观察 30 分钟：确认无误报、进程稳定、三项功能可触发。
3. 需要用户配合的实测：坐姿校准、离席 5 秒倒计时 / 60 秒锁屏 / 返回解除、
   第二人从身后经过的 1/3/5 秒分级。
4. Auto Mute 默认关闭，确认后再在设置页开启（会实际操作系统麦克风）。

## 十、2026-11-11 16:24–16:37 无人值守自动验证

用户离开电脑期间自主执行，未改动运行中的程序。

### 上线与环境

| 项目 | 值 |
| --- | --- |
| 安装文件 | `dist/v5-extract/daozai.exe`，19,062,784 B，SHA256 `c6117622…9f26f9` |
| 回滚点 | `dist/daozai-v3-installed-backup-20261111.exe`（19,055,616 B，`e9202f71…`） |
| 启动方式 | 计划任务 `DaozaiDevTest`（svchost 父进程），PID 9820，16:24:11 启动 |
| 摄像头链路 | `camera devices: Integrated Camera (non-IR preferred)` → `FaceDetector ready (MTA)` → `camera opened: Integrated Camera`，无帧错误、无重开 |
| 配置 | 保留用户已校准的 `posture_calibrated=1`、基线 `0.16875/0.3`、`lock_after_secs=25`；`behind_enabled=1`、`wellness_enabled=1`、`auto_mute_enabled=0` |

### 自动测试结果

| 检查项 | 结果 |
| --- | --- |
| CI run `42984513687` | **success**（逻辑测试 / release 构建 / 上传产物全通过） |
| `Windows 检查` run `42984513732` | **success** |
| 12 分钟进程存活 | **通过**：16:25:48–16:36:49 共 12 个采样点，PID 9820 全程存活，线程 33–37 |
| 资源稳定性 | **通过**：`resources.log` 中 pid=9820 共 26 次采样，`abnormal=false` 26 次、`abnormal=true` 0 次；内存约 78–83 MB、句柄 975 |
| 离席自动锁屏 | **通过（真实场景）**：用户离开后 `[1794385636] lock attempt ok=true`，v5 上 25 秒阈值全链路成功；此前 v3 也有一次 `[1794384020] lock attempt ok=true` |
| 上游同步审计 | `git fetch upstream` 后 `upstream/main = f046d6c`，`HEAD..upstream/main` 为 **0 个新提交**，无功能重叠；自研三项功能保留 |

### panic 风险点静态复核（历史待办第 3 条）

`release` profile 为 `panic = "abort"`，任意线程 panic 会静默终止进程，因此复核了在场保护路径：

- `presence_backend.rs::detect()`：所有 WinRT 调用（`DataWriter::new` / `WriteBytes` /
  `DetachBuffer` / `CreateCopyFromBuffer` / `DetectFacesAsync().get()` / `Size()` / `GetAt()` /
  `FaceBox()`）均以 `Result` 早退，无 `unwrap`；`frame.usable()` 与 `fw/fh <= 0` 守卫在先，
  输出坐标统一 `clamp(0,1)`。
- `GrayFrame::cells()`：`gy`/`gx` 用 `.min(GRID_H-1)` / `.min(GRID_W-1)` 保证索引有界，
  `counts[i]==0` 时返回 0.0，无除零。
- `Watcher::motion()`：`background` 为 `None` 时先补齐再 `unwrap`；`global` 判定有
  `counted > 0` 守卫。
- `Watcher::tick()`：两处 `frame.as_ref().unwrap()` 均在 `!paused`（含 `has_frame`）分支内。
- `PresenceUi::banner()` 的 `unreachable!()` 被 `s.behind != Level::None` 静态守卫。

结论：**在场保护路径无未防护的 panic 源**；配合 v3 连续 1 小时 + v5 连续 12 分钟真机存活，
11-10 的「静默退出」可结案为启动方式问题，不再是待办阻塞项。

### 仍需用户回来后确认（无人时无法覆盖）

1. 身后误报是否根除：需主人在座时观察屏幕，确认打字/耸肩不再出现「身后有人」提示。
   代码级回归单测 `motion_alone_never_triggers_behind` 已覆盖，但真机需目视确认。
2. 第二人从身后经过的 1/3/5 秒白黄红分级与点击消除。
3. 坐姿太近提示（用户已校准基线，可复核触发与解除）。
4. Digital Wellness 20 分钟用眼提醒、Auto Mute 真机麦克风静音/恢复（后者默认关闭）。

> 每周同步的 WorkBuddy 定时任务目前仍为 PAUSED；本次以手动审计代替，
> 待用户确认后再恢复自动调度。所有提交只推送 `origin/presence-v2`，未推上游。
