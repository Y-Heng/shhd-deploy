//! 应用配置的读写与数据结构（服务器、隧道、部署映射、MCP、日志等）。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 服务器操作系统类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OsType {
    Linux,
    Windows,
}

/// SSH 认证方式
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "method")]
pub enum AuthConfig {
    /// 密码认证
    #[serde(rename = "password")]
    Password { password: String },
    /// 私钥认证
    #[serde(rename = "key")]
    Key {
        key_path: String,
        #[serde(default)]
        passphrase: Option<String>,
    },
}

/// 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub id: String,
    pub name: String,
    pub os: OsType,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthConfig,
    /// 通过哪台跳板机连接（null 表示直连）
    #[serde(default)]
    pub jump_server_id: Option<String>,
    /// 分组名称（null 表示未分组）
    #[serde(default)]
    pub group: Option<String>,
    /// SSH 探测到的系统类型（windows / ubuntu / centos / linux）
    #[serde(default)]
    pub detected_os: Option<String>,
    /// 远程桌面分辨率预设（1080p / 900p / 768p / 720p / fullscreen / default）
    #[serde(default)]
    pub rdp_preset: Option<String>,
    /// 该服务器专属的远端快捷目录
    #[serde(default)]
    pub sftp_remote_shortcuts: Vec<String>,
    /// 该服务器专属的本地快捷目录
    #[serde(default)]
    pub sftp_local_shortcuts: Vec<String>,
}

/// 隧道配置：本地端口 -> (经由服务器) -> 远端地址:端口
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelConfig {
    pub id: String,
    pub name: String,
    /// 经由哪台服务器转发（跳板机）
    pub via_server_id: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    /// 应用启动时自动开启
    #[serde(default)]
    pub auto_start: bool,
    /// 分组名称（null 表示未分组）
    #[serde(default)]
    pub group: Option<String>,
}

/// 后端部署项目（一个 IIS 站点 / 应用目录）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendProject {
    pub id: String,
    pub name: String,
    /// 本地发布产物目录（如 D:\Code\JianYue\build-info\to-backend\admin）
    pub local_bin_dir: String,
    /// 服务器上应用目录（与本地一比一对应）
    pub remote_app_dir: String,
    /// 健康检查地址（在服务器本机执行，如 http://localhost:8081/admin/api/health）
    #[serde(default)]
    pub health_check_url: Option<String>,
    /// 健康检查重试次数
    #[serde(default = "default_health_retries")]
    pub health_check_retries: u32,
    /// 每次重试间隔秒数
    #[serde(default = "default_health_delay")]
    pub health_check_delay_secs: u32,
    /// 替换/回滚前执行的 PowerShell（空则看 stop_iis_before_replace）
    #[serde(default)]
    pub stop_script: String,
    /// 替换/回滚后执行的 PowerShell
    #[serde(default)]
    pub start_script: String,
    /// 兼容旧配置：未写脚本时是否套用 IIS 默认方案
    #[serde(default = "default_true")]
    pub stop_iis_before_replace: bool,
    /// gitignore 风格忽略规则（相对本地产物目录），匹配到的文件不打包、不替换。
    #[serde(default)]
    pub ignore_rules: String,
    /// gitignore 风格白名单：即使早于「仅替换日期」也强制打包（如新加的老版本依赖包）。
    #[serde(default)]
    pub whitelist_rules: String,
    /// 只打包此日期当天及之后修改的文件（YYYY-MM-DD），空表示不按时间过滤。
    #[serde(default)]
    pub newer_than: Option<String>,
}

fn default_health_retries() -> u32 {
    10
}

fn default_health_delay() -> u32 {
    3
}

fn default_true() -> bool {
    true
}

/// 主备文件复制方式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CopyMode {
    /// 主服务器通过内网 SMB 共享复制到备服务器（需配置 D$）
    Smb,
    /// SSH 分发 zip：公网上传一次到跳板机，再内网拷到各 Windows
    Upload,
}

