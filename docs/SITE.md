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
   `site/index.html` 里提示用户的通知地址（当前占位为
   `https://lightfox.pages.dev/feed.json`，部署后替换为实际域名）。
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
于本机，用 wrangler 直接部署；公开仓库里只有介绍站和 feed.json，且
`site/index.html` 的页脚隐藏链接与 robots.txt 提示都已移除——公开侧不存在
任何指向蜜罐的线索，蜜罐路径只有攻击者主动扫描才会发现（这对捕获更有利：
来的人都是真的在扫）。

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
蜜罐在 `https://lightfox-honeypot.pages.dev` 独立可用；如果想让蜜罐与介绍站
同域（路径形态更像真实站点），改用自定义域：给两个项目分别绑定同一域名的
不同子域，或在介绍站项目里把 `/admin` 等路径反代到蜜罐项目——后两者是
可选优化，初始部署不需要。

验证：`curl -X POST -d "u=1&p=2" https://lightfox-honeypot.pages.dev/api/login`
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
