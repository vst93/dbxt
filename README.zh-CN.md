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

**连接导入 / 导出 / 迁移（`Alt-E` / `Alt-I`）**
- `Alt-E` 把**全部**已保存连接导出为自描述 JSON 包（`format` / `version` / `connections[]`，默认 `~/dbxt-connections.json`）。**默认不含密码**；`p` 显式开启（红色明文警告确认），`y` 则复制 JSON 到剪贴板而不写文件。状态栏报告目标路径与条数。
- `Alt-I` 从文件路径导入，自动识别格式 —— dbxt 自有 JSON 包、DBeaver `data-sources.json`（标准路径 `~/.local/share/DBeaverData/workspace6/General/.dbeaver/data-sources.json`）、Navicat `.ncx` XML 导出。预览清单逐条列出（名称 / 引擎 / 主机 / SSH / 颜色 / 需补密码），标出同名冲突，并列出无法映射的驱动（跳过）。
- 同名策略可逐条或整批：`s` 跳过、`r` 覆盖（按 `name` 匹配，红色确认，先删原配置）、`b` 都存（名加 `-imported`）；`Space` 勾选该条、`d` 逐条循环策略、`Enter` 导入。所有写入都经 `LocalBackend`，导入后连接立即可用（照常用连接键验证）。
- **DBeaver** 的 provider / driver 映射到 dbxt 引擎（`mysql8`→`mysql`、`postgresql`→`postgres`、`mariadb`、`duckdb`、`mongodb`、`sqlserver` 等），`ssh-tunnel` 段转为真实 SSH 层。**Navicat** `.ncx` XML 用容错的手写标签扫描解析（子元素或属性均可）。两家的密码均以 dbxt 没有的密钥加密，因此**不解析** —— 导入后标「需补密码」，完成状态汇总计数。

**SSH 隧道（跳板机）**
- 在 DBX 桌面端配好的带隧道连接在 dbxt 里直接可用：dbxt 把 `transport_layers` 原样透传给内核，无需重新录入。
- 连接表单新增 `ssh_tunnel` 段：`ssh_host` / `ssh_port`（22）/ `ssh_user`，以及 `ssh_auth` 登录方式 —— `password`、`key`（密钥路径 + 口令）或 `agent`（SSH agent，可填 socket 路径）。
- `ssh_host` 支持 `~/.ssh/config` **别名**；由内核解析，含 `ProxyJump`（自动展开为多跳）。
- 隧道转发到连接自身的 `host:port`，表单以 `远端目标` 显示；需要改转发目标就改连接的 `host` / `port`。
- 首次连接未知跳板机弹出主机密钥指纹确认（`y`/`Enter` 接受并记住，`s` 仅本次会话，`n`/`Esc` 拒绝）。已接受的密钥写入 DBX 自己的 `<存储目录>/known_hosts`；`~/.ssh/known_hosts` 只读不写。
- 隧道失败按阶段明确报错：**SSH 认证失败**、**SSH 主机不可达**、**隧道已建立但远端数据库不可达** —— 密码错误、堡垒机地址错误、对端端口不通三者互不混淆。

