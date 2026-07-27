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

## 架构（v0.3，参照 AlexQuant Desktop）

```
SyncDash/
├─ src/                 syncdash 核心库 + CLI bin
│   └─ foundation/      L0 地基层（fmt/time/path/text/names）：零 crate 内依赖，谁都能用
├─ src-tauri/           Tauri v2 桌面壳：薄 IPC 层（list_jobs / compare_job / apply_job）
├─ typescript/          前端（Vite + 原生 TS，无框架）：main.ts + styles.css
│   └─ core/types/generated/   ts-rs 从 Rust 结构体生成的线上类型 —— 不要手改
├─ Script/gen-types.mjs 类型生成入口（`npm run gen:types`）
├─ index.html           前端入口
├─ dist/                前端构建产物 —— 特意提交进 git（Mac 无 node，见"构建"）
├─ builder.bat          Windows 构建菜单（Dev / Desktop / CLI / All）
└─ builder.command      Mac 构建脚本（纯 cargo）
```

依赖分层：`foundation` → `table/chunk/atomic` → `progress/logging/runlog/settings` →
`filter/scan/compare/apply/pack/preflight/…` → `config/run` → 两个外壳。
依赖只向下，无环（Tarjan 校验）。**不设 re-export 桶**：调用方写全路径，
`foundation::fmt::human_bytes` 而不是 `preflight::human_bytes`。

## 命令

```bash
syncdash jobs                                    # 列出任务配置
syncdash run <job> [--apply] [--i-know]          # 一条龙：扫双侧→比对→闸门→(--apply)执行→刷新 archive
syncdash run --all | --prefix cs- [--apply]      # 批量跑（hub-and-spoke 多端的引擎）
syncdash territories <root>                      # 列出 .ffs-sync 领地
syncdash gen-jobs <root> --target-root R [--remote-host mac --remote-root-base /Users/x/Code]
syncdash gui                                     # 启动桌面版（等同直接跑 syncdash-desktop）
syncdash probe                                   # 本机环境 JSON（远端探测：ssh 对面跑这个）
syncdash scan <root> [--out t.jsonl] [--no-hash] [--force-rehash] [--symlinks-direct] [--progress] [--exclude PHRASE]...
syncdash compare --source a.jsonl --target b.jsonl \
    [--mode mirror|sync|enrich] [--archive last.jsonl] [--resolve-newer] [--case-sensitive] [--out plan.jsonl]
syncdash apply plan.jsonl [--apply] [--verify] [--delta] [--no-fsync] [--source-root R] [--target-root R] [-v]
syncdash mark <root> [--job NAME]                # 打 .syncdash-root 挂载点标记（配 require_marker）
syncdash trash runs|find <pat>|restore <pat> --into R|prune   # 本机回收目录：查看/找回/清理
syncdash logs list [job] [--limit N]             # 运行一览（含被中断的运行）
syncdash logs show <run-id> [--errors|--items|--plan]   # 某次运行的四份产物
syncdash logs prune [--keep-days N] [--max-total-mb N]  # 按保留策略清理
syncdash logs dir                                # 日志目录 / 设置文件在哪
syncdash pack plan.jsonl --out pkg.tar           # 打包 target 侧操作（payload+计划+双 hash 清单）
syncdash apply-pack pkg.tar [--apply] [-v]       # 对端：验 hash→提取→执行（锁/回收/校验全带）
syncdash-desktop                                 # Tauri 桌面版（主力 GUI）
```

## 任务配置（参考 FFS 的"一个 .ffs_gui 一个配置"）

一个 TOML 一个任务，放在 `%APPDATA%\syncdash\jobs\`（mac: `~/.config/syncdash/jobs/`）：

```toml
mode = "sync"              # mirror | sync | enrich
source = 'D:\Code\Utilities\flight'
target = '\\192.168.0.115\xuanbomiao\Code\Utilities\flight'
archive = 'C:\Users\xuanb\AppData\Roaming\syncdash\archive\flight.jsonl'   # sync 模式
# include = ['*']                       # FFS 过滤器语法白名单（留空 = 全部）
# exclude = ['*/big_temp/', '*/*.log']  # FFS 语法；默认垃圾/可重建排除已内置
# rigor = "standard"                    # quick | fast（抽样摘要）| standard | paranoid（见"严谨级"）
# case_sensitive = false                # 默认大小写不敏感（NTFS/APFS 默认行为）
# no_hash = false
```

`run <job> --apply` 成功（0 错误）且为 sync 模式时**自动刷新 archive**（冲突路径会从存档剔除，
下次继续报冲突而不是被悄悄仲裁）。

## 日志（v0.10）

一条统一的诊断链路：引擎的叙事、逐条失败、逐条执行结果全走 `progress::ProgressSink`
这一条已有的事件总线，末端挂文件 sink。**不引 tracing/log** —— 再叠一套 facade
只会让同一件事有两个出口。

```
<log_dir>/                                  # 默认 <config>/logs，可在设置里改
├─ runs.jsonl                               # 索引：一行一次运行
├─ app.jsonl                                # 运行之外的事件（启动/清理/迁移/设置错误）
└─ 20260727-002612-demo-apply/              # 每次 apply 一个目录（compare 只写索引行）
   ├─ summary.json    运行摘要（start 时先写 finished:false，finish 时覆盖）
   ├─ plan.jsonl      计划清单：这次**打算**做什么
   ├─ run.jsonl       事件流：叙事、阶段边界、报错
   ├─ errors.jsonl    报错清单：Error + 警告以上的 Log
   └─ items.jsonl     执行清单：一行一 op 的**实际结局**（ok/failed/kept/cancelled）
