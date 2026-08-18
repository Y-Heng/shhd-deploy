<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { api } from "../api";
import { backendDeployTask } from "../composables/useTask";
import TaskLogPanel from "../components/TaskLogPanel.vue";
import type {
  AppConfig,
  BackendGroup,
  BackendProject,
  CopyMode,
  DeployMode,
  ReleaseRecord,
} from "../types";
import {
  IIS_START_SCRIPT,
  IIS_STOP_SCRIPT,
  JAVA_START_SCRIPT,
  JAVA_STOP_SCRIPT,
} from "../serviceScriptPresets";

const props = defineProps<{ active?: boolean }>();

const config = ref<AppConfig | null>(null);
const releases = ref<ReleaseRecord[]>([]);
const activeTab = ref("deploy");

const selectedGroupId = ref("");
const selectedProjectIds = ref<string[]>([]);
const featureName = ref("");
const copyMode = ref<CopyMode>("smb");
const deployMode = ref<DeployMode>("full");
const backupSibling = ref(true);
const stagedReleaseName = ref("");

const task = backendDeployTask;

const selectedGroup = computed<BackendGroup | null>(
  () =>
    config.value?.backendGroups.find(
      (group) => group.id === selectedGroupId.value
    ) ?? null
);

// 使用本地时间生成 yyyyMMdd 前缀
const now = new Date();
const datePrefix = `${now.getFullYear()}${String(now.getMonth() + 1).padStart(2, "0")}${String(now.getDate()).padStart(2, "0")}`;

const releaseName = computed(() =>
  deployMode.value === "replace"
    ? stagedReleaseName.value.trim()
    : `${datePrefix}-${featureName.value.trim()}`
);

// 当前组的待替换发布
const stagedReleases = computed(() =>
  releases.value.filter(
    (record) =>
      record.status === "staged" && record.groupId === selectedGroupId.value
  )
);

onMounted(async () => {
  await reloadPage();
});

watch(
  () => props.active,
  async (active) => {
    if (!active) return;
    task.dismissed.value = true;
    await reloadPage();
  }
);

async function reloadPage() {
  config.value = await api.getConfig();
  if (!config.value) return;
  for (const group of config.value.backendGroups) {
    if (!group.serverIds || group.serverIds.length === 0)
      group.serverIds = groupServerIds(group);
  }
  releases.value = await api.getReleases();
  if (!selectedGroupId.value && config.value.backendGroups.length > 0)
    selectGroup(config.value.backendGroups[0].id);
}

function selectGroup(groupId: string) {
  selectedGroupId.value = groupId;
  const group = config.value?.backendGroups.find(
    (item) => item.id === groupId
  );
  if (group) {
    // 默认全选项目
    selectedProjectIds.value = group.projects.map((project) => project.id);
    copyMode.value = group.copyMode;
  }
  stagedReleaseName.value = "";
}

// 选中某个待替换发布时，自动勾选它包含的项目
function onStagedReleasePicked(name: string) {
  const record = stagedReleases.value.find(
    (item) => item.releaseName === name
  );
  if (record) selectedProjectIds.value = [...record.projectIds];
}

function serverNameById(serverId?: string | null): string {
  if (!serverId) return "无";
  const server = config.value?.servers.find((item) => item.id === serverId);
  return server ? server.name : serverId;
}

// 兼容旧配置：解析组内有效服务器 id 列表
function groupServerIds(group: BackendGroup): string[] {
  if (group.serverIds && group.serverIds.length > 0) return group.serverIds;
  const ids: string[] = [];
  if (group.primaryServerId) ids.push(group.primaryServerId);
  if (group.secondaryServerId) ids.push(group.secondaryServerId);
  return ids;
}

function groupServerNames(group: BackendGroup): string {
  const names = groupServerIds(group).map((id) => serverNameById(id));
  return names.length > 0 ? names.join(" → ") : "未配置服务器";
}

const modeDescriptions: Record<DeployMode, string> = {
  full: "校验产物 → 压缩上传中转 → 同步备机 → 备份 → 滚动替换 → 健康检查",
  stage: "校验产物 → 压缩上传中转 → 同步备机（不动线上，稍后再替换）",
  replace: "使用已上传的中转内容：备份 → 滚动替换 → 健康检查",
};