/// 后端负载组（一组同构 Windows 服务器，数量不限）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendGroup {
    pub id: String,
    pub name: String,
    /// 组内服务器列表（第一台作为上传中转与滚动发布起点）；为空时回退到旧的主/备字段
    #[serde(default)]
    pub server_ids: Vec<String>,
    /// 兼容旧配置：主服务器
    #[serde(default)]
    pub primary_server_id: Option<String>,
    /// 兼容旧配置：备服务器
    #[serde(default)]
    pub secondary_server_id: Option<String>,
    /// 暂存目录（发布包解压位置，如 D:\code\sites\devlop）
    pub staging_dir: String,
    /// 备份目录（替换前备份位置，如 D:\code\sites\backup）
    pub backup_dir: String,
    #[serde(default = "default_copy_mode")]
    pub copy_mode: CopyMode,
    pub projects: Vec<BackendProject>,
}

impl BackendGroup {
    /// 解析组内有效服务器列表（新字段优先，回退到旧的主+备字段）
    pub fn effective_server_ids(&self) -> Vec<String> {
        if !self.server_ids.is_empty() {
            return self.server_ids.clone();
        }
        let mut ids = Vec::new();
        if let Some(primary) = &self.primary_server_id {
            if !primary.is_empty() {
                ids.push(primary.clone());
            }
        }
        if let Some(secondary) = &self.secondary_server_id {
            if !secondary.is_empty() {
                ids.push(secondary.clone());
            }
        }
        ids
    }
}

fn default_copy_mode() -> CopyMode {
    CopyMode::Upload
}

/// 前端部署目标（本地 dist 目录 -> 各服务器 nginx 目录）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendTarget {
    pub id: String,
    pub name: String,
    /// 要同步到的服务器列表
    pub server_ids: Vec<String>,
    /// 本地构建产物目录
    pub local_dir: String,
    /// 服务器上的静态目录（Linux nginx 或 Windows IIS 目录）
    pub remote_dir: String,
    /// 自定义中转目录（留空时默认使用 <remote_dir>-staging）
    #[serde(default)]
    pub staging_dir: Option<String>,
    /// 是否删除远端多余文件（本地没有的文件）
    #[serde(default)]
    pub delete_extraneous: bool,
    /// 环境分组（如 开发环境 / 正式环境）
    #[serde(default)]
    pub group: Option<String>,
    /// 部署前在本地执行的打包命令（如 npm run build），留空则跳过
    #[serde(default)]
    pub pack_command: Option<String>,
    /// 打包命令工作目录，留空则用 local_dir 的上一级
    #[serde(default)]
    pub pack_work_dir: Option<String>,
}

/// Docker 部署目标
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerTarget {
    pub id: String,
    pub name: String,
    pub server_id: String,
    /// 服务器上的工作目录（compose 文件所在目录）
    pub work_dir: String,
    /// 依次执行的命令
    pub commands: Vec<String>,
    /// 分组名称（null 表示未分组）
    #[serde(default)]
    pub group: Option<String>,
}

/// MCP 权限级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpPermission {
    /// 只读：仅查询配置、发布历史、任务状态
    Readonly,
    /// 仅中转（推荐）：允许上传到中转目录，不允许替换线上
    Stage,
    /// 完全访问：允许替换、回滚、Docker 部署、隧道控制
    Full,
}

fn default_mcp_port() -> u16 {
    17423
}

fn default_mcp_permission() -> McpPermission {
    McpPermission::Stage
}

/// MCP 服务配置（供 AI 客户端调用本工具）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 监听端口（只绑定 127.0.0.1，本机可见）
    #[serde(default = "default_mcp_port")]
    pub port: u16,
    #[serde(default = "default_mcp_permission")]
    pub permission: McpPermission,
    /// 允许访问的后端负载组（null = 全部）
    #[serde(default)]
    pub allowed_backend_group_ids: Option<Vec<String>>,
    /// 允许访问的前端部署目标（null = 全部）
    #[serde(default)]
    pub allowed_frontend_target_ids: Option<Vec<String>>,
    /// 允许访问的 Docker 目标（null = 全部）
    #[serde(default)]
    pub allowed_docker_target_ids: Option<Vec<String>>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_mcp_port(),
            permission: default_mcp_permission(),
            allowed_backend_group_ids: None,
            allowed_frontend_target_ids: None,
            allowed_docker_target_ids: None,
        }
    }
}

