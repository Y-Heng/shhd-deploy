import { ref } from "vue";

export type AppearanceMode = "system" | "light" | "dark";

const STORAGE_KEY = "shhd-deploy.appearance";

const appearanceMode = ref<AppearanceMode>(readAppearanceMode());
let systemListenerBound = false;

export function readAppearanceMode(): AppearanceMode {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === "light" || stored === "dark" || stored === "system") return stored;
  } catch {
    // 无 localStorage 时按跟随系统
  }
  return "system";
}

function systemPrefersDark() {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

function resolvedDark(mode: AppearanceMode) {
  if (mode === "dark") return true;
  if (mode === "light") return false;
  return systemPrefersDark();
}

export function applyAppearance(mode: AppearanceMode) {
  const dark = resolvedDark(mode);
  document.documentElement.classList.toggle("dark", dark);
  document.documentElement.style.colorScheme = dark ? "dark" : "light";
}

function bindSystemListener() {
  if (systemListenerBound) return;
  systemListenerBound = true;
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    if (appearanceMode.value === "system") applyAppearance("system");
  });
}

export function initAppearance() {
  appearanceMode.value = readAppearanceMode();
  applyAppearance(appearanceMode.value);
  bindSystemListener();
}

export function useAppearance() {
  function setAppearance(mode: AppearanceMode | string) {
    if (mode !== "system" && mode !== "light" && mode !== "dark") return;
    appearanceMode.value = mode;
    try {
      localStorage.setItem(STORAGE_KEY, mode);
    } catch {
      // 写入失败时仍立即切换当前窗口主题
    }
    applyAppearance(mode);
  }

  return { appearanceMode, setAppearance };
}