async function startDeploy() {
  if (task.running.value) return;
  if (!selectedGroup.value) return;
  if (selectedProjectIds.value.length === 0) {
    ElMessage.warning("请至少选择一个项目");
    return;
  }
  if (deployMode.value === "replace") {
    if (!stagedReleaseName.value.trim()) {
      ElMessage.warning("请选择或输入要替换的中转发布名称");
      return;
    }
  } else if (!featureName.value.trim()) {
    ElMessage.warning("请填写本次上线的功能名称");
    return;
  }
  const projectNames = selectedGroup.value.projects
    .filter((project) => selectedProjectIds.value.includes(project.id))
    .map((project) => project.name)
    .join("、");
  const modeLabel =
    deployMode.value === "full"
      ? "上传并立即替换"
      : deployMode.value === "stage"
        ? "仅上传到中转"
        : "从中转替换";
  await ElMessageBox.confirm(
    `方式：${modeLabel}\n发布名称：${releaseName.value}\n项目：${projectNames}\n服务器（按顺序滚动）：${groupServerNames(
      selectedGroup.value
    )}${
      deployMode.value !== "stage" && backupSibling.value
        ? `\n附加备份：应用目录 → 目录名-${datePrefix}`
        : ""
    }\n\n确认执行？`,
    "部署确认",
    { type: "warning", confirmButtonText: "执行" }
  );
  try {
    const taskId = await api.startBackendDeploy({
      groupId: selectedGroupId.value,
      projectIds: selectedProjectIds.value,
      releaseName: releaseName.value,
      copyMode: copyMode.value,
      mode: deployMode.value,
      backupSibling: deployMode.value !== "stage" && backupSibling.value,
    });
    await task.attach(taskId);
  } catch (error) {
    ElMessage.error(String(error));
  }
}

// 从历史记录直接执行替换
async function replaceStaged(record: ReleaseRecord) {
  if (task.running.value) return;
  await ElMessageBox.confirm(
    `将把「${record.releaseName}」的中转内容替换到 ${record.groupName} 线上目录：\n${record.projectIds.join(
      "、"
    )}\n\n确认执行替换？`,
    "替换确认",
    { type: "warning", confirmButtonText: "执行替换" }
  );
  try {
    const taskId = await api.startBackendDeploy({
      groupId: record.groupId,
      projectIds: record.projectIds,
      releaseName: record.releaseName,
      mode: "replace",
      backupSibling: backupSibling.value,
    });
    activeTab.value = "deploy";
    await task.attach(taskId);
  } catch (error) {
    ElMessage.error(String(error));
  }
}

async function startRollback(record: ReleaseRecord) {
  await ElMessageBox.confirm(
    `将把 ${record.groupName} 的以下项目恢复到「${record.releaseName}」发布前的备份：\n${record.projectIds.join(
      "、"
    )}\n\n确认回滚？`,
    "回滚确认",
    { type: "warning", confirmButtonText: "确认回滚" }
  );
  try {
    const taskId = await api.startRollback(record.id);
    activeTab.value = "deploy";
    await task.attach(taskId);
  } catch (error) {
    ElMessage.error(String(error));
  }
}

async function refreshReleases() {
  releases.value = await api.getReleases();
}

watch(
  () => task.finalState.value,
  (state) => {
    if (state === "success") refreshReleases();
  }
);

function releaseStatusMeta(status: string): { label: string; type: "success" | "warning" | "info" | "danger" } {
  if (status === "success") return { label: "成功", type: "success" };
  if (status === "staged") return { label: "待替换", type: "warning" };
  if (status === "rolled_back") return { label: "已回滚", type: "info" };
  if (status === "rollback") return { label: "回滚完成", type: "info" };
  return { label: "失败", type: "danger" };
}

// ===== 配置管理 =====
const projectDialogVisible = ref(false);
const isNewProject = ref(false);
const projectForm = reactive<BackendProject>({
  id: "",
  name: "",
  localBinDir: "",
  remoteAppDir: "",
  healthCheckUrl: "",
  healthCheckRetries: 10,
  healthCheckDelaySecs: 3,
  stopScript: IIS_STOP_SCRIPT,
  startScript: IIS_START_SCRIPT,
  stopIisBeforeReplace: false,
});

async function persistConfig() {
  if (!config.value) return;
  await api.saveConfig(config.value);
  ElMessage.success("配置已保存");
}