fn default_log_level() -> String {
    "info".into()
}

/// 诊断日志配置（默认关闭）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            level: default_log_level(),
        }
    }
}

/// 终端常用命令（类似 Termius Snippets）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickCommand {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub group: Option<String>,
}

/// 应用总配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default)]
    pub servers: Vec<ServerConfig>,
    #[serde(default)]
    pub tunnels: Vec<TunnelConfig>,
    #[serde(default)]
    pub backend_groups: Vec<BackendGroup>,
    #[serde(default)]
    pub frontend_targets: Vec<FrontendTarget>,
    #[serde(default)]
    pub docker_targets: Vec<DockerTarget>,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default)]
    pub quick_commands: Vec<QuickCommand>,
    #[serde(default)]
    pub logging: LoggingConfig,
    /// SFTP 公共远端快捷目录（所有服务器共用）
    #[serde(default = "default_sftp_shortcuts")]
    pub sftp_shortcuts: Vec<String>,
    /// SFTP 公共本地快捷目录（所有服务器共用）
    #[serde(default)]
    pub sftp_local_shortcuts: Vec<String>,
}

fn default_sftp_shortcuts() -> Vec<String> {
    vec![
        "/usr/share/nginx/html".into(),
        "/etc/nginx".into(),
        "/var/log".into(),
    ]
}

impl AppConfig {
    pub fn find_server(&self, server_id: &str) -> Result<&ServerConfig> {
        self.servers
            .iter()
            .find(|server| server.id == server_id)
            .with_context(|| format!("找不到服务器配置: {}", server_id))
    }
}

/// 判断主机地址是否为模板占位符（未填写真实 IP/域名）
pub fn is_placeholder_host(host: &str) -> bool {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return true;
    }
    if trimmed.contains("改成") {
        return true;
    }
    let lower = trimmed.to_lowercase();
    [
        "placeholder",
        "changeme",
        "example.com",
        "example.org",
        "your-ip",
        "your_ip",
        "todo",
        "xxx",
    ]
    .iter()
    .any(|keyword| lower.contains(keyword))
}

/// 检查服务器及其跳板链上的 host 是否均已配置为可连接地址
pub fn validate_server_hosts(config: &AppConfig, server_id: &str) -> Result<(), String> {
    fn check(config: &AppConfig, server_id: &str, depth: u8) -> Result<(), String> {
        if depth > 4 {
            return Err("跳板机链过深或存在循环引用".into());
        }
        let server = config
            .find_server(server_id)
            .map_err(|error| error.to_string())?;
        if is_placeholder_host(&server.host) {
            return Err(format!(
                "服务器「{}」主机地址未配置（{}）",
                server.name, server.host
            ));
        }
        if let Some(jump_id) = &server.jump_server_id {
            check(config, jump_id, depth + 1)?;
        }
        Ok(())
    }
    check(config, server_id, 0)
}

/// 配置文件所在目录（%APPDATA%\shhd-deploy）
pub fn config_dir() -> PathBuf {
    let base_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base_dir.join("shhd-deploy")
}

/// 主配置文件路径
pub fn config_file_path() -> PathBuf {
    config_dir().join("config.json")
}

/// 后端发布历史
pub fn releases_file_path() -> PathBuf {
    config_dir().join("releases.json")
}

/// 前端发布历史
pub fn frontend_releases_file_path() -> PathBuf {
    config_dir().join("frontend_releases.json")
}

/// TOFU 主机指纹缓存
pub fn known_hosts_file_path() -> PathBuf {
    config_dir().join("known_hosts.json")
}

/// 加载配置；文件不存在时生成默认模板并保存
pub fn load_or_init() -> Result<AppConfig> {
    let path = config_file_path();
    if path.exists() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
        // 兼容带 UTF-8 BOM 的文件（部分编辑器保存时会加上）
        let content = content.trim_start_matches('\u{feff}');
        let config: AppConfig =
            serde_json::from_str(content).with_context(|| "配置文件格式错误")?;
        return Ok(config);
    }
    let default_config = default_template();
    save(&default_config)?;
    Ok(default_config)
}

