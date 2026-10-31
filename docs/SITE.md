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

## 蜜罐

`site/admin/`、`site/panel/`、`site/wp-admin/` 是一组**假管理后台**，带防御性
捕获能力：

* 页面像登录框，表单提交到 `/api/login`；
* `/api/login` 是一个 Cloudflare Pages Function（`site/functions/api/login.js`），
  它把每次提交记录为一条事件写入 KV 绑定 `HP`，然后返回千篇一律的
  `500 {"ok":false}` —— 不回显、不计数、不区分成功与失败，攻击者得不到任何
  反馈；
* 每条事件包含：UTC 时间、来源 IP（`CF-Connecting-IP`，XFF 只作补充记录）、
  User-Agent、提交的字段（用户名/密码/来源页）、请求头白名单、CF ray/colo/
  国家/ASN。响应与存储里都不含 cookie，不捕获会话；
* 单事件 ≤4 KB、超长截断；单 IP 每小时 ≤20 条（KV 计数器限流）；事件保留
  30 天后自动过期；
* `robots.txt` 列出这些路径 — 这是有意的：爬虫与漏洞扫描器会顺着进来，真人
  几乎不会。这些链接以隐藏方式挂在 index.html 页脚（`display:none`），只对
  解析 HTML 找链接的自动化工具可见。

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

Cloudflare Dashboard → 该 Pages 项目 → Settings → Functions → KV namespace
bindings：新建/选择一个 namespace，绑定变量名填 `HP`。或者：

```bash
npx wrangler kv namespace create HP
# 按输出提示把 namespace_id 绑定到 Pages 项目（Dashboard 或 wrangler pages ...）
```

未绑定 `HP` 时端点返回同样的 500，蜜罐页面照常工作，只是不落盘。

## 边界声明

* 站点不加载任何外部资源（无 CDN 字体、无 JS 库、无统计脚本），无 cookie；
* 启动器默认**不启用**通知功能，首次启动会询问一次，默认答案为否；
* 启动器对 `http://` 地址会提示明文风险；`feed.json` 应始终通过 `https://`
  分发（pages.dev 默认即 HTTPS）；
* `/api/login` 是站点唯一的动态端点，只写不读、无任何查询能力；如果未来
  需要更多动态能力，应重新评估而非就地加接口。
