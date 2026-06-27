# aliyun-directmail-smtp-gateway

把标准 **SMTP** 协议桥接到 **阿里云邮件推送（DirectMail）** 的网关。任何会发 SMTP 的程序（脚本、CI、遗留系统、`swaks`/`telnet`）都可以把邮件投到本网关，由网关解析后通过 DirectMail 的 `SingleSendMail` RPC 接口投递——调用方无需改造成阿里云 SDK。

本仓库是一个 Cargo workspace，包含两个 crate：

| Crate | 类型 | 作用 |
| --- | --- | --- |
| [`aliyun-directmail-smtp-gateway`](aliyun-directmail-smtp-gateway/) | 二进制 | SMTP 服务端网关，接收邮件并经由 `MailSender` 后端投递 |
| [`aliyun-dm-sdk`](aliyun-dm-sdk/) | 库 + 示例二进制 | 独立的 DirectMail RPC 客户端（HMAC-SHA1 签名、自动重试），可单独依赖 |

> 当前为**第一阶段**实现：不提供 SMTP `AUTH` 与 `STARTTLS`，默认仅监听回环地址。生产暴露需自行依赖网络层访问控制（详见[安全说明](#安全说明)）。

---

## 工作原理

```
SMTP 客户端 ──TCP──▶ 网关 (SmtpServer)
                        │  1. 接收 SMTP 会话 (EHLO/MAIL/RCPT/DATA)
                        │  2. 解析 RFC 822 (mail-parser) → ParsedMail
                        │  3. 应用内容策略（拒绝附件 / 空正文 / 超长主题…）
                        │  4. 映射为 SingleSendMailRequest
                        ▼
                  MailSender (trait)
                   ├── DirectMailSender ──HTTPS POST──▶ 阿里云 DirectMail
                   └── RecordingSender  ──▶ 仅记录到内存（本地测试，不真实投递）
```

关键设计：

- **发件身份与信封解耦**：DirectMail 的「已验证发信地址」由 `DIRECTMAIL_ACCOUNT_NAME` 决定；SMTP 信封/头部里的 `From` **不作为可信身份**，仅用于日志，以及在长度合规时映射为 DirectMail 的 `FromAlias`（发件人显示名）。
- **可插拔后端**：`MailSender` trait 让网关与具体投递方式解耦，便于测试（`RecordingSender`）与未来扩展。
- **背压**：通过信号量限制并发连接数，满载时在 listener 边界施加背压而非接收无法服务的连接。
- **至少一次语义**：投递在 `send_timeout` 内未完成时回 `451`，客户端会重试——可能产生重复投递，这是第一阶段权衡。

---

## 快速开始

### 环境要求

- Rust **>= 1.85**（workspace 使用 edition 2024）。

### 构建

```bash
git clone https://github.com/greenhat616/aliyun-directmail-smtp-gateway.git
cd aliyun-directmail-smtp-gateway
cargo build --release
# 产物：target/release/aliyun-directmail-smtp-gateway
```

### 运行（默认：内存后端，不真实投递）

未设置 `GATEWAY_SENDER` 时使用 `in-memory` 后端：收下邮件但**只记录在本地内存、不投递**，适合先验证 SMTP 链路。

```bash
cargo run --release --bin aliyun-directmail-smtp-gateway
# 监听 127.0.0.1:2525，启动时会 warn 提示当前为内存后端
```

### 运行（真实投递：DirectMail 后端）

```bash
export GATEWAY_SENDER=directmail
export DIRECTMAIL_ACCOUNT_NAME="service@mail.example.com"   # 阿里云控制台中已验证的发信地址
export DIRECTMAIL_ACCESS_KEY_ID="LTAI...."
export DIRECTMAIL_ACCESS_KEY_SECRET="********"
# 可选：跨地域时指定 region 或显式 endpoint
# export DIRECTMAIL_REGION="ap-southeast-1"

cargo run --release --bin aliyun-directmail-smtp-gateway
```

---

## 配置（环境变量）

所有配置都通过环境变量注入，构建时校验失败会直接退出。

### 网关参数（`GATEWAY_*`）

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `GATEWAY_BIND_ADDR` | `127.0.0.1:2525` | 监听地址。非回环地址会触发安全告警 |
| `GATEWAY_HOSTNAME` | `localhost` | SMTP 问候语 / `EHLO` 响应中的主机名 |
| `GATEWAY_MAX_MESSAGE_BYTES` | `10485760`（10 MiB） | 单封邮件最大字节数，超出回 `552` |
| `GATEWAY_MAX_RECIPIENTS` | `100` | 单封最大收件人数，范围 `1–100`（DirectMail 上限） |
| `GATEWAY_MAX_CONNECTIONS` | `1024` | 最大并发连接数，范围 `1–100000` |
| `GATEWAY_READ_TIMEOUT_SECS` | `300` | 单次读取空闲超时，超时回 `421` 关闭 |
| `GATEWAY_SEND_TIMEOUT_SECS` | `30` | 单次投递超时，超时回 `451` |
| `GATEWAY_SENDER` | `in-memory` | `directmail`（真实投递）或 `in-memory`（仅记录） |

### DirectMail 参数（`DIRECTMAIL_*`，仅在 `GATEWAY_SENDER=directmail` 时生效）

| 变量 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `DIRECTMAIL_ACCOUNT_NAME` | ✅ | — | 已验证的发信地址（真正的发件人身份） |
| `DIRECTMAIL_ACCESS_KEY_ID` | ✅ | — | 阿里云 AccessKey ID |
| `DIRECTMAIL_ACCESS_KEY_SECRET` | ✅ | — | 阿里云 AccessKey Secret（日志中始终脱敏） |
| `DIRECTMAIL_ENDPOINT` | | — | 显式 endpoint 主机，**优先级高于 region** |
| `DIRECTMAIL_REGION` | | `cn-hangzhou` | 地域，用于推导 endpoint（如 `dm.<region>.aliyuncs.com`） |
| `DIRECTMAIL_ADDRESS_TYPE` | | `1` | DirectMail `AddressType`（`0` 随机账号 / `1` 发信地址） |
| `DIRECTMAIL_REPLY_TO_ADDRESS` | | `false` | 是否设置 `ReplyToAddress` |

### 日志

通过 [`tracing`](https://docs.rs/tracing) 输出，用 `RUST_LOG` 控制级别（默认 `info`）：

```bash
RUST_LOG=debug cargo run --release --bin aliyun-directmail-smtp-gateway
```

---

## 发送邮件示例

网关启动后（假设监听 `127.0.0.1:2525`），用任意 SMTP 客户端投递即可。**无需认证**。

### 1. swaks（最快验证）

```bash
swaks --server 127.0.0.1:2525 \
      --from sender@example.com \
      --to you@example.com \
      --header "Subject: 网关测试" \
      --body "你好，这封信来自 SMTP 网关。"
```

### 2. Python（标准库 `smtplib`）

```python
import smtplib
from email.mime.text import MIMEText

msg = MIMEText("你好，这封信来自 SMTP 网关。", "plain", "utf-8")
msg["Subject"] = "网关测试"
msg["From"] = "Service <sender@example.com>"   # 显示名 "Service" 长度 < 15 时会映射为 FromAlias
msg["To"] = "you@example.com"

with smtplib.SMTP("127.0.0.1", 2525) as smtp:
    smtp.sendmail("sender@example.com", ["you@example.com"], msg.as_string())
```

### 3. 原始 SMTP 会话（`telnet` / `nc`）

`<` 为发送，`>` 为服务端响应：

```text
> 220 localhost ESMTP aliyun-directmail-smtp-gateway
< EHLO client.example
> 250-localhost
> 250-SIZE 10485760
> 250 8BITMIME
< MAIL FROM:<sender@example.com>
> 250 2.1.0 Sender ok
< RCPT TO:<you@example.com>
> 250 2.1.5 Recipient ok
< DATA
> 354 Start mail input; end with <CRLF>.<CRLF>
< From: Alice <sender@example.com>
< Subject: Hello world
< Content-Type: text/plain; charset=utf-8
<
< Body text
< .
> 250 2.0.0 Ok, message accepted
< QUIT
> 221 2.0.0 Bye
```

### 4. Docker

仓库提供多阶段 `Dockerfile`（distroless 运行时，以非 root 用户 `65532` 运行）；发布时还会推送多架构镜像到 GHCR。

```bash
# 本地构建
docker build -t directmail-gateway .

# 运行真实投递（容器内默认监听 0.0.0.0:2525）
docker run --rm -p 2525:2525 \
  -e GATEWAY_SENDER=directmail \
  -e DIRECTMAIL_ACCOUNT_NAME="service@mail.example.com" \
  -e DIRECTMAIL_ACCESS_KEY_ID="LTAI...." \
  -e DIRECTMAIL_ACCESS_KEY_SECRET="********" \
  directmail-gateway

# 或直接使用发布镜像
docker run --rm -p 2525:2525 ghcr.io/greenhat616/aliyun-directmail-smtp-gateway:latest
```

---

## 内容策略与限制（第一阶段）

网关在解析后会执行一组策略，违反者以**永久失败（5xx）**拒绝，避免静默丢内容：

- ❌ **附件**：含任何附件直接拒绝。
- ❌ **空正文**：无 text/html 正文拒绝。
- ❌ **超长主题**：主题 > 100 字符拒绝（不截断）。
- ⚠️ **正文结构**：只能映射单一正文——单个 HTML 优先于单个 text；text + HTML 仅在 `multipart/alternative` 下允许（此时丢弃 text 表示是预期语义），其余多正文结构一律拒绝。
- ✂️ **发件显示名**：`From` 显示名长度 `< 15` 字符时映射为 `FromAlias`，否则丢弃显示名（邮件仍发出）。
- 👥 **收件人**：最多 100 个（DirectMail `SingleSendMail` 上限）。
- 🚫 **未支持的命令**：`AUTH`、`STARTTLS`、`BDAT/CHUNKING` 等暂不支持，返回 `502`。

### 常见 SMTP 回复码

| 码 | 含义 |
| --- | --- |
| `250` | 成功（命令 / 邮件已接收） |
| `354` | `DATA` 开始接收正文 |
| `421` | 空闲超时，关闭连接 |
| `451` | 临时失败（后端超时或可重试错误），客户端可重试 |
| `452` | 收件人过多 |
| `502` | 命令未实现 |
| `503` | 命令顺序错误（如先发 `RCPT`） |
| `552` | 邮件体积超限 |
| `554` | 内容被拒 / 投递方永久拒绝 |

> 投递方返回的原始报文（可能含 RequestId、收件人校验细节）**不会**回传给未认证的 SMTP 客户端，仅记录在内部日志中。

---

## 作为库使用：`aliyun-dm-sdk`

不需要 SMTP 网关、只想在 Rust 程序里直接调 DirectMail，可单独依赖 SDK。

```toml
# Cargo.toml
[dependencies]
aliyun-dm-sdk = { git = "https://github.com/greenhat616/aliyun-directmail-smtp-gateway" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

### 最小示例

```rust
use aliyun_dm_sdk::{DirectMailClient, SingleSendMailRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = DirectMailClient::new("<access_key_id>", "<access_key_secret>");

    // 便捷构造：text(...) / html(...)
    let request = SingleSendMailRequest::text(
        "service@mail.example.com",        // AccountName（已验证发信地址）
        ["a@example.com", "b@example.com"], // 收件人，支持 &str / Vec<String> / 数组
        "Hello",                            // 主题
        "纯文本正文",                        // 正文
    )
    .with_from_alias("客服")              // 可选：发件人显示名（< 15 字符）
    .with_reply_to_address(true);         // 可选：开启回信地址

    let response = client.single_send_mail(request).await?;
    println!("EnvId={}, RequestId={}", response.env_id, response.request_id);
    Ok(())
}
```

### 自定义客户端（builder）

```rust
use std::time::Duration;
use aliyun_dm_sdk::{DirectMailClient, RetryConfig};

let client = DirectMailClient::builder("<id>", "<secret>")
    .region("ap-southeast-1")            // 或 .endpoint("dm.ap-southeast-1.aliyuncs.com")
    .retry_config(RetryConfig {          // 默认 3 次、指数退避 + 抖动
        max_attempts: 5,
        base_delay: Duration::from_millis(200),
        max_delay: Duration::from_secs(4),
    })
    .build();
```

SDK 要点：

- **签名**：自动完成阿里云 RPC 的 HMAC-SHA1 签名，参数以 `application/x-www-form-urlencoded` POST 提交（对齐官方 Go SDK）。
- **重试**：仅对可重试错误（网络、限流、5xx 等）按指数退避 + 抖动重试；校验类错误（`DirectMailError::Validation`）确定性失败，不重试。
- **凭据脱敏**：`Debug` 输出中 `access_key_secret` 始终为 `<redacted>`。
- **可测试性**：builder 暴露 `clock` / `nonce_gen` / `sleeper` 注入点，便于在测试中固定时间戳、随机数与睡眠。

### 运行 SDK 自带示例二进制

```bash
ALIYUN_DIRECTMAIL_EXAMPLE=1 \
ALIYUN_ACCESS_KEY_ID="LTAI...." \
ALIYUN_ACCESS_KEY_SECRET="********" \
ALIYUN_DIRECTMAIL_ACCOUNT_NAME="service@mail.example.com" \
ALIYUN_DIRECTMAIL_TO_ADDRESS="you@example.com" \
cargo run -p aliyun-dm-sdk --bin aliyun-dm-sdk
```

---

## 安全说明

- **无 AUTH / 无 STARTTLS**：本阶段网关不认证客户端、不加密传输。默认绑定 `127.0.0.1`；若绑定非回环地址，启动时会 warn，且**必须**由部署方在网络层（安全组 / 防火墙 / 内网隔离）保证只有可信来源能连入——否则等同于开放的匿名邮件入口。
- **凭据**：`DIRECTMAIL_ACCESS_KEY_SECRET` 通过环境变量注入，日志与 `Debug` 输出均脱敏。建议使用最小权限的 RAM 子账号 AccessKey。
- **发件身份**：真正的发件人由 `DIRECTMAIL_ACCOUNT_NAME` 决定，与客户端声称的 `From` 无关，避免伪造发信地址。

---

## 开发

```bash
cargo build              # 构建整个 workspace
cargo test               # 运行单测 + 集成测试（含 SMTP e2e、wiremock、签名向量）
cargo clippy --all-targets
cargo fmt
```

测试覆盖：

- `aliyun-directmail-smtp-gateway/tests/smtp_e2e.rs`：完整 SMTP 会话、分包、超限、收件人上限、MIME 结构策略、连接数上限、投递超时等。
- `aliyun-dm-sdk/tests/`：`client_wiremock.rs`（HTTP 行为/重试）、`signature_vectors.rs`（签名向量）。

CI（`.github/workflows/`）：`ci.yml` 跑构建/测试/lint；`release.yml` 在推送 `v*.*.*` 标签时产出跨平台二进制（Linux gnu/musl、aarch64、macOS x86/arm、Windows）并推送多架构 GHCR 镜像。

```bash
git tag v0.1.0 && git push origin v0.1.0   # 触发发布
```