**SQL 编辑与结果**
- 多行编辑器，shell 风格 `↑`/`↓` 历史（从 DBX 共享查询历史初始化）；`F5` / `Ctrl-J` 执行。每次执行都会写回该共享历史（含连接、数据库、耗时与成功标记），因此回溯列表与 `Alt-H` 面板同样覆盖你在 dbxt 里跑过的语句。
- `Alt-/` 按上下文补全标识符（表名 / 列名 / 关键字，标注 `T`/`C`/`K`）；`Tab` 上屏。（`Ctrl-Space` 仍作兼容别名可用，但它与输入法切换冲突，故不再宣传。）
- `Alt-H` 打开**查询历史面板**（最近 300 条，最新在前，按 SQL 去重）：每行显示时间、语句首行摘要与来源连接。`↑`/`↓`/`PgUp`/`PgDn` 移动，`Enter` 回填到编辑器（光标到末尾），`f` 在 DBX `saved_sql_files` 中收藏 / 取消收藏，`y` 复制整条语句，`Del` 经红色确认后删除单条（仅删历史，不动数据库数据），`/` 按语句内容过滤（大小写不敏感子串）。光标处语句在列表下方自动换行预览。
- `Alt-F` 格式化编辑器 SQL —— 关键字大写、主子句换行、`JOIN` 单独一行、缩进 2 空格、空白收敛 —— 字符串字面量、引号标识符、注释与函数名（`count(`）保持原样。再按一次将已格式化语句压缩回单行（幂等切换）；`Ctrl-U` 撤销本次格式化。
- 编辑器保留 vim / readline 手感：光标在 `()[]{}` 括号上或旁边时 `%` 跳到配对括号（其余位置照常输入 `%`，`LIKE '%x%'` 不会被抢；字符串与注释里的括号不计入配对），`Ctrl-A`/`Ctrl-E` 到行首/行尾（`Home`/`End` 同），`Ctrl-K` / `Ctrl-Shift-K` 删至行尾，`Ctrl-W` 删前一个词。
- `Alt-G` 打开**全库搜索**：在当前数据库（PostgreSQL 还含当前 schema）的所有 `char` / `varchar` / `text` 列中大小写不敏感地搜索一个词，命中以 `表.列 → 值` 列出并高亮匹配片段。每张表执行一条带 `LIMIT` 的 `SELECT`（`DBXT_SEARCH_SCAN_LIMIT`，默认 1000），估算行数超过 `DBXT_SEARCH_MAX_ROWS`（默认 1,000,000）的表会跳过并标注。`↑`/`↓` 移动，`Enter` 跳到该表并定位到命中行，`y` 复制命中值，`r` 重搜，`Esc` 中止正在进行的扫描（保留已扫描的部分结果）。仅支持 MySQL / PostgreSQL。
- `Alt-L` 加载并执行 **`.sql` 文件**：输入路径（开头 `~` 会展开），确认预览（文件大小 / 语句数 / 目标连接与库，下方是脚本预览）。超过 2 MB 给警告；含 `DROP`、`TRUNCATE` 或无 `WHERE` 的 `UPDATE`/`DELETE` 的文件会先走红色确认层。整个文件作为一条历史记录收录，结果即常规的分语句脚本汇总（错误信息内联）；`e` 可将文件载入编辑器。
- 每次执行保留独立结果标签（`[` / `]` 切换）；显示耗时与受影响行数。
- `Ctrl-P` 执行 `EXPLAIN`，`Ctrl-Y` 导出当前结果（CSV / JSON / NDJSON / Markdown / INSERT），`Ctrl-N` 在结果被行数上限截断时加载更多。
- `Ctrl-O` 插入 DBX 收藏片段；`s` 把编辑器 SQL 存回共享收藏库。
- 多语句脚本（`a; b; c;`）批量执行，逐条列出结果，`Enter` 下钻单条。

