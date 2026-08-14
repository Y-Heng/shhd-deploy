<script setup lang="ts">
import { nextTick, ref, watch } from "vue";
import type { LogLine } from "../composables/useTask";

const props = defineProps<{
  logs: LogLine[];
  running: boolean;
  percent: number;
  step: string;
  finalState: string;
}>();

const emit = defineEmits<{ (event: "cancel"): void }>();

const logContainer = ref<HTMLElement | null>(null);

// 新日志到达自动滚动到底部
watch(
  () => props.logs.length,
  async () => {
    await nextTick();
    if (logContainer.value)
      logContainer.value.scrollTop = logContainer.value.scrollHeight;
  }
);

function levelColor(level: string): string {
  if (level === "error") return "#f56c6c";
  if (level === "warn") return "#e6a23c";
  if (level === "success") return "#67c23a";
  return "#a3a6ad";
}
</script>

<template>
  <div class="task-panel">
    <div class="task-panel-header">
      <el-progress
        :percentage="percent"
        :stroke-width="16"
        :status="
          finalState === 'failed'
            ? 'exception'
            : finalState === 'success'
              ? 'success'
              : undefined
        "
        style="flex: 1"
      />
      <span class="task-percent">{{ percent }}%</span>
      <el-button
        v-if="running"
        size="small"
        type="danger"
        plain
        @click="emit('cancel')"
      >
        取消
      </el-button>
    </div>
    <div v-if="step" class="task-step">{{ running ? '正在进行：' : '最近步骤：' }}{{ step }}</div>
    <div ref="logContainer" class="task-logs">
      <div v-for="(line, index) in logs" :key="index" class="log-line">
        <span class="log-ts">{{ line.ts }}</span>
        <span :style="{ color: levelColor(line.level) }">{{
          line.message
        }}</span>
      </div>
      <div v-if="logs.length === 0" class="log-empty">暂无日志</div>
    </div>
  </div>
</template>

<style scoped>
.task-panel {
  margin-top: 16px;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  padding: 12px;
  background: var(--app-panel, #151a22);
}
.task-panel-header {
  display: flex;
  align-items: center;
  gap: 12px;
}
.task-percent {
  min-width: 48px;
  text-align: right;
  font-variant-numeric: tabular-nums;
  font-weight: 600;
  color: var(--app-text, #e8eaed);
}
.task-step {
  margin-top: 8px;
  font-size: 13px;
  color: var(--app-text, #e8eaed);
  line-height: 1.5;
}
.task-logs {
  margin-top: 8px;
  height: 320px;
  overflow-y: auto;
  font-family: Consolas, "Courier New", monospace;
  font-size: 12px;
  line-height: 1.7;
  background: var(--app-bg, #0f1218);
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  padding: 8px 12px;
}
.log-ts {
  color: var(--app-muted, #8b95a8);
  margin-right: 8px;
}
.log-empty {
  color: var(--app-muted, #8b95a8);
  text-align: center;
  padding-top: 40px;
}
.log-line {
  white-space: pre-wrap;
  word-break: break-all;
}
</style>
