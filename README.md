# SyncDash (`syncdash`)

表驱动的多端文件同步 CLI。三段式：**scan（产表）→ compare（比表出计划）→ apply（执行计划）**，
每一段的输入输出都是人类可读的 JSONL 表——表就是接口，ssh 管道、存档、审计都用同一份格式。

为什么不继续用 FFS / rsync / Unison：

| 痛点 | 现有工具 | SyncDash |
|---|---|---|
| 移动的文件被当作 删+增 | FFS（跨机时）、rsync | ✅ (hash,size) 配对生成真 `move` |
| 多端（1 master 对 N slave、三边表） | 都不支持或很勉强 | ✅ 表是一等公民，N 张表 N 份计划 |
| 远端"打包过去、对面校验后执行" | 无 | v0.4：zip + 清单 + 双 hash + 对端校验 |
| 比对依据可检查、可留档 | 黑盒 | ✅ 快照表/计划表都是 JSONL 文件 |

## 命令

```bash
syncdash probe                                   # 本机环境 JSON（远端探测：ssh 对面跑这个）
syncdash scan <root> [--out t.jsonl] [--no-hash] [--exclude NAME]...
syncdash compare --source a.jsonl --target b.jsonl \
    [--mode mirror|sync|enrich] [--archive last.jsonl] [--resolve-newer] [--out plan.jsonl]
syncdash apply plan.jsonl [--apply] [--source-root R] [--target-root R] [-v]
```

