<script setup lang="ts">
import { nextTick, onMounted, onUnmounted, ref } from "vue";
import { ElMessage } from "element-plus";
import { Close } from "@element-plus/icons-vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { api } from "../api";
import SftpPanel from "../components/SftpPanel.vue";
import { createSshTerminal } from "../sshTerminal";
import type { TermClosedPayload, TermDataPayload } from "../types";

const props = defineProps<{
  sessionId: string;
  title: string;
  serverId: string;
}>();

type PanelMode = "terminal" | "sftp";

const panelMode = ref<PanelMode>("terminal");
const closed = ref(false);
const termHost = ref<HTMLElement | null>(null);

let terminal: Terminal | null = null;
let fitAddon: FitAddon | null = null;
const unlisteners: UnlistenFn[] = [];

function writeBytes(data: string) {
  if (!terminal) return;
  const binary = atob(data);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
  terminal.write(bytes);
}

function fitTerminal() {
  if (panelMode.value !== "terminal" || !fitAddon) return;
  const container = termHost.value;
  if (!container || container.clientWidth < 40 || container.clientHeight < 40) return;
  try {
    fitAddon.fit();
  } catch {
    // 容器可能暂时不可见
  }
}

async function closeWindow() {
  await getCurrentWindow().close();
}

onMounted(async () => {
  const created = createSshTerminal();
  terminal = created.terminal;
  fitAddon = created.fitAddon;
  terminal.onData((data) => {
    if (!closed.value) api.terminalWrite(props.sessionId, data);
  });
  terminal.onResize(({ cols, rows }) => api.terminalResize(props.sessionId, cols, rows));
  await nextTick();
  if (termHost.value) {
    terminal.open(termHost.value);
    fitAddon.fit();
    terminal.focus();
  }
  unlisteners.push(
    await listen<TermDataPayload>("term-data", (event) => {
      if (event.payload.sessionId !== props.sessionId) return;
      writeBytes(event.payload.data);
    }),
    await listen<TermClosedPayload>("term-closed", (event) => {
      if (event.payload.sessionId !== props.sessionId || closed.value) return;
      closed.value = true;
      terminal?.write("\r\n\x1b[31m[会话已断开]\x1b[0m\r\n");
    })
  );
  window.addEventListener("resize", fitTerminal);
});

onUnmounted(() => {
  window.removeEventListener("resize", fitTerminal);
  for (const unlisten of unlisteners) unlisten();
  terminal?.dispose();
});
</script>

<template>
  <div class="popout">
    <div class="popout-bar">
      <span class="popout-title">{{ title }}</span>
      <span v-if="closed" class="popout-closed">已断开</span>
      <div class="popout-actions">
        <button type="button" class="mode-btn" :class="{ active: panelMode === 'terminal' }" @click="panelMode = 'terminal'; nextTick(fitTerminal)">SSH</button>
        <button type="button" class="mode-btn" :class="{ active: panelMode === 'sftp' }" @click="panelMode = 'sftp'">SFTP</button>
        <button type="button" class="ghost-btn" title="关闭窗口（会话回到主窗口）" @click="closeWindow">
          <el-icon :size="14"><Close /></el-icon>
        </button>
      </div>
    </div>
    <div class="popout-body">
      <div v-show="panelMode === 'terminal'" class="term-main">
        <div ref="termHost" class="terminal-container" />
      </div>
      <div v-show="panelMode === 'sftp'" class="sftp-main">
        <SftpPanel :server-id="serverId" :server-name="title" :active="panelMode === 'sftp'" />
      </div>
    </div>
  </div>
</template>

<style scoped>
.popout {
  height: 100%;
  display: flex;
  flex-direction: column;
  background: #1e1e2e;
  color: #d7dde8;
}
.popout-bar {
  display: flex;
  align-items: center;
  gap: 10px;
  height: 42px;
  padding: 0 12px;
  border-bottom: 1px solid #2a3344;
  background: #151a22;
  flex-shrink: 0;
}
.popout-title {
  font-weight: 600;
}
.popout-closed {
  color: #f56c6c;
  font-size: 12px;
}
.popout-actions {
  margin-left: auto;
  display: flex;
  gap: 6px;
}
.mode-btn,
.ghost-btn {
  border: 1px solid #2a3344;
  background: transparent;
  color: #8b95a8;
  height: 28px;
  padding: 0 10px;
  border-radius: 8px;
  cursor: pointer;
}
.ghost-btn {
  width: 32px;
  padding: 0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
}
.mode-btn.active,
.mode-btn:hover,
.ghost-btn:hover {
  color: #3dd68c;
  border-color: #3dd68c;
  background: rgba(61, 214, 140, 0.15);
}
.popout-body {
  flex: 1;
  min-height: 0;
}
.term-main,
.sftp-main {
  height: 100%;
}
.terminal-container {
  height: 100%;
}
</style>
