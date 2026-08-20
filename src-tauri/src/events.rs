//! 任务日志与进度：推到前端事件，同时写入内存注册表供 MCP 查询。

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};

/// 任务快照：供界面之外的调用方（如 MCP）查询任务进度与日志
#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    pub state: String,
    pub message: String,
    pub percent: f64,
    pub step: String,
    pub logs: Vec<String>,
}

/// 任务注册表：记录所有任务的状态与日志（内存中保留最近 50 个）
#[derive(Default)]
pub struct TaskRegistry {
    inner: Mutex<(HashMap<String, TaskSnapshot>, Vec<String>)>,
}

impl TaskRegistry {
    /// 登记一个 running 任务，超出 50 个时丢掉最旧的
    pub fn init(&self, task_id: &str) {
        let mut guard = self.inner.lock().unwrap();
        let (map, order) = &mut *guard;
        map.insert(
            task_id.to_string(),
            TaskSnapshot {
                state: "running".into(),
                ..Default::default()
            },
        );
        order.push(task_id.to_string());
        // 只保留最近 50 个任务
        while order.len() > 50 {
            let oldest = order.remove(0);
            map.remove(&oldest);
        }
    }

    fn update(&self, task_id: &str, apply: impl FnOnce(&mut TaskSnapshot)) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(snapshot) = guard.0.get_mut(task_id) {
            apply(snapshot);
        }
    }

    /// 追加一行带时间戳的任务日志，单任务最多保留约 500 行
    pub fn append_log(&self, task_id: &str, level: &str, message: &str) {
        let line = format!(
            "[{}][{}] {}",
            chrono::Local::now().format("%H:%M:%S"),
            level,
            message
        );
        self.update(task_id, |snapshot| {
            snapshot.logs.push(line);
            // 防止日志无限增长
            if snapshot.logs.len() > 500 {
                snapshot.logs.drain(..100);
            }
        });
    }

    /// 写入任务终态或中间状态
    pub fn set_state(&self, task_id: &str, state: &str, message: &str) {
        let state_owned = state.to_string();
        let message_owned = message.to_string();
        self.update(task_id, |snapshot| {
            snapshot.state = state_owned;
            snapshot.message = message_owned;
        });
    }

    /// 写入进度百分比与当前步骤文案
    pub fn set_progress(&self, task_id: &str, percent: f64, step: &str) {
        let step_owned = step.to_string();
        self.update(task_id, |snapshot| {
            snapshot.percent = percent;
            snapshot.step = step_owned;
        });
    }

    /// 复制一份当前快照；任务不存在则 None
    pub fn snapshot(&self, task_id: &str) -> Option<TaskSnapshot> {
        self.inner.lock().unwrap().0.get(task_id).cloned()
    }
}

/// 任务日志事件负载
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskLogPayload {
    pub task_id: String,
    pub level: String,
    pub message: String,
    pub ts: String,
}

/// 任务状态事件负载
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatePayload {
    pub task_id: String,
    pub state: String,
    pub message: String,
}

/// 任务进度事件负载
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgressPayload {
    pub task_id: String,
    pub percent: f64,
    pub step: String,
}

/// 部署任务日志器：把过程实时推送到前端界面，并同步写入任务注册表
#[derive(Clone)]
pub struct TaskLogger {
    app: AppHandle,
    pub task_id: String,
    registry: Arc<TaskRegistry>,
}

impl TaskLogger {
    pub fn new(app: AppHandle, task_id: String, registry: Arc<TaskRegistry>) -> Self {
        registry.init(&task_id);
        Self {
            app,
            task_id,
            registry,
        }
    }

    fn emit_log(&self, level: &str, message: String) {
        self.registry.append_log(&self.task_id, level, &message);
        let payload = TaskLogPayload {
            task_id: self.task_id.clone(),
            level: level.to_string(),
            message,
            ts: chrono::Local::now().format("%H:%M:%S").to_string(),
        };
        let _ = self.app.emit("task-log", payload);
    }

    /// 普通信息日志
    pub fn info(&self, message: impl Into<String>) {
        self.emit_log("info", message.into());
    }

    /// 警告
    pub fn warn(&self, message: impl Into<String>) {
        self.emit_log("warn", message.into());
    }

    /// 错误
    pub fn error(&self, message: impl Into<String>) {
        self.emit_log("error", message.into());
    }

    /// 成功步骤
    pub fn success(&self, message: impl Into<String>) {
        self.emit_log("success", message.into());
    }

    /// 更新任务状态：running / success / failed / cancelled
    pub fn state(&self, state: &str, message: impl Into<String>) {
        let message = message.into();
        self.registry.set_state(&self.task_id, state, &message);
        let payload = TaskStatePayload {
            task_id: self.task_id.clone(),
            state: state.to_string(),
            message,
        };
        let _ = self.app.emit("task-state", payload);
    }

    /// 更新进度百分比
    pub fn progress(&self, percent: f64, step: impl Into<String>) {
        let step = step.into();
        self.registry.set_progress(&self.task_id, percent, &step);
        let payload = TaskProgressPayload {
            task_id: self.task_id.clone(),
            percent,
            step,
        };
        let _ = self.app.emit("task-progress", payload);
    }
}
