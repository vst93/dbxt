# dbxt

[English](README.md) | **中文**

基于 [DBX](https://github.com/t8y2/dbx) 内核的终端数据库 TUI，键盘优先。连接只需配置一次 —— 在 DBX 桌面端、DBX CLI 或 dbxt 本身 —— 之后任何终端都能用。单个静态二进制：无桌面端、无守护进程、无 HTTP 服务。

## 功能特色

**连接管理** —— 与 DBX 桌面端共享
- 连接存放在 DBX 自己的 SQLite 存储（`dbx.db`）里，你在桌面端配置过的连接自动可见。
- `c` 在 TUI 内新建连接，`p` 复制到表单，`Enter` 连接；连接按数据库家族着色。
- `d` 弹出数据库列表并切换 —— MySQL/PostgreSQL 库、MongoDB 库、Redis 逻辑 db 同一套手势。
- `o` 返回连接选择，`r` 原地刷新列表。

**SQL 编辑与结果**
- 多行编辑器，shell 风格 `↑`/`↓` 历史（从 DBX 共享查询历史初始化）；`F5` / `Ctrl-J` 执行。
- `Ctrl-Space` 按上下文补全标识符（表名 / 列名 / 关键字，标注 `T`/`C`/`K`）；`Tab` 上屏。
- 每次执行保留独立结果标签（`[` / `]` 切换）；显示耗时与受影响行数。
- `Ctrl-P` 执行 `EXPLAIN`，`Ctrl-Y` 导出当前结果（CSV / JSON / NDJSON / Markdown / INSERT），`Ctrl-N` 在结果被行数上限截断时加载更多。
- `Ctrl-O` 插入 DBX 收藏片段；`s` 把编辑器 SQL 存回共享收藏库。
- 多语句脚本（`a; b; c;`）批量执行，逐条列出结果，`Enter` 下钻单条。

**表格数据与编辑**
- 侧栏 `Enter` 执行分页 `SELECT *`；`↑`/`↓` 连续走行并跨页自动衔接，`n`/`p` 翻页时保持相对行号。
- `←`/`→` 移动单元格光标，`z` 钉住首个数据列，`Enter` 弹出完整单元格；底部条显示横向位置。
- `e` 以 diff 确认层编辑单元格：旧值 → 新值、`WHERE` 条件、主键与完整 `UPDATE` 一目了然。
- `i` 按列模板插入，`Delete` / `Ctrl-D` 以带条件的 `WHERE` 删除 —— 所有写入先经确认。
- `Ctrl-T` 排队写入，`Ctrl-S` 以单个 `BEGIN … COMMIT` 执行；`f` 过滤、`s` 排序、`Ctrl-K` 追加排序键、`Ctrl-R` 清除。
- `y` 把当前行复制为 `INSERT INTO … VALUES (…)`；`/` 搜索可见结果行。

**表结构**
- `r` 显示字段列表（类型 / 键 / 可空 / 默认值 / 注释）；`t` 切换按方言生成的 `SHOW CREATE TABLE` DDL。

**导入与导出**
- `I` 把 CSV 导入当前表（已浏览的表，否则侧栏选中的表）。输入路径（开头 `~` 展开为 `$HOME`）后确认预览：编码、分隔符、行数与文件大小、前五行解析结果，以及按列名对齐的列映射与类型推断（`int` / `float` / `bool` / `date` / `datetime` / `text`）。
- CSV 表头按列名（忽略大小写）匹配表列；CSV 中缺失的表列保留其默认值（通常为 `NULL`），多出的 CSV 列则阻止导入并给出明确提示。
- `m` 切换追加 / 覆盖（覆盖先清空表，预览边框变红），`s` 切换遇错停止（默认，报告出错行号）/ 跳过继续（列出所有跳过行）。数据以每批 500 行的事务批量写入，并按批报告进度。
- 自动探测编码 —— UTF-8，否则 GB18030/GBK（常见中文编码）—— 并从表头嗅探分隔符（`,` / `;` / TAB）。明确不支持 Excel `.xlsx`。
- `Ctrl-Y` 把当前结果导出为 CSV、JSON（数组）、NDJSON、Markdown、`INSERT`（每行一条）或批量 `INSERT`（多行 `VALUES`）。先选格式再选去向：留空走 OSC 52 复制，输入路径则写文件；超过 10000 行会提示生成可能耗时。

**Redis**
- 连接 Redis 后打开分页 `SCAN` key 浏览器（绝不 `KEYS *`），带类型与 TTL 徽标、服务端 `MATCH` 模式（`/`）与逻辑 db 切换。
- `Enter` 按类型渲染 value —— string、hash、list、set、zset、stream、RedisJSON，大集合可继续加载。
- `e` / `x` / `m` / `Del` 编辑、设 TTL、重命名、删除；`Space` 多选后批量删除 / 设 TTL / 前缀重命名，均走红色确认层。
- 其他功能仍可用原生 `redis-cli` 命令台（`Ctrl-L`）。

**MongoDB**
- 连接 MongoDB 后列出 collection；`Enter` 以网格浏览文档（顶层字段并集，`_id` 优先）。
- `n`/`p` 翻页，`f` 应用 JSON 过滤，`r` 查看 collection 索引。
- `e` 用 JSON 编辑器编辑文档（`_id` 不可改，字段级 diff），`i` 插入，`Del` 按 `_id` 删除 —— 每次写入确认并原地刷新。

**效率与体验**
- 任意位置 `?` 打开快捷键速查；所有浮层统一 `Esc` 关闭。
- `Ctrl-A` 自动折叠非焦点栏，`Ctrl-W` 收起 / 展开当前栏；低于 50 列时各栏纵向堆叠。
- `Alt-C` 压缩列宽，`Alt-H` 隐藏列，`Alt-R` 直达最近表 —— 选择按 `库.表` 持久化。
- 鼠标与触屏可用：点击选中、再点确认；滚轮滚行，`Shift`/`Alt`/`Ctrl`+滚轮横滚列。
- 失败可见：看门狗把无响应的后端变成错误，执行中显示转圈与已用秒数，服务端错误原样回显。
- 所有界面文案来自同一张表（`src/ui_text.rs`）；默认中文，`DBXT_LANG=en` 切换英文。

## 安装

一键脚本 —— bash（Termux 同样适用）或 PowerShell：

```bash
curl -fsSL https://raw.githubusercontent.com/vst93/dbxt/refs/heads/master/cmd/install.sh | bash
```

```powershell
irm https://raw.githubusercontent.com/vst93/dbxt/master/cmd/install.ps1 | iex
```

国内加速：把地址换成 `https://cdn.jsdelivr.net/gh/vst93/dbxt@master/cmd/install.sh`（或 `.../install.ps1`）。脚本从 GitHub Release 读取版本、校验 `.sha256`，安装到 `~/.local/bin`（Termux 为 `$PREFIX/bin`，Windows 为 `%USERPROFILE%\.local\bin`），并区分安装 / 升级 / 已是最新。若本地版本更新则保留而不降级。选项：`--force`、`--preview`、`--musl`、`--skip-github`、`--install-dir <dir>`、`--lang en|zh`，以及同名 `DBXT_*` 环境变量（`--help` 列出全部）。

每个 Release 附上各平台压缩包及对应 `.sha256`：

| 文件 | 平台 |
| --- | --- |
| `dbxt-linux-amd64.zip` | Linux x86_64（glibc） |
| `dbxt-linux-arm64.zip` | Linux ARM64（glibc） |
| `dbxt-linux-amd64-musl.zip` | Linux x86_64（静态） |
| `dbxt-linux-arm64-musl.zip` | Linux ARM64（静态） |
| `dbxt-darwin-amd64.zip` | macOS Intel |
| `dbxt-darwin-arm64.zip` | macOS Apple Silicon |
| `dbxt-windows-amd64.zip` | Windows x86_64 |

解压后把 `dbxt` 放入 `PATH`。glibc 构建链接的是 CI 运行器的 glibc，老发行版请优先用静态 `musl` 构建。Android/Termux 无专用产物 —— 静态 aarch64 构建通常能跑，否则 `pkg install rust` 源码编译。

源码构建（需要 Rust 1.85+ 与 C 工具链；首次构建会编译 DBX 内核及多个 C 依赖）：

```bash
cargo install --git https://github.com/vst93/dbxt
# 或：git clone https://github.com/vst93/dbxt && cd dbxt && cargo build --release
```

发版由 GitHub Actions 完成：`gh workflow run release.yml`（可加 `-f version=0.2.0`）把最新 tag 的 patch 加一、打 tag、创建 Release 并构建全部七个压缩包。`dbxt --version` 与 `dbxt --help` 不启动 TUI 即可输出。

## 快速上手

1. 运行 `dbxt` —— 读取与 DBX 桌面端相同的存储。
2. 选择连接（`↑` `↓` + `Enter`），或按 `c` 新建。
3. 在表上按 `Enter` 浏览数据；`/` 过滤表名，`d` 切换数据库。
4. `e` 编辑单元格、`i` 插入、`Delete` 删除 —— 每次执行前展示完整 SQL。
5. `F5` 执行编辑器 SQL；`?` 打开完整快捷键帮助。

## 快捷键速查

完整列表在 TUI 的 `?` 浮层与 `dbxt --help` 中；这里只列核心键。

| 场景 | 按键 |
| --- | --- |
| 全局 | `?` 帮助 · `Tab`/`Shift-Tab` 切栏 · `Alt-1/2/3` 聚焦 · `F5`/`Ctrl-J` 执行 · `Ctrl-C` 退出 |
| 连接 | `↑` `↓` 移动 · `Enter` 连接 · `c` 新建 · `p` 复制 · `d` 数据库列表 · `o` 返回选择 |
| 侧栏 | `↑` `↓` 表 · `/` 过滤 · `Enter` 浏览 · `r` 表结构 · `I` 导入 CSV · `t` 最近表 |
| 结果区 | `↑` `↓` 行 · `←` `→` 列 · `n`/`p` 翻页 · `Enter`/`v` 单元格 · `e` 编辑 · `i` 插入 · `Delete` 删除 |
| 结果区（续） | `f` 过滤 · `s` 排序 · `Ctrl-K` 追加排序 · `Ctrl-R` 清除 · `y` 复制行 · `/` 搜索 · `Ctrl-Y` 导出 · `[` `]` 标签 |
| Redis | `Space` 多选 · `a` 全选 · `Del`/`x`/`m` 批量删除/TTL/重命名 · `/` MATCH · `n` 更多 · `e` 编辑 · `Enter` 查看 value |
| MongoDB | `e` 编辑 · `i` 插入 · `Del` 删除 · `f` 过滤 · `n`/`p` 翻页 · `r` 索引 |
| 浮层 | `Enter`/`y` 确认 · `Esc`/`n` 取消 · `↑` `↓` 滚动 |

## 持久化与配置

按表偏好（列宽压缩、隐藏列、排序）写入 `~/.config/dbxt/tui.json`，以 `库.表` 为键；`DBXT_CONFIG` 可覆盖路径，`DBXT_NO_PERSIST=1` 可关闭。`DBXT_LANG=en|zh` 选择界面语言（未设置时由 locale 决定），`DBX_DATA_DIR` 指定其他 DBX 存储，`DBXT_INSTALL_DIR` 是安装脚本的目标目录。

## 状态与路线图

早期但已可用。已对真实 MySQL 8.4、PostgreSQL 16、Redis 和 MongoDB 端到端实测。

**PostgreSQL** —— SQL 后端全程按 PostgreSQL 方言工作。标识符用双引号（`"schema"."table"`，含保留字）；`bytea`（`0x…` 单元格可回写为 `'\x…'::bytea`）、`uuid`、`jsonb`、数组（`ARRAY[…]`）与枚举均完整显示，复制/导出为合法 SQL；插入模板跳过 `serial`/identity 列，让序列保持权威；`Ctrl-P` 走普通 `EXPLAIN`（绝不用 `EXPLAIN ANALYZE`），写语句只生成计划、不实际执行。表结构与 DDL 读取 `pg_catalog`（`information_schema` 兜底），DDL 会解析关系的可见 schema（`CREATE TABLE "public"."accounts"`，含索引、约束与注释）。浏览范围是连接的 `public` schema；其他 schema（如 `inv.items`）暂不列出。

- [x] SQL 浏览、编辑、事务、过滤 / 排序、补全、CSV 导入 / 导出与结果标签
- [x] Redis key 浏览器与批量 key 操作
- [x] MongoDB 文档浏览器与文档 CRUD
- [x] 七个平台的预编译包 —— 目前只有 Linux x86_64 在本机实测，其余未验证
- [ ] TUI 内编辑 / 删除连接（复制已支持 `p`）
- [ ] 结果导出 XLSX，以及跨页搜索
- [ ] Excel（`.xlsx`）导入
- [ ] 官方 Android/Termux 构建

## 许可

Apache-2.0，与 DBX 相同。dbxt 是基于 [t8y2](https://github.com/t8y2) 的 [DBX](https://github.com/t8y2/dbx) 内核构建的独立客户端；不是 fork，与 DBX 项目无隶属或背书关系。
