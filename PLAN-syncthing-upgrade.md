# SyncDash 升级计划 —— 对照 Syncthing 源码

> 生成日期：2026-07-26
> 参照源：`.docs/syncthing`（`git clone --depth 1`，commit `119d5e72` / 2026-07-25，Go 1.25，MPL-2.0）
> 本文只做**语义借鉴**，不搬运代码（MPL-2.0 与本项目许可不一定兼容，且 Go 的并发模型与 Rust 差异大，照抄无意义）。
> 现状基线：SyncDash v0.5（`src/` 2933 行），见 [README.md](README.md)。

---

## 0. 方法与阅读范围

实际通读/精读的 syncthing 文件（下文所有引用都指向 `.docs/syncthing/` 内的真实行号）：

| 模块 | 文件 | 关心什么 |
|---|---|---|
| 版本向量 | `lib/protocol/vector.go`（全文 329 行） | 真 N 向同步的数学基础 |
| 冲突判定 | `lib/protocol/bep_fileinfo.go:190-229` | `InConflictWith` / `WinsConflict` / `PreviousBlocksHash` |
| 相等判定 | `lib/protocol/bep_fileinfo.go:454-540` | `FileInfoComparison`（可选忽略 perms/xattr/owner/blocks） |
| 分块 | `lib/scanner/blocks.go`（131 行）、`bep_fileinfo.go:92-102,395-412` | 块级 hash、块大小自适应 |
| 扫描 | `lib/scanner/walk.go:300-400,615-624` | 临时文件清理、路径归一化、忽略下钻 |
| 拉取引擎 | `lib/model/folder_sendrecv.go`（2235 行，重点 241-310 / 952-1250 / 1657-1710 / 1862-1906 / 1960-2090） | 临时文件、块复用、冲突副本、目录删除安全网 |
| 重命名检测 | `lib/model/folder.go:929-989`（`findRename`） | 扫描期本地 rename 归因 |
| 原子写 | `lib/osutil/atomic.go`（134 行） | temp → fsync → rename |
| 版本化 | `lib/versioner/{trashcan,staggered,simple,util}.go` | 回收站保留期、分级稀释 |
| 忽略语法 | `lib/ignore/ignore.go:359-400,500-560` | `!` 取反、`(?i)`、`(?d)`、`#include` |
| 大小写 FS | `lib/fs/casefs.go:1-70` | `CaseConflictError`，写入前真名解析 |
| mtime 虚拟化 | `lib/fs/mtimefs.go:1-80` | FAT/SMB 上写完回读、(ondisk, virtual) 双记 |
| 健康检查 | `lib/config/folderconfiguration.go:37,63,160-196,236,360-375` | `.stfolder` 标记、`minDiskFree` |
| 监视聚合 | `lib/watchaggregator/aggregator.go:21-25,193-260` | 防抖、`maxFiles=512` 退化全扫 |
| 只收模式 | `lib/model/folder_recvonly.go:69-120` | `Revert` 语义 |

---

## 1. 先说清楚：两者不是一类东西

这决定了哪些能抄、哪些抄了是自残。

| | Syncthing | SyncDash |
|---|---|---|
| 形态 | 常驻 daemon + Web UI，P2P 网状 | CLI + Tauri 桌面，显式触发 |
| 传输 | 自有协议 BEP over TLS，含发现/中继/NAT 穿透 | 借道 SMB 挂载盘 / ssh / tar 包 |
| 状态 | 每 folder 一个索引数据库（sequence / 版本向量 / 块表） | JSONL 快照表 + 可选 archive 表 |
| 拓扑 | 真 N 向，任意设备对等 | 双边为主，1 master 对 N slave 手工展开 |
| 冲突 | 自动落地 + `.sync-conflict-*` 副本 | 报告，不自动仲裁 |
| 可审计 | 需要读 db / 看日志 | **计划就是一个可 diff 的文本文件** |
| 何时动手 | 后台连续，用户看不见中间态 | **默认 dry-run，人点了才动** |

**SyncDash 的核心卖点是"可预览、可审计、可管道"，这一条不能为了对齐 syncthing 而牺牲。**
所以下面凡是要求"常驻""隐式""黑盒"的能力，一律降级为可选模式，绝不改默认行为。

---

## 2. 差距清单