**表格数据与编辑**
- 侧栏 `Enter` 执行分页 `SELECT *`；`↑`/`↓` 连续走行并跨页自动衔接，`n`/`p` 翻页时保持相对行号。
- 侧栏表列表边输边筛：任意可打印字符一步直接进入大小写不敏感的子串过滤（`/` 打开空过滤），`Enter` 打开第一个命中，`Ctrl-U` / `Alt-Backspace` 清除；首字母跳（`Alt`+字母，再用 `;`/`,` 前后循环）在长列表里快速跳过；`s` 循环 名称 / 类型 排序。
- 侧栏是一棵**连接树**：`连接 → 库 → 表` 三级缩进（DBeaver 式），`h`/`l`（或 `←`/`→`）折叠 / 展开，`Enter` 打开（连接=切换并展开、库=切到该库、表=浏览数据），折叠状态在会话内记忆。展开非当前连接会**懒连接**并异步列出它的库（失败显示错误行，不影响整棵树）；`j`/`k` 在整棵树上移动（计数前缀 `3j`）。过滤器命中表名或库名时保留其父节点；`[`/`]` 快速切库。
- 连续翻页在有主键时优先走 **keyset（主键续读）**：下一页是 `WHERE pk > last ORDER BY pk LIMIT n`，上一页反向 seek，因此翻页耗时不再随页码增长（千万级表的首页与第 2 万页都在毫秒级）。不是正好相邻一页的跳页、无主键表、以及对非主键列的排序仍走经典 `LIMIT … OFFSET`。未显式排序时按主键排序，使页边界确定（无主键表保持引擎自然顺序）。
- 行数按 `库.schema.表` + 过滤条件在会话内缓存。大表首次加载最多采样 `DBXT_COUNT_SAMPLE_LIMIT` 行（默认 50 万），超过就显示 `>500000 行`，不再每页做全表 `COUNT(*)` 扫描；设 `DBXT_COUNT_SAMPLE_LIMIT=0` 可恢复始终精确计数。小表仍显示精确总数。
- `←`/`→` 移动单元格光标，`z` 钉住首个数据列，`Enter`（或 `o`）弹出**整行**的纵向 `列 = 值` 列表 —— 看某一行从这里进，标题带主键定位（`第 12 行 · id=4821`）。行弹层内 `↑`/`↓`/`j`/`k`（可带 vim 计数，`5j`）移动选中列，`/` 按列名过滤（宽表 40+ 列找列），`y` 复制当前列值（状态栏带列名），`Enter`/`v` 下钻完整单元格；`Esc` 依次 单元格 → 行弹层 → 表格。表格里直接按 `v` 仍是完整单元格；底部条显示横向位置。
- `e` 以 diff 确认层编辑单元格：旧值 → 新值、`WHERE` 条件、主键与完整 `UPDATE` 一目了然。
- `i` 按列模板插入，`Delete` / `Ctrl-D` 以带条件的 `WHERE` 删除 —— 所有写入先经确认。
- `Ctrl-T` 排队写入，`Ctrl-S` 以单个 `BEGIN … COMMIT` 执行；`f` 过滤、`s` 排序、`Ctrl-K` 追加排序键、`Ctrl-R` 清除。
- `y` 把当前行复制为 `INSERT INTO … VALUES (…)`；`/` 搜索可见结果行（隐藏不匹配行），`gv` 则在排序列 / 主键列内定位值、不隐藏任何行（`n`/`N` 循环命中）；`|` 在宽表上按列号或列名前缀把单元格光标跳到该列。

