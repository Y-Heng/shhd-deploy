<script setup lang="ts">
/** 隧道列表：本地端口转发、自动重连、分组拖拽 */
import { computed, nextTick, onMounted, onUnmounted, reactive, ref } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import { ArrowRight } from "@element-plus/icons-vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api } from "../api";
import GripDots from "../components/GripDots.vue";
import {
  bindPointerDrag,
  dropPlaceByY,
  elementFromPointIgnoringDrag,
  isDropPlaceholder,
  moveGroupedItem,
  reorderGroups,
} from "../composables/groupedDragSort";
import type { AppConfig, TunnelConfig, TunnelStatusInfo } from "../types";

const config = ref<AppConfig | null>(null);
const statusMap = reactive<Record<string, TunnelStatusInfo>>({});
const dialogVisible = ref(false);
const isNewTunnel = ref(false);
const tunnelBusyId = ref("");
const collapsedGroups = ref<Set<string>>(new Set());
const renamingGroup = ref("");
const renameDraft = ref("");
const renameInputRef = ref<HTMLInputElement | null>(null);
const draggingGroup = ref("");
const draggingItemId = ref("");
const dropHint = ref<{
  groupName: string;
  itemId?: string;
  place: "before" | "after" | "into";
} | null>(null);
let suppressNextGroupClick = false;

const editForm = reactive<TunnelConfig>({
  id: "",
  name: "",
  viaServerId: "",
  localPort: 10000,
  remoteHost: "",
  remotePort: 3306,
  autoStart: false,
  group: null,
});

// 按分组归类隧道
const groupedTunnels = computed(() => {
  const groups = new Map<string, TunnelConfig[]>();
  for (const tunnel of config.value?.tunnels ?? []) {
    const groupName = tunnel.group || "未分组";
    if (!groups.has(groupName)) groups.set(groupName, []);
    groups.get(groupName)!.push(tunnel);
  }
  return groups;
});

// 已存在的分组名
const existingGroups = computed(() => {
  const names = new Set<string>();
  for (const tunnel of config.value?.tunnels ?? [])
    if (tunnel.group) names.add(tunnel.group);
  return Array.from(names);
});

let unlisten: UnlistenFn | null = null;
let pollTimer: number | undefined;

onMounted(async () => {
  config.value = await api.getConfig();
  await refreshStatus();
  unlisten = await listen<TunnelStatusInfo>("tunnel-status", (event) => {
    statusMap[event.payload.id] = event.payload;
  });
  pollTimer = window.setInterval(refreshStatus, 3000);
});

onUnmounted(() => {
  if (unlisten) unlisten();
  if (pollTimer) window.clearInterval(pollTimer);
});

async function refreshStatus() {
  const statusList = await api.tunnelStatus();
  for (const status of statusList) statusMap[status.id] = status;
}

function statusOf(tunnelId: string): TunnelStatusInfo {
  return (
    statusMap[tunnelId] ?? {
      id: tunnelId,
      state: "stopped",
      message: "",
      activeConnections: 0,
      totalReconnects: 0,
    }
  );
}

function isRunning(tunnelId: string): boolean {
  const state = statusOf(tunnelId).state;
  return state !== "stopped" && state !== "error";
}

function stateColor(state: string): string {
  if (state === "active") return "#3dd68c";
  if (state === "connecting" || state === "reconnecting") return "#e6a23c";
  if (state === "error") return "#f56c6c";
  return "#6b7280";
}

function stateText(state: string): string {
  const map: Record<string, string> = {
    active: "已连接",
    connecting: "连接中",
    reconnecting: "重连中",
    stopped: "已停止",
    error: "错误",
  };
  return map[state] ?? state;
}

function toggleGroup(groupName: string) {
  if (suppressNextGroupClick) {
    suppressNextGroupClick = false;
    return;
  }
  const next = new Set(collapsedGroups.value);
  if (next.has(groupName)) next.delete(groupName);
  else next.add(groupName);
  collapsedGroups.value = next;
}

function isGroupCollapsed(groupName: string): boolean {
  return collapsedGroups.value.has(groupName);
}

function beginRenameGroup(groupName: string) {
  if (groupName === "未分组") {
    ElMessage.info("「未分组」不能改名，请把条目编辑到新分组");
    return;
  }
  renamingGroup.value = groupName;
  renameDraft.value = groupName;
  nextTick(() => {
    renameInputRef.value?.focus();
    renameInputRef.value?.select();
  });
}

