# 上游同步策略

定制分支 `presence-v2` = 上游 `Renly1994/xiaotang-daozai:main` + 本仓库的「在场保护」自研功能。
本地 `upstream` remote 指向上游，`origin` 指向自己的 fork `lanyaojin/xiaotang-daozai`。

## 每周自动同步

已配置每周自动任务（每周一 09:00），执行内容：

1. `git fetch upstream` / `git fetch origin`。
2. 运行 `scripts/sync-upstream.ps1 -Push`：把 `upstream/main` 合并进 `presence-v2`，保留自研提交。
3. 扫描上游新增代码与提交说明，判断是否出现同类功能（摄像头、在场感知、坐姿、离席锁屏、身后人影）。
4. 编译验证：**本机没有 MSVC 链接器，脚本会自动跳过本地 `cargo check` / `cargo test`**，改由推送后触发的 GitHub Actions「构建在场保护版本」完成（逻辑测试 + release 构建）。将来装了 MSVC 生成工具后，脚本会自动恢复本地验证。
5. 追加同步报告到本文件末尾，并推送分支；自动任务随后查询最新 workflow run 的结论。
6. 用中文汇报本周上游更新要点、合并结果、验证结果与待办事项。

出现合并冲突时脚本会 `git merge --abort` 并保留现场，**不会自动覆盖**；自动任务会报告冲突文件，等待人工/后续处理。

## 手动同步

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\sync-upstream.ps1 -Push
```

可选参数：`-Repo <路径>`、`-Branch <分支>`、`-SkipBuild`（强制跳过编译验证）、不传 `-Push` 只做本地合并。
脚本会自动检测 MSVC `link.exe`；检测不到时不会调用 `cargo`，避免误用 Git Bash 自带的 GNU `link` 导致编译挂住。

## 上游实现更好时，删除自研功能

判定标准（满足其一即进入评估）：上游新增代码中出现 `nokhwa` / `FaceDetector` / `LockWorkStation` / `presence` / `webcam` 等实现，或提交说明明确提到坐姿、离席锁屏、身后人影，且功能覆盖三个目标并经过验证。

评估由同步报告提示，**默认不自动删除代码**（避免误删）。确认上游更优后执行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\drop-presence.ps1 -Confirm -Push
```

该脚本会删除 `src/presence.rs`、`src/presence_backend.rs`、`src/presence_ui.rs`，并把
`Cargo.toml`、`Cargo.lock`、`src/lib.rs`、`src/app.rs`、`src/extras.rs` 恢复为 `upstream/main`
版本，然后提交。**前提**：定制分支除在场保护外没有其它改动；若有，请先备份。

也可以只停用而不删代码：在设置里关闭总开关，或把 `presence.txt` 的 `enabled` 改成 `0`。

## 冲突热点（合并时优先检查）

| 文件 | 自研改动 | 冲突性质 |
|---|---|---|
| `src/app.rs` | `App` 加 1 个字段、`new` 加 1 行、`frame` 加 1 行轮询、`base_size` 加 3 行、绘制分支加 3 行、`repaint_after`/`notice_hit` 各 1 行 | 纯追加，保留双方即可 |
| `src/extras.rs` | `setting_number` 改为 `pub(crate)` + 常规页加 3 行设置卡片 | 纯追加 |
| `src/lib.rs` | 加 4 行模块注册 | 纯追加 |
| `Cargo.toml` / `Cargo.lock` | `nokhwa` 依赖 + 3 个 windows feature | 上游改依赖时注意去重 |
| 新文件 `src/presence*.rs`、`docs/PRESENCE.md`、`scripts/*.ps1`、workflow | 全新文件 | 不会冲突 |

## 同步报告

以下由脚本自动追加（ASCII，避免 PowerShell 5.1 中文编码问题）。

## 2026-11-10 15:58 BLOCKED (manual automation record)
NOTE: sync-upstream.ps1 -Push aborted at the clean-tree guard. No merge, build or push was run.
This section was appended manually by the weekly automation, not by the script.

- Command: scripts/sync-upstream.ps1 -Push
- Fetch: upstream/main and origin fetched OK (FETCH_HEAD updated 2026-11-10 15:58:09).
- Failure: "Working tree is not clean. Commit or stash first."
- HEAD: f046d6c / upstream/main: f046d6c => 0 new commits; a merge would be "already-up-to-date".
- Modified tracked files (5): Cargo.lock, Cargo.toml, src/app.rs, src/extras.rs, src/lib.rs
- Untracked files (8): .github/workflows/presence-build.yml, docs/PRESENCE.md, docs/UPSTREAM_SYNC.md,
  scripts/drop-presence.ps1, scripts/sync-upstream.ps1, src/presence.rs,
  src/presence_backend.rs, src/presence_ui.rs
- Root cause: the presence-v2 rewrite of the camera "presence protection" feature is still
  uncommitted. Only the older M1 is committed on origin/glance-presence-m1 (130e77a, 2026-11-05).
  origin/presence-v2 does not exist; the custom branch has never been pushed.
- Merge: NOT ATTEMPTED. No force, no auto-resolve, no commit, no stash, no checkout.
- cargo check / cargo test presence: NOT RUN (script stopped before the build stage).
- Push: NOT RUN.
- Upstream commits since 2026-11-03: 27; all already contained in HEAD. Since 2026-11-09: 2
  (PR #95 HTTP proxy HTTPS request format, PR #94 Tencent Exmail capability trailing space).
- FEATURE OVERLAP: NONE. Upstream has no nokhwa / FaceDetector / LockWorkStation dependency,
  no camera-presence module, and no matching commit messages in the last 300 commits.
- Script keyword-scan false positives (informational, not real overlap):
  * upstream/main:src/fluid.rs  -> fluid-light test variable named "behind"
  * upstream/main:src/native.rs -> DwmEnableBlurBehindWindow (DWM glass API)
  * Chinese hits: src/calendar/exchange.rs (Exchange policy explicitly states the app does NOT
    enforce lock-screen password), src/model.rs + src/side_dock.rs ("behind" = desktop behind
    the glass UI), src/native.rs (PtInRegion test).
- DECISION: keep all custom presence code. Upstream cannot be "better" because it has no
  equivalent implementation. scripts/drop-presence.ps1 -Confirm -Push is NOT recommended now.
- TODO: commit the presence-v2 work (requires explicit user approval), then re-run the script.
- TODO: run cargo check --locked --lib and cargo test --locked presence after the tree is clean.
- TODO: push origin presence-v2 (remote branch does not exist yet).
- TODO: branch.presence-v2 currently tracks upstream/main; consider setting it to origin/presence-v2.
- TODO: tighten the overlap regex in sync-upstream.ps1 to exclude fluid.rs / native.rs false hits.

## 2026-11-10 16:40 RESOLVED (manual record)

The BLOCKED report above is historical and fully resolved:

- presence-v2 is committed and pushed (6fe0759, f549915, 4fb0d19, dddc817) and tracks origin/presence-v2.
- "Build presence" GitHub Actions run 42780680446 is green (logic tests + release build + artifact upload).
- daozai.exe (19,048,960 bytes, ProductVersion 0.6.63) downloaded to F:\Software Engineering\dist.
- The overlap regex no longer matches generic "behind"; known false-positive files are filtered.
- Local cargo verification is intentionally skipped (no MSVC linker); CI is authoritative.
- Remaining: real-machine verification of the three features with an actual camera.
