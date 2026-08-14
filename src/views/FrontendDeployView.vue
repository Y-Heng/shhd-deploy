<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { api } from "../api";
import { useTask } from "../composables/useTask";
import TaskLogPanel from "../components/TaskLogPanel.vue";
import type {
  AppConfig,
  DeployMode,
  FrontendReleaseRecord,
  FrontendTarget,
} from "../types";

const PRESET_GROUPS = ["开发环境", "正式环境"];

const config = ref<AppConfig | null>(null);
const selectedTargetIds = ref<string[]>([]);
const selectedGroup = ref("开发环境");
const activeTab = ref("deploy");
const dialogVisible = ref(false);
const isNewTarget = ref(false);
const deployMode = ref<DeployMode>("full");
const backupSibling = ref(true);
const releases = ref<FrontendReleaseRecord[]>([]);

const modeDescriptions: Record<DeployMode, string> = {
  full: "本地产物打包后一次上传到服务器再解压覆盖，直接生效",
  stage: "打包上传到服务器的「目录名-staging」中转目录，不动线上，稍后再替换",
  replace: "把中转目录内容替换到线上目录（不重新上传，服务器本地复制，秒级）",
};

const editForm = reactive<FrontendTarget>({
  id: "",
  name: "",
  serverIds: [],
  localDir: "",
  remoteDir: "",
  stagingDir: "",
  deleteExtraneous: false,
  group: "开发环境",
});

const task = useTask();

const groupOptions = computed(() => {
  const names = new Set(PRESET_GROUPS);
  for (const target of config.value?.frontendTargets ?? []) names.add(target.group || "未分组");
  return Array.from(names);
});

const visibleTargets = computed(() =>
  (config.value?.frontendTargets ?? []).filter(
    (target) => (target.group || "未分组") === selectedGroup.value
  )
);

onMounted(async () => {
  config.value = await api.getConfig();
  await refreshReleases();
  const groups = groupOptions.value;
  if (!groups.includes(selectedGroup.value) && groups.length > 0) selectedGroup.value = groups[0];
});

watch(selectedGroup, () => {
  selectedTargetIds.value = [];
});

watch(
  () => task.finalState.value,
  (state) => {
    if (state === "success" || state === "failed") refreshReleases();
  }
);

function serverNames(serverIds: string[]): string {
  if (!config.value) return "";
  return serverIds
    .map(
      (serverId) =>
        config.value?.servers.find((server) => server.id === serverId)?.name ?? serverId
    )
    .join("、");
}

function groupTagType(groupName: string): "success" | "danger" | "info" | "warning" {
  if (groupName === "正式环境") return "danger";
  if (groupName === "开发环境") return "success";
  if (groupName === "未分组") return "info";
  return "warning";
}

async function refreshReleases() {
  releases.value = await api.getFrontendReleases();
}

async function deploySelected() {
  if (task.running.value) return;
  const targets = visibleTargets.value.filter((target) =>
    selectedTargetIds.value.includes(target.id)
  );
  if (targets.length === 0) {
    ElMessage.warning("请先勾选当前环境下要部署的前端项目");
    return;
  }
  const names = targets.map((target) => target.name).join("、");
  const modeLabel =
    deployMode.value === "full"
      ? "直接替换"
      : deployMode.value === "stage"
        ? "仅上传到中转"
        : "从中转替换";
  const backupText =
    deployMode.value !== "stage" && backupSibling.value
      ? "\n发布前备份：开启（可从发布历史回滚）"
      : deployMode.value !== "stage"
        ? "\n发布前备份：关闭（本次无法回滚）"
        : "";
  const isProd = selectedGroup.value.includes("正式");
  await ElMessageBox.confirm(
    `环境：${selectedGroup.value}\n方式：${modeLabel}${backupText}\n项目：${names}\n\n确认执行？`,
    isProd ? "正式环境确认" : "部署确认",
    {
      type: isProd ? "error" : "warning",
      confirmButtonText: isProd ? "确认发布到正式环境" : "执行",
    }
  );
  try {
    const taskId = await api.startFrontendDeploy(
      targets.map((target) => target.id),
      {
        mode: deployMode.value,
        backupSibling: deployMode.value !== "stage" && backupSibling.value,
      }
    );
    activeTab.value = "deploy";
    await task.attach(taskId);
  } catch (error) {
    ElMessage.error(String(error));
  }
}

async function deployOne(target: FrontendTarget) {
  selectedTargetIds.value = [target.id];
  await deploySelected();
}

function openAddDialog() {
  isNewTarget.value = true;
  Object.assign(editForm, {
    id: `frontend-${Date.now()}`,
    name: "",
    serverIds: [],
    localDir: "",
    remoteDir: "",
    stagingDir: "",
    deleteExtraneous: false,
    group: selectedGroup.value === "未分组" ? "开发环境" : selectedGroup.value,
  });
  dialogVisible.value = true;
}

function openEditDialog(target: FrontendTarget) {
  isNewTarget.value = false;
  Object.assign(editForm, JSON.parse(JSON.stringify(target)));
  if (editForm.stagingDir == null) editForm.stagingDir = "";
  if (!editForm.group) editForm.group = "未分组";
  dialogVisible.value = true;
}