### P0 —— 正确性/数据安全，应当立刻做

#### P0-1. 落盘不是原子的（**真数据丢失路径**）

**syncthing**：所有写入都先写 `.syncthing.tmp.<name>`，Close 时 `Sync()` → `Rename()` 到最终名
（`lib/osutil/atomic.go:19,45-90`；temp 名由 `fs.TempName` 生成，扫描时被 `fs.IsTemporary` 跳过，
超过 `TempLifetime` 自动清理 —— `lib/scanner/walk.go:314-320`）。

**SyncDash**：[src/apply.rs:140](src/apply.rs:140) 直接 `std::fs::copy(&src, &dst)` 写进最终路径。

**具体的坏结局**（不是理论风险，SMB 上大文件被打断是常态）：

> sync 模式 + archive。target 上 `a.psd` 正在被 Update：旧版已进 trash（可恢复），
> 新版写到一半断网 → target 留下一个截断文件，mtime 是新的。
> 下一轮 compare：source 对 archive 未变（`s_unchanged=true`），target 对 archive 变了（`t_unchanged=false`）
> → 走 [src/compare.rs:352](src/compare.rs:352) 的 `"target-changed"` 分支
> → **生成 `Update source` —— 把截断文件反向覆盖到 source 上**。
> 源侧那份好文件就没了（target 的旧版还在 trash 里，但没人会想到去那儿找）。

**怎么改**：
- `apply.rs` 加 `write_atomic(src, dst, mtime, expect_hash)`：
  写 `<dir>/.syncdash.tmp.<basename>.<pid>` → （verify 时在 temp 上校验 hash）→ `set_mtime` → `fs::rename` 覆盖。
- 同卷 rename 是原子的；跨卷不会发生（temp 与 dst 同目录）。
- `filter.rs` 的内置排除加上 `.syncdash.tmp.*`，避免临时文件被扫进快照表。
- 扫描时遇到 `.syncdash.tmp.*` 且 mtime 超过 24h → 顺手删掉（对齐 syncthing 的 `TempLifetime`）。
- `pack.rs` 的 staging 已经是"全验完才动 target"，但最后一步落盘同样要走原子写。

**验收**：新增测试 —— 模拟写到一半失败（注入错误的 writer），断言 dst 要么是旧内容、要么不存在，绝不是半截。

---

#### P0-2. 共享盘没挂上 = 生成"删光对面"的计划（**灾难性误判**）

**syncthing**：每个 folder 根下必须有标记目录 `.stfolder`（`DefaultMarkerName`，
`lib/config/folderconfiguration.go:37,160-196`）；`CheckPath` 在 `:236` 校验它存在，
不存在就把 folder 置为错误状态、**拒绝任何同步**。这条规则存在的唯一理由就是防"挂载点没挂上"。

**SyncDash**：没有任何等价检查。`target = '\\192.168.0.115\xuanbomiao\...'`，
Mac 关机或 SMB 掉线时该路径可能是空目录（或被本地自动创建）→
mirror 模式 compare 会生成"复制全部过去"（几十 GB 白传），
反向 job 则生成"删除 target 全部"。`run --apply` 会照做。

**怎么改**：
- 复用已有的 `.ffs-sync` 领地标记概念（[src/territory.rs:11](src/territory.rs:11)），新增 `syncdash mark <root>` 写 `.syncdash-root`（内含 job 名 + 创建时间）。
- job 配置加 `require_marker = true`（**新 job 默认开**，老 job 兼容默认关，一个 release 后翻转）。
- `scan` 阶段就检查：root 存在 + marker 存在 + root 非空（三选一失败就拒绝出表，而不是出一张空表）。
- 额外护栏（syncthing 没有，但我们的显式模型很适合）：**计划体检**——
  单次计划里 `delete` 数量 > target 总条目数的 N%（默认 50%）时，`run` 拒绝 `--apply`，要求 `--i-know`。
  这条比 marker 更普适，能顺带拦住过滤器写错、路径打错。

**验收**：测试 —— target 指向空目录且无 marker 时，mirror compare 返回错误而非 delete 计划。

---

#### P0-3. 没有磁盘空间预检

**syncthing**：`CheckAvailableSpace(req uint64)`（`lib/config/folderconfiguration.go:360-375`），
默认 `MinDiskFree = "1 %"`（`:63`）；处理待拉取文件前查一次（`lib/model/folder_sendrecv.go:495`），
versioner 归档前再查一次（`:1052`）。