async function addGroup() {
  if (!config.value) return;
  const newGroup: BackendGroup = {
    id: `group-${Date.now()}`,
    name: "新负载组",
    serverIds: config.value.servers[0] ? [config.value.servers[0].id] : [],
    primaryServerId: null,
    secondaryServerId: null,
    stagingDir: "D:\\code\\sites\\devlop",
    backupDir: "D:\\code\\sites\\backup",
    copyMode: "smb",
    projects: [],
  };
  config.value.backendGroups.push(newGroup);
  await api.saveConfig(config.value);
  selectGroup(newGroup.id);
  ElMessage.success("已添加负载组，请完善配置");
}

async function removeGroup() {
  if (!config.value || !selectedGroup.value) return;
  await ElMessageBox.confirm(
    `确认删除负载组 ${selectedGroup.value.name} 及其全部项目配置？`,
    "删除确认",
    { type: "warning" }
  );
  config.value.backendGroups = config.value.backendGroups.filter(
    (group) => group.id !== selectedGroupId.value
  );
  await api.saveConfig(config.value);
  if (config.value.backendGroups.length > 0)
    selectGroup(config.value.backendGroups[0].id);
  else selectedGroupId.value = "";
  ElMessage.success("已删除");
}

function openAddProject() {
  isNewProject.value = true;
  Object.assign(projectForm, {
    id: `project-${Date.now()}`,
    name: "",
    localBinDir: "",
    remoteAppDir: "",
    healthCheckUrl: "",
    healthCheckRetries: 10,
    healthCheckDelaySecs: 3,
    stopScript: IIS_STOP_SCRIPT,
    startScript: IIS_START_SCRIPT,
    stopIisBeforeReplace: false,
  });
  projectDialogVisible.value = true;
}

function openEditProject(project: BackendProject) {
  isNewProject.value = false;
  Object.assign(projectForm, JSON.parse(JSON.stringify(project)));
  if (!projectForm.stopScript && !projectForm.startScript && projectForm.stopIisBeforeReplace !== false) {
    projectForm.stopScript = IIS_STOP_SCRIPT;
    projectForm.startScript = IIS_START_SCRIPT;
  }
  if (!projectForm.stopScript) projectForm.stopScript = "";
  if (!projectForm.startScript) projectForm.startScript = "";
  projectDialogVisible.value = true;
}

async function applyServicePreset(kind: "iis" | "java") {
  const stopScript = kind === "iis" ? IIS_STOP_SCRIPT : JAVA_STOP_SCRIPT;
  const startScript = kind === "iis" ? IIS_START_SCRIPT : JAVA_START_SCRIPT;
  const hasCustom =
    (projectForm.stopScript || "") !== "" || (projectForm.startScript || "") !== "";
  const sameAsTarget =
    projectForm.stopScript === stopScript && projectForm.startScript === startScript;
  if (hasCustom && !sameAsTarget) {
    await ElMessageBox.confirm("将覆盖当前停止/启动脚本，确认填入？", "填入方案", {
      type: "warning",
      confirmButtonText: "覆盖",
    });
  }
  projectForm.stopScript = stopScript;
  projectForm.startScript = startScript;
  projectForm.stopIisBeforeReplace = false;
}

async function clearServiceScripts() {
  if (projectForm.stopScript || projectForm.startScript) {
    await ElMessageBox.confirm("清空后替换时不会停止任何服务，确认？", "清空脚本", {
      type: "warning",
    });
  }
  projectForm.stopScript = "";
  projectForm.startScript = "";
  projectForm.stopIisBeforeReplace = false;
}

async function chooseLocalBinDir() {
  const selected = await openDialog({ directory: true });
  if (typeof selected === "string") projectForm.localBinDir = selected;
}

async function saveProject() {
  if (!config.value || !selectedGroup.value) return;
  if (!projectForm.name || !projectForm.localBinDir || !projectForm.remoteAppDir) {
    ElMessage.warning("名称、本地产物目录、服务器应用目录为必填");
    return;
  }
  const clone: BackendProject = JSON.parse(JSON.stringify(projectForm));
  if (!clone.healthCheckUrl) clone.healthCheckUrl = null;
  clone.stopScript = clone.stopScript || "";
  clone.startScript = clone.startScript || "";
  clone.stopIisBeforeReplace = false;
  const group = selectedGroup.value;
  if (isNewProject.value) {
    group.projects.push(clone);
  } else {
    const index = group.projects.findIndex((item) => item.id === clone.id);
    if (index >= 0) group.projects.splice(index, 1, clone);
  }
  await api.saveConfig(config.value);
  projectDialogVisible.value = false;
  // 部署页勾选列表刷新
  selectedProjectIds.value = group.projects.map((project) => project.id);
  ElMessage.success("已保存");
}

