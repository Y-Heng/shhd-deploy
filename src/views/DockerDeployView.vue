<script setup lang="ts">
import { computed, nextTick, onMounted, reactive, ref } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import { ArrowRight, Folder } from "@element-plus/icons-vue";
import { api } from "../api";
import { useTask } from "../composables/useTask";
import TaskLogPanel from "../components/TaskLogPanel.vue";
import GripDots from "../components/GripDots.vue";
import {
  bindPointerDrag,
  dropPlaceByY,
  elementFromPointIgnoringDrag,
  moveGroupedItem,
  reorderGroups,
} from "../composables/groupedDragSort";
import type { AppConfig, DockerTarget } from "../types";

const config = ref<AppConfig | null>(null);
const dialogVisible = ref(false);
const isNewTarget = ref(false);
const commandsText = ref("");
const expandedGroups = ref<Set<string>>(new Set());
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

const editForm = reactive<DockerTarget>({
  id: "",
  name: "",
  serverId: "",
  workDir: "",
  commands: [],
  group: null,
});

const task = useTask();

// 按分组归类 Docker 目标
const groupedTargets = computed(() => {
  const groups = new Map<string, DockerTarget[]>();
  for (const target of config.value?.dockerTargets ?? []) {
    const groupName = target.group || "未分组";
    if (!groups.has(groupName)) groups.set(groupName, []);
    groups.get(groupName)!.push(target);
  }
  return groups;
});

// 已存在的分组名
const existingGroups = computed(() => {
  const names = new Set<string>();
  for (const target of config.value?.dockerTargets ?? [])
    if (target.group) names.add(target.group);
  return Array.from(names);
});

onMounted(async () => {
  config.value = await api.getConfig();
  expandedGroups.value = new Set(groupedTargets.value.keys());
});

function serverNameById(serverId: string): string {
  const server = config.value?.servers.find((item) => item.id === serverId);
  return server ? server.name : serverId;
}

function isGroupExpanded(groupName: string) {
  return expandedGroups.value.has(groupName);
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
  for (const target of config.value.dockerTargets)
    if ((target.group || "未分组") === oldName) target.group = newName;
  await api.saveConfig(config.value);
  const next = new Set(expandedGroups.value);
  if (next.has(oldName)) {
    next.delete(oldName);
    next.add(newName);
  }
  expandedGroups.value = next;
  ElMessage.success("分组已重命名");
}

async function runTarget(target: DockerTarget) {
  if (task.running.value) return;
  await ElMessageBox.confirm(
    `将在 ${serverNameById(target.serverId)} 的 ${target.workDir} 依次执行：\n${target.commands.join(
      "\n",
    )}\n\n确认执行？`,
    "执行确认",
    { type: "warning", confirmButtonText: "执行" },
  );
  try {
    const taskId = await api.startDockerDeploy(target.id);
    await task.attach(taskId);
  } catch (error) {
    ElMessage.error(String(error));
  }
}

function openAddDialog() {
  isNewTarget.value = true;
  Object.assign(editForm, {
    id: `docker-${Date.now()}`,
    name: "",
    serverId: config.value?.servers[0]?.id ?? "",
    workDir: "",
    commands: [],
    group: null,
  });
  commandsText.value = "docker compose pull\ndocker compose up -d\ndocker compose ps";
  dialogVisible.value = true;
}

function openEditDialog(target: DockerTarget) {
  isNewTarget.value = false;
  Object.assign(editForm, JSON.parse(JSON.stringify(target)));
  if (editForm.group === undefined) editForm.group = null;
  commandsText.value = target.commands.join("\n");
  dialogVisible.value = true;
}

async function saveTarget() {
  if (!config.value) return;
  editForm.commands = commandsText.value
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  if (!editForm.name || !editForm.workDir || editForm.commands.length === 0) {
    ElMessage.warning("请完整填写信息");
    return;
  }
  const clone: DockerTarget = JSON.parse(JSON.stringify(editForm));
  if (!clone.group) clone.group = null;
  if (isNewTarget.value) {
    config.value.dockerTargets.push(clone);
  } else {
    const index = config.value.dockerTargets.findIndex((item) => item.id === clone.id);
    if (index >= 0) config.value.dockerTargets.splice(index, 1, clone);
  }
  await api.saveConfig(config.value);
  const groupName = clone.group || "未分组";
  if (!expandedGroups.value.has(groupName)) {
    const next = new Set(expandedGroups.value);
    next.add(groupName);
    expandedGroups.value = next;
  }
  dialogVisible.value = false;
  ElMessage.success("已保存");
}

async function removeTarget(target: DockerTarget) {
  if (!config.value) return;
  await ElMessageBox.confirm(`确认删除 ${target.name}？`, "删除确认", {
    type: "warning",
  });
  config.value.dockerTargets = config.value.dockerTargets.filter(
    (item) => item.id !== target.id,
  );
  await api.saveConfig(config.value);
  ElMessage.success("已删除");
}

