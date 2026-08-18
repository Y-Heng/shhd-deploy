<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import { ArrowRight, Folder } from "@element-plus/icons-vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api } from "../api";
import {
  activeRdpServerIds,
  activeSshServerIds,
  bus,
  connectingSshServerIds,
  markRdpActive,
  markRdpInactive,
  OPEN_SSH_EVENT,
} from "../bus";
import OsIcons from "../components/OsIcons.vue";
import GripDots from "../components/GripDots.vue";
import {
  bindPointerDrag,
  dropPlaceByY,
  elementFromPointIgnoringDrag,
  isDropPlaceholder,
  moveGroupedItem,
  reorderGroups,
} from "../composables/groupedDragSort";
import type { AppConfig, DetectedOs, ServerConfig } from "../types";

const config = ref<AppConfig | null>(null);
const dialogTesting = ref(false);
const rdpLoadingId = ref("");
const dialogVisible = ref(false);
const isNewServer = ref(false);
const hoveredServerId = ref("");
const expandedGroups = ref<Set<string>>(new Set());
const renamingGroup = ref("");
const renameDraft = ref("");
const draggingGroup = ref("");
const draggingServerId = ref("");
const dropHint = ref<{
  groupName: string;
  serverId?: string;
  place: "before" | "after" | "into";
} | null>(null);
let suppressNextGroupClick = false;
let unlistenRdpClosed: UnlistenFn | null = null;

const rdpPresets = [
  { value: "1080p", label: "1920 × 1080" },
  { value: "900p", label: "1600 × 900" },
  { value: "768p", label: "1366 × 768" },
  { value: "720p", label: "1280 × 720" },
  { value: "fullscreen", label: "全屏" },
  { value: "default", label: "系统默认" },
];

const editForm = reactive<ServerConfig>({
  id: "",
  name: "",
  os: "linux",
  host: "",
  port: 22,
  username: "root",
  auth: { method: "password", password: "" },
  jumpServerId: null,
  group: null,
  rdpPreset: "1080p",
  sftpRemoteShortcuts: [],
  sftpLocalShortcuts: [],
});

const groupedServers = computed(() => {
  const groups = new Map<string, ServerConfig[]>();
  for (const server of config.value?.servers ?? []) {
    const groupName = server.group || "未分组";
    if (!groups.has(groupName)) groups.set(groupName, []);
    groups.get(groupName)!.push(server);
  }
  return groups;
});

const existingGroups = computed(() => {
  const names = new Set<string>();
  for (const server of config.value?.servers ?? [])
    if (server.group) names.add(server.group);
  return Array.from(names);
});

onMounted(async () => {
  await loadConfig();
  unlistenRdpClosed = await listen<string>("rdp-closed", (event) => {
    markRdpInactive(event.payload);
  });
});

onUnmounted(() => {
  if (unlistenRdpClosed) unlistenRdpClosed();
});

async function loadConfig() {
  config.value = await api.getConfig();
  const groups = new Set<string>();
  for (const server of config.value?.servers ?? [])
    groups.add(server.group || "未分组");
  expandedGroups.value = groups;
}

function serverNameById(serverId?: string | null): string {
  if (!serverId) return "直连";
  const server = config.value?.servers.find((item) => item.id === serverId);
  return server ? server.name : serverId;
}

function serverSubtitle(server: ServerConfig) {
  const jump = server.jumpServerId ? ` · via ${serverNameById(server.jumpServerId)}` : "";
  return `${server.username} · ${server.host}:${server.port}${jump}`;
}

function effectiveOs(server: ServerConfig): DetectedOs | string {
  return server.detectedOs || server.os;
}

function iconTone(server: ServerConfig) {
  const os = effectiveOs(server);
  if (os === "windows") return "tone-win";
  if (os === "ubuntu") return "tone-ubuntu";
  if (os === "centos") return "tone-centos";
  if (server.jumpServerId && !server.detectedOs) return "tone-jump";
  return "tone-linux";
}

function isWindowsServer(server: ServerConfig) {
  return effectiveOs(server) === "windows";
}

function isSshInUse(serverId: string) {
  return activeSshServerIds.value.includes(serverId);
}

function isRdpInUse(serverId: string) {
  return activeRdpServerIds.value.includes(serverId);
}

function isConnectingSsh(serverId: string) {
  return connectingSshServerIds.value.includes(serverId);
}

function toggleGroup(groupName: string) {
  if (suppressNextGroupClick) {
    suppressNextGroupClick = false;
    return;
  }
  const next = new Set(expandedGroups.value);
  if (next.has(groupName)) next.delete(groupName);
  else next.add(groupName);
  expandedGroups.value = next;
}

