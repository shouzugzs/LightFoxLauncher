# 通知服务器与介绍站点（Cloudflare Pages）

`site/` 目录是一个**纯静态**站点，承担两件事：

1. **项目介绍页** — 特性、下载链接、通知功能说明；
2. **通知服务器** — `site/feed.json` 就是启动器"通知"页要填的那个地址的数据本体。

托管目标是 Cloudflare Pages。选它的原因不是性能，而是**管理面**：所有内容更新只能通过 git 提交推送到托管平台，网站上不存在任何对外可写的接口。

## 为什么要这样设计

启动器的通知功能读取一个用户配置的 URL。如果这个 URL 背后是一个动态服务
（数据库、管理面板、API），那么：

* 多一个可被攻击的服务面（登录、会话、注入、越权）；
* 多一份需要保管的服务端凭据；
* 公告内容与代码分离，审查公告就要审查一个运行中的系统。

静态站把这一切变成不可能：

| 动态通知服务器 | 静态 feed.json |
|---|---|
| 登录接口 → 可被爆破 | 无登录，无接口 |
| 数据库 → 可被注入/篡改 | 一个 git 里的 JSON 文件 |
| 写接口 → 需要鉴权 | **不存在写接口** |
| 服务端漏洞 → 需要跟进修补 | 没有服务端代码 |
| 篡改需要攻破服务器 | 篡改需要攻破 Cloudflare **或** GitHub 账号 |

修改公告的唯一方式是：改 `site/feed.json` → git commit → push → Cloudflare
Pages 自动构建并发布。每一次变更都在 git 历史里，可审计、可回滚。

## 部署步骤

1. Cloudflare Dashboard → Workers & Pages → Create → Pages → **Connect to Git**，
   选 `shouzugzs/LightFoxLauncher` 仓库。
2. 构建配置：
   * Framework preset: `None`
   * Build command: **留空**（站点就是 `site/` 原样静态文件，不需要构建）
   * Build output directory: `site`
3. 保存并部署。首次部署后 Cloudflare 分配 `*.pages.dev` 域名，把它填进
   `site/index.html` 里提示用户的通知地址（当前为
   `https://lightfoxlauncher.pages.dev/feed.json`）。
4. （可选）绑定自定义域。

之后每次 push 到 `main`，Cloudflare 自动重新发布；PR 会得到独立预览 URL。

## feed.json 格式规则

格式由 `src/notify.rs` 的解析器定义（这是唯一的权威来源）：

* 顶层必须是 JSON 对象；
* `latest` — 可选，版本字符串，`数字.数字[.数字]`，可带 `v` 前缀与 `-beta`
  后缀，最长 32 字符；与启动器版本比较后决定是否提示"有新版本"；
* `url` — 可选，`https://` / `http://` 开头，≤2000 字符；**只展示，永不打开**；
* `notes` — 可选数组，≤50 条；每条：
  * `id` ≤128 字符（发布后不要改，它是已读标记）；
  * `date` ≤32 字符，`title` ≤200，`body` ≤2000（可多行 `\n`）；
  * `level`: `info` | `warn` | `update`，其他值降级为 info；
* 所有字段可选，未知字段被忽略；空对象 `{}` 是合法的"没有公告"。

发布新版本时：更新 `latest`、把新版本说明作为一条 note 加到数组**前面**
（启动器按顺序显示），保留旧 note 若干条以便老启动器仍能看到历史。

## 房间号注册表

`site/functions/api/room.js` 是"房间号联机"的注册表：房主把 `房号 → 地址:端口`
登记十分钟，朋友用房号换回地址后直连。**游戏流量不经过它**，它只是短暂的
会合点。

| 端点 | 作用 |
|---|---|
| `POST /api/room` | 注册/续期，body `{"code","addr","port"}` |
| `GET /api/room?code=<CODE>` | 查询，返回 `{"ok":true,"addr","port"}` |

房号走**查询参数**而不是路径段：Pages 是文件路由，`site/functions/api/room.js`
只映射到 `/api/room`，`/api/room/<CODE>` 会被静态 404 直接接走，根本到不了
Function。旧路径形式现在返回 `400 bad_request`（不是 405，也不是 404），
免得和"房号过期"混淆。

* 房号 6 字符，字母表 `ABCDEFGHJKLMNPQRSTUVWXYZ23456789` —— 剔除 `I/O/0/1`，
  因为房号是念出来或看截图输入的，这四个最容易被听错/看错；
* 服务端对房号和地址都做**白名单校验后才碰 KV 键**；客户端（`src/room.rs`
  的 `checked_addr`）重复同一套白名单，不依赖部署行为；
* 限流：注册 10 次/分、查询 60 次/分（按 IP，计数器 120 秒过期）；
* 错误只返回枚举 `bad_code`/`bad_request`/`not_found`/`rate_limited`/
  `server_error`，不回显请求内容；
* 记录里**不含来源 IP**（只有限流计数器短暂保存），房号 600 秒自动过期，
  没有删除端点和历史记录。

KV 绑定名 **`ROOMS`**。启动器侧可用 `Settings.room_server` 覆盖默认地址，
且只在用户点"建房号/用房间号连接"时才访问——启动时从不联系。

## 蜜罐（私有部署，不在公开仓库）

蜜罐由两部分组成，**两部分都不在公开仓库里**：

* `private-site/admin/`、`private-site/panel/`、`private-site/wp-admin/` —
  假管理后台页面；
* `private-site/functions/api/login.js` — 捕获端点 `/api/login`，把每次提交
  记录为一条事件写入 KV 绑定 `HP`，然后返回千篇一律的 `500 {"ok":false}` —
  不回显、不计数、不区分成功与失败，攻击者得不到任何反馈；