pub fn save(config: &AppConfig) -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let content = serde_json::to_string_pretty(config)?;
    std::fs::write(config_file_path(), content)?;
    Ok(())
}

/// 默认配置模板：按用户实际环境预填，首次运行后在界面中完善主机地址与密码
fn default_template() -> AppConfig {
    AppConfig {
        servers: vec![
            ServerConfig {
                id: "linux-proxy".into(),
                name: "Linux-大带宽代理(跳板机)".into(),
                os: OsType::Linux,
                host: "改成公网IP".into(),
                port: 22,
                username: "root".into(),
                auth: AuthConfig::Password {
                    password: "改成密码".into(),
                },
                jump_server_id: None,
                group: Some("Linux".into()),
                detected_os: None,
                rdp_preset: None,
                sftp_remote_shortcuts: vec![],
                sftp_local_shortcuts: vec![],
            },
            ServerConfig {
                id: "win-48-5".into(),
                name: "Windows-A(172.16.48.5)".into(),
                os: OsType::Windows,
                host: "172.16.48.5".into(),
                port: 22,
                username: "administrator".into(),
                auth: AuthConfig::Password {
                    password: "改成密码".into(),
                },
                jump_server_id: Some("linux-proxy".into()),
                group: Some("Windows-service组".into()),
                detected_os: None,
                rdp_preset: None,
                sftp_remote_shortcuts: vec![],
                sftp_local_shortcuts: vec![],
            },
            ServerConfig {
                id: "win-48-16".into(),
                name: "Windows-B(172.16.48.16)".into(),
                os: OsType::Windows,
                host: "172.16.48.16".into(),
                port: 22,
                username: "administrator".into(),
                auth: AuthConfig::Password {
                    password: "改成密码".into(),
                },
                jump_server_id: Some("linux-proxy".into()),
                group: Some("Windows-service组".into()),
                detected_os: None,
                rdp_preset: None,
                sftp_remote_shortcuts: vec![],
                sftp_local_shortcuts: vec![],
            },
            ServerConfig {
                id: "win-0-10".into(),
                name: "Windows-C(172.16.0.10)".into(),
                os: OsType::Windows,
                host: "172.16.0.10".into(),
                port: 22,
                username: "administrator".into(),
                auth: AuthConfig::Password {
                    password: "改成密码".into(),
                },
                jump_server_id: Some("linux-proxy".into()),
                group: Some("Windows-www组".into()),
                detected_os: None,
                rdp_preset: None,
                sftp_remote_shortcuts: vec![],
                sftp_local_shortcuts: vec![],
            },
            ServerConfig {
                id: "win-16-8".into(),
                name: "Windows-D(172.16.16.8)".into(),
                os: OsType::Windows,
                host: "172.16.16.8".into(),
                port: 22,
                username: "administrator".into(),
                auth: AuthConfig::Password {
                    password: "改成密码".into(),
                },
                jump_server_id: Some("linux-proxy".into()),
                group: Some("Windows-www组".into()),
                detected_os: None,
                rdp_preset: None,
                sftp_remote_shortcuts: vec![],
                sftp_local_shortcuts: vec![],
            },
            ServerConfig {
                id: "linux-docker".into(),
                name: "Linux-Docker(新项目)".into(),
                os: OsType::Linux,
                host: "改成IP".into(),
                port: 22,
                username: "root".into(),
                auth: AuthConfig::Password {
                    password: "改成密码".into(),
                },
                jump_server_id: None,
                group: Some("Linux".into()),
                detected_os: None,
                rdp_preset: None,
                sftp_remote_shortcuts: vec![],
                sftp_local_shortcuts: vec![],
            },
        ],
        tunnels: vec![
            TunnelConfig {
                id: "rdp-win-a".into(),
                name: "RDP-WindowsA".into(),
                via_server_id: "linux-proxy".into(),
                local_port: 13389,
                remote_host: "172.16.48.5".into(),
                remote_port: 3389,
                auto_start: false,
                group: Some("远程桌面".into()),
            },
            TunnelConfig {
                id: "mysql".into(),
                name: "生产MySQL".into(),
                via_server_id: "linux-proxy".into(),
                local_port: 13306,
                remote_host: "改成MySQL内网地址".into(),
                remote_port: 3306,
                auto_start: false,
                group: Some("数据库".into()),
            },
            TunnelConfig {
                id: "redis".into(),
                name: "生产Redis".into(),
                via_server_id: "linux-proxy".into(),
                local_port: 16379,
                remote_host: "改成Redis内网地址".into(),
                remote_port: 6379,
                auto_start: false,
                group: Some("数据库".into()),
            },
        ],
        backend_groups: vec![BackendGroup {
            id: "group-service".into(),
            name: "service组(48.5 / 48.16)".into(),
            server_ids: vec!["win-48-5".into(), "win-48-16".into()],
            primary_server_id: None,
            secondary_server_id: None,
            staging_dir: "D:\\code\\sites\\devlop".into(),
            backup_dir: "D:\\code\\sites\\backup".into(),
            copy_mode: CopyMode::Upload,
            projects: vec![
                BackendProject {
                    id: "brand-service-admin".into(),
                    name: "brand-service/admin".into(),
                    local_bin_dir: "D:\\Code\\JianYue\\build-info\\to-backend\\admin".into(),
                    remote_app_dir: "D:\\code\\sites\\to\\brand-service\\admin".into(),
                    health_check_url: Some("http://localhost:8081/admin/swagger".into()),
                    health_check_retries: 10,
                    health_check_delay_secs: 3,
                    stop_script: String::new(),
                    start_script: String::new(),
                    stop_iis_before_replace: true,
                    ignore_rules: "Configs/\nTemplate/\nfavicon.ico\nGlobal.asax\nLog4net.config\nApplicationInsights.config\nWeb.config".into(),
                    whitelist_rules: String::new(),
                    newer_than: None,
                },
                BackendProject {
                    id: "service-rest".into(),
                    name: "service/rest".into(),
                    local_bin_dir: "D:\\Code\\JianYue\\build-info\\to-backend\\client".into(),
                    remote_app_dir: "D:\\code\\sites\\to\\service\\rest".into(),
                    health_check_url: Some("http://localhost:8083/swagger".into()),
                    health_check_retries: 10,
                    health_check_delay_secs: 3,
                    stop_script: String::new(),
                    start_script: String::new(),
                    stop_iis_before_replace: true,
                    ignore_rules: String::new(),
                    whitelist_rules: String::new(),
                    newer_than: None,
                },
            ],
        }],
        frontend_targets: vec![FrontendTarget {
            id: "mch-web".into(),
            name: "商户后台 Mch-Web".into(),
            server_ids: vec!["linux-proxy".into()],
            local_dir: "D:\\Code\\JianYue\\Pages\\to-frontend\\Mch-Web\\dist".into(),
            remote_dir: "/usr/share/nginx/html/to/brand".into(),
            staging_dir: None,
            delete_extraneous: false,
            group: Some("开发环境".into()),
            pack_command: None,
            pack_work_dir: None,
        }],
        docker_targets: vec![DockerTarget {
            id: "zx-infra".into(),
            name: "zx-infra 集群".into(),
            server_id: "linux-docker".into(),
            work_dir: "/opt/zx".into(),
            commands: vec![
                "docker compose pull".into(),
                "docker compose up -d".into(),
                "docker compose ps".into(),
            ],
            group: Some("默认".into()),
        }],
        mcp: McpConfig::default(),
        logging: LoggingConfig::default(),
        quick_commands: vec![
            QuickCommand {
                id: "qc-ll".into(),
                name: "详细列表".into(),
                command: "ls -lah".into(),
                group: Some("常用".into()),
            },
            QuickCommand {
                id: "qc-df".into(),
                name: "磁盘空间".into(),
                command: "df -h".into(),
                group: Some("常用".into()),
            },
            QuickCommand {
                id: "qc-docker-ps".into(),
                name: "Docker 容器".into(),
                command: "docker ps".into(),
                group: Some("Docker".into()),
            },
        ],
        sftp_shortcuts: default_sftp_shortcuts(),
        sftp_local_shortcuts: vec![],
    }
}