function isGroupExpanded(groupName: string) {
  return expandedGroups.value.has(groupName);
}

const renameInputRef = ref<HTMLInputElement | null>(null);

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
  for (const server of config.value.servers)
    if ((server.group || "未分组") === oldName) server.group = newName;
  await api.saveConfig(config.value);
  const next = new Set(expandedGroups.value);
  if (next.has(oldName)) {
    next.delete(oldName);
    next.add(newName);
  }
  expandedGroups.value = next;
  ElMessage.success("分组已重命名");
}

function rdpLaunchOptions(preset?: string | null) {
  if (preset === "fullscreen") return { fullscreen: true };
  if (preset === "900p") return { width: 1600, height: 900 };
  if (preset === "768p") return { width: 1366, height: 768 };
  if (preset === "720p") return { width: 1280, height: 720 };
  if (preset === "default") return {};
  return { width: 1920, height: 1080 };
}

async function testDraftConnection() {
  if (dialogTesting.value) return;
  if (!editForm.host || !editForm.username) {
    ElMessage.warning("请先填写主机与用户名");
    return;
  }
  if (editForm.auth.method === "password" && !editForm.auth.password) {
    ElMessage.warning("请填写密码");
    return;
  }
  if (editForm.auth.method === "key" && !editForm.auth.keyPath) {
    ElMessage.warning("请填写私钥路径");
    return;
  }
  if (editForm.os === "windows" && editForm.port === 3389) {
    try {
      await ElMessageBox.confirm(
        "测试连接走 SSH 协议，请确认已开启 OpenSSH 且端口正确。3389 通常是 RDP，不是 SSH（默认 22）。是否仍要继续测试？",
        "端口提示",
        { type: "warning", confirmButtonText: "继续测试", cancelButtonText: "取消" },
      );
    } catch {
      return;
    }
  }
  dialogTesting.value = true;
  try {
    const draft: ServerConfig = JSON.parse(JSON.stringify(editForm));
    if (!draft.group) draft.group = null;
    const result = await api.testServerDraft(draft);
    await loadConfig();
    ElMessageBox.alert(result, `${draft.name || draft.host} 连接测试`, {
      confirmButtonText: "确定",
    });
  } catch (error) {
    ElMessage.error(String(error));
  } finally {
    dialogTesting.value = false;
  }
}

async function openRemoteDesktop(server: ServerConfig) {
  if (rdpLoadingId.value) return;
  rdpLoadingId.value = server.id;
  try {
    const address = await api.openRdp(server.id, rdpLaunchOptions(server.rdpPreset));
    markRdpActive(server.id);
    ElMessage.success(`远程桌面已启动（${address}），请在 mstsc 窗口输入账号密码`);
  } catch (error) {
    ElMessage.error(String(error));
  } finally {
    rdpLoadingId.value = "";
  }
}

function connectSsh(server: ServerConfig) {
  if (isConnectingSsh(server.id)) return;
  bus.emit(OPEN_SSH_EVENT, server.id);
}

function openAddDialog() {
  isNewServer.value = true;
  Object.assign(editForm, {
    id: `server-${Date.now()}`,
    name: "",
    os: "linux",
    host: "",
    port: 22,
    username: "root",
    auth: { method: "password", password: "" },
    jumpServerId: null,
    group: null,
    rdpPreset: "1080p",
    sftpRemoteShortcuts: [],
    sftpLocalShortcuts: [],
  });
  dialogVisible.value = true;
}

function openEditDialog(server: ServerConfig) {
  isNewServer.value = false;
  Object.assign(editForm, JSON.parse(JSON.stringify(server)));
  if (editForm.group === undefined) editForm.group = null;
  if (!editForm.rdpPreset) editForm.rdpPreset = "1080p";
  if (!editForm.sftpRemoteShortcuts) editForm.sftpRemoteShortcuts = [];
  if (!editForm.sftpLocalShortcuts) editForm.sftpLocalShortcuts = [];
  dialogVisible.value = true;
}

async function saveServer() {
  if (!config.value) return;
  if (!editForm.name || !editForm.host) {
    ElMessage.warning("名称与主机地址不能为空");
    return;
  }
  const clone: ServerConfig = JSON.parse(JSON.stringify(editForm));
  if (!clone.group) clone.group = null;
  if (isNewServer.value) config.value.servers.push(clone);
  else {
    const index = config.value.servers.findIndex((item) => item.id === clone.id);
    if (index >= 0) config.value.servers.splice(index, 1, clone);
  }
  await api.saveConfig(config.value);
  dialogVisible.value = false;
  ElMessage.success("已保存");
}

