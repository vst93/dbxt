# dbxt

[English](README.md) | **中文**

基于 [DBX](https://github.com/t8y2/dbx) 内核的终端数据库 TUI，键盘优先。连接只需配置一次 —— 在 DBX 桌面端、DBX CLI 或 dbxt 本身 —— 之后任何终端都能用。单个静态二进制：无桌面端、无守护进程、无 HTTP 服务。

## 功能特色

**连接管理** —— 与 DBX 桌面端共享
- 连接存放在 DBX 自己的 SQLite 存储（`dbx.db`）里，你在桌面端配置过的连接自动可见。
- `c` 在 TUI 内新建连接，`e` 编辑选中的连接（预填表单，含 SSH 隧道），`p` 复制到表单，`x` 删除连接（红色确认），`Enter` 连接；连接按数据库家族着色。
- 连接表单新增 `color` 行：`Space` 循环预设调色板（无色 → 10 色 → 自定义），`Enter` 输入自由 `#RRGGBB`，右侧色块实时预览。颜色写入 DBX 自己的 `color` 字段，与桌面端互通，编辑与复制全程保留，并贯穿侧栏、连接选择器、标题栏、状态栏与 `d` 切换层头部（颜色只作辅助，名称始终完整可读）。
- 选择器里按 `s` 循环排序：**名称**（默认）→ **类型** → **颜色**，生产（红色）连接一键聚到一组。
- `d` 弹出切换层 —— MySQL 库、PostgreSQL（及其他支持 schema 的引擎）先 schema 后库、MongoDB 库、Redis 逻辑 db 同一套手势。
- `o` 返回连接选择，`r` 原地刷新列表。

**SSH 隧道（跳板机）**
- 在 DBX 桌面端配好的带隧道连接在 dbxt 里直接可用：dbxt 把 `transport_layers` 原样透传给内核，无需重新录入。
- 连接表单新增 `ssh_tunnel` 段：`ssh_host` / `ssh_port`（22）/ `ssh_user`，以及 `ssh_auth` 登录方式 —— `password`、`key`（密钥路径 + 口令）或 `agent`（SSH agent，可填 socket 路径）。
- `ssh_host` 支持 `~/.ssh/config` **别名**；由内核解析，含 `ProxyJump`（自动展开为多跳）。
- 隧道转发到连接自身的 `host:port`，表单以 `远端目标` 显示；需要改转发目标就改连接的 `host` / `port`。
- 首次连接未知跳板机弹出主机密钥指纹确认（`y`/`Enter` 接受并记住，`s` 仅本次会话，`n`/`Esc` 拒绝）。已接受的密钥写入 DBX 自己的 `<存储目录>/known_hosts`；`~/.ssh/known_hosts` 只读不写。
- 隧道失败按阶段明确报错：**SSH 认证失败**、**SSH 主机不可达**、**隧道已建立但远端数据库不可达** —— 密码错误、堡垒机地址错误、对端端口不通三者互不混淆。

**SQL 编辑与结果**
- 多行编辑器，shell 风格 `↑`/`↓` 历史（从 DBX 共享查询历史初始化）；`F5` / `Ctrl-J` 执行。每次执行都会写回该共享历史（含连接、数据库、耗时与成功标记），因此回溯列表与 `Alt-H` 面板同样覆盖你在 dbxt 里跑过的语句。
- `Ctrl-Space` 按上下文补全标识符（表名 / 列名 / 关键字，标注 `T`/`C`/`K`）；`Tab` 上屏。
- `Alt-H` 打开**查询历史面板**（最近 300 条，最新在前，按 SQL 去重）：每行显示时间、语句首行摘要与来源连接。`↑`/`↓`/`PgUp`/`PgDn` 移动，`Enter` 回填到编辑器（光标到末尾），`f` 在 DBX `saved_sql_files` 中收藏 / 取消收藏，`y` 复制整条语句，`Del` 经红色确认后删除单条（仅删历史，不动数据库数据），`/` 按语句内容过滤（大小写不敏感子串）。光标处语句在列表下方自动换行预览。
- `Alt-F` 格式化编辑器 SQL —— 关键字大写、主子句换行、`JOIN` 单独一行、缩进 2 空格、空白收敛 —— 字符串字面量、引号标识符、注释与函数名（`count(`）保持原样。再按一次将已格式化语句压缩回单行（幂等切换）；`Ctrl-U` 撤销本次格式化。
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
- `Alt-C` 压缩列宽，`Alt-V` 隐藏列，`Alt-R` 直达最近表，`Alt-H` 打开查询历史 —— 选择按 `库.表` 持久化。
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

数据库在堡垒机后面时：按 `c`，把 `ssh_tunnel` 设为 `y`，填写 `ssh_host` / `ssh_user` 与登录方式，光标移到保存行按 `Enter`。此后连接链路为 dbxt → 跳板机 → 数据库。

## 快捷键速查

完整列表在 TUI 的 `?` 浮层与 `dbxt --help` 中；这里只列核心键。

| 场景 | 按键 |
| --- | --- |
| 全局 | `?` 帮助 · `Tab`/`Shift-Tab` 切栏 · `Alt-1/2/3` 聚焦 · `F5`/`Ctrl-J` 执行 · `Ctrl-C` 退出 |
| 连接 | `↑` `↓` 移动 · `Enter` 连接 · `c` 新建 · `e` 编辑 · `p` 复制 · `s` 排序（名称/类型/颜色） · `x` 删除 · `d` 数据库/schema 切换 · `o` 返回选择 |
| 连接表单 | `↑` `↓`/`Tab` 切换字段 · `Enter` 编辑/切换/保存 · `Space` 切换 `ssh_tunnel`/`ssl`/`ssh_auth`、循环 `color` 调色板 · `Esc` 返回 |
| SSH 主机密钥 | `y`/`Enter` 接受并记住 · `s` 仅本次会话 · `n`/`Esc` 拒绝 |
| 侧栏 | `↑` `↓` 表 · `/` 过滤 · `Enter` 浏览 · `r` 表结构 · `I` 导入 CSV · `t` 最近表 |
| 编辑器 | `Alt-H` 历史面板 · `Alt-F` 格式化/压缩 · `Ctrl-U` 撤销格式化 · `Ctrl-Space` 补全 · `F5`/`Ctrl-J` 执行 · `↑` `↓` 历史 |
| 结果区 | `↑` `↓` 行 · `←` `→` 列 · `n`/`p` 翻页 · `Enter`/`v` 单元格 · `e` 编辑 · `i` 插入 · `Delete` 删除 |
| 结果区（续） | `f` 过滤 · `s` 排序 · `Ctrl-K` 追加排序 · `Ctrl-R` 清除 · `y` 复制行 · `/` 搜索 · `Ctrl-Y` 导出 · `[` `]` 标签 |
| Redis | `Space` 多选 · `a` 全选 · `Del`/`x`/`m` 批量删除/TTL/重命名 · `/` MATCH · `n` 更多 · `e` 编辑 · `Enter` 查看 value |
| MongoDB | `e` 编辑 · `i` 插入 · `Del` 删除 · `f` 过滤 · `n`/`p` 翻页 · `r` 索引 |
| 浮层 | `Enter`/`y` 确认 · `Esc`/`n` 取消 · `↑` `↓` 滚动 |

## 持久化与配置

按表偏好（列宽压缩、隐藏列、排序）写入 `~/.config/dbxt/tui.json`，以 `库.表` 为键；`DBXT_CONFIG` 可覆盖路径，`DBXT_NO_PERSIST=1` 可关闭。`DBXT_LANG=en|zh` 选择界面语言（未设置时由 locale 决定），`DBX_DATA_DIR` 指定其他 DBX 存储，`DBXT_INSTALL_DIR` 是安装脚本的目标目录。

SSH 隧道的单测（序列化形状、表单映射、认证/错误分类、主机密钥提示）随 `cargo test` 运行。另有两个端到端测试会驱动真实隧道（dbxt → 本机 `sshd` → MySQL），默认跳过，需 `DBXT_SSH_TEST=1` 开启；它们读取 `DBXT_SSH_TEST_USER` / `_PASSWORD` / `_KEY`、`DBXT_SSH_TEST_MYSQL_PORT`（默认 13306）与 `DBXT_SSH_TEST_MYSQL_USER` / `_PASSWORD`。

## 状态与路线图

早期但已可用。已对真实 MySQL 8.4、PostgreSQL 16、Redis 和 MongoDB 端到端实测。

**PostgreSQL** —— SQL 后端全程按 PostgreSQL 方言工作。标识符用双引号（`"schema"."table"`，含保留字）；`bytea`（`0x…` 单元格可回写为 `'\x…'::bytea`）、`uuid`、`jsonb`、数组（`ARRAY[…]`）与枚举均完整显示，复制/导出为合法 SQL；插入模板跳过 `serial`/identity 列，让序列保持权威；`Ctrl-P` 走普通 `EXPLAIN`（绝不用 `EXPLAIN ANALYZE`），写语句只生成计划、不实际执行。表结构与 DDL 读取 `pg_catalog`（`information_schema` 兜底），DDL 会解析关系的可见 schema（`CREATE TABLE "public"."accounts"`，含索引、约束与注释）。浏览全程感知 schema：`d` 列出当前库的 schema（schema 在前、库在后），侧栏显示 `inv.items` 式全限定名，表格数据、表结构、DDL、单元格编辑、插入、删除、过滤、排序、CSV 导入/导出与 INSERT 导出都会带上所选 schema —— `public.orders` 与 `inv.orders` 不会串。列显隐、排序与 `COUNT(*)` 缓存均按 `database.schema.table` 分键。

- [x] SQL 浏览、编辑、事务、过滤 / 排序、补全、CSV 导入 / 导出与结果标签
- [x] Redis key 浏览器与批量 key 操作
- [x] MongoDB 文档浏览器与文档 CRUD
- [x] SSH 隧道（密码 / 密钥 / agent，`~/.ssh/config` 别名与 `ProxyJump`），TUI 内可新建与编辑
- [x] 七个平台的预编译包 —— 目前只有 Linux x86_64 在本机实测，其余未验证
- [x] TUI 内删除连接（红色确认；只删配置，不删数据库数据）
- [ ] 结果导出 XLSX，以及跨页搜索
- [ ] Excel（`.xlsx`）导入
- [ ] 官方 Android/Termux 构建

## 许可

Apache-2.0，与 DBX 相同。dbxt 是基于 [t8y2](https://github.com/t8y2) 的 [DBX](https://github.com/t8y2/dbx) 内核构建的独立客户端；不是 fork，与 DBX 项目无隶属或背书关系。
