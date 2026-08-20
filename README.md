# 部署工具（KurumiDeploy）

一个基于 Tauri（Rust + Vue 3）的桌面运维工具，替代 Termius + WinSCP + mstsc 拖文件的日常操作：

| 功能        | 说明                                                                                                                                                                   |
| ----------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 隧道        | 经 Linux 跳板机做本地端口转发（MySQL / Redis / RDP / Windows SSH），断线自动重连                                                                                       |
| 后端部署    | to-backend 发布产物一键部署到两台负载的 Windows 服务器：校验 → 压缩上传中转 → 内网同步备机 → 备份 → 滚动替换 bin → 健康检查；支持"仅中转/稍后替换"两段式发布与一键回滚 |
| 前端部署    | 本地 dist 与服务器 nginx 目录逐文件对比，增量上传；支持中转/替换两段式（中转目录可自定义）与日期备份                                                                   |
| Docker 部署 | SSH 到 Linux 服务器按顺序执行 compose 命令                                                                                                                             |
| SSH 终端    | 内置 xterm 多标签终端，Linux/Windows 服务器均可用                                                                                                                      |
| 远程桌面    | 一键经跳板机建隧道并拉起 mstsc（macOS 调起 Windows App）                                                                                                               |
| MCP 服务    | 供 AI 客户端（Cursor 等）直接调用部署能力，权限分级可控                                                                                                                |

## MCP 接入（AI 直接调用部署）

本工具必须保持运行，MCP 才可用。服务只监听 `127.0.0.1`，不对局域网暴露；`list_config` 摘要不含密码或私钥。

### 1. 在本工具里开启

「设置 → MCP 服务」打开开关，选择权限级别，需要时再限制允许访问的目标，保存。状态应变为「运行中 · 127.0.0.1:端口」。

默认端口 `17423`。绑定失败说明端口被占用，换一个后再改客户端配置。

### 2. 写入 Cursor 的 mcp.json

全局：`%USERPROFILE%\.cursor\mcp.json`；也可以写到项目 `.cursor\mcp.json`。保存后在 Cursor 里刷新 MCP（或重启 Cursor）。

```json
{
  "mcpServers": {
    "shhd-deploy": { "url": "http://127.0.0.1:17423/mcp" }
  }
}
```

### 3. 权限与工具（服务端强制执行）

| 权限 | 可用工具 | 说明 |
| --- | --- | --- |
| 只读 | `list_config`、`list_releases`、`list_frontend_releases`、`get_task_status`、`list_tunnels` | 只能查询 |
| 仅中转（推荐） | 上述查询 + `backend_deploy` / `frontend_deploy` | `mode` 只能是 `stage`，替换线上需人在「发布历史」点执行替换 |
| 完全访问 | 额外 `rollback`、`frontend_rollback`、`docker_deploy`、`tunnel_control`，以及 `mode=full/replace` | 可改线上，谨慎开启 |

工具要点：

- **list_config**：拿负载组 / 项目 / 前端 / Docker 的 id。部署前先调。
- **backend_deploy**：必填 `groupId`、`releaseName`（`yyyyMMdd-功能名`）。可选 `projectIds`、`mode`、`backupSibling`。返回 `taskId`。
- **frontend_deploy**：必填 `targetIds`。可选 `mode`、`backupSibling`。
- **get_task_status**：必填 `taskId`。建议 `waitSeconds=60` 轮询到 `success` / `failed` / `cancelled`（最长 300 秒）。
- **list_releases / list_frontend_releases**：最近发布历史；回滚用 `releaseId`。
- **rollback**：后端回滚，仅 `success` 记录可回滚。
- **frontend_rollback**：前端回滚，需 `success` 且带 `backupSuffix`。
- **docker_deploy**：按目标配置顺序执行命令。
- **list_tunnels / tunnel_control**：查看或启停隧道。

「指定目标」白名单：未勾选的组/项目一律禁止；空列表 = 全部禁止。`null`（所有目标）= 全部允许。

### 4. 推荐 AI 工作流

本地构建/发布脚本 → `list_config` → `backend_deploy(mode=stage)` 或 `frontend_deploy(mode=stage)` → `get_task_status(waitSeconds=60)` 轮询 → 人在软件「发布历史」点「执行替换」。

示例提示词：「先跑本地发布脚本，再用 shhd-deploy 的 list_config 找到对应负载组和项目，调用 backend_deploy（mode=stage，releaseName=今天日期-本次功能名），用 get_task_status waitSeconds=60 轮询，把结果告诉我。不要替换线上。」

