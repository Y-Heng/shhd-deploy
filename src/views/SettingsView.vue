<script setup lang="ts">
/** 设置：外观、MCP、诊断日志、导入导出配置 */
import { computed, onMounted, ref } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog'
import { api } from '../api'
import type { AppConfig } from '../types'
import HelpView from './HelpView.vue'
import { useAppearance } from '../composables/useAppearance'

const { appearanceMode, setAppearance } = useAppearance()

const configPath = ref('')
const configText = ref('')
const config = ref<AppConfig | null>(null)
const mcpRunningPort = ref<number | null>(null)
const targetScope = ref<'all' | 'custom'>('all')
const logDirPath = ref('')
const helpDialogVisible = ref(false)

onMounted(async () => {
  configPath.value = await api.getConfigPath()
  logDirPath.value = await api.getLogDir()
  await reloadConfig()
})

/** 旧配置可能没有 logging 字段 */
function ensureLoggingConfig() {
  if (!config.value) return
  if (!config.value.logging) config.value.logging = { enabled: false, level: 'info' }
}

/** 从磁盘重载配置，并同步 MCP 运行状态 */
async function reloadConfig() {
  config.value = await api.getConfig()
  ensureLoggingConfig()
  configText.value = JSON.stringify(config.value, null, 2)
  const mcp = config.value.mcp
  targetScope.value = mcp.allowedBackendGroupIds == null && mcp.allowedFrontendTargetIds == null && mcp.allowedDockerTargetIds == null ? 'all' : 'custom'
  mcpRunningPort.value = await api.getMcpStatus()
}

const permissionDescriptions: Record<string, string> = {
  readonly: '仅查询：list_config / list_releases / list_frontend_releases / get_task_status / list_tunnels，不能发起任何部署。',
  stage: '允许 backend_deploy / frontend_deploy，但 mode 只能是 stage（传到中转，不动线上）。替换、回滚、Docker、隧道启停仍需人在软件里操作。推荐给 AI 用。',
  full: '允许 full/replace 模式，以及 rollback / frontend_rollback / docker_deploy / tunnel_control。可改线上，请谨慎开启。'
}

const mcpJsonSnippet = computed(() => {
  const port = config.value?.mcp.port ?? 17423
  return JSON.stringify(
    {
      mcpServers: {
        'shhd-deploy': { url: `http://127.0.0.1:${port}/mcp` }
      }
    },
    null,
    2
  )
})

/** 保存 MCP：白名单 null=全部允许，[]=全部禁止 */
async function saveMcpSettings() {
  if (!config.value) return
  const mcp = config.value.mcp
  if (targetScope.value === 'all') {
    mcp.allowedBackendGroupIds = null
    mcp.allowedFrontendTargetIds = null
    mcp.allowedDockerTargetIds = null
  } else {
    if (!mcp.allowedBackendGroupIds) mcp.allowedBackendGroupIds = []
    if (!mcp.allowedFrontendTargetIds) mcp.allowedFrontendTargetIds = []
    if (!mcp.allowedDockerTargetIds) mcp.allowedDockerTargetIds = []
  }
  await api.saveConfig(config.value)
  // 等服务按新配置启停后刷新状态
  await new Promise(resolve => setTimeout(resolve, 500))
  mcpRunningPort.value = await api.getMcpStatus()
  configText.value = JSON.stringify(config.value, null, 2)
  ElMessage.success('MCP 设置已保存并生效')
}

/** 复制 Cursor mcp.json 片段 */
async function copyMcpSnippet() {
  await navigator.clipboard.writeText(mcpJsonSnippet.value)
  ElMessage.success('已复制，粘贴到 Cursor 的 mcp.json 即可（全局一般在 %USERPROFILE%\\.cursor\\mcp.json）')
}

/** 保存下方原始 JSON 编辑框（会覆盖全部配置） */
async function saveConfigText() {
  try {
    const parsed = JSON.parse(configText.value)
    await api.saveConfig(parsed)
    await reloadConfig()
    ElMessage.success('配置已保存')
  } catch (error) {
    ElMessage.error(`保存失败：${error}`)
  }
}

/** 导出当前配置到用户选择的文件 */
async function exportConfigFile() {
  const targetPath = await saveDialog({
    title: '导出配置',
    defaultPath: 'shhd-deploy-config.json',
    filters: [{ name: 'JSON', extensions: ['json'] }]
  })
  if (!targetPath) return
  try {
    await api.exportConfig(targetPath)
    ElMessage.success(`配置已导出到 ${targetPath}`)
  } catch (error) {
    ElMessage.error(String(error))
  }
}