async function chooseLocalDir() {
  const selected = await openDialog({ directory: true });
  if (typeof selected === "string") editForm.localDir = selected;
}

async function saveTarget() {
  if (!config.value) return;
  if (
    !editForm.name ||
    !editForm.localDir ||
    !editForm.remoteDir ||
    editForm.serverIds.length === 0
  ) {
    ElMessage.warning("请完整填写部署目标信息");
    return;
  }
  const clone: FrontendTarget = JSON.parse(JSON.stringify(editForm));
  if (!clone.stagingDir || !clone.stagingDir.trim()) clone.stagingDir = null;
  clone.group = clone.group?.trim() || "开发环境";
  if (isNewTarget.value) {
    config.value.frontendTargets.push(clone);
  } else {
    const index = config.value.frontendTargets.findIndex((item) => item.id === clone.id);
    if (index >= 0) config.value.frontendTargets.splice(index, 1, clone);
  }
  await api.saveConfig(config.value);
  selectedGroup.value = clone.group || "开发环境";
  dialogVisible.value = false;
  ElMessage.success("已保存");
}

async function removeTarget(target: FrontendTarget) {
  if (!config.value) return;
  await ElMessageBox.confirm(`确认删除 ${target.name}？`, "删除确认", {
    type: "warning",
  });
  config.value.frontendTargets = config.value.frontendTargets.filter(
    (item) => item.id !== target.id
  );
  await api.saveConfig(config.value);
  ElMessage.success("已删除");
}

function releaseStatusMeta(status: string): { label: string; type: "success" | "danger" | "info" | "warning" } {
  if (status === "success") return { label: "成功", type: "success" };
  if (status === "staged") return { label: "待替换", type: "warning" };
  if (status === "failed") return { label: "失败", type: "danger" };
  if (status === "rolled_back") return { label: "已回滚", type: "info" };
  if (status === "rollback") return { label: "回滚完成", type: "info" };
  return { label: status, type: "info" };
}

function canRollback(record: FrontendReleaseRecord): boolean {
  return !!record.backupSuffix && (record.status === "success" || record.status === "failed");
}

async function startRollback(record: FrontendReleaseRecord) {
  if (task.running.value) return;
  const isProd = record.groupName.includes("正式");
  await ElMessageBox.confirm(
    `将把「${record.targetNames.join("、")}」恢复到本次发布前的备份。\n环境：${record.groupName}\n\n确认回滚？`,
    isProd ? "正式环境回滚确认" : "回滚确认",
    {
      type: isProd ? "error" : "warning",
      confirmButtonText: "确认回滚",
    }
  );
  try {
    const taskId = await api.startFrontendRollback(record.id);
    activeTab.value = "deploy";
    await task.attach(taskId);
  } catch (error) {
    ElMessage.error(String(error));
  }
}
</script>