```

三件事是这一版的要害：

- **流式落盘**。事件到达即写（每 64 行或报错/阶段边界 flush）。v0.9 是 `finish()` 时
  一次性写，进程被杀就整份丢失。现在硬杀一次 4000 文件的同步，`plan.jsonl` 完整、
  `items.jsonl` 留下已完成的部分、`summary.json` 的 `finished:false` 说明它没跑完。
- **计划与执行分开**。v0.9 写进明细的是传给 apply 的计划 ops——哪条成功、哪条失败、
  哪条因非空目录被保留，一个字都没有。两份一比就知道哪些 op 根本没轮到。
- **桌面版第一次真的看得见**。`src-tauri` 是 windowed 构建，没有控制台；库内 32 处
  `eprintln!`（"远端 schema 不匹配"、"delta disabled"、"stale lock 接管"、
  "source-side op(s) skipped"）过去说了等于没说。现在它们是 `Log` 事件，进日志面板。

`println!` **一处都没动**：`remote::ssh_capture` 读远端 `syncdash scan` 的 stdout 拿
快照表，stdout 是数据线缆不是日志。

设置在 `<config>/settings.toml`（桌面版日志面板的 ⚙ 里可改）：

```toml
log_dir = ""             # 空 = 默认 <config>/logs；改了保存时旧目录整体迁移
level = "info"           # info | warn | error
keep_days = 30           # 0 = 不按天清
max_total_mb = 512       # 0 = 不限（执行清单全记，这是它的安全带）
log_compare = "summary"  # summary = 只写一行索引不建目录 | off
                         # 不给 full 档：值守 30s 一轮 = 一天 2880 次