function cancelRenameGroup() {
  renamingGroup.value = "";
}

async function commitRenameGroup() {
  const oldName = renamingGroup.value;
  const newName = renameDraft.value.trim();
  renamingGroup.value = "";
  if (!oldName || !newName || newName === oldName || !config.value) return;
  for (const tunnel of config.value.tunnels)
    if ((tunnel.group || "未分组") === oldName) tunnel.group = newName;
  await api.saveConfig(config.value);
  const next = new Set(collapsedGroups.value);
  if (next.has(oldName)) {
    next.delete(oldName);
    next.add(newName);
  }
  collapsedGroups.value = next;
  ElMessage.success("分组已重命名");
}

function tunnelSubtitle(tunnel: TunnelConfig): string {
  const viaName = serverNameById(tunnel.viaServerId);
  return `127.0.0.1:${tunnel.localPort} → ${tunnel.remoteHost}:${tunnel.remotePort} · via ${viaName}`;
}

async function toggleTunnel(tunnel: TunnelConfig) {
  if (tunnelBusyId.value) return;
  const current = statusOf(tunnel.id);
  tunnelBusyId.value = tunnel.id;
  try {
    if (current.state === "stopped" || current.state === "error") {
      await api.startTunnel(tunnel.id);
      ElMessage.success(`隧道 ${tunnel.name} 已启动`);
    } else {
      await api.stopTunnel(tunnel.id);
      ElMessage.info(`隧道 ${tunnel.name} 已停止`);
    }
    await refreshStatus();
  } catch {
    await refreshStatus();
  } finally {
    tunnelBusyId.value = "";
  }
}

function serverNameById(serverId: string): string {
  const server = config.value?.servers.find((item) => item.id === serverId);
  return server ? server.name : serverId;
}

function openAddDialog() {
  isNewTunnel.value = true;
  Object.assign(editForm, {
    id: `tunnel-${Date.now()}`,
    name: "",
    viaServerId: config.value?.servers[0]?.id ?? "",
    localPort: 10000,
    remoteHost: "",
    remotePort: 3306,
    autoStart: false,
    group: null,
  });
  dialogVisible.value = true;
}

function openEditDialog(tunnel: TunnelConfig) {
  isNewTunnel.value = false;
  Object.assign(editForm, JSON.parse(JSON.stringify(tunnel)));
  if (editForm.group === undefined) editForm.group = null;
  dialogVisible.value = true;
}

async function saveTunnel() {
  if (!config.value) return;
  if (!editForm.name || !editForm.remoteHost || !editForm.viaServerId) {
    ElMessage.warning("请完整填写隧道信息");
    return;
  }
  const clone: TunnelConfig = JSON.parse(JSON.stringify(editForm));
  if (!clone.group) clone.group = null;
  if (isNewTunnel.value) {
    config.value.tunnels.push(clone);
  } else {
    const index = config.value.tunnels.findIndex(
      (item) => item.id === clone.id
    );
    if (index >= 0) config.value.tunnels.splice(index, 1, clone);
  }
  await api.saveConfig(config.value);
  dialogVisible.value = false;
  ElMessage.success("已保存");
}

async function removeTunnel(tunnel: TunnelConfig) {
  if (!config.value) return;
  await ElMessageBox.confirm(`确认删除隧道 ${tunnel.name}？`, "删除确认", {
    type: "warning",
  });
  await api.stopTunnel(tunnel.id);
  config.value.tunnels = config.value.tunnels.filter(
    (item) => item.id !== tunnel.id
  );
  await api.saveConfig(config.value);
  ElMessage.success("已删除");
}

function nextFreeLocalPort(preferred: number) {
  const usedPorts = new Set((config.value?.tunnels ?? []).map((item) => item.localPort));
  let port = preferred;
  while (usedPorts.has(port) && port < 65535) port += 1;
  if (!usedPorts.has(port)) return port;
  port = 10000;
  while (usedPorts.has(port) && port < 65535) port += 1;
  return port;
}

async function duplicateTunnel(tunnel: TunnelConfig) {
  if (!config.value) return;
  const clone: TunnelConfig = JSON.parse(JSON.stringify(tunnel));
  clone.id = `tunnel-${Date.now()}`;
  clone.name = `${tunnel.name} 副本`;
  clone.localPort = nextFreeLocalPort(tunnel.localPort + 1);
  clone.autoStart = false;
  const index = config.value.tunnels.findIndex((item) => item.id === tunnel.id);
  if (index >= 0) config.value.tunnels.splice(index + 1, 0, clone);
  else config.value.tunnels.push(clone);
  await api.saveConfig(config.value);
  ElMessage.success(`已复制为「${clone.name}」`);
}