async function removeServer(server: ServerConfig) {
  if (!config.value) return;
  await ElMessageBox.confirm(`确认删除服务器 ${server.name}？`, "删除确认", {
    type: "warning",
  });
  config.value.servers = config.value.servers.filter((item) => item.id !== server.id);
  await api.saveConfig(config.value);
  ElMessage.success("已删除");
}

async function persistServers(next: ServerConfig[]) {
  if (!config.value) return;
  config.value.servers = next;
  try {
    await api.saveConfig(config.value);
  } catch (error) {
    ElMessage.error(`保存排序失败：${error}`);
  }
}

function onGroupGripDown(groupName: string, event: PointerEvent) {
  if (renamingGroup.value) return;
  draggingGroup.value = groupName;
  draggingServerId.value = "";
  suppressNextGroupClick = true;
  bindPointerDrag(event, onListPointerMove, onListPointerUp);
}

function onServerGripDown(server: ServerConfig, event: PointerEvent) {
  draggingServerId.value = server.id;
  draggingGroup.value = "";
  bindPointerDrag(event, onListPointerMove, onListPointerUp);
}

function onListPointerMove(clientX: number, clientY: number) {
  const hit = elementFromPointIgnoringDrag(clientX, clientY);
  if (isDropPlaceholder(hit)) return;
  if (draggingGroup.value) {
    const groupElement = hit instanceof Element ? hit.closest(".server-group") : null;
    if (!(groupElement instanceof HTMLElement) || !groupElement.dataset.groupName) return;
    const groupName = groupElement.dataset.groupName;
    if (groupName === draggingGroup.value) {
      dropHint.value = null;
      return;
    }
    dropHint.value = { groupName, place: dropPlaceByY(clientY, groupElement) };
    return;
  }
  if (!draggingServerId.value) return;
  const row = hit instanceof Element ? hit.closest(".host-row") : null;
  if (row instanceof HTMLElement && row.dataset.serverId) {
    const serverId = row.dataset.serverId;
    const groupName = row.dataset.groupName || "";
    if (serverId === draggingServerId.value) {
      dropHint.value = null;
      return;
    }
    dropHint.value = { groupName, serverId, place: dropPlaceByY(clientY, row) };
    return;
  }
  const groupElement = hit instanceof Element ? hit.closest(".server-group") : null;
  if (groupElement instanceof HTMLElement && groupElement.dataset.groupName)
    dropHint.value = { groupName: groupElement.dataset.groupName, place: "into" };
}

async function onListPointerUp(clientX: number, clientY: number) {
  onListPointerMove(clientX, clientY);
  const fromGroup = draggingGroup.value;
  const fromServerId = draggingServerId.value;
  const hint = dropHint.value;
  onDragEnd();
  if (!config.value || !hint) return;
  if (fromGroup && fromGroup !== hint.groupName) {
    await persistServers(
      reorderGroups(config.value.servers, fromGroup, hint.groupName, hint.place),
    );
    return;
  }
  if (fromServerId)
    await persistServers(
      moveGroupedItem(
        config.value.servers,
        fromServerId,
        hint.groupName,
        hint.serverId || null,
        hint.place,
      ),
    );
}

function onDragEnd() {
  draggingGroup.value = "";
  draggingServerId.value = "";
  dropHint.value = null;
}

function isDropHint(groupName: string, serverId: string | undefined, place: string) {
  return (
    dropHint.value?.groupName === groupName &&
    dropHint.value.serverId === serverId &&
    dropHint.value.place === place
  );
}
</script>