mirror_stderr = true     # CLI 同时按原文进 stderr（终端体验与改造前逐字一致）
```

## GUI（桌面版 `syncdash-desktop`，Tauri v2）

FFS 形态的暗色双栏界面：左侧任务列表（模式徽章：mirror 蓝 / sync 绿 / enrich 橙）→
**Compare**（spawn_blocking 后台跑，UI 不卡）→ 差异表：勾选框 + 彩色动作徽章
（`→ copy` / `← copy` / `⇢ move` / `✕ delete` / `⚡ conflict`）+ path / from / size / reason →
统计条（项数 / 已选 / 待传字节 / 冲突）→ **Synchronize** 执行勾选项，完成后**自动复比对**验证收敛。
conflict/note 行锁定不可勾。前端零框架（Vite + 原生 TS，约 400 行）。

v0.3.2 追加：**逐行翻转方向**（点动作徽章切换，语义由核心 `reverse_op` 预计算：copy↔delete 互逆、update 换边；
翻转行虚线边框+底色提示）、**筛选 chips**（全部/复制/更新/移动/删除/冲突，实时计数，0 项变淡——GitDash 风格）、
**搜索框**（path/from/reason 子串）、**同步前确认单**（分类计数+字节数，删除红色高亮）、
**快捷键**（Ctrl/⌘+R 比对、Ctrl/⌘+F 搜索、Enter 同步、Esc 关弹层）、**Mac 沉浸式标题栏**。

v0.9 "Progress & Polish"（对照 FFS 14.10 源码行为参数，计划见 plans/ffs-ui）：
- **独立进度子窗口**（FFS 同款）：比对期显示双侧扫描的条数/字节实时计数；执行期双累积图
  （字节+条目）、4s 滑窗速率、60s 滑窗 ETA、大字百分比 `(bytesDone+itemsDone)/(bytesTotal+itemsTotal)`、
  已处理/剩余、当前文件、窗题百分比 + Windows 任务栏进度。
- **Pause/Continue**（引擎自旋暂停：elapsed 冻结、RootLock 心跳继续跳，对面机器不会误判遗弃锁）、
  **Stop = 协作取消**（块间响应；原子落盘保证终点永无半截文件、零 `.syncdash.tmp.*` 残留）、
  **错误累积面板**（错误不中断执行——FFS 语义；windowed 构建里 stderr 会丢，错误/警告全走事件流）、
  **Auto-close** 与 **When finished**（睡眠/关机，10 秒可取消倒计时）。
- **Overview 聚合竖栏**（差异表左侧可折叠）：按顶层目录聚合条数/字节/占比条，点击过滤差异表，二层惰性展开；
  **图标化统计条**（0 值置灰、非 0 加粗着色）。
- **运行日志**：每次真实 apply 落 `logs/runs.jsonl` 索引 + 每次运行明细（定稿 op 清单+累积错误）；
  侧栏任务行显示**上次同步**（结果色点+相对时间，超 7 天变红）；GUI 日志面板可回看历史与明细；
  CLI `syncdash history [job] [--prune-days N]`。
- **任务编辑器**（全字段表单：模式/根目录/严谨级/过滤器/守护闸门/远程三件套/watch，新建/编辑/二次确认删除）。
- **值守 Watch**（定时扫描，非 inotify）：`watch_interval_secs` 秒级档＝"准实时"；hash 缓存让未变的树
  只付 walk 成本。桌面 Watch 开关（倒计时+发现差异提醒/自动执行）；CLI `run --watch [--interval N] [--auto-apply]`。
- **remote 任务在 GUI 里走真远程管线**（此前静默落进本地管线，经 UNC 重哈希慢一个数量级）；侧栏 ssh 徽章。
- **egui 旧界面退役删除**（Tauri 功能齐平后按约定移除；CLI 无参数/`syncdash gui` 现在启动桌面版，
  workspace release 构建 ~2.5min → ~56s）。
- 引擎底座：统一 `ProgressEvent` 事件流（PhaseStart/Totals/Progress/Error/Paused/Resumed/Summary，
  节流归 sink）；**apply 五相位执行**，Copy/Update 相位并行（`parallel`，默认 4，SMB 2-4 条流吃满上行；
  开 delta 的 Update 留串行道防内存峰值）；DeleteDir 类内 deepest-first。

v0.9.2 "FFS parity"（对齐 FFS 里天天用、我们一个都没有的那批按钮）：
- **目录选择器 / 拖放 / 路径历史 / 路径体检**：编辑器的两个根目录不再只能手打——
  浏览按钮（tauri-plugin-dialog）、拖文件夹进来即填（Tauri v2 吞掉了 HTML5 drop 事件，
  走 `onDragDropEvent` + 物理像素换算命中测试）、`<datalist>` 记最近 12 个根、
  `inspect_paths` 实时校验（存在/是目录/两根相同/两根嵌套/有无 `.syncdash-root`）。
- **⇄ 交换**：编辑器内一键对调；工具栏的交换会**写回 TOML** 并作废当前计划（带撤销）。
  FFS 换的是内存里那份配置，我们的任务是磁盘上的具名文件——不落盘的话，计划头里的两个根
  和任务文件说的就不是一回事，运行日志与 archive 刷新都会指向错误的方向。
- **差异表右键菜单**：在资源管理器中显示（`reveal`，不过 shell）/ 复制完整或相对路径 /
  排除此类型 `*/*.ext` / 排除此目录 `/rel/dir/`（写回任务 exclude，带撤销）/ 反向此行 /
  只勾选此项 / 取消勾选本目录。
- **双侧 size + 修改时间列**，两侧 mtime 差超 2s 时把较新的一侧染绿——"哪边新"此前只能
  去 reason 里猜，冲突行更是连 size/mtime 都没有。数据来自核心库新增的只读证据层
  `compare::evidence()`（与 `compare()` 共用 `norm_key`/`files_equal`，`Op` 结构与 plan
  落盘格式一个字节没动）。**点表头排序**（path/action/两侧 size/两侧 mtime）——排序与树状
  分组互斥，因为分组依赖"同目录的行在计划里连续"这条不变量。
- **状态条计数**：`显示 X / Y · 隐藏 Z 不执行 · 已扫描 A ⇄ B · 相同 K`（FFS 的
  "Showing 481 of 23,112"）。`source_entries`/`target_entries` 早就在 plan header 里躺着。
- **漏斗筛选**（作用于当前结果，不重扫）：名称掩码（FFS 语法）+ 大小区间 + 时间跨度。
  掩码判定回 Rust 的 `filter::mask_hits`——前端绝不自己写第二份 glob，界面里试通的掩码
  写进任务 exclude 后行为才会一致。面板底部一键把临时掩码**升格**为任务的持久 exclude。
- ⚠ **视图即动作集**：被漏斗/搜索/类别 chips 隐藏的行**不会被执行**（FFS 语义）。
  这修正了旧行为里一个安静的坑——过去搜索框一过滤，被隐藏但仍勾着的行照样跟着
  Synchronize 跑掉。确认单会明写"被筛选隐藏，不执行 N 项"，统计条口径同步改为
  勾选 ∩ 可见。
- **相同项面板**（FFS 底部那个 "22,631" 按钮）：列出两侧判定相同的文件，分页 300 条一批，
  自带路径过滤；数据源是上一次 compare 留在内存里的两侧快照，**不重扫**。内容相同但两侧
  时间戳漂移超过 2s 的行把 target 时间标橙（FAT/SMB 粒度导致的常见现象）。
  单槽缓存，换任务或重新比对即覆盖；远程任务同样可用（远端快照是经 ssh 拉回的完整表）。
- **CSV 导出**：导出当前视图（含勾选态与双侧 size/时间），转义只在 Rust 侧做一次，
  UTF-8 **带 BOM**——不加 BOM 的话 Excel 按本地代码页解释，中文路径整列乱码。
  枚举字面量用 serde 的 snake_case，与 plan JSONL、事件流同源。
- **计划任务命令**（FFS "Save as batch job" 的对应物）：编辑器里一键复制
  `schtasks /create ... syncdash run <job> --yes`。**不代为注册系统计划任务**——
  那是系统设置级动作，该由人自己在管理员终端里按下。
- **类别 chips 改为各自独立的开关**（可同时只看"新增+删除"），F5 / F9 = 比对 / 同步，
  Compare 与 Synchronize 按钮加副标题直接写明当前 rigor 与 mode，旁边齿轮跳编辑器对应分组。

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

### 严谨级（rigor）——单调阶梯：每一级 = 本轮实际读得更多

设计原则（v0.9.3 重构）：**"一致 ✓"必须是本轮实测，不是缓存记忆**。缓存只存在于 fast
一级（且明示）；standard 起每轮都实读每个文件。两条横切增强：

- **分歧升级**（fast/standard）：抽样摘要相等但 |Δmtime| > 2s 时不直接判相等——双侧升级
  全量哈希重裁。真机验证：400MB 文件在采样窗外改 64 字节，升级规则当场抓出。
- **写后校验**（standard/paranoid 默认开）：期望值 = **本次复制流的全量 blake3**（复制本就
  读了全文，流上算哈希零成本），落盘后重读比对，不合格不 rename。与扫描证据深度解耦。

| 级别 | 本轮实读 | "一致 ✓"的含义 | 移动检测 | 写后校验 | 适用 |
|---|---|---|---|---|---|
| `quick` | 0 字节 | 本轮实测元数据（size+mtime±2s） | ❌ | ❌ | 结构巡检 |
| `fast` | 仅变化面的采样窗（缓存加速未变面） | 变化面实测＋未变面为缓存记忆 | ✅ | ❌ | 云盘/媒体库（占位文件只水合三小段） |
| `standard`（默认） | **每个文件的采样窗，不用缓存** | 本轮实测每个文件的头/中/尾 | ✅ | ✅ | 日常 |
| `paranoid` | 每个文件的全部字节 | 本轮实测每个字节 | ✅ | ✅ | 首迁/冷备年检/可疑介质 |

采样窗 = size + 头/中/尾各 256KB（<4MB 全量；`~` 前缀与全量哈希在缓存中严格隔离）。

**威胁覆盖 × 检测延迟**（所有级别共有的元数据/结构审计不再赘述——存在性、路径归一、
类型、size/mtime、symlink 指向、权限位、非法名预检、archive 归因、执行闸门）：

| 威胁 | `quick` | `fast` | `standard` | `paranoid` |
|---|---|---|---|---|
| 正常修改（动 size/mtime） | 立刻 | 立刻 | 立刻 | 立刻 |
| 动过 mtime 的任意改动（含采样窗外） | 立刻（仅元数据级） | **立刻**（升级规则全量重验） | **立刻**（升级规则） | 立刻 |
| 保 size+mtime 的改写（timestomp） | 永不 | 采样窗内立刻；窗外永不 | 采样窗内**每轮实测**；窗外永不 | 立刻 |
| 静默 bitrot | 永不 | 未变面永不（缓存） | 采样窗内**每轮实测**；窗外永不 | 立刻 |
| 传输损坏 | 永不 | 永不 | **立刻**（写后校验） | 立刻 |
| 移动身份 | 删+增 | 配对 | 配对 | 配对 |

### 跨平台正确性（v0.2.2）

- **Unicode 归一化**：Mac（HFS+ 强制 NFD；APFS 保留写入形态但按归一化不敏感匹配）与
  Windows/Linux（NFC 惯例）会给同一个名字不同的字节序列。比对键统一 NFC 归一——
  `café`(NFC) 与 `café`(NFD) 判为同一个文件；**落盘 I/O 永远用各侧自己的原拼写，
  绝不像 Syncthing 那样改写对方的形态**（它有把 NFC 转 NFD 弄断引用的前科）。
- **大小写**：NTFS/APFS 默认大小写不敏感 → 比对键默认折叠大小写（job 里 `case_sensitive = true` 可关；
  关掉后大小写改名会被移动检测配对成一次 rename）。同侧归一化撞名（NFD/NFC 双胞胎、大小写双胞胎）
  → Note 报告并保留先出现者，绝不静默合并。
- **Windows 非法名预检**：要在 Windows 侧新建的路径先查保留设备名（CON/AUX/NUL/COM1-9/LPT1-9）、
  非法字符（`<>:"|?*`、控制符）、尾部点/空格 → **计划阶段**直接标 `Conflict("illegal-on-windows")`，
  不让 apply 执行到一半才炸。
- **文件属性**：unix mode（exec 位等）已记录进快照表（`mode` 字段）；SMB 带不动它，
  v0.4 打包模式负责恢复。复制后显式回写 mtime，保证下次比对的相等判定成立。

### 版本控制（v0.8，可选：`versioning = true`）

开启后，被删/被覆盖的文件不再进本机 trash，而是存进**该 root 自己的 `.version_syncDash/`**——
历史跟着数据走，两台机器经 SMB 都能看见、都能恢复：

```
<root>/.version_syncDash/
  index.jsonl                版本索引（id、时间、主机、op 数、保存数、字节）
  <id>/plan.jsonl            本次执行的指令清单（审计）
  <id>/manifest.json         保存条目：whole|rdelta、各 hash、原 mtime/mode
  <id>/files/<rel>           原内容整存（小文件、被删除文件）
  <id>/rdelta/<rel>          FastCDC 反向补丁（≥4MB 被覆盖文件：旧文件 = 新文件已有块 + 旧独有块 blob）
```

- `syncdash versions <root> [--prune N]` —— 列出/清理版本历史
- `syncdash restore <root> --version <id> [--file rel]... [--apply]` —— 找回（默认 dry-run；
  rdelta 要求当前文件与记录的 new_hash 一致，重组后按 old_hash 校验；被顶掉的当前内容留在旁路目录，不销毁）
- 实测：5MB 文件被覆盖，版本库只占 70,602 B（1.3%）；restore 后 SHA256 与原始逐位一致
- 全链路生效：本地 apply、`apply-pack --versioning`、远程管线自动透传；scan 与 FFS 生成器
  模板都排除 `.version_syncDash/`，版本库绝不会被当成数据同步出去

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

## 远端模式（v0.4，已实现并真机验证）

1. `ssh <host> syncdash probe` —— 探测对面：OS/arch/版本/schema；二进制不存在则给出安装指引
   （两台机器都有 Rust 工具链，`cargo build --release` 即可；或经共享盘直接拷二进制）。
2. `ssh <host> syncdash scan <root>` —— stdout 收表。
3. 本地 `compare` 出计划。
4. `syncdash pack plan.jsonl --out pkg.zip`：**待写入文件 + 计划（含删除清单）+ 数据区 hash + 计划 hash**。
5. 传输（scp / 共享盘均可）后 `ssh <host> syncdash apply-pack pkg.zip`：先验两个 hash，再按计划逐步执行，同样默认 dry-run。

Win↔Mac 的 SSH 已验证可用（Mac 22 端口开着，免密只差把公钥写进 authorized_keys）。

## 多端（v0.6 定型：hub-and-spoke）

**支持的拓扑 = hub-and-spoke**：Win01 当 hub，每个 spoke（Mac、E: 冷备、未来任何一台）
一份 job（sync/mirror 各取所需、各自 archive），`run --all` / `run --prefix cs-` 一键全跑。
两两 sync＋各自 archive 在数学上等价于经 hub 的 N 向传播——对"单 hub、多 spoke"的现实完全正确。

**真 P2P N 向（版本向量，Syncthing 思路）明确列为非目标**，除非哪天出现"绕过 hub 的
spoke↔spoke 直连写入"需求——届时再上版本向量，架构上表格式已预留（表是一等公民，
每端一张表天然成立）。

远程管线（job 配 `remote_host`/`remote_root`/`remote_exe`）：`run` 自动走
ssh 探测 → **远端在自己盘上扫描**（免 UNC 拉数据哈希，大领地快一个量级）→ 本地比对 →
target 侧打包经 ssh stdin 送达 → 远端 `apply-pack`（自带锁/回收/校验）→
source 侧回拉经挂载路径直落 → archive 刷新。`gen-jobs --remote-host mac
--remote-root-base /Users/xxx/Code` 可为全部领地一次生成远程管线任务。

## 与 CodeSync（FFS）的关系

先并行：FFS 继续管日常，SyncDash 拿 `.ffs-sync` 领地练手（`Update-CodeSyncConfig.ps1` 的标记扫描
将来可以直接改产 syncdash 的领地清单）。行为可信之后再接管。

## 从 FFS 14.10 源码借鉴的（`.docs/FreeFileSync_14.10_Source`，GPL——按语义重写，未搬代码）

- **path_filter.cpp → src/filter.rs**：过滤器语法与 FFS 完全兼容——大小写不敏感、`/` 与 `\` 通吃、
  `*` 跨层级、`?` 不跨、尾 `/`＝目录、首 `/`＝根相对、`*/abc` 兼命中根级；无通配符路径走哈希集常数查找；
  include 侧用"前缀可能命中"决定是否下钻（白名单能穿透中间目录的机制）。**FFS 的排除列表可原句粘进 job 配置**。
- **dir_lock.cpp → src/lock.rs**：apply 前在两侧 root 放 `.syncdash.lock`，持锁进程每 4s 心跳刷 mtime
  （经 SMB 对面机器可见）；发现他人锁且心跳仍在 → 拒绝执行；观察 12s 无心跳 → 判定遗弃并接管。
  防的正是双机场景的真实风险：Win 和 Mac 同时 apply 同一目录。
- **algorithm.cpp（记入设计，未改代码）**：FFS 的移动检测靠 db 锚点＋文件 ID＋精确 size/date
  （注释强调"容差不得进容器谓词，破坏传递性"）；我们以内容 hash 配对证据更强，维持现状。
  FFS 把同目录 rename 合并为单行展示——列入 v0.3。
- **parallel_scan.cpp**：目录树并行遍历（我们目前 walkdir 串行＋单文件 rayon 哈希）——列入 v0.3。

## 从 Syncthing 源码借鉴的（`.docs/syncthing` @ `119d5e72`，MPL-2.0——按语义重写，未搬代码）

完整对照分析见 [PLAN-syncthing-upgrade.md](PLAN-syncthing-upgrade.md)（每条都带双侧真实行号）。落地的：

- **`lib/osutil/atomic.go` → [src/atomic.rs](src/atomic.rs)**：写同目录临时文件 → fsync → rename。
  同卷 rename 原子，中断只留临时文件。这条修的是一个真实的数据丢失路径，不是洁癖。
- **`lib/config/folderconfiguration.go` 的 `.stfolder` → [src/preflight.rs](src/preflight.rs)**：
  挂载点标记。它跟着**数据**走，盘没挂上标记就不在——这是"共享盘掉线"唯一可靠的判据。
  标记自身必须排除出同步（否则空目录会凭空长出标记），syncthing 同样把它列为 internal。
- **`CheckAvailableSpace` / `minDiskFree`** → 写入前按计划里的 size 汇总预检。
- **`deleteDirOnDiskHandleChildren`（folder_sendrecv.go:1985）** → 目录删不掉时分类汇报，
  区分"被过滤器保护""可连带删""真错误"，不再静默。
- **`conflictName`（:2219）/ `WinsConflict`（bep_fileinfo.go:212）** → `.sync-conflict-<ts>-<host>` 副本，
  mtime 新者胜、host 名做稳定 tie-break（我们没有 device id）。**默认仍是只报告**——不自动仲裁是 SyncDash 的立身之本。
- **`PreviousBlocksHash`（bep_fileinfo.go:200）** → archive 的 `prev` 多代链：一侧只是"落后一代"
  不是并发修改。这是 archive 模型下对版本向量的廉价近似。
- **`lib/fs/mtimefs.go`** → 设完 mtime 回读，把 (ondisk, intended) 记进本机缓存，
  不再让 ±2s 容差当唯一判据（`rigor = "quick"` 时尤其要紧）。
- **`lib/ignore/ignore.go` 的 `!` 与 `(?d)`** → 过滤器的 `!` 例外与 `deletable` 列表，FFS 语法的严格超集。
- **`lib/versioner/staggered.go:toRemove`** → 回收站的分级稀释（近期密、远期疏），配 `trash prune`。
- **`lib/model/folder.go:930` 的 `Size == 0` 守卫** → 空文件不参与移动配对。
  所有零长文件 blake3 相同，过去会被配成一堆编出来的"重命名"。
- **`lib/fs/casefs.go` 的 `CaseConflictError`** → 大小写敏感模式下的落盘撞名预检（计划阶段拦，不到 apply 才炸）。
- **`shortcutFile`（:1253）** → `Action::Chmod`：内容相同只差权限位时不重传。
- **`lib/protocol/vector.go` → [src/vclock.rs](src/vclock.rs)**：版本向量数学核心（含代数性质穷举验证）。
  **尚未接管 archive 归因**——真 N 向是 v1.0 的收敛性工程，见"多端"。

明确**不抄**的：BEP 协议栈 / TLS / 设备发现 / 中继 / NAT 穿透（我们走 ssh + SMB 是刻意的简化）、
常驻 daemon（会破坏"默认 dry-run、人点才动"的核心承诺）、索引数据库（JSONL 表可读可 diff 可管道，是卖点）、
加密文件夹、文件系统监视（`watchaggregator` 的聚合策略值得读，但引入它就得常驻）。

## 算法调研来源

- Unison 形式化规范与 archive 模型：[Balboa/Pierce, "What's in Unison?"](https://www.researchgate.net/publication/32205844_What's_in_Unison_A_Formal_Specification_and_Reference_Implementation_of_a_File_Synchronizer)、[Unison: A File Synchronizer and Its Specification](https://link.springer.com/chapter/10.1007/3-540-45500-0_28)、[Unison (Wikipedia)](https://en.wikipedia.org/wiki/Unison_(software))
- N 向同步的版本向量：[File Synchronization with Vector Time Pairs](https://www.researchgate.net/publication/37991997_File_Synchronization_with_Vector_Time_Pairs)、[Syncthing: Understanding Synchronization](https://docs.syncthing.net/users/syncing.html)、[Syncthing 冲突检测改进 PR#10351](https://github.com/syncthing/syncthing/pull/10351)
- 代数化的文件系统调和：[An Algebraic Approach to File Synchronization](https://www.cs.tufts.edu/~nr/pubs/sync.pdf)
- 增量传输（v2 备选）：[The rsync algorithm](https://www.samba.org/rsync/tech_report/node2.html)、[Dsync: Lightweight Delta Synchronization](https://lingfenghsiang.github.io/docs/DSync.pdf)（FastCDC 内容分块）；.mph 等压缩二进制增量收益低，优先级放后
- Unicode 归一化与跨平台文件名：[Explainer: Unicode, normalization and APFS](https://eclecticlight.co/2021/05/08/explainer-unicode-normalization-and-apfs/)、[APFS's "Bag of Bytes" Filenames](https://mjtsai.com/blog/2017/03/24/apfss-bag-of-bytes-filenames/)、[Apple APFS FAQ](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/APFS_Guide/FAQ/FAQ.html)、[File names & unicode normalization problems](https://nicolasbouliane.com/blog/unicode-normalization)、[Windows 文件名规则 (Microsoft Learn)](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file)

## Roadmap

- [x] v0.1 `scan`（表+hash 缓存）、`compare`（mirror/sync/enrich + 移动检测 + archive 归因）、`apply`（本地/挂载盘，dry-run 默认，回收目录）、`probe`
- [x] v0.2 任务配置（jobs/*.toml）、`run` 一条龙、GUI（Compare→勾选→Synchronize）、sync 成功后自动刷新 archive
- [x] v0.2.1 FFS 语法过滤器（include/exclude 完全兼容，含单测）＋根目录心跳锁（防双机并发 apply，遗弃锁自动接管）
- [x] v0.2.2 严谨级 quick/standard/paranoid（复制后校验）＋跨平台正确性：NFC 归一比对键、大小写折叠、Windows 非法名预检、unix mode 记录（含单测）
- [x] v0.3 Tauri v2 桌面壳（参照 AlexQuant Desktop：Vite+TS 前端、builder 双平台脚本、dist 入库使 Mac 免 node 纯 cargo 构建）；
      rigor 严谨级（quick/standard/paranoid：免hash / hash缓存 / 全量重hash+复制后校验）；NFC+大小写折叠比对键；Windows 非法路径预检
- [x] v0.3.x compare 分类矩阵单测（archive 归因全矩阵，20 项测试）；两阶段并行扫描（rayon 全文件并行哈希，≥32MB 内部再分块）；`compare::reverse_op` 逐行翻方向（egui 点动作徽章翻转；Tauri 壳可直接复用同一 lib 函数）
- [x] v0.4 远端：`pack` / `apply-pack`——tar 容器（plan.jsonl＋payload＋收尾 manifest），计划 blake3＋逐文件 blake3＋合并 hash；staging 全部验完才动 target；复用 apply 的锁/回收/复制后校验；unix mode 恢复。**Win 打包 → SMB 送包 → Mac apply-pack → 远程复扫 0 ops，真机全流程验证**
- [x] v0.5 `territories` / `gen-jobs`：扫 `.ffs-sync` 标记为每个领地生成 `cs-<slug>.toml`（sync 模式＋自动 archive 路径）——syncdash 版 CodeSync 生成器，11 个领地实测生成；与 FFS 并行运行，切换时机由使用者定
- [x] v0.6 `run --all`/`--prefix`；ssh 远程管线一条龙（job 配 remote_host 即启用，真机验证：dry→apply→复跑 0 ops，含 symlink）；symlink 策略 exclude/direct（按指向比对，apply 建/换/删链接本身）；同父目录 rename 优先配对（reason 区分 rename/move）；git bundle 经 SMB 更新 Mac（挂载不在线时的通道）
- [x] v0.7 三个"后续候选"全部落地：**Windows 作为远端**（`recv` 子命令用 Rust 原始 stdin 收包、按 probe 的 os 选 shell 方言：PowerShell 单引号翻倍＋chcp 65001 前奏＋`& 'exe'`；实测 Mac 反向驱动 Windows：8.4MB 包经 ssh stdin 落地、apply-pack 执行、复跑 0 ops）；**FastCDC 增量传输**（16K/64K/256K v2020，远端 `chunks` 出块表，≥4MB 更新只传缺失块＋重组 recipe，blob/base/成品三重 blake3；实测 8MB 改 6KB 只传 148KB，省 98.2%）；**GUI 任务编辑**（egui：New/Edit/Delete 全字段表单＋校验＋二次确认删除；`config::save_job/delete_job` 供桌面壳复用）
- [x] v0.8 可选版本控制：root 内 `.version_syncDash/`（plan 指令清单＋整存＋FastCDC 反向补丁）＋ `versions`/`restore` 命令；实测大文件旧版占 1.3%、restore 哈希逐位一致
- [x] v0.9 **对照 syncthing 源码的安全网与能力补齐**（计划见 [PLAN-syncthing-upgrade.md](PLAN-syncthing-upgrade.md)，78 项测试）：
      **原子落盘**（同目录临时文件→fsync→rename，中断绝不在最终路径留半截——此前 `fs::copy` 直写目标，
      Update 断掉会让截断文件在下一轮被反向传播回 source）；
      **挂载点标记 `.syncdash-root` + 计划体检**（`require_marker` / `max_delete_ratio` / `--i-know`：
      共享盘没挂上、过滤器写错、source/target 写反，长得一模一样，这道闸把三者一起拦住）；
      **磁盘空间预检**（`min_free_pct`）；**目录删除分类汇报**（不再 `Err(_) => Ok(())` 静默吞掉）；
      **冲突副本**（`on_conflict = copy|newer`，`.sync-conflict-<ts>-<host>` + `max_conflicts`，默认仍是只报告）；
      **多代 archive**（`prev` 链：一侧只是"落后一代"不再误报 both-changed）；
      **mtime 回读校正**（FAT/SMB 截断时间戳时不再靠 ±2s 容差硬扛）；
      **过滤器 `!` 取反 + `deletable`**（FFS 语法的超集，粘进来的 FFS 规则行为不变）；
      **回收站保留期**（`trash runs/find/restore/prune` + syncthing staggered 稀释算法）；
      **本地增量**（`delta`：大文件按 FastCDC 块补写，SMB 上传划算）；
      **`Action::Chmod`**（`sync_mode`：内容相同只差权限位时不重传）；
      空文件不再被乱配成"重命名"、歧义配对如实标注候选数、大小写撞名预检、扫描进度（CLI `--progress` + GUI 进度条）
- [x] v0.9 **"Progress & Polish"——对照 FFS 14.10 的执行期体验补齐**（90 项测试）：统一进度/取消/暂停事件流底座；
      apply 五相位并行执行（`parallel`）；独立进度子窗口（双累积图/速率/ETA/暂停/停止/When-finished）；
      Overview 聚合竖栏＋图标统计条；运行日志＋"上次同步"；Tauri 全字段任务编辑器；值守定时扫描
      （`watch_interval_secs`/`--watch`）；remote 任务 GUI 真远程管线；**egui 退役删除**（详见上方 GUI 一节）
- **roadmap 全部完成**。仅存的远期方向：版本向量 P2P（见"多端"——明确非目标，除非出现绕过 hub 的直连写入）。
  数学前置件已就位：[src/vclock.rs](src/vclock.rs) 是照 syncthing `lib/protocol/vector.go` 语义重写的完整实现
  （`update` 单调性、`merge` 上确界、比较关系反对称性均有穷举验证），但**尚未接管 archive 归因**——
  真 N 向要求每次 apply 后精确维护向量并保证收敛，那是 v1.0 的工程。

## 构建

**Windows**：双击 `builder.bat`（[1] Dev HMR / [2] Desktop / [3] CLI / [4] All），或手动：

```bash
npm run build && cargo build --release -p syncdash-desktop   # 桌面版
cargo build --release -p syncdash                            # CLI
```

**Mac（无需 node）**：`dist/` 前端产物随 git 提供，Tauri 编译期直接嵌入，纯 cargo 出完整 GUI：

```bash
bash builder.command     # = cargo build --release -p syncdash-desktop -p syncdash
```

改了前端（typescript/、index.html）之后：在 Windows 跑一次 `npm run build` 并把 dist/ 一起提交，Mac 拉取后重编即可。

**把仓库送上 Mac**（无 GitHub 远端时）：Mac 挂载了 D 盘就 `git clone /Volumes/D-AnonyD/Code/Utilities/SyncDash ~/Code/Utilities/SyncDash`；
没挂载则从 Windows 反向推：`git -c windows.appendAtomically=false -c core.autocrlf=false clone D:\Code\Utilities\SyncDash '\\192.168.0.115\xuanbomiao\Code\Utilities\SyncDash'`
（macOS 的 SMB 不支持 git 的原子追加写，必须关 `windows.appendAtomically`；`autocrlf=false` 保住 .command/.sh 的 LF）。
