<script setup lang="ts">
/**
 * 系统信息监控面板（类似 FinalShell 系统信息）：
 * - 运行时间、负载 (1m, 5m, 15m)
 * - CPU / 内存 / 交换分区 条形进度条
 * - Top 进程列表（按 CPU / 内存排序切换，展示 PID、CPU%、内存占用、进程命令）
 * - 实时网络流量收发速率与波形历史图、网卡选择
 * - 磁盘挂载列表与可用/总空间占比
 */
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { ElMessage } from 'element-plus'
import { ArrowDown, Check, CopyDocument, Refresh, Warning } from '@element-plus/icons-vue'
import { api } from '../api'
import type { DiskMount, NetworkInterface, ProcessInfo, SystemStats } from '../types'

const props = defineProps<{
  serverId: string
  serverHost: string
  active: boolean
}>()

const stats = ref<SystemStats | null>(null)
const loading = ref(false)
const errorMsg = ref('')
const processSort = ref<'cpu' | 'mem'>('cpu')
const selectedNetwork = ref<string>('all')
const networkHistory = ref<{ rx: number; tx: number }[]>([])
const MAX_HISTORY = 40

let unlistenStats: UnlistenFn | null = null
let unlistenError: UnlistenFn | null = null

function formatBytes(bytes: number): string {
  if (bytes <= 0 || isNaN(bytes)) return '0 B'
  const k = 1024
  const sizes = ['B', 'K', 'M', 'G', 'T']
  const i = Math.floor(Math.log(bytes) / Math.log(k))
  const idx = Math.min(i, sizes.length - 1)
  return `${(bytes / Math.pow(k, idx)).toFixed(idx === 0 ? 0 : 1)}${sizes[idx]}`
}

function formatSpeed(bytesPerSec: number): string {
  if (bytesPerSec <= 0 || isNaN(bytesPerSec)) return '0 B/s'
  const k = 1024
  const sizes = ['B/s', 'KB/s', 'MB/s', 'GB/s']
  const i = Math.floor(Math.log(bytesPerSec) / Math.log(k))
  const idx = Math.min(i, sizes.length - 1)
  return `${(bytesPerSec / Math.pow(k, idx)).toFixed(1)}${sizes[idx]}`
}

const sortedProcesses = computed(() => {
  if (!stats.value?.topProcesses) return []
  const procs = [...stats.value.topProcesses]
  if (processSort.value === 'cpu') {
    return procs.sort((a, b) => b.cpuPercent - a.cpuPercent)
  } else {
    return procs.sort((a, b) => b.memoryBytes - a.memoryBytes)
  }
})

const currentNetwork = computed(() => {
  if (!stats.value?.networks || stats.value.networks.length === 0) {
    return {
      rxSpeed: stats.value?.rxSpeedBytes ?? 0,
      txSpeed: stats.value?.txSpeedBytes ?? 0
    }
  }
  if (selectedNetwork.value === 'all') {
    return {
      rxSpeed: stats.value.rxSpeedBytes,
      txSpeed: stats.value.txSpeedBytes
    }
  }
  const iface = stats.value.networks.find(n => n.name === selectedNetwork.value)
  return {
    rxSpeed: iface?.rxSpeedBytes ?? 0,
    txSpeed: iface?.txSpeedBytes ?? 0
  }
})

// 计算折线图 SVG points
const networkChartPoints = computed(() => {
  if (networkHistory.value.length < 2) return { rxPoints: '', txPoints: '', maxSpeed: 1024 }
  let max = 1024 // 至少 1KB/s
  for (const h of networkHistory.value) {
    if (h.rx > max) max = h.rx
    if (h.tx > max) max = h.tx
  }

  const width = 280
  const height = 55
  const step = width / (MAX_HISTORY - 1)
  const offset = (MAX_HISTORY - networkHistory.value.length) * step

  const rxCoords = networkHistory.value.map((h, index) => {
    const x = offset + index * step
    const y = height - (h.rx / max) * (height - 6) - 3
    return `${x.toFixed(1)},${y.toFixed(1)}`
  })

  const txCoords = networkHistory.value.map((h, index) => {
    const x = offset + index * step
    const y = height - (h.tx / max) * (height - 6) - 3
    return `${x.toFixed(1)},${y.toFixed(1)}`
  })

  return {
    rxPoints: rxCoords.join(' '),
    txPoints: txCoords.join(' '),
    maxSpeed: max
  }
})

