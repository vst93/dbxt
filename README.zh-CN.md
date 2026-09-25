# dbxt

[English](README.md) | **中文**

基于 [DBX](https://github.com/t8y2/dbx) 内核的终端数据库客户端（TUI），键盘和鼠标/触屏都支持。连接只需配置一次 —— 在 DBX 桌面端、DBX CLI 或 dbxt 本身 —— 然后任何终端都能用。

## 状态

早期但已可用。已对真实 MySQL 8.4、Redis 和 MongoDB 端到端实测：

- ✅ 启动、连接选择、TUI 内新建连接
- ✅ 连接 MySQL，浏览库/表
- ✅ **表格数据浏览**：侧栏 `Enter` 自动执行分页 `SELECT *`（每页 50 行，`n`/`p` 翻页），带行号列、按内容自适应列宽，并显示总行数
- ✅ **表结构**：`r` 显示字段列表（类型 / 键 / 可空 / 默认值 / 注释）；`t` 切换 `SHOW CREATE TABLE` DDL（由 DBX 内核按方言生成）
- ✅ **结果网格**：`h`/`l` 滚动列时表头同步跟随；`NULL`（斜体）与空串（`''`）视觉区分；显示耗时与受影响行数
- ✅ **DML**：`INSERT`/`UPDATE`/`DELETE` 返回受影响行数；`DROP`/`TRUNCATE` 与无 `WHERE` 的 `UPDATE`/`DELETE` 先弹出红色确认
- ✅ **多语句脚本**：`a; b; c;` 批量执行，逐条列出结果，`Enter` 下钻看单条结果集
- ✅ **SQL 编辑器**：多行编辑，shell 风格 `↑`/`↓` 历史（从 DBX 共享查询历史初始化），执行后焦点自动到结果区
- ✅ MySQL 服务端错误原样回显在状态栏
- ✅ 120×32（桌面）与 42×22（窄窗格 / 手机竖屏）自适应布局
- ✅ Redis：连接、`SET`/`GET`/`KEYS`/`DBSIZE`、带引号参数、`[` `]` 切换 db 均已端到端实测
- ✅ MongoDB：`db.col.find({})`、`use <db>` 切库、多行输出均已端到端实测
- ⚠️ 跨平台发布构建（Windows / macOS / Android-Termux）规划中，未验证

## 与 DBX 的关系

本项目离不开 [DBX](https://github.com/t8y2/dbx)（作者 t8y2，Apache-2.0）。它**不是** fork，也无关联 —— 而是**以库依赖方式内嵌** DBX 的 Rust 内核，在其上加了一层 TUI 界面。

```
DBX 桌面端 (Tauri)   DBX CLI   DBX MCP   dbxt (本项目)
        │                 │         │            │
        └────────────┬────┴─────────┴─────┬──────┘
                     ▼                    ▼
              dbx-core (业务编排层)       dbx-mcp (LocalBackend)
                     │
              90+ 数据库原生驱动
                     │
              共享连接存储：dbx.db
```

- `dbx-core` + `dbx-mcp` 以 **git 依赖方式固定在 tag `v0.6.9`**。dbxt 进程内直接调用 `dbx_mcp::backend::LocalBackend`：连接增删查改、元数据、SQL 执行、批量、事务、Redis 和 MongoDB 命令，全部走 DBX 桌面端同一套代码路径。
- **无需桌面端、无 Node.js、无守护进程、无 HTTP 服务。** 单个静态二进制，一切发生在终端里。
- `Cargo.toml` 的 `[patch.crates-io]` 段镜像了 DBX 自己工作区的补丁（gaussdb 兼容的 `tokio-postgres` fork 和 `mysql_async` fork）。Cargo 不会从 git 依赖传播 `[patch]` 段，所以 dbxt 必须重新声明 —— 升级 DBX tag 时，请对照对应版本 DBX 的 `Cargo.toml` 复查这一段。

### 数据库覆盖

遵循 DBX 自身的执行模型：

| 层级 | 数据库 | dbxt 可用？ |
| --- | --- | --- |
| 原生驱动（编译进二进制） | MySQL、PostgreSQL、SQLite、Redis、MongoDB、SQL Server、ClickHouse、Elasticsearch、Doris、StarRocks 等 | ✅ 无头运行，不需要桌面端 |
| 官方 CLI 直连白名单 | postgres、mysql、sqlite、redshift、doris、starrocks、manticoresearch、rqlite、kwdb、questdb | ✅ |
| Agent / JDBC 类型 | Oracle、达梦、DB2、Hive、Snowflake、SAP HANA 等 | ❌ 需要 DBX Agent 运行时（Java），超出范围 |

dbxt 不受官方 CLI 静态白名单限制 —— 那份清单是 `dbx-cli` 里的产品决策，不是内核限制。凡 `LocalBackend` 能原生执行的，这里都能用。

### 连接存储（与 DBX 共享）

所有连接保存在一个 SQLite 文件 `dbx.db` 中，DBX 桌面端、DBX CLI、DBX MCP 和 dbxt 共用。你已配置好的连接会被自动读取。

| 平台 | 默认路径 |
| --- | --- |
| Linux | `~/.local/share/com.dbx.app/dbx.db` |
| macOS | `~/Library/Application Support/com.dbx.app/dbx.db` |
| Windows | `%APPDATA%\com.dbx.app\dbx.db` |
| 便携（全平台） | `$DBX_DATA_DIR/dbx.db` |

备份说明（源自 DBX 的 `storage.rs`，对照 v0.6.9 核实）：

- 一个文件包含全部内容：连接、密码、查询历史、设置。
- 密码在 `connection_secrets` 表中**明文存储**，靠文件 owner-only 权限（600）保护。没有外部密钥，拷到其他机器直接可用 —— 拷完记得 `chmod 600`。
- DBX 运行时可能存在 `-wal`/`-shm` 伴生文件。要么退出 DBX 后再拷贝，要么做一致性快照：`sqlite3 dbx.db ".backup '/备份路径/dbx.db'"`。

## 构建

需要 Rust 1.85+（edition 2021）。

```bash
cargo build --release
```

首次构建会编译完整的 DBX 内核（需几分钟；SQLite 已打包内置，无需系统 sqlite）。release 配置已启用符号裁剪和 thin LTO。

## 使用

```bash
# 默认存储（与 DBX 桌面端相同）
dbxt

# 显式指定存储目录（你下载的备份、便携目录等）
dbxt /path/to/dir-containing-dbx.db
# 或
DBX_DATA_DIR=/path/to/dir dbxt
```

### 快捷键

| 场景 | 按键 | 动作 |
| --- | --- | --- |
| 全局 | `Ctrl-C` | 退出 |
| 全局 | `Ctrl-L` | 切换命令模式：SQL → Redis → MongoDB |
| 全局 | `F5` / `Ctrl-J` | 执行当前 SQL |
| 连接选择 | `↑` `↓` / `Enter` | 选择 / 连接 |
| 连接选择 | `c` | 新建连接表单 |
| 侧栏（已连接） | `↑` `↓` | 移动表列表 |
| 侧栏 | `←` `→` | 切换数据库 |
| 侧栏 | `Enter` | 浏览表格数据（分页 `SELECT *`） |
| 侧栏 | `r` | 查看表结构（字段 + DDL） |
| 侧栏 | `o` | 返回连接选择 |
| 任意位置 | `Tab` | 下一区域（侧栏 → 编辑器 → 结果） |
| 编辑器 | `Enter` | 换行 |
| 编辑器 | `↑` / `↓` | 历史命令（光标在首行 / 末行时） |
| 编辑器 | `Esc` | 回到侧栏 |
| Redis 输入行 | `[` `]` | 切换 Redis db（0/1/2…） |
| MongoDB 输入行 | `use dbname` + `Enter` | 切换数据库 |
| 结果区 | `↑` `↓` `j` `k` | 滚动行 |
| 结果区 | `PgUp` / `PgDn` | 整屏滚动行 |
| 结果区 | `n` / `p` | 下一页 / 上一页（表格数据） |
| 结果区 | `h` `l` | 滚动列 |
| 结果区 | `t` | 切换 字段 ↔ DDL（表结构视图） |
| 结果区 | `Enter` | 打开某条语句的结果（脚本视图） |
| 结果区 | `e` / `Esc` | 回编辑器 / 收起 |
| 确认框 | `Enter` `y` / `Esc` `n` | 执行 / 取消危险语句 |

### 鼠标 / 触屏

终端里的触屏点按以鼠标按下事件传入，因此触屏设备（包括 Android Termux）和 tmux 鼠标穿透都能用：

- 点击行选中；再次点击同一行确认（连接、浏览数据）
- 点击区域（编辑器、命令输入、结果）切换焦点
- 滚轮滚动行；横向滚轮滚动结果列

## 路线图

- [ ] TUI 内编辑 / 删除连接
- [ ] 数据网格的过滤与排序
- [ ] 结果导出（CSV / JSON）
- [ ] 把 dbxt 执行的 SQL 写回 DBX 共享查询历史（目前只读）
- [ ] schema 感知的 SQL 编辑 / 补全
- [ ] 发布构建：Windows、macOS（Intel/Apple Silicon）、Linux（glibc + musl）、Android Termux（aarch64 musl 静态）

## 许可

Apache-2.0，与 DBX 相同。DBX 是 [t8y2](https://github.com/t8y2) 的项目；dbxt 是独立客户端，与 DBX 项目无隶属或背书关系。