**SyncDash**：`grep -rn "free_space\|disk" src/` 无结果。计划里明明已经有每个 op 的 `size`，
汇总一下就知道要写多少字节，却从不检查。写满目标盘的后果在 SMB 上尤其难看（对面系统盘写满）。

**怎么改**：
- 依赖 `sysinfo` 或 `fs2`（或 Windows `GetDiskFreeSpaceExW` / unix `statvfs` 各写十行，免依赖）。
- `apply` 开始前：`需要字节 = Σ(copy/update 的 size)`，加 10% 余量 + `min_free`（默认 1%，job 可配）。
- 不足 → 直接拒绝，报"需要 X，可用 Y，缺 Z"。
- trash 也要算：Update/Delete 会先把旧文件挪进 trash，同卷 rename 不占空间，跨卷会。

---

#### P0-4. 目录删不掉时静默吞掉

**syncthing**：`deleteDirOnDiskHandleChildren`（`lib/model/folder_sendrecv.go:1985-2085`）
把"删不掉"细分成四种并**分别报错**：`errDirHasToBeScanned`（有 db 不认识的东西 → 排一次扫描）、
`errDirHasIgnored`（里面有被忽略的文件 → 永远删不掉，必须用户处理）、
`errDirNotEmpty`（有 db 认识且合法的文件 → 状态不一致，值得警惕）、
以及只删 `(?d)` 标记的可删除项与临时文件。

**SyncDash**：[src/apply.rs:187](src/apply.rs:187) —— `Err(_) => Ok(())`，注释写着
"非空（里面还有被排除的文件等）：保留，不视为错误"。**行为是安全的，但用户永远不知道发生了什么**：
mirror 跑完显示 0 错误，可对面那个目录还在，下次比对又出现同一条 DeleteDir，永远收敛不了。

**怎么改**：
- 不改删除策略（继续不递归删，这是对的），改**汇报**：
  区分 `NotFound`（真删了/本来就没有 → 静默）、`DirectoryNotEmpty`（列出前 5 个残留项名字，计入 skipped 并打印原因）、其他错误（计入 errors）。
- 残留项如果全部命中当前 job 的 exclude → 提示"被过滤器保护，这是预期行为，可用 `--prune-excluded` 一并清理"。
- 收敛性：`run` 结尾的自动复比对如果发现同一条 DeleteDir 再次出现，明确标注"上轮未能删除"。

---

### P1 —— 高价值能力，值得排进 v0.6 / v0.7

#### P1-1. 块级传输（delta sync）—— 最大的性能杠杆

**syncthing**：文件切成块，每块单独 SHA-256（`lib/scanner/blocks.go:42-120`）。
块大小自适应：从 128 KiB 起翻倍到 16 MiB，目标是每文件约 `DesiredPerFileBlocks` 块
（`lib/protocol/bep_fileinfo.go:92-102,402-412`）。
拉取时先 `blockDiff` 算出"我已经有哪些块"，只传缺的（`folder_sendrecv.go:1132-1250`）；
还会从**同机的其它 folder** 里找相同块直接本地复制（`copyBlockFromFolder:1435`），
以及从上次没传完的临时文件里捡剩块（`reuseBlocks:1173-1250`）。

**SyncDash**：整文件复制。改一个 40 GB VM 镜像的一个扇区 → 传 40 GB。
README 里 rsync 算法被列进"v2 备选"，但块级 hash 其实比 rsync 的滚动校验简单得多，
而且**我们已经在扫描时读全文件算 blake3 了 —— 顺手切块几乎零额外成本**。

**怎么改**（分两步，第一步就能吃到大部分收益）：

- **步骤 A（v0.6）：块清单进表**
  - `Entry` 加两个可选字段：`bs`（块大小）、`bh`（`Vec<String>`，块 hash 列表）。
    只对 `size >= 8 MiB` 的文件产出，避免小文件把表撑爆。
  - 块大小自适应照抄 syncthing 的思路：`128 KiB` 起翻倍，目标每文件 ≤ 2000 块。
  - **表体积问题**：一个 40 GB 文件按 16 MiB 块 = 2560 个 hash × 64 hex ≈ 164 KB。
    对策：块清单**不进主表**，另存 sidecar `<table>.blocks.jsonl`（一行一个文件的块清单），
    主表只留 `bh_ref`（sidecar 里的行号）+ `blocks_hash`（整个块列表的 blake3，用于快速判等）。
    ssh 管道场景不需要 sidecar 时可 `--no-blocks` 关掉。
  - hash 缓存（`scan.rs:56-90`）跟着扩展：缓存行加块清单，`(path,size,mtime)` 未变则整份复用。

