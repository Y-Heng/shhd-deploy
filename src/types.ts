// 与 Rust 侧 config.rs 对应的类型定义

export type OsType = "linux" | "windows";

/** SSH 探测到的系统类型 */
export type DetectedOs = "windows" | "ubuntu" | "centos" | "linux";

/** SSH 认证；密码会写入本机配置文件 */
export interface AuthConfig {
  method: "password" | "key";
  password?: string;
  keyPath?: string;
  passphrase?: string | null;
}

/** 一台可 SSH 的服务器（Windows 端口填 22，不是 RDP 3389） */
export interface ServerConfig {
  id: string;
  name: string;
  os: OsType;
  host: string;
  port: number;
  username: string;
  auth: AuthConfig;
  jumpServerId?: string | null;
  group?: string | null;
  /** SSH 连接/测试后探测到的系统类型 */
  detectedOs?: DetectedOs | string | null;
  /** 远程桌面分辨率预设：1080p / 900p / 768p / 720p / fullscreen / default */
  rdpPreset?: string | null;
  /** 该服务器专属的远端快捷目录 */
  sftpRemoteShortcuts?: string[];
  /** 该服务器专属的本地快捷目录 */
  sftpLocalShortcuts?: string[];
}

/** 经跳板机的本地端口转发 */
export interface TunnelConfig {
  id: string;
  name: string;
  viaServerId: string;
  localPort: number;
  remoteHost: string;
  remotePort: number;
  autoStart: boolean;
  group?: string | null;
}

/** 终端输出事件：data 为 base64 原始字节 */
export interface TermDataPayload {
  sessionId: string;
  data: string;
}

/** 终端会话关闭事件 */
export interface TermClosedPayload {
  sessionId: string;
}

/** 后端负载组里的一个项目（本地产物目录 ↔ 服务器应用目录） */
export interface BackendProject {
  id: string;
  name: string;
  localBinDir: string;
  remoteAppDir: string;
  healthCheckUrl?: string | null;
  healthCheckRetries: number;
  healthCheckDelaySecs: number;
  /** 替换前停止脚本（PowerShell），空则按旧配置决定是否用 IIS 方案 */
  stopScript?: string;
  /** 替换后启动脚本 */
  startScript?: string;
  /** 兼容旧配置：未写脚本时是否套用 IIS 默认方案 */
  stopIisBeforeReplace?: boolean;
  /** gitignore 风格忽略规则，匹配到的文件不打包、不替换 */
  ignoreRules?: string;
  /** gitignore 风格白名单，即使早于日期也强制打包 */
  whitelistRules?: string;
  /** 兼容旧配置；实际过滤以当次部署的改动起始日为准 */
  newerThan?: string | null;
}

/** 后端打包预览树节点 */
export interface PackTreeNode {
  path: string;
  name: string;
  isDir: boolean;
  included: boolean;
  ignored?: boolean;
  disabled?: boolean;
  reason: string;
  modifiedAt?: string | null;
  children: PackTreeNode[];
}

/** 单个后端项目的打包预览 */
export interface ProjectPackPreview {
  projectId: string;
  projectName: string;
  localDir: string;
  includedCount: number;
  oldCount: number;
  ignoredCount?: number;
  tree: PackTreeNode[];
}

/** smb=主服务器 robocopy 到备机；upload=经跳板机 SSH 分发 zip */
export type CopyMode = "smb" | "upload";

/** full=上传并替换；stage=仅上传中转；replace=从中转替换 */
export type DeployMode = "full" | "stage" | "replace";

/** 一组 Windows 负载：滚动发布、中转目录、备机同步方式 */
export interface BackendGroup {
  id: string;
  name: string;
  /** 组内服务器列表（第一台作为上传中转与滚动起点） */
  serverIds: string[];
  /** 兼容旧配置的主/备字段 */
  primaryServerId?: string | null;
  secondaryServerId?: string | null;
  stagingDir: string;
  backupDir: string;
  copyMode: CopyMode;
  projects: BackendProject[];
}

