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

const menus = [
  { key: "servers", label: "服务器", icon: Platform, component: ServersView },
  { key: "tunnels", label: "隧道", icon: Connection, component: TunnelsView },
  { key: "terminal", label: "SSH 终端", icon: Monitor, component: TerminalView },
  { key: "backend", label: "后端部署", icon: Upload, component: BackendDeployView },
  { key: "frontend", label: "前端部署", icon: ChromeFilled, component: FrontendDeployView },
  { key: "docker", label: "Docker 部署", icon: Box, component: DockerDeployView },
  { key: "settings", label: "设置", icon: Setting, component: SettingsView },
];

const activeKey = ref("servers");
const activeComponent = shallowRef<any>(ServersView);
// 终端页保持常驻（切走不销毁会话），其余页面切换时重新挂载以刷新配置
const isTerminalActive = computed(() => activeKey.value === "terminal");

watch(activeKey, (key) => {
  if (key === "terminal") return;
  const menu = menus.find((item) => item.key === key);
  if (menu) activeComponent.value = menu.component;
});

function onOpenSsh() {
  activeKey.value = "terminal";
}

onMounted(() => bus.on(OPEN_SSH_EVENT, onOpenSsh));
onUnmounted(() => bus.off(OPEN_SSH_EVENT, onOpenSsh));
</script>

<template>
  <el-container class="app-root">
    <el-aside width="168px" class="app-aside">
      <div class="app-brand">
        <img class="app-logo" src="./assets/app-icon.png" alt="部署工具" />
        <span class="app-brand-name">部署工具</span>
      </div>
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
      <!-- key 保证切换页面时重新加载最新配置 -->
      <component
        v-show="!isTerminalActive"
        :is="activeComponent"
        :key="activeKey === 'terminal' ? 'last' : activeKey"
      />
      <!-- 终端页常驻，避免切换页面时会话被销毁 -->
      <TerminalView v-show="isTerminalActive" class="terminal-host" />
    </el-main>
  </el-container>
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

.app-brand {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 8px 12px;
}

.app-logo {
  width: 32px;
  height: 32px;
  border-radius: 8px;
  object-fit: cover;
  flex-shrink: 0;
}

.app-brand-name {
  font-size: 14px;
  font-weight: 600;
  color: var(--app-text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
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
</style>