<template>
  <div v-if="config" class="hosts-view">
    <div class="view-header">
      <h2>服务器</h2>
      <el-button type="primary" @click="openAddDialog">添加服务器</el-button>
    </div>

    <template v-for="[groupName, servers] in groupedServers" :key="groupName">
    <div
      v-if="isDropHint(groupName, undefined, 'before')"
      class="drop-placeholder"
    />
    <div
      class="server-group"
      :data-group-name="groupName"
      :class="{
        'is-dragging': draggingGroup === groupName,
        'is-drop-into': isDropHint(groupName, undefined, 'into'),
      }"
    >
      <div class="group-header" @click="toggleGroup(groupName)">
        <el-icon class="group-arrow" :class="{ open: isGroupExpanded(groupName) }">
          <ArrowRight />
        </el-icon>
        <el-icon class="group-folder"><Folder /></el-icon>
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
        <span class="group-count">{{ servers.length }}</span>
        <span
          class="drag-grip"
          title="拖动分组排序"
          @click.stop
          @pointerdown.stop="onGroupGripDown(groupName, $event)"
        >
          <GripDots />
        </span>
      </div>
      <div v-show="isGroupExpanded(groupName)" class="host-list">
        <template v-for="server in servers" :key="server.id">
        <div
          v-if="isDropHint(groupName, server.id, 'before')"
          class="drop-placeholder"
        />
        <div
          class="host-row"
          :data-group-name="groupName"
          :data-server-id="server.id"
          :class="{ 'is-dragging': draggingServerId === server.id }"
          @mouseenter="hoveredServerId = server.id"
          @mouseleave="hoveredServerId = ''"
          @dblclick="connectSsh(server)"
        >
          <button type="button" class="host-main" :disabled="isConnectingSsh(server.id)" @click="connectSsh(server)">
            <div class="host-icon" :class="iconTone(server)">
              <OsIcons :os="effectiveOs(server)" :size="24" />
            </div>
            <div class="host-text">
              <div class="host-name-line">
                <div class="host-name">{{ server.name }}</div>
                <el-tag v-if="isConnectingSsh(server.id)" size="small" type="warning" effect="plain">连接中</el-tag>
                <el-tag v-if="isSshInUse(server.id)" size="small" type="success" effect="plain">SSH</el-tag>
                <el-tag v-if="isRdpInUse(server.id)" size="small" type="primary" effect="plain">远程桌面</el-tag>
              </div>
              <div class="host-sub">{{ serverSubtitle(server) }}</div>
            </div>
          </button>
          <div class="host-actions" :class="{ show: hoveredServerId === server.id }">
            <el-button
              type="success"
              :loading="isConnectingSsh(server.id)"
              :disabled="isConnectingSsh(server.id)"
              @click.stop="connectSsh(server)"
            >
              SSH
            </el-button>
            <el-button
              v-if="isWindowsServer(server)"
              type="primary"
              plain
              :loading="rdpLoadingId === server.id"
              :disabled="Boolean(rdpLoadingId)"
              @click.stop="openRemoteDesktop(server)"
            >
              远程桌面
            </el-button>
            <el-button @click.stop="openEditDialog(server)">编辑</el-button>
            <el-button type="danger" plain @click.stop="removeServer(server)">删除</el-button>
          </div>
          <span
            class="drag-grip"
            title="拖动服务器排序"
            @click.stop
            @dblclick.stop
            @pointerdown.stop="onServerGripDown(server, $event)"
          >
            <GripDots />
          </span>
        </div>
        <div
          v-if="isDropHint(groupName, server.id, 'after')"
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
      :title="isNewServer ? '添加服务器' : '编辑服务器'"
      width="560px"
    >
      <el-form label-width="90px">
        <el-form-item label="名称">
          <el-input v-model="editForm.name" placeholder="如 Windows-A(172.16.48.5)" />
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
        <el-form-item label="系统">
          <el-radio-group v-model="editForm.os">
            <el-radio value="linux">Linux</el-radio>
            <el-radio value="windows">Windows</el-radio>
          </el-radio-group>
        </el-form-item>
        <el-form-item v-if="editForm.os === 'windows'" label="远程桌面">
          <el-select v-model="editForm.rdpPreset" style="width: 100%">
            <el-option
              v-for="preset in rdpPresets"
              :key="preset.value"
              :label="preset.label"
              :value="preset.value"
            />
          </el-select>
          <div class="form-hint">连接时直接使用此分辨率，不再每次询问</div>
        </el-form-item>
        <el-form-item label="主机">
          <el-input v-model="editForm.host" placeholder="IP 或域名" />
        </el-form-item>
        <el-form-item label="端口">
          <el-input-number
            v-model="editForm.port"
            :min="1"
            :max="65535"
            controls-position="right"
            style="width: 160px"
          />
          <div v-if="editForm.os === 'windows'" class="form-hint">
            SSH 默认 22（不是 RDP 3389）；测试连接走 SSH，需已开启 OpenSSH Server
          </div>
          <div v-else class="form-hint">SSH 默认端口 22</div>
        </el-form-item>
        <el-form-item label="用户名">
          <el-input v-model="editForm.username" style="width: 200px" />
        </el-form-item>
        <el-form-item label="认证方式">
          <el-radio-group v-model="editForm.auth.method">
            <el-radio value="password">密码</el-radio>
            <el-radio value="key">私钥</el-radio>
          </el-radio-group>
        </el-form-item>
        <el-form-item v-if="editForm.auth.method === 'password'" label="密码">
          <el-input v-model="editForm.auth.password" type="password" show-password />
        </el-form-item>
        <template v-else>
          <el-form-item label="私钥路径">
            <el-input
              v-model="editForm.auth.keyPath"
              placeholder="如 C:\Users\xxx\.ssh\id_ed25519"
            />
          </el-form-item>
          <el-form-item label="密钥口令">
            <el-input
              v-model="editForm.auth.passphrase"
              type="password"
              show-password
              placeholder="无口令留空"
            />
          </el-form-item>
        </template>
        <el-form-item label="跳板机">
          <el-select
            v-model="editForm.jumpServerId"
            clearable
            placeholder="直连（不经过跳板）"
            style="width: 100%"
          >
            <el-option
              v-for="server in config.servers.filter((item) => item.id !== editForm.id)"
              :key="server.id"
              :label="server.name"
              :value="server.id"
            />
          </el-select>
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button :loading="dialogTesting" :disabled="dialogTesting" @click="testDraftConnection">测试连接</el-button>
        <el-button @click="dialogVisible = false">取消</el-button>
        <el-button type="primary" @click="saveServer">保存</el-button>
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
.server-group {
  margin-bottom: 14px;
  padding: 0;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius-lg, 10px);
  background: var(--app-bg, #0f1218);
  overflow: hidden;
}
.server-group.is-dragging,
.host-row.is-dragging {
  opacity: 0.55;
}
.server-group.is-drop-into {
  outline: 1px dashed var(--app-accent, #3dd68c);
  outline-offset: -2px;
}
.group-header {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  padding: 10px 14px;
  margin: 0;
  border: none;
  border-bottom: none;
  background: var(--app-panel, #151a22);
  color: var(--el-text-color-secondary);
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
  border-radius: 0;
  box-sizing: border-box;
  transition: background 0.15s;
}
.group-header:hover {
  background: var(--app-panel-2, #1a2130);
}
.group-arrow {
  transition: transform 0.15s;
  color: var(--el-text-color-secondary);
}
.group-arrow.open {
  transform: rotate(90deg);
}
.group-folder {
  color: #6aa8ff;
}
.group-name {
  flex: 0 1 auto;
  min-width: 0;
  text-align: left;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.group-count {
  font-size: 12px;
  color: var(--el-text-color-placeholder);
  font-weight: 500;
  flex-shrink: 0;
  padding-right: 2px;
  margin-left: auto;
}
.host-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 10px 12px 12px;
  border-top: 1px solid var(--app-border, #2a3344);
}
.host-row {
  display: flex;
  align-items: center;
  justify-content: flex-start;
  gap: 12px;
  min-height: 64px;
  padding: 8px 12px;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  background: var(--app-panel, #151a22);
  transition: background 0.15s, border-color 0.15s;
}
.host-row:hover {
  background: var(--app-panel-2, #1a2130);
  border-color: var(--el-border-color-extra-light, #323c50);
}
.host-main {
  display: flex;
  align-items: center;
  gap: 14px;
  border: none;
  background: transparent;
  color: inherit;
  cursor: pointer;
  padding: 0;
  text-align: left;
  flex: 0 1 auto;
  min-width: 0;
}
.host-icon {
  width: 42px;
  height: 42px;
  border-radius: var(--app-radius, 8px);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: #f3f6fb;
  flex-shrink: 0;
}
.tone-linux {
  background: linear-gradient(135deg, #2f9e6b, #1f7a52);
}
.tone-ubuntu {
  background: linear-gradient(135deg, #e95420, #c4411a);
}
.tone-centos {
  background: linear-gradient(135deg, #932279, #6b1a5c);
}
.tone-win {
  background: linear-gradient(135deg, #3b82f6, #1d4ed8);
}
.tone-jump {
  background: linear-gradient(135deg, #f59e0b, #d97706);
}
.host-text {
  min-width: 0;
}
.host-name-line {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 2px;
  min-width: 0;
}
.host-name-line :deep(.el-tag) {
  flex-shrink: 0;
}
.host-name {
  font-size: 15px;
  font-weight: 600;
  color: var(--el-text-color-primary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.host-sub {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.host-actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
  flex-shrink: 0;
  min-width: 360px;
  height: 40px;
  margin-left: auto;
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.12s;
}
.host-actions :deep(.el-button) {
  height: 34px;
  padding: 8px 14px;
  margin: 0;
  font-size: 14px;
}
.host-actions.show {
  opacity: 1;
  pointer-events: auto;
}
.form-hint {
  margin-top: 6px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
  line-height: 1.4;
}
</style>