/** 前端 nginx 静态资源部署目标 */
export interface FrontendTarget {
  id: string;
  name: string;
  serverIds: string[];
  localDir: string;
  remoteDir: string;
  /** 自定义中转目录，留空默认 <remoteDir>-staging */
  stagingDir?: string | null;
  deleteExtraneous: boolean;
  /** 环境分组，如 开发环境 / 正式环境 */
  group?: string | null;
  /** 部署前本地打包命令，如 npm run build */
  packCommand?: string | null;
  /** 打包命令工作目录，留空则用 localDir 上一级 */
  packWorkDir?: string | null;
}

/** Linux 上按顺序执行的 Docker/compose 目标 */
export interface DockerTarget {
  id: string;
  name: string;
  serverId: string;
  workDir: string;
  commands: string[];
  group?: string | null;
}

/** MCP 权限级别 */
export type McpPermission = "readonly" | "stage" | "full";

export interface McpConfig {
  enabled: boolean;
  port: number;
  permission: McpPermission;
  /** null = 全部允许；[] = 全部禁止；有值则只允许列出的 id */
  allowedBackendGroupIds?: string[] | null;
  allowedFrontendTargetIds?: string[] | null;
  allowedDockerTargetIds?: string[] | null;
}

/** 诊断日志配置（默认关闭） */
export interface LoggingConfig {
  enabled: boolean;
  level?: string;
}

/** 终端常用命令（类似 Termius Snippets） */
export interface QuickCommand {
  id: string;
  name: string;
  command: string;
  /** 分组名，空则归入「未分组」 */
  group?: string | null;
}

/** SFTP 远端目录条目 */
export interface SftpEntry {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
  mtime: number;
  hidden?: boolean;
}

/** 本地目录条目（与 SftpEntry 字段对齐） */
export interface LocalDirEntry {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
  mtime: number;
  hidden?: boolean;
}

/** SFTP 单文件上传进度 */
export interface SftpProgressPayload {
  transferId: string;
  fileName: string;
  transferred: number;
  total: number;
  done: boolean;
  fileIndex: number;
  fileCount: number;
}

/** 本地待上传文件 */
export interface LocalFileEntry {
  localPath: string;
  relativePath: string;
}

/** 本机持久化配置（密码也在这里，导入导出时注意） */
export interface AppConfig {
  servers: ServerConfig[];
  tunnels: TunnelConfig[];
  backendGroups: BackendGroup[];
  frontendTargets: FrontendTarget[];
  dockerTargets: DockerTarget[];
  mcp: McpConfig;
  quickCommands?: QuickCommand[];
  logging?: LoggingConfig;
  /** SFTP 公共远端快捷目录 */
  sftpShortcuts?: string[];
  /** SFTP 公共本地快捷目录 */
  sftpLocalShortcuts?: string[];
}

/** 隧道运行状态快照 */
export interface TunnelStatusInfo {
  id: string;
  state: "stopped" | "connecting" | "active" | "reconnecting" | "error";
  message: string;
  activeConnections: number;
  totalReconnects: number;
}

/** 后端发布历史一条 */
export interface ReleaseRecord {
  id: string;
  releaseName: string;
  groupId: string;
  groupName: string;
  projectIds: string[];
  serverIds: string[];
  createdAt: string;
  status: string;
}

/** 前端发布历史一条 */
export interface FrontendReleaseRecord {
  id: string;
  createdAt: string;
  mode: string;
  groupName: string;
  targetIds: string[];
  targetNames: string[];
  serverNames: string[];
  serverIds?: string[];
  /** 有值表示这次发布做了线上快照，可回滚 */
  backupSuffix?: string | null;
  status: string;
  message: string;
}

/** 部署任务日志事件 */
export interface TaskLogPayload {
  taskId: string;
  level: "info" | "warn" | "error" | "success";
  message: string;
  ts: string;
}

/** 部署任务状态事件 */
export interface TaskStatePayload {
  taskId: string;
  state: "running" | "success" | "failed" | "cancelled";
  message: string;
}

/** 部署任务进度事件 */
export interface TaskProgressPayload {
  taskId: string;
  percent: number;
  step: string;
}