**表结构**
- `r` 显示字段列表（类型 / 键 / 可空 / 默认值 / 注释）；`t` 切换按方言生成的 `SHOW CREATE TABLE` DDL。
- `Alt-D` **对比两张表结构**：当前聚焦的表为源，再选目标表（按 `c` 可换到**另一个连接**，从而跨库 / 跨方言对比）。浮层逐列给出 `+`（目标缺少，需新增）/ `-`（目标多余，需删除）/ `~`（属性不同：类型 / 可空 / 默认值 / 注释 / 字符集 / 排序规则 / extra / 主键 / 唯一），第二个标签页列索引（名 / 列 / 唯一 / 过滤条件），两侧引擎不同时标题带 `⚠ 跨方言`。类型比较是方言感知的：同引擎忽略展示宽度（`int(11)` = `int`），跨引擎按常见映射归一（`varchar(255)` ≈ `character varying(255)`、`int` ≈ `integer`、`tinyint(1)` ≈ `smallint`、`jsonb` ≈ `json` ……）；映射表外的类型标 `?` 并原样展示两侧。
- `y` 复制纯文本差异摘要（可贴进工单）；`g` 生成把**目标**改写为源的 `ALTER` 脚本（`ADD` / `DROP` / `MODIFY`，含索引，PostgreSQL 另有 `COMMENT ON COLUMN`）—— dbxt **绝不执行**，只在一个预览标签里展示，你可复制到别的客户端或编辑器运行。
- `Shift+Alt-D` 对比两个**库的表清单**（仅源 / 仅目标 / 共有）；`Enter` 落在共有的表上进入单表对比。生成的语句按方言加引号（MySQL 反引号、PostgreSQL `"双引号"`）。
- `Alt-K` **按主键对比两张表的数据** —— 结构对比的行级续作（在对比浮层里按 `m` 可在 结构 ↔ 数据 间切换）。选择目标表（按 `c` 可换到**另一个连接**，从而 MySQL ↔ PostgreSQL 对比），可选输入两边同时生效的 `WHERE`（`w`）。两表都必须有主键：它就是行对齐的键；列按列名交集对比，列序不同不影响。比对在后台按每块 1000 行流式拉取（先 `COUNT(*)` 预报规模；`Esc` 可中止并保留已比结果；差异行超过 5000 条时截断并提示缩小范围）。浮层汇总显示 源 / 目标 行数与 仅源 `<` / 仅目标 `>` / 差异 `≠` 计数；`Tab` 切到这三个明细列表，`Enter` 把 `≠` 行展开为列级左右对照。值按文本比较，跨方言用 canonical 映射归一（`1` = `1.0`、`true` = `1`、`2026-01-01T…` = `2026-01-01 …`）；映射表外的类型原样比较并标 `?`。`y` 复制纯文本摘要；`g` 生成 源 → 目标 的同步脚本（`INSERT` / `UPDATE` / `DELETE`，值按目标方言转义）到预览标签 —— dbxt **绝不执行**。
- `Alt-T` **把一张表的结构和/或数据搬到另一个 SQL 连接**（迁移 / 备例 / 给测试库灌数）。三步向寻：① 选目标连接（默认当前连接，因此 MySQL → PostgreSQL 双向都行），② 改目标库 / 模式 / 表名（默认同名；改名即表复制），③ 选模式 —— **建表+搬数据**（默认）/ **仅建表**（结构）/ **插入已有表**（append）—— 以及选项。目标表已存在时 dbxt 默认报错停下（绝不静默覆盖）；`o` 切到**覆盖**，因会先执行 `DROP TABLE`，必须过红色确认层。选项：`w` WHERE 子集、`l` LIMIT 上限、`i` 带索引、`a` 带自增值（MySQL）或建 `serial` 列（PostgreSQL）、`s` 出错 停止报行号 / 跳过继续。搬运按源 keyset 分成 1000 行/块（无主键则退回 OFFSET），写入目标 500 行/事务批量；类型走与结构对比同一张跨方言映射表（包括 `bytea`/`blob`、数组与按目标方言转义的字符串）。源预估超过 100 万行时会多问一次 `Enter`；单批失败重试 1 次；状态栏实时显示 行数 / 块数 / 行每秒。`Esc` 中止并**保留已提交批次**，报告断点主键。完成汇总显示 源行数 / 已搬 / 跳过 / 耗时 / 速率 / 目标；`g` 复制摘要，`b` 在当前连接上直接打开目标表。Redis / MongoDB 不在范围（文档模型映射另议）。

**导入与导出**
- `I` 把 CSV 导入当前表（已浏览的表，否则侧栏选中的表）。输入路径（开头 `~` 展开为 `$HOME`）后确认预览：编码、分隔符、行数与文件大小、前五行解析结果，以及按列名对齐的列映射与类型推断（`int` / `float` / `bool` / `date` / `datetime` / `text`）。
- CSV 表头按列名（忽略大小写）匹配表列；CSV 中缺失的表列保留其默认值（通常为 `NULL`），多出的 CSV 列则阻止导入并给出明确提示。
- `m` 切换追加 / 覆盖（覆盖先清空表，预览边框变红），`s` 切换遇错停止（默认，报告出错行号）/ 跳过继续（列出所有跳过行）。数据以每批 500 行的事务批量写入，并按批报告进度。
- 自动探测编码 —— UTF-8，否则 GB18030/GBK（常见中文编码）—— 并从表头嗅探分隔符（`,` / `;` / TAB）。明确不支持 Excel `.xlsx`。
- `Ctrl-Y` 把当前结果导出为 CSV、JSON（数组）、NDJSON、Markdown、`INSERT`（每行一条）或批量 `INSERT`（多行 `VALUES`）。先选格式再选去向：留空走 OSC 52 复制，输入路径则写文件；超过 10000 行会提示生成可能耗时。