async function persistTunnels(next: TunnelConfig[]) {
  if (!config.value) return;
  config.value.tunnels = next;
  try {
    await api.saveConfig(config.value);
  } catch (error) {
    ElMessage.error(`保存排序失败：${error}`);
  }
}

function onGroupGripDown(groupName: string, event: PointerEvent) {
  if (renamingGroup.value) return;
  draggingGroup.value = groupName;
  draggingItemId.value = "";
  suppressNextGroupClick = true;
  bindPointerDrag(event, onListPointerMove, onListPointerUp);
}

function onItemGripDown(itemId: string, event: PointerEvent) {
  draggingItemId.value = itemId;
  draggingGroup.value = "";
  bindPointerDrag(event, onListPointerMove, onListPointerUp);
}

function onListPointerMove(clientX: number, clientY: number) {
  const hit = elementFromPointIgnoringDrag(clientX, clientY);
  if (isDropPlaceholder(hit)) return;
  if (draggingGroup.value) {
    const groupElement = hit instanceof Element ? hit.closest(".tunnel-group") : null;
    if (!(groupElement instanceof HTMLElement) || !groupElement.dataset.groupName) return;
    const groupName = groupElement.dataset.groupName;
    if (groupName === draggingGroup.value) {
      dropHint.value = null;
      return;
    }
    dropHint.value = { groupName, place: dropPlaceByY(clientY, groupElement) };
    return;
  }
  if (!draggingItemId.value) return;
  const row = hit instanceof Element ? hit.closest(".tunnel-row") : null;
  if (row instanceof HTMLElement && row.dataset.itemId) {
    const itemId = row.dataset.itemId;
    const groupName = row.dataset.groupName || "";
    if (itemId === draggingItemId.value) {
      dropHint.value = null;
      return;
    }
    dropHint.value = { groupName, itemId, place: dropPlaceByY(clientY, row) };
    return;
  }
  const groupElement = hit instanceof Element ? hit.closest(".tunnel-group") : null;
  if (groupElement instanceof HTMLElement && groupElement.dataset.groupName)
    dropHint.value = { groupName: groupElement.dataset.groupName, place: "into" };
}

async function onListPointerUp(clientX: number, clientY: number) {
  onListPointerMove(clientX, clientY);
  const fromGroup = draggingGroup.value;
  const fromItemId = draggingItemId.value;
  const hint = dropHint.value;
  onDragEnd();
  if (!config.value || !hint) return;
  if (fromGroup && fromGroup !== hint.groupName) {
    await persistTunnels(
      reorderGroups(config.value.tunnels, fromGroup, hint.groupName, hint.place),
    );
    return;
  }
  if (fromItemId)
    await persistTunnels(
      moveGroupedItem(
        config.value.tunnels,
        fromItemId,
        hint.groupName,
        hint.itemId || null,
        hint.place,
      ),
    );
}

function onDragEnd() {
  draggingGroup.value = "";
  draggingItemId.value = "";
  dropHint.value = null;
}

function isDropHint(groupName: string, itemId: string | undefined, place: string) {
  return (
    dropHint.value?.groupName === groupName &&
    dropHint.value.itemId === itemId &&
    dropHint.value.place === place
  );
}
</script>