* 每条事件包含：UTC 时间、来源 IP（`CF-Connecting-IP`，XFF 只作补充记录）、
  User-Agent、提交的字段（用户名/密码/来源页）、请求头白名单、CF ray/colo/
  国家/ASN。响应与存储里都不含 cookie，不捕获会话；
* 单事件 ≤4 KB、超长截断；单 IP 每小时 ≤20 条（KV 计数器限流）；事件保留
  30 天后自动过期。

**为什么私有**：蜜罐源码一旦公开，扫描器 grep 一下页面特征或直接看仓库就
知道这是假后台，捕获即失效。所以 `private-site/` 在 `.gitignore` 里，只存在
于本机，用 wrangler 直接部署；公开仓库里只有介绍站和 feed.json。

**流量从哪来（跨域导流）**：蜜罐是独立域名，扫描器不会凭空知道它。导流
靠介绍站页脚三条 `display:none` 的隐藏链接——指向蜜罐域名的**绝对 URL**
（`https://lightfox-cdn.pages.dev/admin` 等），以及 robots.txt 里的对应
Disallow。公开仓库里因此出现蜜罐**域名**，但这不等于暴露蜜罐：让蜜罐露馅
的是它的**源码**（页面结构、Function 逻辑），源码始终不在任何仓库里；而
"站内有个 admin 链接"本身是所有真实站点都有的正常现象。解析 HTML 顺链接
而来的自动化工具会一路走进登录页并被捕获，真人不可见、不可聚焦。

### 隐私与边界

* 采集对象是**攻击者的提交内容**，这是蜜罐的目的数据；请求头用白名单，
  不收集 cookie 或 bearer token，避免把第三方凭据存进自己的存储；
* 读路径不存在：KV 只能通过 Cloudflare 控制台或 wrangler（账号凭据）访问，
  网站上没有任何读取接口；
* 代码在 git 里、由托管平台部署，修改仍然只能通过提交完成。

### 对接网安流程

```bash
# 一次性：npm i -g wrangler 并登录（wrangler login）
node tools/honeypot-export.mjs --since 2025-11-01 --out hits.ndjson
# 可选：导出后删除已导出的键（KV 也会 30 天自动过期）
node tools/honeypot-export.mjs --delete
```

`hits.ndjson` 每行一个 JSON 事件，可直接投喂 SIEM/SOC 管道。IP 维度的
统计、与 Cloudflare 自带请求日志的关联（用事件里的 `ray`/`colo` 字段）、
封禁决策由接收方完成。

### KV 绑定配置（部署时一次性）

**介绍站需要两个 KV 绑定**（房间号注册表用）：

```bash
npx wrangler kv namespace create ROOMS
# 输出里的 id 填到 Dashboard → Pages 项目 lightfoxlauncher →
#   Settings → Bindings → 添加 KV namespace，变量名必须精确为 ROOMS
```

绑定后重新部署一次生效（`npx wrangler pages deploy site --project-name
lightfoxlauncher --branch main`）。未绑定时 `/api/room` 返回
`{"ok":false,"error":"server_error"}`，其余页面照常工作。

**蜜罐项目单独一个绑定**（`HP`）：

```bash
npx wrangler kv namespace create HP
# 输出里有 namespace id，填进 private-site/wrangler.toml 的 [[kv_namespaces]] id=
```

未绑定 `HP` 时端点返回同样的 500，蜜罐页面照常工作，只是不落盘。

### 蜜罐单独部署（wrangler，从本机直传）

蜜罐部署为**独立的 Pages 项目**，与介绍站分离：一个从 GitHub 自动构建
（公开），一个从本机 wrangler 直传（私有、不进任何仓库）。

```bash
# 一次性
npm i -g wrangler && wrangler login
npx wrangler kv namespace create HP        # 把 id 填进 private-site/wrangler.toml

# 部署/更新蜜罐（private-site/public/ 是静态页，functions/ 是捕获端点）
cd private-site
mkdir -p public
cp -r admin panel wp-admin public/          # Windows PowerShell: Copy-Item -Recurse
npx wrangler pages deploy public --project-name lightfox-honeypot
```

首次 `deploy` 会询问项目名与生产分支，按上面给的名字即可。部署完成后
蜜罐在 `https://lightfox-cdn.pages.dev` 独立可用；如果想让蜜罐与介绍站
同域（路径形态更像真实站点），改用自定义域：给两个项目分别绑定同一域名的
不同子域，或在介绍站项目里把 `/admin` 等路径反代到蜜罐项目——后两者是
可选优化，初始部署不需要。

验证：`curl -X POST -d "u=1&p=2" https://lightfox-cdn.pages.dev/api/login`
应返回 `{"ok":false}`，KV 里出现一条 `hit:` 事件。

## 边界声明

* 站点不加载任何外部资源（无 CDN 字体、无 JS 库、无统计脚本），无 cookie；
* 启动器默认**不启用**通知功能，首次启动会询问一次，默认答案为否；
* 启动器对 `http://` 地址会提示明文风险；`feed.json` 应始终通过 `https://`
  分发（pages.dev 默认即 HTTPS）；
* 蜜罐端点是蜜罐项目唯一的动态端点，只写不读、无任何查询能力；介绍站
  本身保持纯静态、零后端；
* 蜜罐源码与捕获配置只存在于本机 `private-site/`（gitignored），不进入
  任何仓库；如果未来需要动态能力，应重新评估而非就地加接口。
