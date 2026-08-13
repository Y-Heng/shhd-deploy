<script setup lang="ts">
import { onMounted, reactive, ref } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { api } from "../api";
import { useTask } from "../composables/useTask";
import TaskLogPanel from "../components/TaskLogPanel.vue";
import type { AppConfig, DeployMode, FrontendTarget } from "../types";

const config = ref<AppConfig | null>(null);
const selectedTargetIds = ref<string[]>([]);
const dialogVisible = ref(false);
const isNewTarget = ref(false);
const deployMode = ref<DeployMode>("full");
const backupSibling = ref(true);

// 使用本地时间生成 yyyyMMdd 后缀
const now = new Date();
const dateSuffix = `${now.getFullYear()}${String(now.getMonth() + 1).padStart(2, "0")}${String(now.getDate()).padStart(2, "0")}`;

const modeDescriptions: Record<DeployMode, string> = {
  full: "本地产物与服务器目录逐文件对比，只上传有变化的文件，直接生效",
  stage: "上传到服务器的「目录名-staging」中转目录，不动线上，稍后再替换",
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
});

const task = useTask();

onMounted(async () => {
  config.value = await api.getConfig();
});

function serverNames(serverIds: string[]): string {
  if (!config.value) return "";
  return serverIds
    .map(
      (serverId) =>
        config.value?.servers.find((server) => server.id === serverId)?.name ??
        serverId
    )
    .join("、");
}

async function deploySelected() {
  if (task.running.value) return;
  if (selectedTargetIds.value.length === 0) {
    ElMessage.warning("请先勾选要部署的前端项目");
    return;
  }
  const names = config.value?.frontendTargets
    .filter((target) => selectedTargetIds.value.includes(target.id))
    .map((target) => target.name)
    .join("、");
  const modeLabel =
    deployMode.value === "full"
      ? "直接替换"
      : deployMode.value === "stage"
        ? "仅上传到中转"
        : "从中转替换";
  const backupText =
    deployMode.value !== "stage" && backupSibling.value
      ? `\n附加备份：目录 → 目录名-${dateSuffix}`
      : "";
  await ElMessageBox.confirm(
    `方式：${modeLabel}${backupText}\n项目：${names}\n\n确认执行？`,
    "部署确认",
    { type: "warning", confirmButtonText: "执行" }
  );
  try {
    const taskId = await api.startFrontendDeploy(selectedTargetIds.value, {
      mode: deployMode.value,
      backupSibling: deployMode.value !== "stage" && backupSibling.value,
    });
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
  });
  dialogVisible.value = true;
}

function openEditDialog(target: FrontendTarget) {
  isNewTarget.value = false;
  Object.assign(editForm, JSON.parse(JSON.stringify(target)));
  if (editForm.stagingDir == null) editForm.stagingDir = "";
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
  if (isNewTarget.value) {
    config.value.frontendTargets.push(clone);
  } else {
    const index = config.value.frontendTargets.findIndex(
      (item) => item.id === clone.id
    );
    if (index >= 0) config.value.frontendTargets.splice(index, 1, clone);
  }
  await api.saveConfig(config.value);
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
</script>

<template>
  <div v-if="config">
    <div class="view-header">
      <h2>前端部署</h2>
      <div>
        <el-button
          type="primary"
          :disabled="task.running.value"
          @click="deploySelected"
        >
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
    </div>
    <div class="deploy-options">
      <el-radio-group v-model="deployMode">
        <el-radio-button value="full">直接替换</el-radio-button>
        <el-radio-button value="stage">仅上传到中转</el-radio-button>
        <el-radio-button value="replace">从中转替换</el-radio-button>
      </el-radio-group>
      <el-checkbox v-if="deployMode !== 'stage'" v-model="backupSibling">
        替换前备份为 目录名-{{ dateSuffix }}（当天已备份则跳过）
      </el-checkbox>
    </div>
    <el-alert
      :title="modeDescriptions[deployMode]"
      type="info"
      :closable="false"
      style="margin-bottom: 12px"
    />
    <el-checkbox-group v-model="selectedTargetIds" style="width: 100%">
      <el-table :data="config.frontendTargets" stripe>
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

    <TaskLogPanel
      :logs="task.logs.value"
      :running="task.running.value"
      :percent="task.percent.value"
      :step="task.step.value"
      :final-state="task.finalState.value"
      @cancel="task.cancel"
    />

    <el-dialog
      v-model="dialogVisible"
      :title="isNewTarget ? '添加前端项目' : '编辑前端项目'"
      width="620px"
    >
      <el-form label-width="110px">
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
            placeholder="如 /usr/share/nginx/html/to/brand"
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
              :label="server.name"
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
  gap: 20px;
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
</style>