<template>
  <div v-if="config" class="tunnels-view">
    <div class="view-header">
      <h2>隧道</h2>
      <el-button type="primary" @click="openAddDialog">添加隧道</el-button>
    </div>

    <template v-for="[groupName, tunnels] in groupedTunnels" :key="groupName">
    <div
      v-if="isDropHint(groupName, undefined, 'before')"
      class="drop-placeholder"
    />
    <div
      class="tunnel-group"
      :data-group-name="groupName"
      :class="{
        'is-dragging': draggingGroup === groupName,
        'is-drop-into': isDropHint(groupName, undefined, 'into'),
      }"
    >
      <div class="group-header" @click="toggleGroup(groupName)">
        <el-icon class="group-chevron" :class="{ collapsed: isGroupCollapsed(groupName) }">
          <ArrowRight />
        </el-icon>
        <span
          v-if="renamingGroup !== groupName"
          class="group-name"
          title="双击重命名"
          @dblclick.stop="beginRenameGroup(groupName)"
        >{{ groupName }}</span>
        <input
          v-else
          ref="renameInputRef"
          v-model="renameDraft"
          class="group-rename-input"
          @click.stop
          @dblclick.stop
          @keydown.enter.prevent="commitRenameGroup"
          @keydown.esc.prevent="cancelRenameGroup"
          @blur="commitRenameGroup"
        />
        <span class="group-count">{{ tunnels.length }}</span>
        <span
          class="drag-grip"
          title="拖动分组排序"
          @click.stop
          @pointerdown.stop="onGroupGripDown(groupName, $event)"
        >
          <GripDots />
        </span>
      </div>

      <div v-show="!isGroupCollapsed(groupName)" class="tunnel-list">
        <template v-for="tunnel in tunnels" :key="tunnel.id">
        <div
          v-if="isDropHint(groupName, tunnel.id, 'before')"
          class="drop-placeholder"
        />
        <div
          class="tunnel-row"
          :data-group-name="groupName"
          :data-item-id="tunnel.id"
          :class="{ 'is-dragging': draggingItemId === tunnel.id }"
        >
          <div class="tunnel-main">
            <span
              class="state-dot"
              :class="statusOf(tunnel.id).state"
              :title="stateText(statusOf(tunnel.id).state)"
            />
            <div class="tunnel-text">
              <div class="tunnel-name-line">
                <span class="tunnel-name">{{ tunnel.name }}</span>
                <el-tag
                  v-if="tunnel.autoStart"
                  size="small"
                  type="success"
                  effect="plain"
                  class="auto-tag"
                >
                  自启动
                </el-tag>
              </div>
              <div class="tunnel-sub">{{ tunnelSubtitle(tunnel) }}</div>
              <div
                v-if="statusOf(tunnel.id).message"
                class="tunnel-hint"
                :class="{ 'is-error': statusOf(tunnel.id).state === 'error' }"
              >
                {{ statusOf(tunnel.id).message }}
              </div>
            </div>
          </div>
          <div class="tunnel-actions">
            <el-button
              :type="isRunning(tunnel.id) ? 'warning' : 'success'"
              plain
              :loading="tunnelBusyId === tunnel.id"
              :disabled="Boolean(tunnelBusyId)"
              @click.stop="toggleTunnel(tunnel)"
            >
              {{ isRunning(tunnel.id) ? "停止" : "启动" }}
            </el-button>
            <el-button @click.stop="duplicateTunnel(tunnel)">复制</el-button>
            <el-button @click.stop="openEditDialog(tunnel)">编辑</el-button>
            <el-button type="danger" plain @click.stop="removeTunnel(tunnel)">删除</el-button>
          </div>
          <span
            class="drag-grip"
            title="拖动隧道排序"
            @click.stop
            @pointerdown.stop="onItemGripDown(tunnel.id, $event)"
          >
            <GripDots />
          </span>
        </div>
        <div
          v-if="isDropHint(groupName, tunnel.id, 'after')"
          class="drop-placeholder"
        />
        </template>
        <div
          v-if="isDropHint(groupName, undefined, 'into')"
          class="drop-placeholder"
        />
      </div>
    </div>
    <div
      v-if="isDropHint(groupName, undefined, 'after')"
      class="drop-placeholder"
    />
    </template>

    <el-dialog
      v-model="dialogVisible"
      :title="isNewTunnel ? '添加隧道' : '编辑隧道'"
      width="520px"
    >
      <el-form label-width="90px">
        <el-form-item label="名称">
          <el-input v-model="editForm.name" placeholder="如 生产MySQL" />
        </el-form-item>
        <el-form-item label="分组">
          <el-select
            v-model="editForm.group"
            filterable
            allow-create
            clearable
            default-first-option
            placeholder="选择或输入新分组名"
            style="width: 100%"
          >
            <el-option
              v-for="groupName in existingGroups"
              :key="groupName"
              :label="groupName"
              :value="groupName"
            />
          </el-select>
        </el-form-item>
        <el-form-item label="经由服务器">
          <el-select v-model="editForm.viaServerId" style="width: 100%">
            <el-option
              v-for="server in config.servers"
              :key="server.id"
              :label="server.name"
              :value="server.id"
            />
          </el-select>
        </el-form-item>
        <el-form-item label="本地端口">
          <el-input-number v-model="editForm.localPort" :min="1" :max="65535" />
        </el-form-item>
        <el-form-item label="远端地址">
          <el-input
            v-model="editForm.remoteHost"
            placeholder="内网 IP 或域名"
            style="width: 100%"
          />
        </el-form-item>
        <el-form-item label="远端端口">
          <el-input-number v-model="editForm.remotePort" :min="1" :max="65535" />
        </el-form-item>
        <el-form-item label="自动启动">
          <el-switch v-model="editForm.autoStart" />
          <span class="form-hint">应用启动时自动开启该隧道</span>
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="dialogVisible = false">取消</el-button>
        <el-button type="primary" @click="saveTunnel">保存</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<style scoped>