- **步骤 B（v0.7）：apply / pack 用上块**
  - 本地/挂载盘：`Update` 时打开 dst，逐块比对 —— 相同则跳过，不同则 seek+写。
    配合 P0-1 的原子写：先把 dst 复制成 temp（或用 `CopyFileRange`/`copy_file_range`/`FSCTL_DUPLICATE_EXTENTS`，
    syncthing 的 `lib/fs/basicfs_copy_range*.go` 有全平台实现清单可参照），在 temp 上打补丁，再 rename。
  - `pack`：payload 只装差异块 + 块偏移清单，包体积可能缩小一到两个数量级。
    manifest 加 `base_blocks_hash`，对端 apply 前先验"我这边的基线确实是你以为的那份"，不匹配就退回整文件。
  - 统计口径抄 syncthing 的 `blockStats`（total / reused / renamed / pulled），
    GUI 上直接显示"实传 X MB / 逻辑 Y MB"。

**验收**：40 GB 文件改 1 MB → 实传 < 32 MB；块清单 sidecar 让主表体积增长 < 5%。

---

#### P1-2. 冲突副本：让 sync 能自己往前走

**syncthing**：冲突时不停在原地。败方被改名成
`name.sync-conflict-20060102-150405-<DEVICEID>.ext`（`folder_sendrecv.go:2219-2222`），
胜方正常落地。`WinsConflict`（`bep_fileinfo.go:212-229`）的仲裁顺序是：
非 invalid > mtime 新 > 版本向量的 device id 做 tie-break。
`MaxConflicts` 限制每个文件保留几份，超了删最老的（`:1888-1898`）；
已经是冲突副本的文件不再产生冲突副本（`:1863`）。

**SyncDash**：冲突只报告（[src/compare.rs:356](src/compare.rs:356) 等），GUI 里锁定不可勾选。
安全，但**双机日常使用时一个冲突会卡住这个文件直到人工干预**，而人往往不干预。

**怎么改**：
- job 加 `on_conflict = "report" | "copy" | "newer"`，**默认仍是 `report`**（不改现有语义）。
- `"copy"`：compare 产出两条 op —— 先 `Move` 败方到 `name.sync-conflict-<YYYYMMDD-HHMMSS>-<host>.ext`，
  再 `Copy` 胜方过去。仲裁规则照抄 syncthing：mtime 新者胜，相同则 host 名字典序（我们没有 device id，用 hostname 做稳定 tie-break）。
- 冲突副本本身要进内置排除的"不参与移动检测"名单（否则会被 detect_moves 配成 move）。
- `max_conflicts`（默认 5，-1 = 不限，0 = 关）+ 清理最老的。
- GUI：冲突行加一个"生成副本"按钮，等价于逐行开启一次。

---

#### P1-3. 冲突误报太多 —— 借 `PreviousBlocksHash` 的思路