async function persistTargets(next: DockerTarget[]) {
  if (!config.value) return;
  config.value.dockerTargets = next;
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
  if (draggingGroup.value) {
    const groupElement = hit instanceof Element ? hit.closest(".docker-group") : null;
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
  const card = hit instanceof Element ? hit.closest(".docker-card-wrap") : null;
  if (card instanceof HTMLElement && card.dataset.itemId) {
    const itemId = card.dataset.itemId;
    const groupName = card.dataset.groupName || "";
    if (itemId === draggingItemId.value) {
      dropHint.value = null;
      return;
    }
    dropHint.value = { groupName, itemId, place: dropPlaceByY(clientY, card) };
    return;
  }
  const groupElement = hit instanceof Element ? hit.closest(".docker-group") : null;
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
    await persistTargets(
      reorderGroups(config.value.dockerTargets, fromGroup, hint.groupName, hint.place),
    );
    return;
  }
  if (fromItemId)
    await persistTargets(
      moveGroupedItem(
        config.value.dockerTargets,
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
  <div v-if="config">
    <div class="view-header">
      <h2>Docker 部署</h2>
      <el-button type="primary" @click="openAddDialog">添加目标</el-button>
    </div>

    <div
      v-for="[groupName, targets] in groupedTargets"
      :key="groupName"
      class="docker-group"
      :data-group-name="groupName"
      :class="{
        'is-dragging': draggingGroup === groupName,
        'is-drop-before': isDropHint(groupName, undefined, 'before'),
        'is-drop-after': isDropHint(groupName, undefined, 'after'),
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
        <span class="group-count">{{ targets.length }}</span>
        <span
          class="drag-grip"
          title="拖动分组排序"
          @click.stop
          @pointerdown.stop="onGroupGripDown(groupName, $event)"
        >
          <GripDots />
        </span>
      </div>
      <div v-show="isGroupExpanded(groupName)" class="group-body">
        <el-row :gutter="16">
          <el-col v-for="target in targets" :key="target.id" :span="12">
            <div
              class="docker-card-wrap"
              :data-group-name="groupName"
              :data-item-id="target.id"
              :class="{
                'is-dragging': draggingItemId === target.id,
                'is-drop-before': isDropHint(groupName, target.id, 'before'),
                'is-drop-after': isDropHint(groupName, target.id, 'after'),
              }"
            >
              <el-card shadow="hover" class="docker-card">
              <div class="docker-title">
                <b>{{ target.name }}</b>
                <el-tag size="small" effect="plain" style="margin-left: 8px">
                  {{ serverNameById(target.serverId) }}
                </el-tag>
                <span
                  class="drag-grip"
                  title="拖动目标排序"
                  @click.stop
                  @pointerdown.stop="onItemGripDown(target.id, $event)"
                >
                  <GripDots />
                </span>
              </div>
              <div class="docker-info">
                <div>工作目录：{{ target.workDir }}</div>
                <div class="docker-commands">
                  <div v-for="(command, index) in target.commands" :key="index">
                    $ {{ command }}
                  </div>
                </div>
              </div>
              <div class="docker-actions">
                <el-button
                  type="primary"
                  size="small"
                  :disabled="task.running.value"
                  @click="runTarget(target)"
                >
                  执行
                </el-button>
                <el-button size="small" text @click="openEditDialog(target)">编辑</el-button>
                <el-button size="small" text type="danger" @click="removeTarget(target)">
                  删除
                </el-button>
              </div>
            </el-card>
            </div>
          </el-col>
        </el-row>
      </div>
    </div>

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
      :title="isNewTarget ? '添加 Docker 目标' : '编辑 Docker 目标'"
      width="560px"
    >
      <el-form label-width="90px">
        <el-form-item label="名称">
          <el-input v-model="editForm.name" placeholder="如 zx-infra 集群" />
        </el-form-item>
        <el-form-item label="分组">
          <el-select
            v-model="editForm.group"
            filterable
            allow-create
            clearable
            default-first-option
            placeholder="选择或输入新分组"
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
        <el-form-item label="服务器">
          <el-select v-model="editForm.serverId" style="width: 100%">
            <el-option
              v-for="server in config.servers"
              :key="server.id"
              :label="server.name"
              :value="server.id"
            />
          </el-select>
        </el-form-item>
        <el-form-item label="工作目录">
          <el-input v-model="editForm.workDir" placeholder="如 /opt/zx" />
        </el-form-item>
        <el-form-item label="执行命令">
          <el-input
            v-model="commandsText"
            type="textarea"
            :rows="5"
            placeholder="每行一条命令，按顺序执行"
          />
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
.docker-group {
  margin-bottom: 14px;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius-lg, 10px);
  background: var(--app-bg, #0f1218);
  overflow: hidden;
}
.docker-group.is-dragging,
.docker-card-wrap.is-dragging {
  opacity: 0.55;
}
.docker-group.is-drop-before,
.docker-card-wrap.is-drop-before {
  box-shadow: inset 0 2px 0 #3dd68c;
}
.docker-group.is-drop-after,
.docker-card-wrap.is-drop-after {
  box-shadow: inset 0 -2px 0 #3dd68c;
}
.docker-group.is-drop-into {
  outline: 1px dashed #3dd68c;
  outline-offset: -2px;
}
.docker-card-wrap {
  display: block;
  margin-bottom: 12px;
}
.docker-card-wrap .docker-card {
  min-width: 0;
  margin-bottom: 0;
}
.docker-title {
  display: flex;
  align-items: center;
  gap: 8px;
}
.docker-title .drag-grip {
  flex-shrink: 0;
  margin-left: auto;
}
.group-header {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  padding: 10px 14px;
  border-bottom: none;
  background: var(--app-panel, #151a22);
  color: var(--el-text-color-secondary);
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
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
.group-body {
  padding: 12px;
  border-top: 1px solid var(--app-border, #2a3344);
}
.docker-card {
  margin-bottom: 0;
}
.docker-info {
  margin-top: 8px;
  font-size: 13px;
  color: var(--el-text-color-regular);
}
.docker-commands {
  margin-top: 6px;
  padding: 8px 10px;
  background: var(--app-bg, #0f1218);
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  font-family: Consolas, monospace;
  font-size: 12px;
  line-height: 1.8;
}
.docker-actions {
  margin-top: 10px;
}
</style>