- `scan` 默认写 stdout（ssh 友好：`ssh mac syncdash scan ~/Data > mac.jsonl`）。
- `apply` **默认 dry-run**，`--apply` 才动手；删除/覆盖的文件先进本机
  `%LOCALAPPDATA%\syncdash\trash\<时间戳>\`（mac: `~/.cache/syncdash/trash/...`），不原地销毁。
- hash 是 BLAKE3（mmap+rayon，多核），带缓存：`(path,size,mtime)` 没变就复用上次结果，
  缓存放本机用户目录，绝不污染被扫描的目录。

## 三种模式的语义

| | 增（对面缺） | 改（两边不同） | 删（对面多出） | 移动 |
|---|---|---|---|---|
| **mirror**（source=master） | ✅ 补到 target | ✅ master 无条件赢 | ✅ 删 target 多余 | ✅ target 上就地 move |
| **sync**（双向） | ✅ 双向补 | archive 归因单边改→传播；双边改→**冲突** | 有 archive 才删（区分"删除 vs 新增"） | ✅ 单边移动在另一边重演 |
| **enrich**（只增不删） | ✅ 补到 target | source 严格较新才更新 | ❌ 永不 | ❌（move 含删，违背只增） |

相等判定：双方都有 hash → hash 相等；否则 size 相等且 |Δmtime| ≤ 2s（FAT/SMB 时间粒度）。

### sync 与 archive（Unison 思路）

“对面没有这个文件”天然有两种解释：**它是我新增的**（该复制过去）还是**对面删掉的**（该跟着删）？
唯一可靠的判据是**上次成功同步时的状态存档**（archive）——Unison 与 FFS 的数据库同理：

- 存档里有、target 没有、source 没改过 → target 删了 → 传播删除；source 改过 → **删改冲突**
- 存档里没有 → 新增 → 复制
- 两边都和存档不一样 → **冲突**，绝不自动仲裁（除非显式 `--resolve-newer`）

**无 archive 时 sync 自动降级为安全模式**：只做双向补齐，差异报冲突，疑似移动只报告（`possible-move-needs-archive`）——宁可少做，不做错事。
存档就是一张普通快照表：**同步成功后重扫任一侧存下来，下次 `--archive` 传入**（v0.3 会自动化这一步）。

### 移动检测（治 FFS 的删+增）

比对后，把"待复制清单"与"待删除清单"按 `(hash, size)` 配对：同一内容从旧路径消失、在新路径出现 → 生成 `move` op（优先配对同文件名，应对整目录改名）。实测：

```
{"side":"target","action":"move","path":"moved/old_name.dat","from":"old_name.dat","reason":"move-detected-by-hash"}
```

target 上一次 `rename` 完事，大文件零重传。`--no-hash` 扫描时自动退回复制+删除（并放弃移动检测）。

## 远端模式（v0.4 设计，未实现）

1. `ssh <host> syncdash probe` —— 探测对面：OS/arch/版本/schema；二进制不存在则给出安装指引
   （两台机器都有 Rust 工具链，`cargo build --release` 即可；或经共享盘直接拷二进制）。
2. `ssh <host> syncdash scan <root>` —— stdout 收表。
3. 本地 `compare` 出计划。
4. `syncdash pack plan.jsonl --out pkg.zip`：**待写入文件 + 计划（含删除清单）+ 数据区 hash + 计划 hash**。
5. 传输（scp / 共享盘均可）后 `ssh <host> syncdash apply-pack pkg.zip`：先验两个 hash，再按计划逐步执行，同样默认 dry-run。

Win↔Mac 的 SSH 已验证可用（Mac 22 端口开着，免密只差把公钥写进 authorized_keys）。

## 多端（v0.5 方向）

- 1 master 对 N slave：master 表分别与每张 slave 表比对 → N 份计划（现在就能手动做）。
- 真 N 向 sync 需要版本向量（Syncthing 思路）；hub-and-spoke（Win01 当 hub）在此之前够用。

## 与 CodeSync（FFS）的关系

先并行：FFS 继续管日常，SyncDash 拿 `.ffs-sync` 领地练手（`Update-CodeSyncConfig.ps1` 的标记扫描
将来可以直接改产 syncdash 的领地清单）。行为可信之后再接管。

## 算法调研来源

- Unison 形式化规范与 archive 模型：[Balboa/Pierce, "What's in Unison?"](https://www.researchgate.net/publication/32205844_What's_in_Unison_A_Formal_Specification_and_Reference_Implementation_of_a_File_Synchronizer)、[Unison: A File Synchronizer and Its Specification](https://link.springer.com/chapter/10.1007/3-540-45500-0_28)、[Unison (Wikipedia)](https://en.wikipedia.org/wiki/Unison_(software))
- N 向同步的版本向量：[File Synchronization with Vector Time Pairs](https://www.researchgate.net/publication/37991997_File_Synchronization_with_Vector_Time_Pairs)、[Syncthing: Understanding Synchronization](https://docs.syncthing.net/users/syncing.html)、[Syncthing 冲突检测改进 PR#10351](https://github.com/syncthing/syncthing/pull/10351)
- 代数化的文件系统调和：[An Algebraic Approach to File Synchronization](https://www.cs.tufts.edu/~nr/pubs/sync.pdf)
- 增量传输（v2 备选）：[The rsync algorithm](https://www.samba.org/rsync/tech_report/node2.html)、[Dsync: Lightweight Delta Synchronization](https://lingfenghsiang.github.io/docs/DSync.pdf)（FastCDC 内容分块）；.mph 等压缩二进制增量收益低，优先级放后

## Roadmap

- [x] v0.1 `scan`（表+hash 缓存）、`compare`（mirror/sync/enrich + 移动检测 + archive 归因）、`apply`（本地/挂载盘，dry-run 默认，回收目录）、`probe`
- [ ] v0.2 单测覆盖 compare 分类矩阵；`--exclude` 支持路径模式；symlink 策略
- [ ] v0.3 archive 自动化（apply 成功后自动落档）+ `sync` 的一条龙命令
- [ ] v0.4 远端：`pack` / `apply-pack` / ssh 传输封装
- [ ] v0.5 多端配置文件（节点×领地×模式），领地清单与 `.ffs-sync` 标记打通

## 构建

```bash
cargo build --release        # Windows: target\release\syncdash.exe
# Mac：ssh 过去跑同一条命令（仓库经 git 到位后），产物 target/release/syncdash
```
