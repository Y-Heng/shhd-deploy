<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, shallowRef, watch } from "vue";
import {
  Platform,
  Connection,
  Monitor,
  Upload,
  ChromeFilled,
  Box,
  Setting,
} from "@element-plus/icons-vue";
import ServersView from "./views/ServersView.vue";
import TunnelsView from "./views/TunnelsView.vue";
import TerminalView from "./views/TerminalView.vue";
import BackendDeployView from "./views/BackendDeployView.vue";
import FrontendDeployView from "./views/FrontendDeployView.vue";
import DockerDeployView from "./views/DockerDeployView.vue";
import SettingsView from "./views/SettingsView.vue";
import { bus, OPEN_SSH_EVENT } from "./bus";
import { sharedDeployTasks, type DeployTask } from "./composables/useTask";
import { sftpTransfer } from "./composables/useSftpTransfer";

const menus = [
  { key: "servers", label: "服务器", icon: Platform, component: ServersView },
  { key: "tunnels", label: "隧道", icon: Connection, component: TunnelsView },
  { key: "terminal", label: "SSH 终端", icon: Monitor, component: TerminalView },
  { key: "backend", label: "后端部署", icon: Upload, component: BackendDeployView },
  { key: "frontend", label: "前端部署", icon: ChromeFilled, component: FrontendDeployView },
  { key: "docker", label: "Docker 部署", icon: Box, component: DockerDeployView },
  { key: "settings", label: "设置", icon: Setting, component: SettingsView },
];

const keepAliveKeys = ["terminal", "frontend", "backend"];

const activeKey = ref("servers");
const activeComponent = shallowRef<any>(ServersView);
const isTerminalActive = computed(() => activeKey.value === "terminal");
const isKeepAlivePage = computed(() => keepAliveKeys.includes(activeKey.value));

const floatingTasks = computed(() =>
  [...sharedDeployTasks, sftpTransfer].filter((task) => {
    if (activeKey.value === task.pageKey) return false;
    if (task.running.value) return true;
    return Boolean(task.finalState.value) && !task.dismissed.value;
  })
);

watch(activeKey, (key) => {
  if (keepAliveKeys.includes(key)) return;
  const menu = menus.find((item) => item.key === key);
  if (menu) activeComponent.value = menu.component;
});

function onOpenSsh() {
  activeKey.value = "terminal";
}

function openTaskPage(task: DeployTask) {
  task.dismissed.value = true;
  activeKey.value = task.pageKey;
}

function dismissTask(task: DeployTask) {
  task.dismissed.value = true;
}

function floatStatus(task: DeployTask) {
  if (task.running.value) return undefined;
  if (task.finalState.value === "failed") return "exception";
  if (task.finalState.value === "success") return "success";
  return undefined;
}

onMounted(() => {
  bus.on(OPEN_SSH_EVENT, onOpenSsh);
  sftpTransfer.ensureReady();
});
onUnmounted(() => bus.off(OPEN_SSH_EVENT, onOpenSsh));
</script>

<template>
  <el-container class="app-root">
    <el-aside width="168px" class="app-aside">
      <el-menu
        :default-active="activeKey"
        class="app-menu"
        @select="(key: string) => (activeKey = key)"
      >
        <el-menu-item v-for="menu in menus" :key="menu.key" :index="menu.key">
          <el-icon><component :is="menu.icon" /></el-icon>
          <span>{{ menu.label }}</span>
        </el-menu-item>
      </el-menu>
    </el-aside>
    <el-main class="app-main" :class="{ 'is-terminal': isTerminalActive }">
      <component
        v-show="!isKeepAlivePage"
        :is="activeComponent"
        :key="activeKey === 'terminal' ? 'last' : activeKey"
      />
      <TerminalView v-show="isTerminalActive" class="terminal-host" />
      <FrontendDeployView v-show="activeKey === 'frontend'" :active="activeKey === 'frontend'" class="keep-page" />
      <BackendDeployView v-show="activeKey === 'backend'" :active="activeKey === 'backend'" class="keep-page" />
    </el-main>
  </el-container>

  <div v-if="floatingTasks.length" class="deploy-floats">
    <div v-for="task in floatingTasks" :key="task.pageKey" class="deploy-float">
      <div class="deploy-float-title">
        <span>{{ task.title }}</span>
        <button type="button" class="deploy-float-close" title="关闭" @click="dismissTask(task)">×</button>
      </div>
      <el-progress
        :percentage="task.percent.value"
        :stroke-width="10"
        :status="floatStatus(task)"
      />
      <div class="deploy-float-step">{{ task.step.value || (task.running.value ? '进行中…' : '已结束') }}</div>
      <div v-if="task.route?.value" class="deploy-float-route">{{ task.route.value }}</div>
      <div v-if="task.detail?.value" class="deploy-float-step">{{ task.detail.value }}</div>
      <div class="deploy-float-actions">
        <el-button size="small" type="primary" @click="openTaskPage(task)">查看</el-button>
        <el-button v-if="task.running.value" size="small" type="danger" plain @click="task.cancel()">取消</el-button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.app-root {
  height: 100%;
  background: var(--app-bg);
}

.app-aside {
  border-right: 1px solid var(--app-border);
  background: var(--app-panel);
  padding: 8px 8px 0;
}

.app-menu {
  border-right: none;
  background: transparent;
}

.app-menu :deep(.el-menu-item) {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 44px;
  line-height: 44px;
  margin: 0 0 4px;
  border: 1px solid transparent;
  border-radius: var(--app-radius, 8px);
  color: var(--app-muted);
}

.app-menu :deep(.el-menu-item:hover) {
  background: var(--app-panel-2);
  border-color: var(--app-border);
  color: var(--app-text);
}

.app-menu :deep(.el-menu-item.is-active) {
  background: var(--app-accent-dim);
  border-color: var(--app-border);
  color: var(--app-accent);
}

.app-menu :deep(.el-icon) {
  font-size: 16px;
  margin-right: 0;
}

.app-main {
  padding: 16px 20px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  min-height: 0;
  background: var(--app-bg);
  color: var(--app-text);
  border-left: none;
}

.app-main.is-terminal {
  padding: 16px 20px;
  overflow: hidden;
}

.terminal-host {
  flex: 1;
  min-height: 0;
  height: 100%;
}

.keep-page {
  flex: 1;
  min-height: 0;
}

.deploy-floats {
  position: fixed;
  right: 18px;
  bottom: 18px;
  z-index: 3000;
  display: flex;
  flex-direction: column;
  gap: 10px;
  width: 280px;
  pointer-events: none;
}

.deploy-float {
  pointer-events: auto;
  padding: 12px 14px;
  border: 1px solid var(--app-border);
  border-radius: var(--app-radius-lg, 10px);
  background: var(--app-panel);
  box-shadow: 0 10px 28px rgba(0, 0, 0, 0.35);
}

.deploy-float-title {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 8px;
  font-weight: 600;
  color: var(--app-text);
}

.deploy-float-close {
  border: none;
  background: transparent;
  color: var(--app-muted);
  cursor: pointer;
  font-size: 18px;
  line-height: 1;
  padding: 0 2px;
}

.deploy-float-close:hover {
  color: var(--app-text);
}

.deploy-float-step {
  margin: 6px 0 10px;
  font-size: 12px;
  color: var(--app-muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.deploy-float-route {
  margin: 0 0 8px;
  font-size: 12px;
  line-height: 1.5;
  color: var(--app-text);
  white-space: pre-line;
  word-break: break-all;
}

.deploy-float-actions {
  display: flex;
  gap: 8px;
}
</style>
