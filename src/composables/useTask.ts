import { onUnmounted, ref, type Ref } from "vue";
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

export type TaskFinalState = "" | "success" | "failed" | "cancelled";

export interface DeployTask {
  title: string;
  pageKey: string;
  logs: Ref<LogLine[]>;
  running: Ref<boolean>;
  percent: Ref<number>;
  step: Ref<string>;
  finalState: Ref<TaskFinalState>;
  dismissed: Ref<boolean>;
  detail?: Ref<string>;
  route?: Ref<string>;
  attach: (taskId: string) => Promise<void>;
  cancel: () => Promise<void>;
}

function createTaskState(title: string, pageKey: string): DeployTask {
  const logs = ref<LogLine[]>([]);
  const running = ref(false);
  const percent = ref(0);
  const step = ref("");
  const finalState = ref<TaskFinalState>("");
  const dismissed = ref(true);

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
        if (logs.value.length > 3000) logs.value.splice(0, 500);
      }),
      await listen<TaskStatePayload>("task-state", (event) => {
        if (event.payload.taskId !== currentTaskId) return;
        if (event.payload.state === "running") return;
        running.value = false;
        finalState.value = event.payload.state as TaskFinalState;
      }),
      await listen<TaskProgressPayload>("task-progress", (event) => {
        if (event.payload.taskId !== currentTaskId) return;
        percent.value = Math.min(100, Math.round(event.payload.percent));
        step.value = event.payload.step;
      })
    );
  }

  async function attach(taskId: string) {
    await setupListeners();
    currentTaskId = taskId;
    logs.value = [];
    percent.value = 0;
    step.value = "";
    finalState.value = "";
    dismissed.value = false;
    running.value = true;
  }

  async function cancel() {
    if (currentTaskId) await api.cancelTask(currentTaskId);
  }

  function dispose() {
    for (const unlisten of unlisteners) unlisten();
    unlisteners.length = 0;
  }

  return { title, pageKey, logs, running, percent, step, finalState, dismissed, attach, cancel, dispose };
}

// 部署任务的日志与进度跟踪（随页面卸载取消监听）
export function useTask() {
  const task = createTaskState("部署", "");
  onUnmounted(() => task.dispose());
  return task;
}

export const frontendDeployTask = createTaskState("前端部署", "frontend");
export const backendDeployTask = createTaskState("后端部署", "backend");

export const sharedDeployTasks = [frontendDeployTask, backendDeployTask];