**Redis**
- 连接 Redis 后打开分页 `SCAN` key 浏览器（绝不 `KEYS *`），带类型与 TTL 徽标、服务端 `MATCH` 模式（`/`）与逻辑 db 切换。
- key 列表与 SQL 侧栏同一套快捷手感：任意可打印字符一步开启**子串过滤**（命中高亮，`Enter` 打开首位，`Esc` 清除），`Alt+字母` **首字母循环跳**（`;`/`,` 前后重复），`1-9` 直跳第 N 个 key，`3j`/`3k` 计数移动。
- `Enter` 按类型渲染 value —— string、hash、list、set、zset、stream、RedisJSON，大集合可继续加载；`y` 复制当前值，`Esc` 返回列表。
- 窄屏下类型徽标与 TTL 融合为单 token（`S·12s`），key 行不换行。
- `e` / `x` / `m` / `Del` 编辑、设 TTL、重命名、删除；`Space` 多选后批量删除 / 设 TTL / 前缀重命名，均走红色确认层。
- 其他功能仍可用原生 `redis-cli` 命令台（`Ctrl-L`）。

**MongoDB**
- 连接 MongoDB 后列出 collection；`Enter` 以网格浏览文档（顶层字段并集，`_id` 优先）。集合列表与 SQL 表列表共用 子串过滤 / 首字母跳 / 直跳 键位。
- `n`/`p` 翻页，`f` 应用 JSON 过滤，`r` 查看 collection 索引。
- `e` 用 JSON 编辑器编辑文档（`_id` 不可改，字段级 diff），`i` 插入，`Del` 按 `_id` 删除 —— 每次写入确认并原地刷新。