.view-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 16px;
  padding: 12px 14px;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  background: var(--app-panel, #151a22);
}
.view-header h2 {
  margin: 0;
}
.tunnel-group {
  margin-bottom: 14px;
  padding: 0;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius-lg, 10px);
  background: var(--app-bg, #0f1218);
  overflow: hidden;
}
.tunnel-group.is-dragging,
.tunnel-row.is-dragging {
  opacity: 0.55;
}
.tunnel-group.is-drop-into {
  outline: 1px dashed var(--app-accent, #3dd68c);
  outline-offset: -2px;
}
.group-header {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 10px 14px;
  margin: 0;
  border: none;
  border-bottom: none;
  background: var(--app-panel, #151a22);
  color: var(--el-text-color-secondary);
  cursor: pointer;
  text-align: left;
  border-radius: 0;
  box-sizing: border-box;
  transition: background 0.15s, color 0.15s;
}
.group-header:hover {
  color: var(--el-text-color-primary);
  background: var(--app-panel-2, #1a2130);
}
.group-chevron {
  transition: transform 0.15s;
  font-size: 14px;
}
.group-chevron.collapsed {
  transform: rotate(0deg);
}
.group-chevron:not(.collapsed) {
  transform: rotate(90deg);
}
.group-name {
  font-size: 13px;
  font-weight: 600;
  flex: 0 1 auto;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.group-count {
  font-size: 12px;
  color: var(--el-text-color-placeholder);
  margin-left: auto;
  flex-shrink: 0;
  padding-right: 2px;
}
.tunnel-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 10px 12px 12px;
  border-top: 1px solid var(--app-border, #2a3344);
}
.tunnel-row {
  display: flex;
  align-items: center;
  justify-content: flex-start;
  gap: 12px;
  min-height: 58px;
  padding: 8px 12px;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  background: var(--app-panel, #151a22);
  transition: background 0.15s, border-color 0.15s;
}
.tunnel-row:hover {
  background: var(--app-panel-2, #1a2130);
  border-color: var(--el-border-color-extra-light, #323c50);
}
.tunnel-main {
  display: flex;
  align-items: flex-start;
  gap: 12px;
  flex: 0 1 auto;
  min-width: 0;
}
.state-dot {
  width: 9px;
  height: 9px;
  border-radius: 50%;
  margin-top: 6px;
  flex-shrink: 0;
  background: #6b7280;
  box-shadow: 0 0 0 2px rgba(107, 114, 128, 0.25);
}
.state-dot.active {
  background: #3dd68c;
  box-shadow: 0 0 0 2px rgba(62, 207, 142, 0.25);
}
.state-dot.connecting,
.state-dot.reconnecting {
  background: #e6a23c;
  box-shadow: 0 0 0 2px rgba(230, 162, 60, 0.25);
}
.state-dot.error {
  background: #f56c6c;
  box-shadow: 0 0 0 2px rgba(245, 108, 108, 0.25);
}
.tunnel-text {
  min-width: 0;
}
.tunnel-name-line {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 2px;
}
.tunnel-name {
  font-size: 14px;
  font-weight: 600;
  color: var(--el-text-color-primary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.auto-tag {
  flex-shrink: 0;
}
.tunnel-sub {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: "Cascadia Code", "Consolas", monospace;
}
.tunnel-hint {
  margin-top: 2px;
  font-size: 11px;
  color: var(--el-text-color-placeholder);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tunnel-hint.is-error {
  color: #f89898;
}
.tunnel-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
  margin-left: auto;
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.12s;
}
.tunnel-actions :deep(.el-button) {
  height: 34px;
  padding: 8px 14px;
  margin: 0;
  font-size: 14px;
}
.tunnel-row:hover .tunnel-actions {
  opacity: 1;
  pointer-events: auto;
}
.form-hint {
  margin-left: 10px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
</style>
