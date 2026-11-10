# 每周上游同步（presence-v2）执行记忆

## 2026-11-10 15:58（首次记录）
- 执行 `scripts/sync-upstream.ps1 -Push`：fetch 成功，但被「工作区不干净」守卫阻断，脚本抛错退出；未合并、未编译测试、未推送。
- 阻断原因：`presence-v2` 的在场保护 v2 改写全部未提交（5 个已跟踪文件被修改 + 8 个未跟踪新文件）。较早的 M1 版本在 `origin/glance-presence-m1`（130e77a）；`origin/presence-v2` 尚不存在。
- 按用户要求停止，未提交/暂存/覆盖/自动解决冲突；仅在 `docs/UPSTREAM_SYNC.md` 追加了一条 BLOCKED 审计记录。
- HEAD == upstream/main（f046d6c），本周上游 27 个提交均已在分支基线内，0 个待合并提交。
- 重叠评估：上游无同类功能。脚本关键词扫描的 `src/fluid.rs`、`src/native.rs` 命中均为误报（流体光效变量 behind / DWM 毛玻璃 API）。保留自研代码，不建议执行 drop-presence.ps1。
- 待办：需用户明确批准后提交 v2 工作 → 重跑脚本 → 运行 cargo check / cargo test presence → 推送 origin presence-v2；另建议修正分支 tracking 与重叠正则。

### 下次运行注意
- 先检查 `git status --porcelain`；若仍为同样的 13 项，说明 v2 仍未提交，会再次被阻断，直接汇报待办即可，不要重复尝试或自行提交。