## 环境要求（开发机）

### Windows

- Node.js 18+（当前使用 v24）
- Rust stable（rustup 安装，MSVC 工具链）
- Visual Studio 2022 Build Tools（C++ 桌面开发工作负载）

### macOS

- Xcode 命令行工具：`xcode-select --install`
- Rust stable：`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- Node.js 18+（建议用 brew 或 nvm 安装）

## 常用命令

```powershell
npm install          # 安装前端依赖
npm run tauri dev    # 开发模式运行
npm run tauri build  # 打包生成安装程序
```

- Windows 产物：`src-tauri\target\release\bundle\nsis\*.exe`
- macOS 产物：`src-tauri/target/release/bundle/dmg/*.dmg`（Apple 限制，macOS 包只能在 Mac 上构建）

### macOS 补充说明

- 一键远程桌面需要先从 App Store 安装 **Windows App**（原 Microsoft Remote Desktop），工具会生成 `.rdp` 文件自动调起它。
- 同时支持 Intel 与 Apple Silicon 的通用包：

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
npm run tauri build -- --target universal-apple-darwin
```

- 未做签名公证的包首次打开被 Gatekeeper 拦截时，执行 `xattr -cr /Applications/KurumiDeploy.app` 或右键 → 打开。
- 换机器迁移配置：旧机器「设置 → 导出配置」，新机器「导入配置」；注意配置里"本地目录"（发布产物、前端 dist）是每台机器自己的路径，跨系统需要改成对应路径。

## 服务器准备（一次性）

### 四台 Windows 服务器开启 OpenSSH Server

以管理员身份在每台服务器执行（Server 2019 及以上自带该可选功能）：

```powershell
# 安装并设置自启动
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Set-Service -Name sshd -StartupType Automatic
Start-Service sshd

# 防火墙只允许内网访问 22 端口（Windows 服务器本来就不通外网，此步为双保险）
New-NetFirewallRule -Name sshd-lan -DisplayName 'OpenSSH Server (LAN only)' `
  -Enabled True -Direction Inbound -Protocol TCP -Action Allow `
  -LocalPort 22 -RemoteAddress 172.16.0.0/12
```

验证：在本工具「服务器」页点「测试」，应显示计算机名与系统版本。

### Linux 服务器

已有 SSH 即可，无需额外准备。

## 首次使用

1. 启动后先到「服务器」页，把默认模板里的主机地址、密码补全（跳板机公网 IP、四台 Windows 内网 IP 已预填）。也可以让同事「设置 → 导出配置」，你再「导入配置」。
2. 每台服务器点一次「测试」确认连通（第一次会记录主机指纹）。
3. 「设置」页检查外观、MCP（如需 AI 接入）、诊断日志；「后端部署 → 项目配置」核对本机产物目录、服务器应用目录与健康检查地址。本地路径必须是你这台电脑上的实际目录。
4. 到「后端部署」页选组、勾项目、填功能名，开始部署。软件内完整说明见「设置 → 使用说明」。

## 后端部署流程细节

1. 校验本地 `build-info` 产物存在且非空，产物超过 24 小时会警告（防发旧包）。
2. 每个项目的 bin 压缩为 zip；有跳板机时先传到 Linux 跳板机一次，再内网拷到组内各 Windows 解压（不依赖 `D$`）。也可改用主服务器 SMB/`robocopy` 到备机。
3. 服务器端解压，目录结构与你手工习惯一致：`D:\code\sites\devlop\20260812-功能名\to\service\rest\bin`。
4. 逐台执行：备份当前 bin 到 `备份目录\发布名\` → robocopy /MIR 替换 → 本机健康检查通过 → 才处理下一台（线上始终有一台在服务）。
5. 任何一步失败立即停止；「发布历史」页可一键回滚（从备份恢复 + 健康检查）。

## 安全说明

- 所有流量走 SSH 加密通道；Windows 服务器经 Linux 跳板访问，不暴露公网。
- 主机密钥采用首次信任（TOFU），指纹变化会拒绝连接并提示，防中间人攻击。
- 密码保存在本机 `%APPDATA%\shhd-deploy\config.json`，建议尽量改用私钥认证。
- 诊断日志默认关闭，开启后写在 `%APPDATA%\shhd-deploy\logs\`。
- MCP 只绑定本机回环地址；关掉本工具后 AI 客户端立即连不上。
