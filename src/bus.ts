/** 简易事件总线：跨页面通知（如服务器页 → SSH 终端） */

import { ref } from "vue";

type BusHandler = (payload: unknown) => void;

const handlers = new Map<string, Set<BusHandler>>();

export const bus = {
  on(eventName: string, handler: BusHandler) {
    if (!handlers.has(eventName)) handlers.set(eventName, new Set());
    handlers.get(eventName)!.add(handler);
  },
  off(eventName: string, handler: BusHandler) {
    handlers.get(eventName)?.delete(handler);
  },
  emit(eventName: string, payload?: unknown) {
    const list = handlers.get(eventName);
    if (!list) return;
    for (const handler of list) handler(payload);
  },
};

/** 打开指定服务器的 SSH 会话 */
export const OPEN_SSH_EVENT = "open-ssh";

/** 正在建立 SSH 连接的服务器 ID，防止连点开出多个会话 */
export const sshConnectingId = ref("");

/** 当前有未断开 SSH 会话的服务器 ID */
export const activeSshServerIds = ref<string[]>([]);

/** 本应用已拉起远程桌面、且会话仍可能在使用的服务器 ID */
export const activeRdpServerIds = ref<string[]>([]);

export function markRdpActive(serverId: string) {
  if (activeRdpServerIds.value.includes(serverId)) return;
  activeRdpServerIds.value = [...activeRdpServerIds.value, serverId];
}

export function markRdpInactive(serverId: string) {
  activeRdpServerIds.value = activeRdpServerIds.value.filter((id) => id !== serverId);
}

export function syncActiveSshServerIds(serverIds: string[]) {
  activeSshServerIds.value = serverIds;
}
