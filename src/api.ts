import { invoke } from "@tauri-apps/api/core";
import type {
  AppConfig,
  ReleaseRecord,
  FrontendReleaseRecord,
  TunnelStatusInfo,
  CopyMode,
  DeployMode,
  ServerConfig,
  LocalDirEntry,
  LocalFileEntry,
  SftpEntry,
} from "./types";

// Rust 命令调用封装
export const api = {
  getConfig: () => invoke<AppConfig>("get_config"),
  saveConfig: (config: AppConfig) => invoke<void>("save_config", { config }),
  getConfigPath: () => invoke<string>("get_config_path"),
  testServer: (serverId: string) => invoke<string>("test_server", { serverId }),
  testServerDraft: (server: ServerConfig) =>
    invoke<string>("test_server_draft", { server }),

  startTunnel: (tunnelId: string) => invoke<void>("start_tunnel", { tunnelId }),
  stopTunnel: (tunnelId: string) => invoke<void>("stop_tunnel", { tunnelId }),
  tunnelStatus: () => invoke<TunnelStatusInfo[]>("tunnel_status"),

  startBackendDeploy: (request: {
    groupId: string;
    projectIds: string[];
    releaseName: string;
    copyMode?: CopyMode | null;
    mode?: DeployMode;
    backupSibling?: boolean;
  }) => invoke<string>("start_backend_deploy", { request }),
  startRollback: (releaseId: string) =>
    invoke<string>("start_rollback", { releaseId }),
  getReleases: () => invoke<ReleaseRecord[]>("get_releases"),
  getFrontendReleases: () =>
    invoke<FrontendReleaseRecord[]>("get_frontend_releases"),
  startFrontendRollback: (releaseId: string) =>
    invoke<string>("start_frontend_rollback", { releaseId }),

  startFrontendDeploy: (
    targetIds: string[],
    options: { mode: DeployMode; backupSibling: boolean }
  ) => invoke<string>("start_frontend_deploy", { targetIds, options }),
  startDockerDeploy: (targetId: string) =>
    invoke<string>("start_docker_deploy", { targetId }),
  cancelTask: (taskId: string) => invoke<void>("cancel_task", { taskId }),

  exportConfig: (path: string) => invoke<void>("export_config", { path }),
  importConfig: (path: string) => invoke<AppConfig>("import_config", { path }),

  terminalOpen: (serverId: string, cols: number, rows: number) =>
    invoke<string>("terminal_open", { serverId, cols, rows }),
  terminalWrite: (sessionId: string, data: string) =>
    invoke<void>("terminal_write", { sessionId, data }),
  terminalResize: (sessionId: string, cols: number, rows: number) =>
    invoke<void>("terminal_resize", { sessionId, cols, rows }),
  terminalClose: (sessionId: string) =>
    invoke<void>("terminal_close", { sessionId }),

  getHomeDir: () => invoke<string>("get_home_dir"),
  listLocalDrives: () => invoke<LocalDirEntry[]>("list_local_drives"),
  listLocalDir: (path: string) =>
    invoke<LocalDirEntry[]>("list_local_dir", { path }),

  sftpList: (serverId: string, path: string) =>
    invoke<SftpEntry[]>("sftp_list", { serverId, path }),
  sftpUpload: (
    serverId: string,
    localPath: string,
    remotePath: string,
    transferId: string,
    fileIndex?: number,
    fileCount?: number
  ) =>
    invoke<void>("sftp_upload", {
      serverId,
      localPath,
      remotePath,
      transferId,
      fileIndex,
      fileCount,
    }),
  sftpCancelUpload: (transferId: string) =>
    invoke<void>("sftp_cancel_upload", { transferId }),
  sftpCollectLocalFiles: (directory: string) =>
    invoke<LocalFileEntry[]>("sftp_collect_local_files", { directory }),
  sftpDisconnect: (serverId: string) =>
    invoke<void>("sftp_disconnect", { serverId }),
  sftpDownload: (serverId: string, remotePath: string, localPath: string) =>
    invoke<void>("sftp_download", { serverId, remotePath, localPath }),
  sftpMkdir: (serverId: string, path: string) =>
    invoke<void>("sftp_mkdir", { serverId, path }),
  sftpRemove: (serverId: string, path: string) =>
    invoke<void>("sftp_remove", { serverId, path }),
  sftpRename: (serverId: string, fromPath: string, toPath: string) =>
    invoke<void>("sftp_rename", { serverId, fromPath, toPath }),

  openRdp: (
    serverId: string,
    options?: { width?: number; height?: number; fullscreen?: boolean }
  ) =>
    invoke<string>("open_rdp", {
      serverId,
      width: options?.width,
      height: options?.height,
      fullscreen: options?.fullscreen,
    }),

  getMcpStatus: () => invoke<number | null>("get_mcp_status"),

  getLogPath: () => invoke<string>("get_log_path"),
  getLogDir: () => invoke<string>("get_log_dir"),
  setLoggingEnabled: (enabled: boolean) =>
    invoke<void>("set_logging_enabled", { enabled }),
  openLogDir: () => invoke<void>("open_log_dir"),
  readRecentLogs: (maxLines?: number) =>
    invoke<string>("read_recent_logs", { maxLines }),
};