/** 导入配置：覆盖服务器、隧道、部署映射等全部内容 */
async function importConfigFile() {
  const sourcePath = await openDialog({
    title: '导入配置',
    filters: [{ name: 'JSON', extensions: ['json'] }],
    multiple: false
  })
  if (typeof sourcePath !== 'string') return
  await ElMessageBox.confirm('导入将覆盖当前全部配置（服务器、隧道、部署映射），确认继续？', '导入确认', { type: 'warning', confirmButtonText: '导入并覆盖' })
  try {
    await api.importConfig(sourcePath)
    await reloadConfig()
    ElMessage.success('配置导入成功')
  } catch (error) {
    ElMessage.error(String(error))
  }
}

/** 复制配置文件路径 */
async function copyPath() {
  await navigator.clipboard.writeText(configPath.value)
  ElMessage.success('路径已复制')
}

/** 保存诊断日志开关 */
async function saveLoggingSettings() {
  if (!config.value?.logging) return
  await api.setLoggingEnabled(config.value.logging.enabled)
  await reloadConfig()
  ElMessage.success('诊断日志设置已保存')
}

/** 用资源管理器打开日志目录 */
async function openLogDirectory() {
  try {
    await api.openLogDir()
  } catch (error) {
    ElMessage.error(String(error))
  }
}

/** 复制当日日志末尾 200 行，便于发给 AI 分析 */
async function copyRecentLogs() {
  try {
    const text = await api.readRecentLogs(200)
    if (!text.trim()) {
      ElMessage.warning('暂无日志内容')
      return
    }
    await navigator.clipboard.writeText(text)
    ElMessage.success('最近日志已复制')
  } catch (error) {
    ElMessage.error(String(error))
  }
}
</script>

