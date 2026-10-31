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

`site/admin/`、`site/panel`、`site/wp-admin` 路径是一组**假管理后台**：

* 页面长得像登录框，表单提交到不存在的 `/api/login`；
* Cloudflare Pages 对静态站点的 POST 一律返回 `405 Method Not Allowed` —
  这就是"捕获"：请求被拒绝，不存储任何东西，不产生任何反馈；
* 站点没有任何后端、数据库、日志服务，蜜罐本身**无数据可泄露**；
* `robots.txt` 列出这些路径 — 这是有意的：爬虫与漏洞扫描器会顺着进来撞上
  405，真人几乎不会。

这些链接以隐藏方式挂在 index.html 页脚（`display:none`），只对解析 HTML
找链接的自动化工具可见。

## 边界声明

* 站点不加载任何外部资源（无 CDN 字体、无 JS 库、无统计脚本），无 cookie；
* 启动器默认**不启用**通知功能，首次启动会询问一次，默认答案为否；
* 启动器对 `http://` 地址会提示明文风险；`feed.json` 应始终通过 `https://`
  分发（pages.dev 默认即 HTTPS）；
* 本站与 Cloudflare 之间没有任何自定义后端 — 如果未来需要动态能力
  （例如按版本下发不同公告），应重新评估而非就地加接口。
