// 与 Rust 侧 config.rs 对应的类型定义

export type OsType = "linux" | "windows";

/** SSH 探测到的系统类型 */
export type DetectedOs = "windows" | "ubuntu" | "centos" | "linux";

export interface AuthConfig {
  method: "password" | "key";
  password?: string;
  keyPath?: string;
  passphrase?: string | null;
}

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

export interface TermDataPayload {
  sessionId: string;
  data: string;
}

export interface TermClosedPayload {
  sessionId: string;
}

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

export interface ProjectPackPreview {
  projectId: string;
  projectName: string;
  localDir: string;
  includedCount: number;
  oldCount: number;
  ignoredCount?: number;
  tree: PackTreeNode[];
}

export type CopyMode = "smb" | "upload";

/** full=上传并替换；stage=仅上传中转；replace=从中转替换 */
export type DeployMode = "full" | "stage" | "replace";

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

export interface TunnelStatusInfo {
  id: string;
  state: "stopped" | "connecting" | "active" | "reconnecting" | "error";
  message: string;
  activeConnections: number;
  totalReconnects: number;
}

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

export interface TaskLogPayload {
  taskId: string;
  level: "info" | "warn" | "error" | "success";
  message: string;
  ts: string;
}

export interface TaskStatePayload {
  taskId: string;
  state: "running" | "success" | "failed" | "cancelled";
  message: string;
}

export interface TaskProgressPayload {
  taskId: string;
  percent: number;
  step: string;
}