async function removeProject(project: BackendProject) {
  if (!config.value || !selectedGroup.value) return;
  await ElMessageBox.confirm(`确认删除项目 ${project.name}？`, "删除确认", {
    type: "warning",
  });
  const group = selectedGroup.value;
  group.projects = group.projects.filter((item) => item.id !== project.id);
  await api.saveConfig(config.value);
  selectedProjectIds.value = group.projects.map((item) => item.id);
  ElMessage.success("已删除");
}
</script>

<template>
  <div v-if="config">
    <div class="view-header">
      <h2>后端部署</h2>
      <el-select
        v-if="config.backendGroups.length > 0"
        :model-value="selectedGroupId"
        style="width: 280px"
        @change="selectGroup"
      >
        <el-option
          v-for="group in config.backendGroups"
          :key="group.id"
          :label="group.name"
          :value="group.id"
        />
      </el-select>
    </div>
    <el-alert
      v-if="config.backendGroups.length === 0"
      title="尚未配置后端负载组，请到「项目配置」页签添加"
      type="warning"
      :closable="false"
      style="margin-bottom: 12px"
    />
    <el-tabs v-model="activeTab">
      <el-tab-pane label="部署" name="deploy">
        <el-form label-width="110px" style="max-width: 900px">
          <el-form-item label="部署方式">
            <el-radio-group v-model="deployMode">
              <el-radio-button value="full">上传并立即替换</el-radio-button>
              <el-radio-button value="stage">仅上传到中转</el-radio-button>
              <el-radio-button value="replace">从中转替换</el-radio-button>
            </el-radio-group>
            <div class="mode-description">{{ modeDescriptions[deployMode] }}</div>
          </el-form-item>

          <el-form-item v-if="selectedGroup" label="目标服务器">
            <span class="server-order">{{ groupServerNames(selectedGroup) }}</span>
          </el-form-item>

          <el-form-item v-if="selectedGroup" label="部署项目">
            <el-checkbox-group v-model="selectedProjectIds">
              <div
                v-for="project in selectedGroup.projects"
                :key="project.id"
                class="project-row"
              >
                <el-checkbox :value="project.id">
                  <b>{{ project.name }}</b>
                  <span class="project-path">
                    {{ project.localBinDir }} → {{ project.remoteAppDir }}\bin
                  </span>
                </el-checkbox>
              </div>
            </el-checkbox-group>
            <div v-if="selectedGroup.projects.length === 0" class="project-path">
              该组还没有项目，请到「项目配置」页签添加
            </div>
          </el-form-item>

          <el-form-item
            v-if="deployMode !== 'replace'"
            label="发布名称"
          >
            <el-input
              v-model="featureName"
              placeholder="本次上线的功能名称，如：优惠券功能"
              style="width: 320px"
            >
              <template #prepend>{{ datePrefix }}-</template>
            </el-input>
          </el-form-item>
          <el-form-item v-else label="中转发布">
            <el-select
              v-model="stagedReleaseName"
              filterable
              allow-create
              default-first-option
              placeholder="选择待替换的发布，或输入中转目录名"
              style="width: 420px"
              @change="onStagedReleasePicked"
            >
              <el-option
                v-for="record in stagedReleases"
                :key="record.id"
                :label="`${record.releaseName}（${record.createdAt}）`"
                :value="record.releaseName"
              />
            </el-select>
          </el-form-item>

          <el-form-item v-if="deployMode !== 'replace'" label="备机同步方式">
            <el-radio-group v-model="copyMode">
              <el-radio value="smb">内网 SMB 复制（推荐，只上传一次）</el-radio>
              <el-radio value="upload">分别上传</el-radio>
            </el-radio-group>
          </el-form-item>

          <el-form-item v-if="deployMode !== 'stage'" label="附加备份">
            <el-checkbox v-model="backupSibling">
              替换前把应用目录备份为 目录名-{{ datePrefix }}（当天已备份则跳过）
            </el-checkbox>
          </el-form-item>

          <el-form-item>
            <el-button
              type="primary"
              size="large"
              :disabled="task.running.value"
              @click="startDeploy"
            >
              {{
                deployMode === "stage"
                  ? "上传到中转"
                  : deployMode === "replace"
                    ? "执行替换"
                    : "开始部署"
              }}
            </el-button>
          </el-form-item>
        </el-form>
        <TaskLogPanel
          :logs="task.logs.value"
          :running="task.running.value"
          :percent="task.percent.value"
          :step="task.step.value"
          :final-state="task.finalState.value"
          @cancel="task.cancel"
        />
      </el-tab-pane>

      <el-tab-pane label="发布历史 / 回滚" name="history">
        <div style="margin-bottom: 10px">
          <el-button size="small" @click="refreshReleases">刷新</el-button>
        </div>
        <el-table :data="releases" stripe>
          <el-table-column prop="releaseName" label="发布名称" min-width="200" />
          <el-table-column prop="groupName" label="负载组" width="170" />
          <el-table-column label="项目" min-width="180">
            <template #default="{ row }">{{ row.projectIds.join("、") }}</template>
          </el-table-column>
          <el-table-column prop="createdAt" label="时间" width="165" />
          <el-table-column label="状态" width="120">
            <template #default="{ row }">
              <el-tag :type="releaseStatusMeta(row.status).type">
                {{ releaseStatusMeta(row.status).label }}
              </el-tag>
            </template>
          </el-table-column>
          <el-table-column label="操作" width="130" fixed="right">
            <template #default="{ row }">
              <el-button
                v-if="row.status === 'staged'"
                size="small"
                type="primary"
                plain
                :disabled="task.running.value"
                @click="replaceStaged(row)"
              >
                执行替换
              </el-button>
              <el-button
                v-else-if="row.status === 'success'"
                size="small"
                type="warning"
                plain
                :disabled="task.running.value"
                @click="startRollback(row)"
              >
                回滚
              </el-button>
            </template>
          </el-table-column>
        </el-table>
      </el-tab-pane>

      <el-tab-pane label="项目配置" name="settings">
        <div style="margin-bottom: 12px">
          <el-button size="small" @click="addGroup">添加负载组</el-button>
          <el-button
            v-if="selectedGroup"
            size="small"
            type="danger"
            plain
            @click="removeGroup"
          >
            删除当前组
          </el-button>
        </div>
        <template v-if="selectedGroup">
          <el-form label-width="110px" style="max-width: 760px">
            <el-form-item label="组名称">
              <el-input v-model="selectedGroup.name" style="width: 320px" />
            </el-form-item>
            <el-form-item label="组内服务器">
              <div style="width: 100%">
                <el-select
                  v-model="selectedGroup.serverIds"
                  multiple
                  style="width: 100%"
                  placeholder="选择该组的所有服务器（高峰期可多加几台）"
                >
                  <el-option
                    v-for="server in config.servers.filter((item) => item.os === 'windows')"
                    :key="server.id"
                    :label="server.name"
                    :value="server.id"
                  />
                </el-select>
                <div class="form-hint" style="margin: 6px 0 0">
                  第一台作为上传中转与滚动起点，其余通过内网从第一台同步；部署时按此顺序逐台替换，线上始终保留在服务的机器。可选任意台数。
                </div>
                <div
                  v-if="selectedGroup.serverIds && selectedGroup.serverIds.length > 1"
                  class="server-order"
                >
                  顺序：{{ selectedGroup.serverIds.map((id) => serverNameById(id)).join(" → ") }}
                </div>
              </div>
            </el-form-item>
            <el-form-item label="中转目录">
              <el-input
                v-model="selectedGroup.stagingDir"
                placeholder="如 D:\code\sites\devlop"
                style="width: 420px"
              />
              <span class="form-hint">发布包上传解压位置（服务器上）</span>
            </el-form-item>
            <el-form-item label="备份目录">
              <el-input
                v-model="selectedGroup.backupDir"
                placeholder="如 D:\code\sites\backup"
                style="width: 420px"
              />
              <span class="form-hint">替换前 bin 的备份位置（回滚数据源）</span>
            </el-form-item>
            <el-form-item label="备机同步">
              <el-radio-group v-model="selectedGroup.copyMode">
                <el-radio value="smb">内网 SMB 复制</el-radio>
                <el-radio value="upload">分别上传</el-radio>
              </el-radio-group>
            </el-form-item>
            <el-form-item>
              <el-button type="primary" @click="persistConfig">保存组配置</el-button>
            </el-form-item>
          </el-form>

          <div class="projects-header">
            <h3>项目列表</h3>
            <el-button size="small" type="primary" @click="openAddProject">
              添加项目
            </el-button>
          </div>
          <el-table :data="selectedGroup.projects" stripe>
            <el-table-column prop="name" label="项目" min-width="150" />
            <el-table-column
              prop="localBinDir"
              label="本地产物目录"
              min-width="260"
              show-overflow-tooltip
            />
            <el-table-column
              prop="remoteAppDir"
              label="服务器应用目录"
              min-width="240"
              show-overflow-tooltip
            />
            <el-table-column
              prop="healthCheckUrl"
              label="健康检查"
              min-width="200"
              show-overflow-tooltip
            />
            <el-table-column label="操作" width="180" fixed="right">
              <template #default="{ row }">
                <el-button @click="openEditProject(row)">编辑</el-button>
                <el-button type="danger" plain @click="removeProject(row)">删除</el-button>
              </template>
            </el-table-column>
          </el-table>
        </template>
      </el-tab-pane>
    </el-tabs>

    <el-dialog
      v-model="projectDialogVisible"
      :title="isNewProject ? '添加项目' : '编辑项目'"
      width="820px"
    >
      <el-form label-width="120px">
        <el-form-item label="项目名称">
          <el-input v-model="projectForm.name" placeholder="如 service/rest" />
        </el-form-item>
        <el-form-item label="本地产物目录">
          <el-input
            v-model="projectForm.localBinDir"
            placeholder="发布产物 bin 目录"
          >
            <template #append>
              <el-button @click="chooseLocalBinDir">选择</el-button>
            </template>
          </el-input>
        </el-form-item>
        <el-form-item label="服务器应用目录">
          <el-input
            v-model="projectForm.remoteAppDir"
            placeholder="如 D:\code\sites\to\service\rest（bin 的父目录）"
          />
        </el-form-item>
        <el-form-item label="健康检查地址">
          <el-input
            v-model="projectForm.healthCheckUrl"
            placeholder="服务器本机可访问的地址，如 http://localhost:8083/swagger，留空跳过"
          />
        </el-form-item>
        <el-form-item label="重试次数/间隔">
          <el-input-number
            v-model="projectForm.healthCheckRetries"
            :min="1"
            :max="60"
          />
          <span style="margin: 0 8px">次，每次间隔</span>
          <el-input-number
            v-model="projectForm.healthCheckDelaySecs"
            :min="1"
            :max="60"
          />
          <span style="margin-left: 8px">秒</span>
        </el-form-item>
        <el-form-item label="停止/启动脚本">
          <div class="script-toolbar">
            <el-button size="small" @click="applyServicePreset('iis')">填入 IIS 方案</el-button>
            <el-button size="small" @click="applyServicePreset('java')">填入 Java 方案</el-button>
            <el-button size="small" @click="clearServiceScripts">清空</el-button>
          </div>
          <div class="form-hint" style="margin: 0 0 8px">
            只停本项目关联的站点/程序池或 Windows 服务，不会停整个 IIS。Java 方案请把脚本里的服务名改成实际名称。占位符
            <code>{{ "{appDir}" }}</code>
            <code>{{ "{appBin}" }}</code>
            <code>{{ "{projectName}" }}</code>
            部署时自动替换。不要写 exit，以免跳过启动脚本。
          </div>
          <el-input
            v-model="projectForm.stopScript"
            type="textarea"
            :autosize="{ minRows: 8, maxRows: 16 }"
            placeholder="替换前停止脚本（PowerShell）"
            class="script-textarea"
          />
          <el-input
            v-model="projectForm.startScript"
            type="textarea"
            :autosize="{ minRows: 6, maxRows: 12 }"
            placeholder="替换后启动脚本（PowerShell）"
            class="script-textarea"
            style="margin-top: 8px"
          />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="projectDialogVisible = false">取消</el-button>
        <el-button type="primary" @click="saveProject">保存</el-button>
      </template>
    </el-dialog>
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
.form-hint {
  margin-left: 12px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.mode-description {
  width: 100%;
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-top: 4px;
}
.server-order {
  font-size: 13px;
  color: var(--el-text-color-regular);
}
.project-row {
  line-height: 2.2;
  padding: 6px 10px;
  margin-bottom: 6px;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  background: var(--app-bg, #0f1218);
}
.project-path {
  margin-left: 10px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.projects-header {
  display: flex;
  align-items: center;
  gap: 12px;
  margin: 16px 0 10px;
  padding: 10px 12px;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  background: var(--app-panel, #151a22);
}
.projects-header h3 {
  margin: 0;
}
.script-toolbar {
  display: flex;
  gap: 8px;
  margin-bottom: 8px;
}
.script-textarea :deep(textarea) {
  font-family: Consolas, "Courier New", monospace;
  font-size: 12px;
  line-height: 1.45;
}
</style>