<template>
  <div v-if="config">
    <div class="view-header">
      <h2>设置</h2>
      <div>
        <el-button @click="helpDialogVisible = true">使用说明</el-button>
        <el-button @click="exportConfigFile">导出配置</el-button>
        <el-button @click="importConfigFile">导入配置</el-button>
      </div>
    </div>

    <el-dialog v-model="helpDialogVisible" title="使用说明" width="720px" class="help-dialog" destroy-on-close>
      <div class="help-dialog-body">
        <HelpView embedded />
      </div>
    </el-dialog>

    <el-card class="appearance-card" shadow="never">
      <template #header>
        <b>外观</b>
      </template>
      <el-form label-width="130px">
        <el-form-item label="主题">
          <el-radio-group :model-value="appearanceMode" @change="setAppearance">
            <el-radio-button value="system">跟随系统</el-radio-button>
            <el-radio-button value="light">浅色</el-radio-button>
            <el-radio-button value="dark">深色</el-radio-button>
          </el-radio-group>
          <span class="form-hint">默认跟随系统，只保存在本机，不会随配置导出</span>
        </el-form-item>
      </el-form>
    </el-card>

    <el-card class="mcp-card" shadow="never">
      <template #header>
        <div class="mcp-header">
          <b>MCP 服务（供 AI 客户端调用）</b>
          <el-tag v-if="mcpRunningPort" type="success"> 运行中 · 127.0.0.1:{{ mcpRunningPort }} </el-tag>
          <el-tag v-else type="info">已停止</el-tag>
        </div>
      </template>
      <el-form label-width="130px">
        <el-form-item label="启用 MCP 服务">
          <el-switch v-model="config.mcp.enabled" />
          <span class="form-hint"> 只监听本机 127.0.0.1。本工具必须保持运行，Cursor 等才能调用。保存后若 Cursor 看不到工具，请刷新 MCP。 </span>
        </el-form-item>
        <el-form-item label="监听端口">
          <el-input-number v-model="config.mcp.port" :min="1024" :max="65535" />
          <span class="form-hint">绑定失败说明端口被占用，换一个后同步改 mcp.json</span>
        </el-form-item>
        <el-form-item label="执行权限">
          <div style="width: 100%">
            <el-radio-group v-model="config.mcp.permission">
              <el-radio-button value="readonly">只读</el-radio-button>
              <el-radio-button value="stage">仅中转（推荐）</el-radio-button>
              <el-radio-button value="full">完全访问</el-radio-button>
            </el-radio-group>
            <div class="permission-description">
              {{ permissionDescriptions[config.mcp.permission] }}
            </div>
          </div>
        </el-form-item>
        <el-form-item label="允许访问的目标">
          <div style="width: 100%">
            <el-radio-group v-model="targetScope">
              <el-radio-button value="all">所有目标</el-radio-button>
              <el-radio-button value="custom">指定目标</el-radio-button>
            </el-radio-group>
            <div class="form-hint" style="margin-top: 6px">
              「指定目标」时未勾选的一律禁止；下拉框留空等于全部禁止。
            </div>
            <template v-if="targetScope === 'custom'">
              <div class="scope-row">
                <span class="scope-label">后端负载组</span>
                <el-select v-model="config.mcp.allowedBackendGroupIds" multiple collapse-tags placeholder="不选择 = 全部禁止" style="flex: 1">
                  <el-option v-for="group in config.backendGroups" :key="group.id" :label="group.name" :value="group.id" />
                </el-select>
              </div>
              <div class="scope-row">
                <span class="scope-label">前端项目</span>
                <el-select v-model="config.mcp.allowedFrontendTargetIds" multiple collapse-tags placeholder="不选择 = 全部禁止" style="flex: 1">
                  <el-option v-for="target in config.frontendTargets" :key="target.id" :label="target.name" :value="target.id" />
                </el-select>
              </div>
              <div class="scope-row">
                <span class="scope-label">Docker 目标</span>
                <el-select v-model="config.mcp.allowedDockerTargetIds" multiple collapse-tags placeholder="不选择 = 全部禁止" style="flex: 1">
                  <el-option v-for="target in config.dockerTargets" :key="target.id" :label="target.name" :value="target.id" />
                </el-select>
              </div>
            </template>
          </div>
        </el-form-item>
        <el-form-item label="客户端接入配置">
          <div style="width: 100%">
            <pre class="mcp-snippet">{{ mcpJsonSnippet }}</pre>
            <el-button size="small" @click="copyMcpSnippet"> 复制（粘贴到 %USERPROFILE%\.cursor\mcp.json） </el-button>
            <div class="form-hint" style="margin-top: 6px">
              也可写到项目 <code>.cursor\mcp.json</code>。改端口后两边都要改。完整工具说明见「使用说明 → AI 接入」。
            </div>
          </div>
        </el-form-item>
        <el-form-item>
          <el-button type="primary" @click="saveMcpSettings"> 保存 MCP 设置 </el-button>
        </el-form-item>
      </el-form>
    </el-card>

    <el-card class="logging-card" shadow="never">
      <template #header>
        <b>诊断日志</b>
      </template>
      <el-form label-width="130px">
        <el-form-item label="启用诊断日志">
          <el-switch v-model="config.logging!.enabled" />
          <span class="form-hint">默认关闭；开启后记录关键操作，便于提供给 AI 分析</span>
        </el-form-item>
        <el-form-item label="日志目录">
          <el-input :model-value="logDirPath" readonly>
            <template #append>
              <el-button @click="openLogDirectory">打开目录</el-button>
            </template>
          </el-input>
        </el-form-item>
        <el-form-item>
          <el-button type="primary" @click="saveLoggingSettings">保存日志设置</el-button>
          <el-button @click="copyRecentLogs">复制最近日志</el-button>
        </el-form-item>
      </el-form>
    </el-card>

    <el-form label-width="90px" class="config-block" style="margin-top: 16px">
      <el-form-item label="配置文件">
        <el-input :model-value="configPath" readonly>
          <template #append>
            <el-button @click="copyPath">复制路径</el-button>
          </template>
        </el-input>
      </el-form-item>
      <el-form-item label="配置内容">
        <el-input v-model="configText" type="textarea" :rows="20" class="config-editor" spellcheck="false" />
      </el-form-item>
      <el-form-item>
        <el-button type="primary" @click="saveConfigText">校验并保存</el-button>
        <el-button @click="reloadConfig">重新加载</el-button>
      </el-form-item>
    </el-form>
    <el-alert type="warning" :closable="false" title="安全提示：配置中的密码仅保存在本机配置文件中。建议服务器尽量使用私钥认证（在服务器编辑里选择「私钥」）" />
  </div>
</template>

<style scoped>
.view-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 12px;
  padding: 12px 14px;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  background: var(--app-panel, #151a22);
}
.view-header h2 {
  margin: 0;
}
.mcp-card {
  margin-bottom: 8px;
}
.appearance-card {
  margin-bottom: 8px;
}
.logging-card {
  margin-bottom: 8px;
}
.mcp-header {
  display: flex;
  align-items: center;
  gap: 12px;
}
.form-hint {
  margin-left: 12px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.permission-description {
  margin-top: 6px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
  line-height: 1.6;
}
.scope-row {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 10px;
}
.scope-label {
  width: 90px;
  font-size: 13px;
  color: var(--el-text-color-regular);
}
.mcp-snippet {
  background: var(--app-bg, #0f1218);
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  padding: 10px 14px;
  font-family: Consolas, Menlo, monospace;
  font-size: 12px;
  margin: 0 0 8px;
  line-height: 1.6;
}
.config-block {
  padding: 14px 16px 4px;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  background: var(--app-panel, #151a22);
}
.config-editor :deep(textarea) {
  font-family: Consolas, 'Courier New', monospace;
  font-size: 12px;
}
.help-dialog-body {
  max-height: min(70vh, 640px);
  overflow-y: auto;
  padding-right: 4px;
}
</style>
