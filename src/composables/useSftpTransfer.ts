/** 全局 SFTP 上传进度：离开终端页后仍以浮动条显示，可取消 */

import { ref } from "vue";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api } from "../api";
import type { SftpProgressPayload } from "../types";
import type { DeployTask, LogLine, TaskFinalState } from "./useTask";

export interface SftpTransferStartPayload {
  transferId: string;
  serverId: string;
  serverName: string;
  fileCount: number;
  fromRoot?: string;
  toRoot?: string;
}

export interface SftpTransferFilePayload {
  transferId: string;
  fromPath: string;
  toPath: string;
}

export interface SftpTransferEndPayload {
  transferId: string;
  state: Exclude<TaskFinalState, "">;
  successCount: number;
  failedCount: number;
}

function formatSize(size: number) {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / 1024 / 1024).toFixed(2)} MB`;
}

const running = ref(false);
const percent = ref(0);
const step = ref("");
const detail = ref("");
const route = ref("");
const finalState = ref<TaskFinalState>("");
const dismissed = ref(true);
const cancelled = ref(false);
const transferId = ref("");
const serverId = ref("");
const serverName = ref("");
const fileName = ref("");
const fileIndex = ref(0);
const fileCount = ref(0);
const transferred = ref(0);
const total = ref(0);
const currentFilePercent = ref(0);
const fromRoot = ref("");
const toRoot = ref("");
const fromPath = ref("");
const toPath = ref("");

const logs = ref<LogLine[]>([]);
let ready = false;
const unlisteners: UnlistenFn[] = [];

function syncDerived() {
  if (!fileCount.value) percent.value = running.value ? 0 : percent.value;
  else {
    const fileWeight = 100 / fileCount.value;
    const completed = Math.max(0, fileIndex.value - 1) * fileWeight;
    const current = (currentFilePercent.value / 100) * fileWeight;
    percent.value = Math.min(100, Math.round(completed + current));
  }

  const files =
    fileCount.value > 1 ? `文件 ${fileIndex.value}/${fileCount.value} · ` : "";
  const currentName = fileName.value || "准备上传";
  const source = fromPath.value || fromRoot.value;
  const target = toPath.value || toRoot.value;
  const targetLabel = [serverName.value, target].filter(Boolean).join(" ");
  route.value =
    source || targetLabel ? `从 ${source || "本地"}\n到 ${targetLabel || "远端"}` : "";

  if (finalState.value === "cancelled") {
    step.value = `${serverName.value} · 已终止`;
    detail.value = successDetail();
    return;
  }
  if (finalState.value === "failed") {
    step.value = `${serverName.value} · 上传失败`;
    detail.value = `${files}${currentName}`;
    return;
  }
  if (finalState.value === "success") {
    step.value = `${serverName.value} · 已完成 ${fileCount.value} 个文件`;
    detail.value = "";
    return;
  }
  step.value = `${serverName.value} · ${files}${currentName}`;
  detail.value = `${formatSize(transferred.value)} / ${formatSize(total.value)}`;
}

function successDetail() {
  const files =
    fileCount.value > 1 ? `文件 ${fileIndex.value}/${fileCount.value}` : fileName.value;
  return files;
}

function applyStart(payload: SftpTransferStartPayload) {
  transferId.value = payload.transferId;
  serverId.value = payload.serverId;
  serverName.value = payload.serverName;
  fileCount.value = payload.fileCount;
  fileIndex.value = 0;
  fileName.value = "";
  transferred.value = 0;
  total.value = 0;
  currentFilePercent.value = 0;
  fromRoot.value = payload.fromRoot || "";
  toRoot.value = payload.toRoot || "";
  fromPath.value = payload.fromRoot || "";
  toPath.value = payload.toRoot || "";
  cancelled.value = false;
  finalState.value = "";
  dismissed.value = false;
  running.value = true;
  syncDerived();
}

function applyProgress(payload: SftpProgressPayload) {
  if (transferId.value && payload.transferId !== transferId.value) return;
  if (!transferId.value) transferId.value = payload.transferId;
  fileName.value = payload.fileName;
  transferred.value = payload.transferred;
  total.value = payload.total;
  fileIndex.value = payload.fileIndex;
  fileCount.value = payload.fileCount;
  if (payload.total > 0)
    currentFilePercent.value = Math.min(
      100,
      Math.round((payload.transferred / payload.total) * 100)
    );
  else if (payload.done) currentFilePercent.value = 100;
  if (running.value) syncDerived();
}

function applyCurrentFile(payload: SftpTransferFilePayload) {
  if (transferId.value && payload.transferId !== transferId.value) return;
  fromPath.value = payload.fromPath;
  toPath.value = payload.toPath;
  const name = payload.toPath.replace(/\\/g, "/").split("/").filter(Boolean).pop();
  if (name) fileName.value = name;
  if (running.value) syncDerived();
}

function applyEnd(payload: SftpTransferEndPayload) {
  if (transferId.value && payload.transferId !== transferId.value) return;
  running.value = false;
  cancelled.value = payload.state === "cancelled";
  finalState.value = payload.state;
  if (payload.state === "success") percent.value = 100;
  syncDerived();
}

async function setupListeners() {
  if (ready) return;
  ready = true;
  unlisteners.push(
    await listen<SftpTransferStartPayload>("sftp-transfer-start", (event) => {
      applyStart(event.payload);
    }),
    await listen<SftpProgressPayload>("sftp-progress", (event) => {
      applyProgress(event.payload);
    }),
    await listen<SftpTransferFilePayload>("sftp-transfer-file", (event) => {
      applyCurrentFile(event.payload);
    }),
    await listen<SftpTransferEndPayload>("sftp-transfer-end", (event) => {
      applyEnd(event.payload);
    })
  );
}

async function begin(payload: SftpTransferStartPayload) {
  await setupListeners();
  applyStart(payload);
  await emit("sftp-transfer-start", payload);
}

async function setCurrentFile(from: string, to: string) {
  applyCurrentFile({ transferId: transferId.value, fromPath: from, toPath: to });
  await emit("sftp-transfer-file", {
    transferId: transferId.value,
    fromPath: from,
    toPath: to,
  });
}

async function finish(payload: SftpTransferEndPayload) {
  applyEnd(payload);
  await emit("sftp-transfer-end", payload);
}

async function cancel() {
  if (!running.value || !transferId.value) return;
  cancelled.value = true;
  await api.sftpCancelUpload(transferId.value);
}

function dispose() {
  for (const unlisten of unlisteners) unlisten();
  unlisteners.length = 0;
  ready = false;
}

export const sftpTransfer: DeployTask & {
  cancelled: typeof cancelled;
  transferId: typeof transferId;
  serverId: typeof serverId;
  serverName: typeof serverName;
  fileName: typeof fileName;
  fileIndex: typeof fileIndex;
  fileCount: typeof fileCount;
  transferred: typeof transferred;
  total: typeof total;
  fromRoot: typeof fromRoot;
  toRoot: typeof toRoot;
  fromPath: typeof fromPath;
  toPath: typeof toPath;
  route: typeof route;
  begin: typeof begin;
  setCurrentFile: typeof setCurrentFile;
  finish: typeof finish;
  ensureReady: typeof setupListeners;
  dispose: typeof dispose;
} = {
  title: "SFTP 上传",
  pageKey: "terminal",
  logs,
  running,
  percent,
  step,
  detail,
  route,
  finalState,
  dismissed,
  cancelled,
  transferId,
  serverId,
  serverName,
  fileName,
  fileIndex,
  fileCount,
  transferred,
  total,
  fromRoot,
  toRoot,
  fromPath,
  toPath,
  attach: async () => {},
  cancel,
  begin,
  setCurrentFile,
  finish,
  ensureReady: setupListeners,
  dispose,
};