**效率与体验**
- 任意位置 `?` 打开**当前上下文迷你速查**（单屏放下，不滚动）；再按 `?` 进全量速查。底部提示条按终端宽度分级显示（窄屏 4 个 / 中屏 6 个 / 宽屏全量，隐藏时显示 `? 更多`）。所有浮层统一 `Esc` 关闭。
- 列表支持数字直跳与计数前缀：侧栏 `1-9` 直跳第 N 个连接/表，`3j`/`5n` 在侧栏 / 结果 / 历史中重复移动或翻页；结果区 `gd` 看表结构、`gt` 回表数据、`gv` 定位值、`|` 跳列。
- `Alt-1..9` 直切第 N 个连接（按侧栏连接顺序），`Alt-Tab` / `` Alt-` `` 在当前连接与上一个之间对切；切换时智能恢复上次的库/表 —— 新连接有同名库表则直达，否则落在库列表首屏，状态栏提示落点；编辑器里有未提交内容时提示一次，绝不静默丢弃。
- 编辑器选中一段（`Shift`+方向键）后 `F5`/`Ctrl-J` 只执行选区；无选区时 `Alt+Enter` 按分号智能识别光标处语句（字符串 / 注释里的分号不算 —— 复用 `%` 配对括号的同一套词法状态机），有选区则执行选区。`Alt-P` 打开片段收藏并插到光标处（一步），`Ctrl-O` 仍是追加到末尾。
- `Ctrl-A` 自动折叠非焦点栏，`Ctrl-W` 收起 / 展开当前栏；低于 50 列时各栏纵向堆叠。横向布局下侧栏宽度自适应：按最宽的 `库.表` 名收窄（不低于可读下限，宽屏仍保留 28 列），把让出的列还给数据区；名字过长则截断并以 `~` 标记。
- `Alt-C` 压缩列宽，`Alt-V` 隐藏列，`Alt-R` 直达最近表，`Alt-H` 打开查询历史 —— 选择按 `库.表` 持久化。
- `Alt-←` / `Alt-→` 像浏览器的后退 / 前进一样在最近 50 个节点之间往返 —— SQL 表、MongoDB 集合、Redis key 同级都是导航节点（value / 文档详情不进栈，后退会回到打开它的列表项）；打开新节点会截断前进分支，跨库切换同样可用。状态栏以 `← 表名` 确认落点；侧栏 `t` 浮层仍列出最近 5 张供直接跳转。
- 多语句输出兼具控制台手感：`Home`/`End` 与 `gg`/`G` 直达语句列表顶 / 底，错误行 `Enter` 打开错误框，`y` 复制当前语句结果为 CSV（与 `Ctrl-Y` 导出的首选格式一致），`Alt-O` 开关语句分隔线 + 紧凑耗时前缀（`12ms`、`1.23s`，默认关；`Alt-T` 已被数据搬运占用，故用 `Alt-O`）。
- 鼠标与触屏可用：点击选中、再点确认；滚轮滚行，`Shift`/`Alt`/`Ctrl`+滚轮横滚列。
- 失败可见：看门狗把无响应的后端变成错误，执行中显示转圈与已用秒数，服务端错误原样回显。
- 大结果集不卡：20,000×12 表格滚动约 0.6 ms/帧（列宽缓存 + 只切片可视窗口，不再每帧重扫全部行）；文件导出在后台任务里逐块直写磁盘 —— 20,000 行 × 12 列不到 1 秒，内存峰值只相当于一行（整进程约 36 MB，而把整个文档留在内存时约 96 MB）。
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
3. 在表上按 `Enter` 浏览数据；侧栏直接输入字母即过滤表名（`Enter` 打开第一个命中），`d` 切换数据库。
4. `e` 编辑单元格、`i` 插入、`Delete` 删除 —— 每次执行前展示完整 SQL。
5. `F5` 执行编辑器 SQL；`?` 打开完整快捷键帮助。

数据库在堡垒机后面时：按 `c`，把 `ssh_tunnel` 设为 `y`，填写 `ssh_host` / `ssh_user` 与登录方式，光标移到保存行按 `Enter`。此后连接链路为 dbxt → 跳板机 → 数据库。

## 快捷键速查

完整列表在 TUI 的 `?` 浮层与 `dbxt --help` 中；这里只列核心键。

| 场景 | 按键 |
| --- | --- |
| 全局 | `?` 当前上下文迷你速查（再按一次进全量帮助） · `Tab`/`Shift-Tab` 切栏 · `Alt-Shift-1/2/3` 聚焦（终端可能报成 `Alt-!` `Alt-@` `Alt-#`） · `Alt-←`/`Alt-→` 表 / 集合 / Redis key 后退前进（浏览器语义，最多 50 个） · `F5`/`Ctrl-J` 执行（有选区只跑选区） · `Alt-Enter` 只执行光标处语句 · `Alt-O` 脚本输出分隔+耗时 · `Ctrl-C` 退出 |
| 连接 | `↑` `↓` 移动 · `Enter` 连接 · `Alt-1..9` 直切第 N 个连接（智能恢复上次库/表） · `Alt-Tab`/`` Alt-` `` 与上一个连接对切 · `c` 新建 · `e` 编辑 · `p` 复制 · `s` 排序（名称/类型/颜色） · `x` 删除 · `d` 数据库/schema 切换 · `o` 返回选择 |
| 导入 / 导出 | `Alt-E` 导出全部连接为 JSON（`p` 含密码需红色确认 · `y` 复制到剪贴板） · `Alt-I` 导入 dbxt / DBeaver / Navicat 文件（`s` 跳过 · `r` 覆盖需红色确认 · `b` 都存 · `Space` 勾选 · `d` 逐条） |
| 连接表单 | `↑` `↓`/`Tab` 切换字段 · `Enter` 编辑/切换/保存 · `Space` 切换 `ssh_tunnel`/`ssl`/`ssh_auth`、循环 `color` 调色板 · `Esc` 返回 |
| SSH 主机密钥 | `y`/`Enter` 接受并记住 · `s` 仅本次会话 · `n`/`Esc` 拒绝 |
| 侧栏 | `↑` `↓`/`j` `k` 在 连接→库→表 树上移动 · `h`/`l`（`←`/`→`）折叠/展开 · `Enter` 打开（连接=切换展开 / 库=切库 / 表=浏览） · 直接输入字母即过滤（`/` 也可，命中表名或库名，父节点保留） · `Alt`+字母 加 `;`/`,` 首字母循环跳 · `s` 排序（名称/类型） · `1-9` 直跳第 N 个连接/表 · `Alt-1..9` 直切连接 · `3j`/`3k` 计数前缀（移动 3 项） · `[`/`]` 切库 · `Ctrl-U` 清除过滤 · `r` 表结构 · `I` 导入 CSV · `t` 最近表 |
| 编辑器 | `Alt-H` 历史面板 · `Alt-G` 全库搜索 · `Alt-L` 执行 `.sql` 文件 · `Alt-F` 格式化/压缩 · `Ctrl-U` 撤销格式化 · `Alt-/` 补全 · `Alt-Enter` 执行光标处语句（分号边界，字面量/注释里的分号不算） · `Alt-P` 片段插到光标 · `%` 跳配对括号（光标在 `()[]{}` 上或旁；否则照常输入 `%`） · `Ctrl-A`/`Ctrl-E` 行首/行尾（`Home`/`End` 同） · `Ctrl-K`/`Ctrl-Shift-K` 删至行尾 · `Ctrl-W` 删前一个词 · `F5`/`Ctrl-J` 执行（有选区只跑选区） · `↑` `↓` 历史 |
| 结构对比 | `Alt-D` 当前表 vs 选中的表（`c` 换连接到别的连接） · `Shift+Alt-D` 两库表清单对比 · `Tab` 列 / 索引 / ALTER · `y` 复制摘要 · `g` 生成 ALTER · `Esc` 关闭 |
| 数据对比 | `Alt-K` 按主键对比两张表数据（`c` 换连接到别的连接） · `m` 切换 结构/数据 · `w` WHERE · `Tab` 汇总/仅源/仅目标/差异 · `Enter` 展开差异行 · `y` 摘要 · `g` 同步 SQL · `Esc` 关闭 |
| 数据搬运 | `Alt-T` 把结构和/或数据搬到另一个连接（`o` 覆盖需红色确认 · `m` 模式 · `w`/`l` WHERE/LIMIT · `i` 索引 · `a` 自增值 · `s` 停止/跳过） · ① 选连接 · ② 库/模式/表名 · ③ 选项 · `g` 摘要 · `b` 浏览目标表 · `Esc` 中止 |
| 结果区 | `↑` `↓` 行 · `←` `→` 列 · `n`/`p` 翻页（`5n` = 翻 5 页） · `gd`/`gt` 表结构/表数据 · `gv` 定位值（`n`/`N` 循环命中） · `|` 按列号/列名跳列 · `Enter`/`o` 整行（`↑↓`/`5j` 移动、`/` 过滤列、`y` 复制、`Enter`/`v` 下钻单元格） · `v` 单元格 · `e` 编辑 · `i` 插入 · `Delete` 删除 |
| 结果区（续） | `f` 过滤 · `s` 排序 · `Ctrl-K` 追加排序 · `Ctrl-R` 清除 · `y` 复制行 · `/` 搜索（隐藏不匹配行） · `Ctrl-Y` 导出 · `[` `]` 标签 |
| Redis | `Space` 多选 · `a` 全选 · `Del`/`x`/`m` 批量删除/TTL/重命名 · `/` MATCH · `f`/`a-z` 过滤 · `Alt+a-z` 首字母跳 · `n` 更多 · `e` 编辑 · `Enter` 查看 value |
| MongoDB | `e` 编辑 · `i` 插入 · `Del` 删除 · `f` 过滤 · `y` 复制 JSON · `n`/`p` 翻页 · `r` 索引 |
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
