# dbxt

[English](README.md) | **中文**

键盘优先的终端数据库客户端，单个静态二进制，基于 [DBX](https://github.com/t8y2/dbx) 内核构建，与 DBX 桌面端共享连接存储。

## Why dbxt · 为什么用 dbxt

- 单个静态二进制 —— 无桌面端、无守护进程、无 HTTP 服务。
- 与 DBX 桌面端共享 `dbx.db` 连接存储：在任何一方配置过的连接都会自动出现。
- 内置 MCP 服务（`dbxt mcp`），把 DBX 原生的 `dbx_*` 工具提供给 AI agent。
- 支持 MySQL、PostgreSQL、Redis、MongoDB、SQLite，其他引擎可经内核接入。
- 写操作安全优先：只读模式、红色确认层，且每次写入都先展示完整 SQL。

## 30-second quick start · 30 秒上手

1. 运行 `dbxt`，`↑` `↓` + `Enter` 选连接（`c` 新建）。
2. `↑` `↓` + `Enter` 打开表；侧栏直接打字即过滤。
3. `Tab` 到 SQL 编辑器输入语句，`Ctrl-J` 跑光标处、`F5` 跑整段。
4. 结果区 `e` 编辑单元格、`i` 插入、`Delete` 删除 —— 执行前展示完整 SQL。
5. `?` 打开当前栏迷你速查，`F1` 看全部键位。
6. 连接列表页顶部是今日 Tip，按 `T` 换下一条。

## Install · 安装

一键脚本 —— bash（Termux 同样适用）或 PowerShell：

```bash
curl -fsSL https://raw.githubusercontent.com/vst93/dbxt/refs/heads/master/cmd/install.sh | bash
```

```powershell
irm https://raw.githubusercontent.com/vst93/dbxt/master/cmd/install.ps1 | iex
```

国内加速：把地址换成 `https://cdn.jsdelivr.net/gh/vst93/dbxt@master/cmd/install.sh`（或 `.../install.ps1`）。

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

源码构建（需要 Rust 1.85+ 与 C 工具链；首次构建会编译 DBX 内核及多个 C 依赖）：

```bash
cargo install --git https://github.com/vst93/dbxt
```

`dbxt --version` 与 `dbxt --help` 不启动 TUI 即可输出；`dbxt [DBX_STORE] --last` 启动即重连上次会话的连接并重新打开其 `库.schema.表`。

## Feature map · 功能地图

**连接管理**
- 连接存放在 DBX 自己的 SQLite 存储（`dbx.db`）里，你在桌面端配置过的连接自动可见。
- `c` 在 TUI 内新建连接，`e` 编辑选中的连接，`p` 复制到表单，`x` 红色确认后删除，`Enter` 连接；`s` 按名称 / 类型 / 颜色排序，`color` 行设置连接色。
- DBX 桌面端的连接分组映射成树里的可折叠 `▾ 名称 [n]` 节点。
- 每个连接根带状态点 —— `●` 活跃 / `○` 已断开 / `◐` 连接中 —— `x` 红色确认后断开池。
- `read_only` 标记在唯一收口拒绝所有写语句，`SELECT` / `SHOW` / `EXPLAIN` 照常。
- `query_timeout` 按连接填秒数 —— 留空沿用内核默认（60s），`0` 表示不限。
- `L` 打开 `.db` / `.sqlite` / `.sqlite3` 文件为会话级临时连接，绝不写入连接配置。
- `Alt-E` 把全部连接导出为 JSON；`Alt-I` 导入 dbxt 自有包、DBeaver `data-sources.json` 或 Navicat `.ncx` 导出。
- SSH 隧道透传 DBX 的 `transport_layers`，支持 `~/.ssh/config` 别名（含 `ProxyJump`），未知主机弹主机密钥确认，失败按阶段报错 —— 认证失败 / 主机不可达 / 远端数据库不可达。

**SQL 编辑与结果**
- `Alt-/` 按光标上下文补全 —— `FROM` / `JOIN` 后只给表，`WHERE` / `ON` / `SELECT` 后只给列。
- `Alt-H` 打开历史面板 —— 重复语句合并为一行并计数 `×n`，`f` 收藏置顶，`Ctrl-Enter` / `p` 直接重跑；`Alt-F` 格式化，`Ctrl-F` 在编辑器内查找。
- `Ctrl-O` 插入收藏片段，`Alt-S` 一步收藏当前 SQL，`Alt-T` 打开内置模板。
- `Alt-↓` / `Alt-↑` 逐条走多语句脚本；`F8` / `Shift-F8` 跳到执行出错的语句。
- 每次执行保留独立结果标签（`[` / `]` 切换，`Alt-W` 关闭）；`Ctrl-P` 执行 `EXPLAIN`，`Ctrl-Y` 导出为 CSV / XLSX / JSON / NDJSON / Markdown / Text / INSERT，可复制到剪贴板或写文件。
- 多语句脚本批量执行，逐条一行结果并带耗时汇总。

**表格数据**
- 有主键时翻页走 keyset（主键续读），大表在任何页都快；行数按表 + 过滤条件在会话内缓存。
- `e` / `i` / `Delete` 编辑、插入、删除 —— 每次写入先展示完整 SQL。
- `V` 进入行选模式，`Ctrl-U` 把整段选区的当前列批量置为同一个值。
- `f` 过滤、`s` 排序、`g w` / `g W` 按内容适配当前列 / 全部可视列宽度。
- `Enter` / `o` 打开行弹层，`v` 单元格弹层，`g c` 列结构弹层（就地显示值分布）。
- `Y` 复制当前单元格值；`y` 把当前行复制为 `INSERT`。

**结构与对比**
- `g d` 查看表结构；`g c` 列结构弹层（`D` 完整 DDL，`y` 把结构复制为 Markdown）；连接树上 `E` 导出数据字典。
- `Alt-D` 对比两张表结构（可跨连接）；`Shift+Alt-D` 对比两个库的表清单。
- `Alt-K` 按主键对齐对比两表数据，跨方言可用。
- `Alt-T` 把一张表的结构和/或数据搬到另一个连接。
- 所有对比只生成 `ALTER` / 同步脚本，绝不执行。

**Redis**
- 分页 `SCAN` key 浏览器（绝不 `KEYS *`），带类型与 TTL 徽标。
- `t` 循环类型过滤，`Ctrl-T` 按 TTL 排序，`T` 给焦点 key 设 TTL，`M` 采样内存、`Ctrl-M` 按内存排序。
- `Space` 多选后批量删除 / 设 TTL / 重命名，走红色确认层；`Ctrl-L` 打开原生 `redis-cli` 命令台。

**MongoDB**
- collection 浏览器，与 SQL 表列表共用子串过滤 / 首字母跳键位。
- `f` 应用 JSON 过滤，`g f` 跳到首个含该字段的已加载文档，`c` 复制点路径子值。
- `e` / `i` / `Del` 编辑、插入、删除文档，带字段级 diff；`y` 把文档复制为美化 JSON，`r` 查看索引。

**界面**
- `?` 打开当前上下文迷你速查（编辑器里 `?` 是字面字符，改用 `F1`）；面板内 `/` 按键位或功能名过滤。
- 连接树在表节点上用 `>` / `<` 展开可折叠的列清单。
- 鼠标与触屏可用（点击选中、再点确认、双击打开行弹层）；20,000×12 表格滚动约 0.6 ms/帧。
- 所有文案来自同一张表（默认中文，`DBXT_LANG=en` 切换英文）；`F10` 打开静态关于弹窗。

## Keys (essentials) · 快捷键速查

完整列表在 TUI 的 `?` 浮层与 `dbxt --help` 中；这里只列核心键。

| 场景 | 按键 |
| --- | --- |
| 全局 | `?` 当前上下文迷你速查（再按一次进全量帮助，面板内 `/` 过滤） · `F1` 任意上下文打开本帮助 · `Tab`/`Shift-Tab` 切栏 · `Alt-←`/`Alt-→` 在表 / 集合 / Redis key 间后退前进 · `F5` 执行整段 · `Ctrl-J` 执行光标处语句（有选区则都只跑选区） · `Alt-Enter` 只执行光标处语句 · `Ctrl-L` 切换命令模式 SQL → Redis → MongoDB · `Ctrl-A` 自动折叠非焦点栏（行选模式下为全选本页） · `Ctrl-W` 收起 / 展开当前栏 · `Ctrl-O` DBX SQL 片段 · `Ctrl-P` EXPLAIN（SQL） · `Ctrl-S`/`Ctrl-X` 提交 / 清空批量队列 · `F10` 关于弹窗 · `q`/`Ctrl-C` 退出（编辑器有未执行语句时两段确认） |
| 连接 | `↑`/`↓` 移动 · `Enter` 连接 · `Alt-1..9` 直切第 N 个连接（智能恢复上次库/表） · `Alt-Tab`/`` Alt-` `` 与上一个连接对切 · `Alt-Shift-H` 最近连接列表（本会话最多 8 个） · `c` 新建 · `e` 编辑 · `p` 复制 · `Y` 复制为 `xxx-copy` · `x` 删除 / 断开（红色确认） · `P` 健康探测 · `Ctrl-P` 全量探测全部已保存连接 · `L` 打开 SQLite 文件 · `s` 排序（名称/类型/颜色） · `d` 数据库/schema 切换，在选择列表里则断开高亮连接 · `!` 切换只读开关 · `T` 换下一条今日 Tip |
| 编辑器 | `Alt-H` 历史面板（`Enter` 回填 · `Ctrl-Enter`/`p` 直跑 · `y`/`Y` 复制 · 青色 `●` 为本次会话内存记录） · `Alt-G` 全库搜索 · `Alt-L` 执行 `.sql` 文件 · `Alt-F` 格式化/压缩 · `Ctrl-Z`/`Ctrl-U` 撤销 · `Ctrl-Y`/`Ctrl-R` 重做 · `Alt-/` 补全 · `Alt-Enter` 执行光标处语句 · `Alt-↓`/`Alt-↑` 逐条走语句 · `F8`/`Shift-F8` 跳到本次执行的出错语句 · `Ctrl-F` 编辑器内查找 · `Ctrl-/`/`Alt-C` 切换 `-- ` 行注释 · `Ctrl-Shift-V` 从会话剪贴板环粘贴 · `Alt-P` 片段插到光标 · `Alt-T` 内置 SQL 模板 · `%` 跳配对括号 · `F2` 语句序号栏 · `F5` 执行整段 · `Ctrl-J` 执行光标处语句 · `↑`/`↓` 历史 |
| 结果区 | `↑`/`↓` 行 · `←`/`→` 列 · `Home`/`End`/`gg`/`G` 首行/末行 · `:` 按行号跳行（`:$` 末行） · `n`/`p` 翻页 · `PgUp`/`PgDn` 整屏翻页、`Ctrl-U` 半屏向上 · `gd`/`gt` 表结构/表数据 · `g b` 同库表切换 · `g w`/`g W` 适配列宽 · `g c` 列结构弹层 · `D` 完整 DDL · `Alt-F` 钉住/解除结果区 · `gv` 定位值 · `|` 跳列 · `g f`/`z` 冻结列 · `g s` 钉参照行 · `#` 大数字显示 · `Enter`/`o` 整行弹层 · `v` 单元格弹层 · `\` 在单元格里找词 · `*` 按当前列过滤 · `%` 奇偶行斑马纹 · `S` 数值快照 · `y` 复制行（INSERT） · `Y` 复制当前单元格值 · `V` 行选模式（`Ctrl-U` 批量置值，`d`/`c` 生成 DELETE/UPDATE 模板） · `<`/`>` 列宽 · `Ctrl-Y` 导出 · `[`/`]` 标签 · `Ctrl-Shift-D` 结果集快照 |
| 结构 / 对比 | `r` 表结构 · `t` 切换 DDL · `D` 完整 DDL 弹层 · `Alt-D` 结构对比（`c` 换连接） · `Shift+Alt-D` 两库表清单对比 · `Alt-K` 数据对比（`m` 切换 结构/数据，`w` WHERE） · `Alt-T` 编辑器之外的数据搬运向导 · `Tab` 切栏 · `y` 复制摘要 · `g` 生成脚本 · `Esc` 关闭 |
| Redis | `Space` 多选 · `a` 全选 · `Del`/`x`/`m` 批量删除/TTL/重命名 · `T` 设 TTL · `t` 类型过滤 · `Ctrl-T` TTL 排序 · `M` 采样内存 · `Ctrl-M` 内存排序 · `Shift-M` 清缓存 · `/` MATCH · `Alt`+字母 首字母跳 · `n` 更多 · `e` 编辑 · `Enter` 查看 value |
| MongoDB | `e` 编辑 · `i` 插入 · `Del` 删除 · `f` JSON 过滤 · `g f` 字段跳转 · `Ctrl-S` 大小排序 · `c` 按路径提取 · `y` 复制 JSON · `n`/`p` 翻页 · `r` 索引 |
| 浮层 | `Enter`/`y` 确认 · `Esc`/`n` 取消 · `↑`/`↓` 滚动 · 每次 `Esc` 状态栏闪 1.5 秒提示 |

## MCP server (`dbxt mcp`) · MCP 服务

`dbxt mcp` 由 dbxt 二进制直接运行 DBX 原生的 MCP 服务。它不是重新实现：服务端就是官方 `dbx-mcp` 所服务的同一个 `dbx_mcp::DbxMcpServer` 结构，因此工具、资源、会话、事务与 `Settings → MCP` 策略完全一致。

| 命令 | 传输 | 适用 |
| --- | --- | --- |
| `dbxt mcp` | stdio（换行分隔 JSON-RPC） | Claude Code、Cursor、Codex 等由客户端 spawn 命令的客户端 |
| `dbxt mcp --http` | Streamable HTTP（仅回环 + Bearer 令牌） | 支持 HTTP 的客户端、Docker、长期端点 |

stdio 客户端配置（command 用 `dbxt` 的绝对路径；GUI 客户端不一定继承 `PATH`）：

```json
{ "mcpServers": { "dbx": { "command": "/absolute/path/to/dbxt", "args": ["mcp"] } } }
```

HTTP 只监听回环地址且始终要求 Bearer 令牌；`dbxt mcp --help` 列出全部选项与环境变量（`DBX_DATA_DIR`、`DBX_SECRET_KEY_FILE`、`DBX_MCP_HTTP_TOKEN` 等）。MCP 权限、白名单与库范围就是 DBX（`Settings → MCP`）里保存的那一套，每次调用重新校验。

## Configuration · 配置

- `~/.config/dbxt/tui.json` 保存界面偏好与按表记忆的列宽（上限 200 条、LRU 淘汰）。
- `~/.config/dbxt/last-session.json` 记录上次退出时的位置；`--last` 启动即自动恢复。
- `DBXT_LANG=en|zh` 选择界面语言（未设置时由 locale 决定）。
- `DBX_DATA_DIR` 指定其他 DBX 存储。
- `DBXT_NO_PERSIST=1` 关闭会话与界面配置文件。

## Secret Store compatibility · Secret Store 兼容

- dbxt 基于 DBX v0.6.34 构建；自 v0.6.27 起，内核对连接 / 插件 / AI / 隧道等敏感字段加密落库，密钥存放在数据库之外。
- 遇到旧的明文库时，dbxt 在启动阶段执行内核自带的数据安全升级 —— 备份 → 加密 → 校验 —— 升级后的库需要 DBX v0.6.21+ 才能读取。
- 密钥来源（内核解析顺序）：`DBX_SECRET_KEY_FILE`（密钥文件）、`DBX_SECRET_KEY`（环境变量）、系统钥匙串、内核管理的 `<data-dir>/.dbx/secret.key`。
- 升级时 dbxt 按桌面向导的方式创建密钥，绝不覆盖已有密钥；若库里已有密文却找不到对应密钥，dbxt 会停下要求提供原密钥。
- 系统钥匙串存在但处于锁定状态时，dbxt 捕获 `KEYRING_WRITE_FAILED` / `KEYRING_ACCESS_FAILED`，回退到内核受管密钥文件，并在之后每次启动自动认领。
- 基于加密前内核（v0.6.20 及更早）构建的 dbxt 无法读取已升级的库。

## Status · 状态与路线图

- 已对真实 MySQL 8.4、PostgreSQL 16、Redis 和 MongoDB 端到端实测。
- 七个平台的预编译包；目前只有 Linux x86_64 在本机实测，其余六个在 CI 构建但未验证。
- 路线图：跨页搜索、Excel（`.xlsx`）导入、官方 Android/Termux 构建。

## Known issues · 已知问题

- **表尺寸元数据仅支持 MySQL / PostgreSQL** —— 在其他引擎的库行上按 `s` 会提示不支持。
- **全局搜索（`Alt-G`）仅支持 MySQL / PostgreSQL。**
- **不支持 `.xlsx` 导入** —— 有意不做，请使用 CSV；*导出*为 XLSX 已支持。
- **搜索是页内搜索** —— `/` 只搜可见行，不搜整个结果集；跨页结果搜索尚未实现。
- **大结果导出只警告不阻断** —— 结果集超过 10,000 行时 `Ctrl-Y` 会先提示。
- **只读判定保守失败** —— 未知动词一律算写语句，因此极少数“看着像读”的语句会被按设计拒绝。
- **Redis / MongoDB 不在范围内** —— 不参与 `Alt-T` 搬运向导，也不参与 `Alt-D` / `Alt-K` 的对比。
- **预编译包只在本机验证过 Linux x86_64** —— 其余六个平台在 CI 构建但未实测。
- **没有独立的 Android/Termux 产物** —— 静态 aarch64 构建通常可用，否则从源码构建。
- 连接树**每会话只读一次**桌面端分组布局，因此在 DBX 桌面端重新分组后，下次打开 dbxt 才会生效。

## License · 许可

Apache-2.0，与 DBX 相同。dbxt 是基于 [t8y2](https://github.com/t8y2) 的 [DBX](https://github.com/t8y2/dbx) 内核构建的独立客户端；不是 fork，与 DBX 项目无隶属或背书关系。