**syncthing**：版本向量并发 **≠** 一定冲突。`InConflictWith`（`bep_fileinfo.go:190-208`）里有个漂亮的逃生口：
如果新文件的 `PreviousBlocksHash` 等于我本地当前的 `BlocksHash`，
说明**对方就是基于我这份内容改的**，不是并发修改，直接放行。
（这正是 README 引用的 [PR#10351](https://github.com/syncthing/syncthing/pull/10351) 的改进。）

**SyncDash**：archive 模型下，只要两侧都与 archive 不同就报 `both-changed`
（[src/compare.rs:356](src/compare.rs:356)）。但有一类常见情形被误伤：
**A 侧改了，同步过去了，但 archive 刷新失败（比如中途 Ctrl-C）** →
下轮两侧内容其实相同…… 这个 `files_equal` 已经挡住了。
真正被误伤的是：**A 改了 → 传到 B → B 又基于这份改了 → archive 还停在两代之前** → 报 both-changed，
而其实是一条干净的线性历史。

**怎么改**：
- archive 表升级为**多代**：`archive.jsonl` 保留最近 K 代（默认 3）的条目 hash 集合（只存 `path → [hash]`，很小）。
- 判定放宽：若 target 当前 hash 出现在该 path 的历史 hash 集合里 → target 只是"落在某个历史版本上"，不算并发改 → 单边传播而非冲突。
- 这是 archive 模型下对版本向量的**廉价近似**，不需要引入 device id，成本极低。
- 与 P2-1（真版本向量）不冲突，是它的前置台阶。

---

#### P1-4. mtime 精度：写完要回读

**syncthing**：`mtimeFS.Chtimes`（`lib/fs/mtimefs.go:68-80+`）设完时间后**立刻 stat 回来**，
把 `(ondisk, virtual)` 一起存进 db，之后对外一律报 virtual。
这样 FAT（2 秒粒度）、exFAT、某些 SMB 服务端的时间截断就完全不影响判等。
比较时另有 `ModTimeWindow` 作为兜底（`bep_fileinfo.go:455`）。

**SyncDash**：[src/apply.rs:49](src/apply.rs:49) `set_mtime` 后不回读，
靠 [src/compare.rs:22](src/compare.rs:22) 的 `MTIME_SLACK_MS = 2000` 硬容差。
在 standard/paranoid 级别有 hash 兜底所以问题不大，但 **`rigor = "quick"`（不 hash）时容差就是唯一判据**：
2 秒容差既可能漏判（真改动在 2 秒内）也可能误判（SMB 偏移 > 2 秒）。

**怎么改**：
- 复制后 `set_mtime` → `metadata()` 回读 → 若不等，把 `(ondisk, intended)` 记进
  hash 缓存文件（已有的 `hashcache/*.jsonl` 加两列即可，不需要新数据库）。
- 下次 scan 读到某文件 mtime 恰等于缓存里的 `ondisk` → 对外报 `intended`。
- 容差保留做兜底，但可以收窄到 1s，并允许 job 配 `mtime_window`（FAT 卷设 2s，NTFS↔APFS 设 0）。

---

#### P1-5. 过滤器加 `!` 取反

**syncthing**：单一 `.stignore` 文件，前缀修饰 `!`（取反/白名单）、`(?i)`（大小写不敏感）、
`(?d)`（该项可被删除，删父目录时不算阻塞）、`#include`（引入另一个文件）
（`lib/ignore/ignore.go:359-400,500-560`）。规则**从上到下首个命中生效**。

**SyncDash**：[src/filter.rs](src/filter.rs) 是 FFS 语义的 include/exclude 两张单。
兼容 FFS 是个真实优势（FFS 的排除列表能原句粘过来），不该丢。
但"排除 `*.log` **但保留** `deploy/important.log`"这类需求，两张单表达起来很别扭。

**怎么改**：
- 在 exclude 列表里支持 `!` 前缀作为**例外**（命中 `!` 规则的路径直接放行，不再看后续 exclude）。
  这是 FFS 语法的超集，粘过来的 FFS 规则行为完全不变。
- 借 `(?d)` 的语义：加一类 `deletable = ['*/node_modules/']`——被它命中的东西
  不参与同步，但删父目录时**允许连带删掉**（直接解掉 P0-4 里"目录永远删不掉"的死结）。
- `#include` 暂不做（我们的配置是 TOML，可以直接用数组拼接）。

---

### P2 —— 有价值但成本高或收益局部

#### P2-1. 真 N 向：版本向量

**syncthing**：`lib/protocol/vector.go` 全文 300 行，实现干净得可以直接当教科书：
- `Counter{ID: ShortID, Value: u64}`，按 ID 有序数组（不是 map，比较时双指针线性扫）。
- `Update`：`value = max(old+1, unix_now)` —— 用时间戳保证即使计数器被回滚也单调。
- `Compare` 返回五态 `Equal / Greater / Lesser / ConcurrentGreater / ConcurrentLesser`
  （后两个不是真的"并发大小"，只是为了给排序一个稳定序）。

**移植到 SyncDash 的代价**（这才是关键）：
- 需要稳定的节点 ID（可以是 `hostname + 首次运行时生成的 UUID`，存本机配置）。
- archive 要从"上次快照"升级成"带版本向量的索引库"——每个 path 一行 `(hash, size, mtime, version_vector)`。
  仍然可以是 JSONL（保持可读可审计），但语义变了：它不再是"某次扫描的快照"，而是"我所知的全局状态"。
- 每次成功 apply 后要更新向量，**且要区分"我改的"和"我从别人那儿收的"**（前者 `Update(me)`，后者 `Merge(peer)`）。
- N 个节点两两比对会退化成 O(N²)；syncthing 靠 P2P gossip 解决，我们靠 hub-and-spoke 也够。

**结论**：这是 v1.0 级别的改造，**不该在 v0.6/v0.7 做**。
README 的判断（hub-and-spoke 先够用）是对的。真要做时，`vector.go` 的语义可以 1:1 用 Rust 重写，
大约 200 行 + 一套 property test（`Compare` 的自反/对称/传递性质很适合 proptest）。
前置台阶是 P1-3 的多代 archive。

---

#### P2-2. 回收站会无限膨胀

**syncthing**：三种 versioner。`trashcan` 按 `cleanoutDays` 清理过期
（`lib/versioner/trashcan.go:57-100`）；`staggered` 做**分级稀释**
（`lib/versioner/staggered.go:47-53,63-110`）——
第一小时每 30 秒留一份、当天每小时一份、30 天内每天一份、之后每周一份，直到 `maxAge`；
`simple` 按份数保留。删完还会清空目录（`empty_dir_tracker.go`）。

**SyncDash**：[src/apply.rs:27](src/apply.rs:27) —— `trash/<时间戳>/`，**从不清理**。
每次 apply 一个新目录。日常跑几个月后 `%LOCALAPPDATA%\syncdash\trash\` 会很可观，
而且几百个时间戳目录里找一个文件基本靠运气。

**怎么改**：
- job / 全局配置加 `trash_keep_days`（默认 30）、`trash_max_bytes`（默认 10 GiB）。
- `syncdash trash list|restore|prune` 三个子命令：
  `list <path-glob>` 跨所有时间戳目录找同一个文件的历史版本（这才是回收站的正确用法）；
  `restore <path> [--at <ts>]`；`prune` 按上面两个上限清理，顺手删空目录。
- `run` 结束时机会性调用一次 prune（syncthing 是定时器驱动，我们没有常驻进程，就搭在 run 上）。
- `staggered` 的稀释算法值得直接照搬语义 —— 它比"保留 N 天"聪明得多，且实现只有 40 行。

---

#### P2-3. 大小写不敏感 FS 的写入保护

**syncthing**：`caseFilesystem` 在写入前解析目录里的真实拼写，
发现 `Foo.txt` 与已存在的 `foo.txt` 只差大小写 → 返回 `CaseConflictError`
（`lib/fs/casefs.go:27-37`），带 1 秒 TTL 的 LRU 目录名缓存避免每次 readdir。

**SyncDash**：**比对阶段**做了大小写折叠（[src/compare.rs:121](src/compare.rs:121)，做得对），
但 **apply 阶段**没有防护：`case_sensitive = true` 的 job 在 NTFS 上执行
"复制 `Foo.txt`"时会静默覆盖已存在的 `foo.txt`。属于自找的边角，但确实是静默数据丢失。

**怎么改**：apply 创建新文件前，若执行侧 FS 大小写不敏感（可探测：在 root 建个临时文件再用变体大小写 stat）
且计划里同目录存在只差大小写的另一路径 → 转成 Conflict。
成本低，加在 P0-2 的"计划体检"里一起做。

---

#### P2-4. 元数据变更没有对应的 op

**syncthing**：`shortcutFile`（`folder_sendrecv.go:1253`）——
只有权限/mtime 变化时不传内容，只改元数据。
`FileInfoComparison`（`bep_fileinfo.go:454-462`）允许按需忽略 perms / xattr / ownership / blocks，
每个维度都能独立开关。

**SyncDash**：[src/table.rs:44](src/table.rs:44) 记了 `mode`，但
[src/compare.rs:126](src/compare.rs:126) 的 `files_equal` 只看 hash / size / mtime —— **`mode` 记而不用**。
Mac 上给脚本加了 exec 位，同步过去还是没有。
（`pack` 路径会恢复 mode，挂载盘路径不会 —— 行为不一致。）

**怎么改**：
- 新增 `Action::Chmod`（携带目标 mode），仅当两侧都是 unix 且 `sync_mode = true` 时产出。
- job 加 `sync_mode = false`（默认关，因为 Win↔Mac 场景下 Windows 侧没有 mode，开了会一直报差异）。
- 两侧 OS 都是 unix 时可默认开。
- 同时统一 pack 与直连两条路径的 mode 行为。

---

#### P2-5. 空文件/同内容文件的移动配对是任意的

**syncthing**：`findRename`（`lib/model/folder.go:930-932`）**第一件事就是**
`if len(file.Blocks) == 0 || file.Size == 0 { return false }` —— 空文件不参与重命名归因。

**SyncDash**：[src/compare.rs:183](src/compare.rs:183) 的 `detect_moves` 按 `(hash, size)` 建桶。
所有空文件 size=0、blake3 相同 → 挤进同一个桶；仓库里大量相同的 `__init__.py`、`LICENSE`、`.gitkeep` 同理。
配对结果**内容上仍然正确**（收敛没问题），但 `from` 字段是任意挑的，
"rename-detected-by-hash" 这个 reason 就成了噪音——而计划的可读性正是 SyncDash 的立身之本。

**怎么改**：桶内候选 > 1 时，若 `size == 0` 直接放弃配对（退回 copy+delete）；
非空但多候选时，现有的"同父目录 → 同文件名 → 任意"三级优先已经够，只是要在 reason 里
如实写成 `move-detected-by-hash (ambiguous: N candidates)`，不要假装确定。

---

#### P2-6. 扫描进度

**syncthing**：`ProgressTicker` + `byteCounter`，定期发 `FolderScanProgress` 事件，
带 current/total 字节与 MiB/s（`lib/scanner/walk.go:55-62,148-200`）。

**SyncDash**：Tauri 前端有"扫描 source → 扫描 target → 比对中"三段状态，但没有百分比/速率。
大树（几十 GB）扫描时用户不知道是卡了还是在跑。

**怎么改**：`scan()` 加可选 `progress: Option<&dyn Fn(ScanProgress)>`；
阶段 1 遍历完就知道总字节数，阶段 2 的 rayon 并行哈希里用 `AtomicU64` 累加已哈希字节，
每 500 ms 回调一次。Tauri 侧接到事件更新进度条 —— 前端已有事件通道，改动很小。

---

### 明确**不抄**的

| syncthing 的东西 | 为什么不抄 |
|---|---|
| BEP 协议 / TLS / 设备发现 / 中继 / NAT 穿透 | SyncDash 刻意走 ssh + SMB + tar 包。少一整个网络栈是特性不是缺陷 |
| 常驻 daemon + Web UI + REST API | 已有 Tauri 桌面版；常驻会破坏"默认 dry-run、人点才动"的核心承诺 |
| 索引数据库（sequence / LevelDB / SQLite） | JSONL 表可读、可 diff、可管道，是卖点。块清单用 sidecar 解决体积问题即可 |
| 加密文件夹（`folder_recvenc.go`） | 无对应场景（内网 SMB + ssh） |
| 文件系统监视（inotify / ReadDirectoryChangesW） | 见下 |
| 只收文件夹的 `Revert`（`folder_recvonly.go:69`） | 我们的 mirror 模式 + 逐行翻方向已经覆盖同类需求 |

**关于文件监视**：`lib/watchaggregator/aggregator.go` 的聚合策略很值得读
（防抖、按目录合并、`maxFiles=512` / `maxFilesPerDir=128` 超了就退化为扫整个目录，`:21-25,193-260`）。
但引入 watcher 意味着 SyncDash 变成常驻进程，与"显式触发、可预览"的定位冲突。
**建议做法**：v0.8 加一个**可选**的 `syncdash run <job> --watch`，
watcher 只负责**触发一次 compare 并把结果推给 GUI**，绝不自动 apply。
这样既拿到"改完立刻看到差异"的体感，又不放弃人工确认这道闸。

---

## 3. 版本规划

### v0.6 —— 安全网（P0 全做完，不加新能力）

- [ ] P0-1 原子写：temp + fsync + rename；`.syncdash.tmp.*` 进内置排除；过期临时文件自动清
- [ ] P0-2 root marker（`.syncdash-root`）+ `require_marker` + 计划体检（删除占比阈值）
- [ ] P0-3 磁盘空间预检（含 trash 跨卷的情况）
- [ ] P0-4 DeleteDir 失败分类汇报，不再静默
- [ ] P2-5 空文件不参与移动配对；歧义配对在 reason 里如实标注
- [ ] P2-3 apply 阶段的大小写冲突预检（并入计划体检）
- [ ] 回归：现有 20 项 compare 矩阵测试全绿 + 新增原子性/marker/空间三组测试
- [ ] 真机验证：Win → SMB → Mac 全流程，含**故意中断复制**后的恢复行为

**这一版一行新功能都不加，专门补数据安全。P0-1 和 P0-2 是真会丢数据/白传几十 GB 的。**

### v0.7 —— 块级传输 + 冲突自动化

- [ ] P1-1 步骤 A：块清单 sidecar + 自适应块大小 + hash 缓存扩展
- [ ] P1-1 步骤 B：apply 差异块写入（配合原子写）；`pack` 只装差异块 + `base_blocks_hash` 校验
- [ ] P1-2 冲突副本（`on_conflict` / `max_conflicts`），GUI 加"生成副本"按钮
- [ ] P1-3 多代 archive（K=3）降低冲突误报
- [ ] P2-6 扫描进度事件（字节数 + 速率）
- [ ] 基准：40 GB 文件改 1 MB，实传 < 32 MB；主表体积增长 < 5%

### v0.8 —— 打磨与运维

- [ ] P1-4 mtime 回读校正 + 可配 `mtime_window`
- [ ] P1-5 过滤器 `!` 取反 + `deletable` 类别
- [ ] P2-2 回收站保留期 / 体积上限 + `trash list|restore|prune`
- [ ] P2-4 `Action::Chmod` + 统一 pack 与直连的 mode 行为
- [ ] 可选 `run --watch`（只触发比对，绝不自动 apply）
- [ ] 原 roadmap 遗留：同目录 rename 合并显示、GUI 任务编辑、`run --all`、ssh 一条龙

### v1.0 —— 真 N 向（大改造，独立评估后再启动）

- [ ] 节点 ID + 版本向量（照 `vector.go` 语义用 Rust 重写 + proptest）
- [ ] archive 升级为带版本向量的索引库
- [ ] N 节点收敛性测试（模拟 3-5 节点随机改动 + 随机同步顺序，断言最终一致）

---

## 4. 表结构（schema）演进

现在 `SCHEMA = 1`（[src/table.rs:8](src/table.rs:8)）。上面的改动会动到表，规划如下：

```
schema 2（v0.7）
  Entry  += bs: Option<u32>            块大小
         += bh_ref: Option<u64>        块清单在 sidecar 中的行号
         += blocks_hash: Option<String> 整个块列表的 blake3（快速判等）
  Header += blocks_sidecar: Option<String>  sidecar 文件名
         += node_id: String            为 v1.0 预留，现在填 hostname 即可

  新文件 <table>.blocks.jsonl —— 一行一个文件：{"path":..,"bs":..,"blocks":[..]}

schema 3（v1.0）
  archive 从 "snapshot" 变成 "index"：每行 {path, hash, size, mtime, version: Vector}
```

**兼容策略**（`pack`/`apply-pack` 跨版本尤其要紧）：
- 读端：`schema <= 我支持的最高版本` 就接受，未知字段 serde 忽略（现有结构已有 `#[serde(default)]` 习惯，继续保持）。
- 写端：只在双方 `probe` 都报告支持时才产出新字段 —— `probe` 已经在报 schema
  （[src/main.rs](src/main.rs) 的 `Cmd::Probe`），扩展成报 `schema_max` 即可。
- `apply-pack` 遇到高于自己的 schema → **明确拒绝并提示升级**，不要试图猜。

---

## 5. 一句话优先级

> **v0.6 之前不要碰任何新功能。**
> 原子写（P0-1）和挂载点检测（P0-2）各自都能造成真实的数据损失/几十 GB 白传，
> 而两者加起来的实现量还不到一天。块级传输很诱人，但它是性能优化 —— 在一个会把
> 截断文件反向覆盖到源侧的系统上做性能优化，顺序错了。