<template>
  <div v-if="config">
    <div class="view-header">
      <h2>前端部署</h2>
      <el-select v-model="selectedGroup" style="width: 220px">
        <el-option v-for="groupName in groupOptions" :key="groupName" :label="groupName" :value="groupName" />
      </el-select>
    </div>
    <el-alert
      :title="`当前环境：${selectedGroup}。开发和正式请分开配置，避免发错。`"
      :type="selectedGroup.includes('正式') ? 'error' : 'success'"
      :closable="false"
      style="margin-bottom: 12px"
    />
    <el-tabs v-model="activeTab">
      <el-tab-pane label="部署" name="deploy">
        <div class="deploy-options">
          <el-radio-group v-model="deployMode">
            <el-radio-button value="full">直接替换</el-radio-button>
            <el-radio-button value="stage">仅上传到中转</el-radio-button>
            <el-radio-button value="replace">从中转替换</el-radio-button>
          </el-radio-group>
          <el-checkbox v-if="deployMode !== 'stage'" v-model="backupSibling">
            替换前备份（用于回滚）
          </el-checkbox>
          <el-button type="primary" :disabled="task.running.value" @click="deploySelected">
            {{
              deployMode === "stage"
                ? "上传选中到中转"
                : deployMode === "replace"
                  ? "替换选中"
                  : "部署选中"
            }}
          </el-button>
          <el-button @click="openAddDialog">添加项目</el-button>
        </div>
        <el-alert
          :title="modeDescriptions[deployMode]"
          type="info"
          :closable="false"
          style="margin-bottom: 12px"
        />
        <el-checkbox-group v-model="selectedTargetIds" style="width: 100%">
          <el-table :data="visibleTargets" stripe>
            <el-table-column width="50">
              <template #default="{ row }">
                <el-checkbox :value="row.id"><span /></el-checkbox>
              </template>
            </el-table-column>
            <el-table-column prop="name" label="项目" min-width="160" />
            <el-table-column prop="localDir" label="本地目录" min-width="260" show-overflow-tooltip />
            <el-table-column prop="remoteDir" label="服务器目录" min-width="220" show-overflow-tooltip />
            <el-table-column label="服务器" min-width="160">
              <template #default="{ row }">{{ serverNames(row.serverIds) }}</template>
            </el-table-column>
            <el-table-column label="清理多余文件" width="110">
              <template #default="{ row }">
                <el-tag :type="row.deleteExtraneous ? 'warning' : 'info'" effect="plain">
                  {{ row.deleteExtraneous ? "是" : "否" }}
                </el-tag>
              </template>
            </el-table-column>
            <el-table-column label="操作" width="200" fixed="right">
              <template #default="{ row }">
                <el-button
                  size="small"
                  type="primary"
                  plain
                  :disabled="task.running.value"
                  @click="deployOne(row)"
                >
                  部署
                </el-button>
                <el-button size="small" @click="openEditDialog(row)">编辑</el-button>
                <el-button size="small" type="danger" text @click="removeTarget(row)">
                  删除
                </el-button>
              </template>
            </el-table-column>
          </el-table>
        </el-checkbox-group>
        <div v-if="visibleTargets.length === 0" class="empty-hint">
          当前环境还没有项目，点击「添加项目」并把分组设为 {{ selectedGroup }}
        </div>
        <TaskLogPanel
          :logs="task.logs.value"
          :running="task.running.value"
          :percent="task.percent.value"
          :step="task.step.value"
          :final-state="task.finalState.value"
          @cancel="task.cancel"
        />
      </el-tab-pane>

      <el-tab-pane label="发布历史" name="history">
        <div style="margin-bottom: 10px">
          <el-button size="small" @click="refreshReleases">刷新</el-button>
        </div>
        <el-table :data="releases" stripe>
          <el-table-column prop="createdAt" label="时间" width="170" />
          <el-table-column label="环境" width="120">
            <template #default="{ row }">
              <el-tag :type="groupTagType(row.groupName)" effect="plain">{{ row.groupName }}</el-tag>
            </template>
          </el-table-column>
          <el-table-column prop="mode" label="方式" width="120" />
          <el-table-column label="项目" min-width="180">
            <template #default="{ row }">{{ row.targetNames.join("、") }}</template>
          </el-table-column>
          <el-table-column label="服务器" min-width="160">
            <template #default="{ row }">{{ row.serverNames.join("、") }}</template>
          </el-table-column>
          <el-table-column label="状态" width="90">
            <template #default="{ row }">
              <el-tag :type="releaseStatusMeta(row.status).type">
                {{ releaseStatusMeta(row.status).label }}
              </el-tag>
            </template>
          </el-table-column>
          <el-table-column prop="message" label="说明" min-width="220" show-overflow-tooltip />
          <el-table-column label="操作" width="100" fixed="right">
            <template #default="{ row }">
              <el-button
                v-if="canRollback(row)"
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
    </el-tabs>

    <el-dialog
      v-model="dialogVisible"
      :title="isNewTarget ? '添加前端项目' : '编辑前端项目'"
      width="620px"
    >
      <el-form label-width="110px">
        <el-form-item label="环境分组">
          <el-select
            v-model="editForm.group"
            filterable
            allow-create
            default-first-option
            placeholder="开发环境 / 正式环境"
            style="width: 100%"
          >
            <el-option v-for="groupName in groupOptions" :key="groupName" :label="groupName" :value="groupName" />
          </el-select>
        </el-form-item>
        <el-form-item label="名称">
          <el-input v-model="editForm.name" placeholder="如 商户后台 Mch-Web" />
        </el-form-item>
        <el-form-item label="本地目录">
          <el-input v-model="editForm.localDir" placeholder="构建产物目录（dist）">
            <template #append>
              <el-button @click="chooseLocalDir">选择</el-button>
            </template>
          </el-input>
        </el-form-item>
        <el-form-item label="服务器目录">
          <el-input
            v-model="editForm.remoteDir"
            placeholder="Linux: /usr/share/nginx/html/xxx  Windows: C:\code\sites\xxx"
          />
        </el-form-item>
        <el-form-item label="中转目录">
          <el-input
            v-model="editForm.stagingDir"
            placeholder="留空默认 服务器目录-staging，可自定义到任意位置"
          />
        </el-form-item>
        <el-form-item label="部署服务器">
          <el-select v-model="editForm.serverIds" multiple style="width: 100%">
            <el-option
              v-for="server in config.servers"
              :key="server.id"
              :label="`${server.name}（${server.os === 'windows' ? 'Windows' : 'Linux'}）`"
              :value="server.id"
            />
          </el-select>
        </el-form-item>
        <el-form-item label="清理多余文件">
          <el-switch v-model="editForm.deleteExtraneous" />
          <span class="form-hint">删除服务器上本地没有的文件（谨慎开启）</span>
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="dialogVisible = false">取消</el-button>
        <el-button type="primary" @click="saveTarget">保存</el-button>
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
.deploy-options {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 12px 20px;
  margin-bottom: 10px;
  padding: 12px 14px;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  background: var(--app-panel, #151a22);
}
.form-hint {
  margin-left: 10px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.empty-hint {
  margin: 16px 0;
  font-size: 13px;
  color: var(--el-text-color-secondary);
}
</style>
