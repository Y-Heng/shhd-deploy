import { onUnmounted, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { api } from "../api";
import type {
  TaskLogPayload,
  TaskProgressPayload,
  TaskStatePayload,
} from "../types";

export interface LogLine {
  level: string;
  message: string;
  ts: string;
}

// 部署任务的日志与进度跟踪
export function useTask() {
  const logs = ref<LogLine[]>([]);
  const running = ref(false);
  const percent = ref(0);
  const step = ref("");
  const finalState = ref<"" | "success" | "failed" | "cancelled">("");

  let currentTaskId = "";
  const unlisteners: UnlistenFn[] = [];

  async function setupListeners() {
    if (unlisteners.length > 0) return;
    unlisteners.push(
      await listen<TaskLogPayload>("task-log", (event) => {
        if (event.payload.taskId !== currentTaskId) return;
        logs.value.push({
          level: event.payload.level,
          message: event.payload.message,
          ts: event.payload.ts,
        });
        // 防止日志无限增长
        if (logs.value.length > 3000) logs.value.splice(0, 500);
      }),
      await listen<TaskStatePayload>("task-state", (event) => {
        if (event.payload.taskId !== currentTaskId) return;
        if (event.payload.state === "running") return;
        running.value = false;
        finalState.value = event.payload.state;
      }),
      await listen<TaskProgressPayload>("task-progress", (event) => {
        if (event.payload.taskId !== currentTaskId) return;
        percent.value = Math.min(100, Math.round(event.payload.percent));
        step.value = event.payload.step;
      })
    );
  }

  // 绑定新任务并开始收集其日志
  async function attach(taskId: string) {
    await setupListeners();
    currentTaskId = taskId;
    logs.value = [];
    percent.value = 0;
    step.value = "";
    finalState.value = "";
    running.value = true;
  }

  async function cancel() {
    if (currentTaskId) await api.cancelTask(currentTaskId);
  }

  onUnmounted(() => {
    for (const unlisten of unlisteners) unlisten();
  });

  return { logs, running, percent, step, finalState, attach, cancel };
}