function copyIp() {
  if (!props.serverHost) return
  navigator.clipboard.writeText(props.serverHost)
  ElMessage.success('已复制 IP')
}

async function refreshOnce() {
  if (!props.serverId) return
  loading.value = true
  errorMsg.value = ''
  try {
    const s = await api.getSystemStats(props.serverId)
    stats.value = s
    pushHistory(s.rxSpeedBytes, s.txSpeedBytes)
  } catch (e: any) {
    errorMsg.value = String(e)
  } finally {
    loading.value = false
  }
}

function pushHistory(rx: number, tx: number) {
  networkHistory.value.push({ rx: Math.max(0, rx), tx: Math.max(0, tx) })
  if (networkHistory.value.length > MAX_HISTORY) {
    networkHistory.value.shift()
  }
}

async function startMonitor() {
  if (!props.serverId) return
  try {
    await api.startSystemMonitor(props.serverId, 2)
  } catch (e) {
    console.error('Failed to start system monitor:', e)
  }
}

async function stopMonitor() {
  if (!props.serverId) return
  try {
    await api.stopSystemMonitor(props.serverId)
  } catch (e) {
    console.error('Failed to stop system monitor:', e)
  }
}

watch(
  () => props.serverId,
  async (newId, oldId) => {
    if (oldId && oldId !== newId) {
      await stopMonitor()
    }
    stats.value = null
    networkHistory.value = []
    errorMsg.value = ''
    if (newId && props.active) {
      refreshOnce()
      startMonitor()
    }
  }
)

watch(
  () => props.active,
  (isActive) => {
    if (isActive) {
      if (!stats.value) refreshOnce()
      startMonitor()
    } else {
      stopMonitor()
    }
  }
)

onMounted(async () => {
  unlistenStats = await listen<SystemStats>('sysinfo-stats', (event) => {
    if (event.payload.serverId === props.serverId) {
      stats.value = event.payload
      pushHistory(event.payload.rxSpeedBytes, event.payload.txSpeedBytes)
      errorMsg.value = ''
    }
  })

  unlistenError = await listen<{ serverId: string; error: string }>('sysinfo-error', (event) => {
    if (event.payload.serverId === props.serverId) {
      errorMsg.value = event.payload.error
    }
  })

  if (props.active && props.serverId) {
    refreshOnce()
    startMonitor()
  }
})

onUnmounted(async () => {
  if (unlistenStats) unlistenStats()
  if (unlistenError) unlistenError()
  await stopMonitor()
})
</script>

<template>
  <div class="sysinfo-panel">
    <!-- 顶部状态栏 -->
    <div class="sysinfo-header">
      <div class="sysinfo-sync">
        <span class="sync-dot" :class="{ live: !errorMsg && stats }" />
        <span class="sync-title">同步状态</span>
      </div>
      <div class="sysinfo-actions">
        <button type="button" class="icon-action-btn" title="手动刷新" :disabled="loading" @click="refreshOnce">
          <el-icon :class="{ 'is-loading': loading }"><Refresh /></el-icon>
        </button>
      </div>
    </div>

    <!-- IP 与基础信息 -->
    <div class="sysinfo-ip-bar">
      <span class="ip-label">IP</span>
      <span class="ip-val">{{ serverHost || '127.0.0.1' }}</span>
      <button type="button" class="copy-link-btn" title="复制 IP" @click="copyIp">
        复制
      </button>
    </div>

    <div v-if="errorMsg" class="sysinfo-error">
      <el-icon><Warning /></el-icon>
      <span>{{ errorMsg }}</span>
    </div>

    <div class="sysinfo-scroll-body">
      <!-- 运行时间与负载 -->
      <div class="info-section">
        <div class="info-label-val">
          <span class="info-label">运行</span>
          <span class="info-val">{{ stats?.uptimeDisplay || '0 天' }}</span>
        </div>
        <div class="info-label-val">
          <span class="info-label">负载</span>
          <span class="info-val">{{ stats ? `${stats.load1.toFixed(2)}, ${stats.load5.toFixed(2)}, ${stats.load15.toFixed(2)}` : '0.00, 0.00, 0.00' }}</span>
        </div>
      </div>

      <!-- CPU / 内存 / 交换 进度条卡片 -->
      <div class="metrics-cards">
        <!-- CPU -->
        <div class="metric-row">
          <span class="metric-name">CPU</span>
          <div class="metric-track">
            <div
              class="metric-fill cpu-fill"
              :style="{ width: `${Math.min(100, Math.max(0, stats?.cpuPercent ?? 0))}%` }"
            >
              <span v-if="(stats?.cpuPercent ?? 0) >= 15" class="metric-inner-text">{{ (stats?.cpuPercent ?? 0).toFixed(0) }}%</span>
            </div>
            <span v-if="(stats?.cpuPercent ?? 0) < 15" class="metric-outer-text">{{ (stats?.cpuPercent ?? 0).toFixed(0) }}%</span>
          </div>
        </div>

        <!-- 内存 -->
        <div class="metric-row">
          <span class="metric-name">内存</span>
          <div class="metric-track">
            <div
              class="metric-fill mem-fill"
              :style="{ width: `${Math.min(100, Math.max(0, stats?.memoryPercent ?? 0))}%` }"
            >
              <span v-if="(stats?.memoryPercent ?? 0) >= 15" class="metric-inner-text">{{ (stats?.memoryPercent ?? 0).toFixed(0) }}%</span>
            </div>
            <span class="metric-right-label">
              {{ formatBytes(stats?.memoryUsed ?? 0) }} / {{ formatBytes(stats?.memoryTotal ?? 0) }}
            </span>
          </div>
        </div>

        <!-- 交换分区 -->
        <div class="metric-row">
          <span class="metric-name">交换</span>
          <div class="metric-track">
            <div
              class="metric-fill swap-fill"
              :style="{ width: `${Math.min(100, Math.max(0, stats?.swapPercent ?? 0))}%` }"
            >
              <span v-if="(stats?.swapPercent ?? 0) >= 15" class="metric-inner-text">{{ (stats?.swapPercent ?? 0).toFixed(0) }}%</span>
            </div>
            <span class="metric-right-label">
              {{ formatBytes(stats?.swapUsed ?? 0) }} / {{ formatBytes(stats?.swapTotal ?? 0) }}
            </span>
          </div>
        </div>
      </div>

      <!-- 进程排行表格 -->
      <div class="proc-section">
        <div class="proc-table-header">
          <div
            class="proc-col mem-col"
            :class="{ active: processSort === 'mem' }"
            @click="processSort = 'mem'"
          >
            内存
          </div>
          <div
            class="proc-col cpu-col"
            :class="{ active: processSort === 'cpu' }"
            @click="processSort = 'cpu'"
          >
            CPU
          </div>
          <div class="proc-col cmd-col">命令</div>
        </div>

        <div class="proc-table-body">
          <div v-if="sortedProcesses.length === 0" class="proc-empty">
            暂无进程数据
          </div>
          <div
            v-for="proc in sortedProcesses"
            :key="proc.pid"
            class="proc-row"
            :title="`PID: ${proc.pid}\nCPU: ${proc.cpuPercent.toFixed(1)}%\n内存: ${proc.memoryDisplay}\n命令: ${proc.name}`"
          >
            <div class="proc-cell mem-cell">{{ proc.memoryDisplay }}</div>
            <div class="proc-cell cpu-cell">
              <span class="cpu-badge" :class="{ high: proc.cpuPercent > 50 }">
                {{ proc.cpuPercent.toFixed(1) }}
              </span>
            </div>
            <div class="proc-cell cmd-cell">{{ proc.name }}</div>
          </div>
        </div>
      </div>

      <!-- 网络折线图与速率 -->
      <div class="network-section">
        <div class="net-header">
          <div class="net-rates">
            <span class="rate-up" title="上传速率">
              ↑{{ formatSpeed(currentNetwork.txSpeed) }}
            </span>
            <span class="rate-down" title="下载速率">
              ↓{{ formatSpeed(currentNetwork.rxSpeed) }}
            </span>
          </div>
          <div class="net-select-wrap">
            <select v-model="selectedNetwork" class="net-select">
              <option value="all">全部网卡</option>
              <option v-for="net in stats?.networks" :key="net.name" :value="net.name">
                {{ net.name }}
              </option>
            </select>
          </div>
        </div>

        <!-- 速率曲线 SVG 图表 -->
        <div class="net-chart-container">
          <div class="chart-max-label">{{ formatSpeed(networkChartPoints.maxSpeed) }}</div>
          <svg class="net-svg-chart" viewBox="0 0 280 55" preserveAspectRatio="none">
            <!-- 网格线 -->
            <line x1="0" y1="18" x2="280" y2="18" stroke="rgba(255,255,255,0.06)" stroke-dasharray="3,3" />
            <line x1="0" y1="36" x2="280" y2="36" stroke="rgba(255,255,255,0.06)" stroke-dasharray="3,3" />

            <!-- 上传 TX 折线 (橙红) -->
            <polyline
              v-if="networkChartPoints.txPoints"
              fill="none"
              stroke="#f56c6c"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              :points="networkChartPoints.txPoints"
            />
            <!-- 下载 RX 折线 (绿色) -->
            <polyline
              v-if="networkChartPoints.rxPoints"
              fill="none"
              stroke="#67c23a"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              :points="networkChartPoints.rxPoints"
            />
          </svg>
        </div>
      </div>

      <!-- 磁盘挂载列表 -->
      <div class="disk-section">
        <div class="disk-table-header">
          <div class="disk-col path-col">路径</div>
          <div class="disk-col size-col">可用/大小</div>
        </div>
        <div class="disk-table-body">
          <div v-if="!stats?.disks?.length" class="disk-empty">
            暂无挂载点数据
          </div>
          <div
            v-for="disk in stats?.disks"
            :key="disk.mountPoint"
            class="disk-row"
            :title="`文件系统: ${disk.filesystem}\n挂载点: ${disk.mountPoint}\n已用: ${formatBytes(disk.usedBytes)} (${disk.usePercent.toFixed(1)}%)\n可用: ${formatBytes(disk.freeBytes)}\n总容量: ${formatBytes(disk.totalBytes)}`"
          >
            <div class="disk-cell path-cell">
              <span class="mount-name">{{ disk.mountPoint }}</span>
            </div>
            <div class="disk-cell size-cell">
              <span class="avail-text">{{ formatBytes(disk.freeBytes) }}</span>
              <span class="sep">/</span>
              <span class="total-text">{{ formatBytes(disk.totalBytes) }}</span>
            </div>
            <!-- 磁盘使用率背景微进度条 -->
            <div
              class="disk-percent-bg"
              :style="{ width: `${Math.min(100, disk.usePercent)}%` }"
              :class="{ warn: disk.usePercent > 80, danger: disk.usePercent > 90 }"
            />
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.sysinfo-panel {
  display: flex;
  flex-direction: column;
  height: 100%;
  width: 100%;
  background: var(--app-panel, #181c24);
  color: var(--app-text, #d8dee9);
  font-size: 12px;
  user-select: none;
}

.sysinfo-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 12px;
  border-bottom: 1px solid var(--app-border, #2d3446);
  flex-shrink: 0;
}

.sysinfo-sync {
  display: flex;
  align-items: center;
  gap: 6px;
  font-weight: 600;
  font-size: 13px;
}

.sync-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #717c91;
}

.sync-dot.live {
  background: #3dd68c;
  box-shadow: 0 0 6px rgba(61, 214, 140, 0.7);
}

.icon-action-btn {
  background: transparent;
  border: none;
  color: var(--app-muted, #8b9bb4);
  cursor: pointer;
  padding: 4px;
  border-radius: 4px;
  display: flex;
  align-items: center;
  justify-content: center;
}

.icon-action-btn:hover {
  color: #fff;
  background: rgba(255, 255, 255, 0.08);
}

.sysinfo-ip-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  border-bottom: 1px solid var(--app-border, #2d3446);
  background: rgba(0, 0, 0, 0.15);
  flex-shrink: 0;
}

.ip-label {
  color: var(--app-muted, #8b9bb4);
  font-weight: 600;
}

.ip-val {
  font-family: monospace;
  font-weight: 500;
  color: #e2e8f0;
}

.copy-link-btn {
  margin-left: auto;
  border: none;
  background: transparent;
  color: #409eff;
  cursor: pointer;
  font-size: 11px;
}

.copy-link-btn:hover {
  text-decoration: underline;
}

.sysinfo-error {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  background: rgba(245, 108, 108, 0.15);
  color: #f56c6c;
  font-size: 11px;
  border-bottom: 1px solid rgba(245, 108, 108, 0.25);
  flex-shrink: 0;
}

.sysinfo-scroll-body {
  flex: 1;
  overflow-y: auto;
  padding: 8px 10px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* 运行时间与负载 */
.info-section {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding-bottom: 8px;
  border-bottom: 1px solid var(--app-border, #2d3446);
}

.info-label-val {
  display: flex;
  justify-content: space-between;
  align-items: center;
  font-size: 12px;
}

.info-label {
  color: var(--app-muted, #8b9bb4);
}

.info-val {
  font-family: monospace;
  font-weight: 600;
  color: #f1f5f9;
}

/* CPU / 内存 / 交换卡片 */
.metrics-cards {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.metric-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.metric-name {
  width: 32px;
  color: var(--app-muted, #8b9bb4);
  font-size: 12px;
  flex-shrink: 0;
}

.metric-track {
  flex: 1;
  position: relative;
  height: 20px;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid var(--app-border, #2d3446);
  border-radius: 4px;
  overflow: hidden;
  display: flex;
  align-items: center;
}

.metric-fill {
  height: 100%;
  display: flex;
  align-items: center;
  padding-left: 6px;
  transition: width 0.3s ease;
}

.cpu-fill {
  background: linear-gradient(90deg, #3dd68c, #67c23a);
}

.mem-fill {
  background: linear-gradient(90deg, #e6a23c, #f59e0b);
}

.swap-fill {
  background: linear-gradient(90deg, #e6a23c, #d97706);
}

.metric-inner-text {
  font-size: 10px;
  font-weight: 700;
  color: #111827;
  white-space: nowrap;
}

.metric-outer-text {
  font-size: 10px;
  font-weight: 700;
  color: #94a3b8;
  padding-left: 6px;
  white-space: nowrap;
}

.metric-right-label {
  position: absolute;
  right: 6px;
  font-size: 10px;
  font-family: monospace;
  color: #cbd5e1;
  text-shadow: 0 1px 2px rgba(0, 0, 0, 0.8);
}

/* 进程排行 */
.proc-section {
  border: 1px solid var(--app-border, #2d3446);
  border-radius: 4px;
  overflow: hidden;
  background: rgba(0, 0, 0, 0.12);
}

.proc-table-header {
  display: flex;
  background: #252b3b;
  border-bottom: 1px solid var(--app-border, #2d3446);
  font-size: 11px;
  font-weight: 600;
}

.proc-col {
  padding: 4px 6px;
  cursor: pointer;
  display: flex;
  align-items: center;
  color: var(--app-muted, #8b9bb4);
}

.proc-col.active {
  color: #fff;
  background: #2065d8;
}

.mem-col {
  width: 60px;
  border-right: 1px solid var(--app-border, #2d3446);
}

.cpu-col {
  width: 50px;
  border-right: 1px solid var(--app-border, #2d3446);
}

.cmd-col {
  flex: 1;
  cursor: default;
}

.proc-table-body {
  max-height: 280px;
  min-height: 140px;
  overflow-y: auto;
}

.proc-empty {
  padding: 12px;
  text-align: center;
  color: var(--app-muted, #8b9bb4);
  font-size: 11px;
}

.proc-row {
  display: flex;
  align-items: center;
  font-size: 11px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.04);
}

.proc-row:hover {
  background: rgba(255, 255, 255, 0.05);
}

.proc-cell {
  padding: 3px 6px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  font-family: monospace;
}

.mem-cell {
  width: 60px;
  color: #94a3b8;
}

.cpu-cell {
  width: 50px;
  color: #67c23a;
}

.cpu-badge.high {
  color: #f56c6c;
  font-weight: 700;
}

.cmd-cell {
  flex: 1;
  color: #e2e8f0;
}

/* 网络区域 */
.network-section {
  border: 1px solid var(--app-border, #2d3446);
  border-radius: 4px;
  padding: 8px;
  background: rgba(0, 0, 0, 0.12);
}

.net-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 6px;
}

.net-rates {
  display: flex;
  align-items: center;
  gap: 8px;
  font-family: monospace;
  font-size: 11px;
  font-weight: 600;
}

.rate-up {
  color: #f56c6c;
}

.rate-down {
  color: #67c23a;
}

.net-select {
  background: #252b3b;
  border: 1px solid var(--app-border, #2d3446);
  color: var(--app-text, #cbd5e1);
  font-size: 11px;
  padding: 2px 4px;
  border-radius: 3px;
  outline: none;
}

.net-chart-container {
  position: relative;
  height: 55px;
  width: 100%;
  background: rgba(0, 0, 0, 0.25);
  border: 1px solid rgba(255, 255, 255, 0.05);
  border-radius: 3px;
}

.chart-max-label {
  position: absolute;
  top: 2px;
  left: 4px;
  font-size: 9px;
  color: #64748b;
  font-family: monospace;
}

.net-svg-chart {
  width: 100%;
  height: 100%;
}

/* 磁盘挂载区域 */
.disk-section {
  border: 1px solid var(--app-border, #2d3446);
  border-radius: 4px;
  overflow: hidden;
  background: rgba(0, 0, 0, 0.12);
}

.disk-table-header {
  display: flex;
  justify-content: space-between;
  background: #252b3b;
  border-bottom: 1px solid var(--app-border, #2d3446);
  padding: 4px 8px;
  font-size: 11px;
  font-weight: 600;
  color: var(--app-muted, #8b9bb4);
}

.disk-table-body {
  display: flex;
  flex-direction: column;
}

.disk-empty {
  padding: 12px;
  text-align: center;
  color: var(--app-muted, #8b9bb4);
  font-size: 11px;
}

.disk-row {
  position: relative;
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 5px 8px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.04);
  font-size: 11px;
}

.disk-cell {
  position: relative;
  z-index: 1;
}

.path-cell {
  font-family: monospace;
  color: #f1f5f9;
  max-width: 130px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.size-cell {
  font-family: monospace;
  color: #94a3b8;
}

.avail-text {
  color: #3dd68c;
  font-weight: 600;
}

.sep {
  margin: 0 2px;
  color: #64748b;
}

.total-text {
  color: #cbd5e1;
}

.disk-percent-bg {
  position: absolute;
  top: 0;
  left: 0;
  bottom: 0;
  background: rgba(61, 214, 140, 0.1);
  pointer-events: none;
  transition: width 0.3s ease;
}

.disk-percent-bg.warn {
  background: rgba(230, 162, 60, 0.15);
}

.disk-percent-bg.danger {
  background: rgba(245, 108, 108, 0.2);
}
</style>
