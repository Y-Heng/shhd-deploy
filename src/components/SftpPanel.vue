<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import type { TableInstance } from "element-plus";
import { ArrowUp, Document, Folder, Plus, Refresh } from "@element-plus/icons-vue";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api } from "../api";
import type { LocalDirEntry, LocalFileEntry, SftpEntry, SftpProgressPayload } from "../types";

const LOCAL_DRIVES_PATH = "::drives";
const LOCAL_DRAG_MIME = "application/x-jy-local-paths";
const REMOTE_DRAG_MIME = "application/x-jy-remote-paths";
const RUBBER_THRESHOLD = 5;

function isDriveListPath(path: string) {
  return path === LOCAL_DRIVES_PATH;
}

type SortKey = "name" | "size" | "mtime";
type PaneSide = "local" | "remote";

interface ContextMenuState {
  visible: boolean;
  x: number;
  y: number;
  side: PaneSide;
  /** 右键落在条目上时的目标；空白处为 null */
  entry: SftpEntry | LocalDirEntry | null;
}

interface CustomDragState {
  source: PaneSide;
  label: string;
  x: number;
  y: number;
}

interface RubberBandState {
  active: boolean;
  side: PaneSide;
  startX: number;
  startY: number;
  currentX: number;
  currentY: number;
}

const props = defineProps<{
  serverId: string;
  serverName: string;
  /** 当前是否正在查看此面板；仅激活时才连远端 */
  active?: boolean;
}>();

const emit = defineEmits<{
  pathChange: [path: string];
}>();

/** —— 远端状态 —— */
const remotePath = ref("/");
const remotePathInput = ref("/");
const remoteEntries = ref<SftpEntry[]>([]);
const remoteLoading = ref(false);
const remoteReady = ref(false);
const remoteSelectedRows = ref<SftpEntry[]>([]);
const remoteSelectedPath = ref("");
const remoteAnchorPath = ref("");
const remoteTableRef = ref<TableInstance>();
const remotePaneRef = ref<HTMLElement | null>(null);
const remoteTableWrapRef = ref<HTMLElement | null>(null);
const remoteDragOver = ref(false);
const remoteSortKey = ref<SortKey>("name");

/** —— 本地状态 —— */
const localPath = ref("");
const localPathInput = ref("");
const localEntries = ref<LocalDirEntry[]>([]);
const localLoading = ref(false);
const localSelectedRows = ref<LocalDirEntry[]>([]);
const localSelectedPath = ref("");
const localAnchorPath = ref("");
const localTableRef = ref<TableInstance>();
const localPaneRef = ref<HTMLElement | null>(null);
const localTableWrapRef = ref<HTMLElement | null>(null);
const localDragOver = ref(false);
const localSortKey = ref<SortKey>("name");

/** 快捷目录：公共（全局）+ 专属（当前服务器） */
type ShortcutScope = "public" | "dedicated";
type ShortcutSide = "local" | "remote";

const publicRemoteShortcuts = ref<string[]>([]);
const publicLocalShortcuts = ref<string[]>([]);
const dedicatedRemoteShortcuts = ref<string[]>([]);
const dedicatedLocalShortcuts = ref<string[]>([]);

const localShortcutGroups = computed(() => [
  { scope: "public" as const, label: "公共", paths: publicLocalShortcuts.value },
  { scope: "dedicated" as const, label: "专属", paths: dedicatedLocalShortcuts.value },
]);

const remoteShortcutGroups = computed(() => [
  { scope: "public" as const, label: "公共", paths: publicRemoteShortcuts.value },
  { scope: "dedicated" as const, label: "专属", paths: dedicatedRemoteShortcuts.value },
]);

const busy = ref(false);
let htmlRemoteDragDepth = 0;
let htmlLocalDragDepth = 0;
/** 内部拖拽来源：自定义 MIME 在部分环境 dragover 不可见，用标志兜底 */
let internalDragSource: "local" | "remote" | null = null;
/** Tauri 原生拖放可用时，由 onDragDropEvent 处理系统文件上传，避免与 HTML drop 重复 */
let useTauriDragDrop = false;
/** 框选时压制行上 HTML5 dragstart */
let suppressNativeDrag = false;
/** 从已选中行按下，允许多选拖拽；mouseup 且未拖拽时再收成单选 */
let deferSingleSelect: { side: PaneSide; path: string } | null = null;
let pointerTracking = false;
let pointerStartX = 0;
let pointerStartY = 0;
let pointerSide: PaneSide | null = null;
let pointerFromSelected = false;
let didPointerDrag = false;
let pointerRowPath = "";
let customDragActive = false;
let skipNextGlobalClick = false;

const rubberBand = ref<RubberBandState>({
  active: false,
  side: "local",
  startX: 0,
  startY: 0,
  currentX: 0,
  currentY: 0,
});

const customDrag = ref<CustomDragState | null>(null);

const contextMenu = ref<ContextMenuState>({
  visible: false,
  x: 0,
  y: 0,
  side: "local",
  entry: null,
});

const uploadVisible = ref(false);
const uploadFileName = ref("");
const uploadTransferred = ref(0);
const uploadTotal = ref(0);
const uploadFileIndex = ref(0);
const uploadFileCount = ref(0);
const uploadCurrentFilePercent = ref(0);
const uploadPercent = computed(() => {
  if (!uploadFileCount.value) return 0;
  const fileWeight = 100 / uploadFileCount.value;
  const completed = Math.max(0, uploadFileIndex.value - 1) * fileWeight;
  const current = (uploadCurrentFilePercent.value / 100) * fileWeight;
  return Math.min(100, Math.round(completed + current));
});
const uploadSizeText = computed(() => {
  const format = (size: number) => {
    if (size < 1024) return `${size} B`;
    if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
    return `${(size / 1024 / 1024).toFixed(2)} MB`;
  };
  const filePart =
    uploadFileCount.value > 1
      ? `文件 ${uploadFileIndex.value}/${uploadFileCount.value} · `
      : "";
  return `${filePart}${format(uploadTransferred.value)} / ${format(uploadTotal.value)}`;
});

let progressUnlisten: UnlistenFn | null = null;
let dragDropUnlisten: UnlistenFn | null = null;
let activeTransferId = "";

const remoteBreadcrumbs = computed(() => {
  const normalized = remotePath.value.replace(/\\/g, "/");
  if (!normalized || normalized === "/") return [{ label: "/", path: "/" }];
  const parts = normalized.split("/").filter(Boolean);
  const crumbs = [{ label: "/", path: "/" }];
  let current = "";
  for (const part of parts) {
    current = `${current}/${part}`;
    crumbs.push({ label: part, path: current });
  }
  return crumbs;
});

/** Windows 路径面包屑：此电脑 / D: / Users */
const localBreadcrumbs = computed(() => {
  if (isDriveListPath(localPath.value) || !localPath.value)
    return [{ label: "此电脑", path: LOCAL_DRIVES_PATH }];
  const raw = localPath.value.replace(/\//g, "\\").replace(/\\+$/, "");
  const crumbs = [{ label: "此电脑", path: LOCAL_DRIVES_PATH }];
  const driveMatch = raw.match(/^([A-Za-z]:)(.*)$/);
  if (driveMatch) {
    const driveRoot = `${driveMatch[1]}\\`;
    crumbs.push({ label: driveMatch[1], path: driveRoot });
    const rest = driveMatch[2].replace(/^\\+/, "");
    if (!rest) return crumbs;
    let current = driveRoot.replace(/\\+$/, "");
    for (const part of rest.split("\\").filter(Boolean)) {
      current = `${current}\\${part}`;
      crumbs.push({ label: part, path: current });
    }
    return crumbs;
  }
  const parts = raw.split("\\").filter(Boolean);
  let current = "";
  for (const part of parts) {
    current = current ? `${current}\\${part}` : part;
    crumbs.push({ label: part, path: current });
  }
  return crumbs;
});

const canGoLocalParent = computed(() => !isDriveListPath(localPath.value) && Boolean(localPath.value));

function isHiddenEntry(entry: { name: string; hidden?: boolean }) {
  return Boolean(entry.hidden) || entry.name.startsWith(".");
}

function compareEntries<T extends { name: string; isDir: boolean; size: number; mtime: number; hidden?: boolean }>(
  left: T,
  right: T,
  sortKey: SortKey
) {
  if (left.isDir !== right.isDir) return left.isDir ? -1 : 1;
  const leftHidden = isHiddenEntry(left);
  const rightHidden = isHiddenEntry(right);
  if (leftHidden !== rightHidden) return leftHidden ? 1 : -1;
  if (sortKey === "size") {
    if (left.size !== right.size) return left.size - right.size;
  } else if (sortKey === "mtime") {
    if (left.mtime !== right.mtime) return left.mtime - right.mtime;
  }
  return left.name.localeCompare(right.name, undefined, { sensitivity: "base" });
}

const sortedLocalEntries = computed(() =>
  [...localEntries.value].sort((left, right) => compareEntries(left, right, localSortKey.value))
);

const sortedRemoteEntries = computed(() =>
  [...remoteEntries.value].sort((left, right) => compareEntries(left, right, remoteSortKey.value))
);

const rubberBandStyle = computed(() => {
  if (!rubberBand.value.active) return { display: "none" };
  const left = Math.min(rubberBand.value.startX, rubberBand.value.currentX);
  const top = Math.min(rubberBand.value.startY, rubberBand.value.currentY);
  const width = Math.abs(rubberBand.value.currentX - rubberBand.value.startX);
  const height = Math.abs(rubberBand.value.currentY - rubberBand.value.startY);
  return {
    display: "block",
    left: `${left}px`,
    top: `${top}px`,
    width: `${width}px`,
    height: `${height}px`,
  };
});

function formatSize(size: number, isDir: boolean) {
  if (isDir) return "-";
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  if (size < 1024 * 1024 * 1024) return `${(size / 1024 / 1024).toFixed(1)} MB`;
  return `${(size / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function formatTime(mtime: number) {
  if (!mtime) return "-";
  const date = new Date(mtime * 1000);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

function setRemotePath(path: string) {
  remotePath.value = path;
  remotePathInput.value = path;
  emit("pathChange", path);
}

function setLocalPath(path: string) {
  localPath.value = path;
  localPathInput.value = isDriveListPath(path) ? "" : path;
}

function clearRemoteSelection() {
  remoteSelectedRows.value = [];
  remoteSelectedPath.value = "";
  remoteAnchorPath.value = "";
}

function clearLocalSelection() {
  localSelectedRows.value = [];
  localSelectedPath.value = "";
  localAnchorPath.value = "";
}

function setRemoteSelection(rows: SftpEntry[], anchorPath?: string) {
  remoteSelectedRows.value = rows;
  remoteSelectedPath.value = rows.length ? rows[rows.length - 1].path : "";
  if (anchorPath !== undefined) remoteAnchorPath.value = anchorPath;
  else if (rows.length === 1) remoteAnchorPath.value = rows[0].path;
}

function setLocalSelection(rows: LocalDirEntry[], anchorPath?: string) {
  localSelectedRows.value = rows;
  localSelectedPath.value = rows.length ? rows[rows.length - 1].path : "";
  if (anchorPath !== undefined) localAnchorPath.value = anchorPath;
  else if (rows.length === 1) localAnchorPath.value = rows[0].path;
}

async function loadRemoteDir(path: string) {
  if (props.active === false) return;
  if (!props.serverId) {
    ElMessage.warning("请先选择服务器");
    return;
  }
  remoteLoading.value = true;
  clearRemoteSelection();
  try {
    const target = path || "/";
    remoteEntries.value = await api.sftpList(props.serverId, target);
    setRemotePath(target);
    remoteReady.value = true;
  } catch (error) {
    ElMessage.error(String(error));
  } finally {
    remoteLoading.value = false;
  }
}

async function loadLocalDir(path: string) {
  localLoading.value = true;
  clearLocalSelection();
  try {
    if (isDriveListPath(path)) {
      const drives = await api.listLocalDrives();
      localEntries.value = drives;
      setLocalPath(LOCAL_DRIVES_PATH);
      return;
    }
    const entries = await api.listLocalDir(path);
    localEntries.value = entries;
    if (!path) {
      const home = await api.getHomeDir();
      setLocalPath(home);
    } else {
      setLocalPath(path);
    }
  } catch (error) {
    ElMessage.error(String(error));
  } finally {
    localLoading.value = false;
  }
}

async function loadShortcuts() {
  try {
    const config = await api.getConfig();
    publicRemoteShortcuts.value = [...(config.sftpShortcuts || [])];
    publicLocalShortcuts.value = [...(config.sftpLocalShortcuts || [])];
    const server = config.servers.find((item) => item.id === props.serverId);
    dedicatedRemoteShortcuts.value = [...(server?.sftpRemoteShortcuts || [])];
    dedicatedLocalShortcuts.value = [...(server?.sftpLocalShortcuts || [])];
  } catch {
    publicRemoteShortcuts.value = [];
    publicLocalShortcuts.value = [];
    dedicatedRemoteShortcuts.value = [];
    dedicatedLocalShortcuts.value = [];
  }
}

function currentShortcutPath(side: ShortcutSide) {
  if (side === "local") return localPath.value;
  return remotePath.value || "/";
}

function shortcutAlreadyExists(side: ShortcutSide, path: string) {
  const groups = side === "local" ? localShortcutGroups.value : remoteShortcutGroups.value;
  return groups.some((group) => group.paths.includes(path));
}

async function persistShortcutList(side: ShortcutSide, scope: ShortcutScope, nextList: string[]) {
  const config = await api.getConfig();
  if (scope === "public") {
    if (side === "remote") config.sftpShortcuts = nextList;
    else config.sftpLocalShortcuts = nextList;
  } else {
    if (!props.serverId) {
      ElMessage.warning("请先选择服务器后再添加专属指定");
      return false;
    }
    const server = config.servers.find((item) => item.id === props.serverId);
    if (!server) {
      ElMessage.warning("找不到当前服务器配置");
      return false;
    }
    if (side === "remote") server.sftpRemoteShortcuts = nextList;
    else server.sftpLocalShortcuts = nextList;
  }
  await api.saveConfig(config);
  await loadShortcuts();
  return true;
}

async function addShortcut(side: ShortcutSide, scope: ShortcutScope, path: string) {
  const trimmed = path.trim();
  if (!trimmed) {
    ElMessage.warning(side === "local" ? "请先打开本地目录" : "当前远端路径无效");
    return;
  }
  if (side === "local" && isDriveListPath(trimmed)) {
    ElMessage.warning("请先进入具体目录再添加指定");
    return;
  }
  if (scope === "dedicated" && !props.serverId) {
    ElMessage.warning("请先选择服务器后再添加专属指定");
    return;
  }
  if (shortcutAlreadyExists(side, trimmed)) {
    ElMessage.info("该路径已在指定中");
    return;
  }
  const currentList =
    side === "local"
      ? scope === "public"
        ? publicLocalShortcuts.value
        : dedicatedLocalShortcuts.value
      : scope === "public"
        ? publicRemoteShortcuts.value
        : dedicatedRemoteShortcuts.value;
  try {
    const saved = await persistShortcutList(side, scope, [...currentList, trimmed]);
    if (saved) ElMessage.success(scope === "public" ? "已添加公共指定" : "已添加专属指定");
  } catch (error) {
    ElMessage.error(String(error));
  }
}

async function addCurrentShortcut(side: ShortcutSide, scope: ShortcutScope) {
  await addShortcut(side, scope, currentShortcutPath(side));
}

async function addShortcutFromEntry(
  side: ShortcutSide,
  scope: ShortcutScope,
  entry: SftpEntry | LocalDirEntry | null,
) {
  const path = entry?.isDir ? entry.path : currentShortcutPath(side);
  await addShortcut(side, scope, path);
}

async function removeShortcut(side: ShortcutSide, scope: ShortcutScope, path: string) {
  const currentList =
    side === "local"
      ? scope === "public"
        ? publicLocalShortcuts.value
        : dedicatedLocalShortcuts.value
      : scope === "public"
        ? publicRemoteShortcuts.value
        : dedicatedRemoteShortcuts.value;
  try {
    await persistShortcutList(
      side,
      scope,
      currentList.filter((item) => item !== path),
    );
  } catch (error) {
    ElMessage.error(String(error));
  }
}

function openShortcut(side: ShortcutSide, path: string) {
  if (side === "local") loadLocalDir(path);
  else loadRemoteDir(path);
}

function shortcutLabel(path: string) {
  if (path === "/") return "/";
  const parts = path.replace(/\\/g, "/").split("/").filter(Boolean);
  return parts[parts.length - 1] || path;
}

function goRemoteParent() {
  if (remotePath.value === "/" || !remotePath.value) return;
  const normalized = remotePath.value.replace(/\\/g, "/").replace(/\/+$/, "");
  const slashIndex = normalized.lastIndexOf("/");
  const parent = slashIndex <= 0 ? "/" : normalized.slice(0, slashIndex);
  loadRemoteDir(parent);
}

function goLocalParent() {
  if (!canGoLocalParent.value) return;
  const normalized = localPath.value.replace(/\//g, "\\").replace(/\\+$/, "");
  const lastSlash = normalized.lastIndexOf("\\");
  if (/^[A-Za-z]:$/i.test(normalized) || lastSlash < 0) {
    loadLocalDir(LOCAL_DRIVES_PATH);
    return;
  }
  if (lastSlash <= 2 && /^[A-Za-z]:/i.test(normalized)) {
    loadLocalDir(`${normalized.slice(0, 2)}\\`);
    return;
  }
  loadLocalDir(normalized.slice(0, lastSlash));
}

function enterRemotePath() {
  const target = remotePathInput.value.trim() || "/";
  loadRemoteDir(target.startsWith("/") || /^[A-Za-z]:/.test(target) ? target : `/${target}`);
}

function enterLocalPath() {
  const target = localPathInput.value.trim();
  if (!target) {
    loadLocalDir("");
    return;
  }
  loadLocalDir(target);
}

function onRemoteRowDblClick(entry: SftpEntry) {
  if (entry.isDir) loadRemoteDir(entry.path);
  else downloadEntry(entry);
}

function onLocalRowDblClick(entry: LocalDirEntry) {
  if (entry.isDir) loadLocalDir(entry.path);
}

function getRemoteRowClassName({ row }: { row: SftpEntry }) {
  const classes: string[] = [];
  if (remoteSelectedRows.value.some((item) => item.path === row.path)) classes.push("is-row-selected");
  if (isHiddenEntry(row)) classes.push("is-hidden-entry");
  return classes.join(" ");
}

function getLocalRowClassName({ row }: { row: LocalDirEntry }) {
  const classes: string[] = [];
  if (localSelectedRows.value.some((item) => item.path === row.path)) classes.push("is-row-selected");
  if (isHiddenEntry(row)) classes.push("is-hidden-entry");
  return classes.join(" ");
}

function rangeSelectRemote(target: SftpEntry) {
  const list = sortedRemoteEntries.value;
  const anchorPath = remoteAnchorPath.value || target.path;
  const anchorIndex = list.findIndex((item) => item.path === anchorPath);
  const targetIndex = list.findIndex((item) => item.path === target.path);
  if (anchorIndex < 0 || targetIndex < 0) {
    setRemoteSelection([target], target.path);
    return;
  }
  const start = Math.min(anchorIndex, targetIndex);
  const end = Math.max(anchorIndex, targetIndex);
  setRemoteSelection(list.slice(start, end + 1), anchorPath);
}

function rangeSelectLocal(target: LocalDirEntry) {
  const list = sortedLocalEntries.value;
  const anchorPath = localAnchorPath.value || target.path;
  const anchorIndex = list.findIndex((item) => item.path === anchorPath);
  const targetIndex = list.findIndex((item) => item.path === target.path);
  if (anchorIndex < 0 || targetIndex < 0) {
    setLocalSelection([target], target.path);
    return;
  }
  const start = Math.min(anchorIndex, targetIndex);
  const end = Math.max(anchorIndex, targetIndex);
  setLocalSelection(list.slice(start, end + 1), anchorPath);
}

function applyRemoteClick(row: SftpEntry, event: MouseEvent) {
  if (event.shiftKey) {
    rangeSelectRemote(row);
    return;
  }
  if (event.ctrlKey || event.metaKey) {
    const exists = remoteSelectedRows.value.some((item) => item.path === row.path);
    if (exists)
      setRemoteSelection(
        remoteSelectedRows.value.filter((item) => item.path !== row.path),
        remoteAnchorPath.value || row.path
      );
    else setRemoteSelection([...remoteSelectedRows.value, row], row.path);
    return;
  }
  const alreadySelected = remoteSelectedRows.value.some((item) => item.path === row.path);
  if (alreadySelected && remoteSelectedRows.value.length > 1) {
    deferSingleSelect = { side: "remote", path: row.path };
    return;
  }
  setRemoteSelection([row], row.path);
}

function applyLocalClick(row: LocalDirEntry, event: MouseEvent) {
  if (event.shiftKey) {
    rangeSelectLocal(row);
    return;
  }
  if (event.ctrlKey || event.metaKey) {
    const exists = localSelectedRows.value.some((item) => item.path === row.path);
    if (exists)
      setLocalSelection(
        localSelectedRows.value.filter((item) => item.path !== row.path),
        localAnchorPath.value || row.path
      );
    else setLocalSelection([...localSelectedRows.value, row], row.path);
    return;
  }
  const alreadySelected = localSelectedRows.value.some((item) => item.path === row.path);
  if (alreadySelected && localSelectedRows.value.length > 1) {
    deferSingleSelect = { side: "local", path: row.path };
    return;
  }
  setLocalSelection([row], row.path);
}

function findRowElement(target: EventTarget | null): HTMLElement | null {
  if (!(target instanceof Element)) return null;
  return target.closest("tr.el-table__row") as HTMLElement | null;
}

function findEntryPathFromRow(rowElement: HTMLElement | null) {
  if (!rowElement) return "";
  return (
    rowElement.getAttribute("data-row-key") ||
    rowElement.querySelector("[data-path]")?.getAttribute("data-path") ||
    ""
  );
}

function rectsIntersect(
  left: { left: number; top: number; right: number; bottom: number },
  right: { left: number; top: number; right: number; bottom: number }
) {
  return !(
    left.right < right.left ||
    left.left > right.right ||
    left.bottom < right.top ||
    left.top > right.bottom
  );
}

function updateRubberSelection(side: PaneSide) {
  const wrap = side === "local" ? localTableWrapRef.value : remoteTableWrapRef.value;
  if (!wrap) return;
  const bandLeft = Math.min(rubberBand.value.startX, rubberBand.value.currentX);
  const bandTop = Math.min(rubberBand.value.startY, rubberBand.value.currentY);
  const bandRight = Math.max(rubberBand.value.startX, rubberBand.value.currentX);
  const bandBottom = Math.max(rubberBand.value.startY, rubberBand.value.currentY);
  const bandRect = { left: bandLeft, top: bandTop, right: bandRight, bottom: bandBottom };
  const rowNodes = wrap.querySelectorAll("tr.el-table__row");
  if (side === "local") {
    const hit: LocalDirEntry[] = [];
    rowNodes.forEach((node) => {
      const element = node as HTMLElement;
      const path = findEntryPathFromRow(element);
      const entry = sortedLocalEntries.value.find((item) => item.path === path);
      if (!entry) return;
      const box = element.getBoundingClientRect();
      if (rectsIntersect(bandRect, { left: box.left, top: box.top, right: box.right, bottom: box.bottom }))
        hit.push(entry);
    });
    setLocalSelection(hit, hit[0]?.path || "");
    return;
  }
  const hit: SftpEntry[] = [];
  rowNodes.forEach((node) => {
    const element = node as HTMLElement;
    const path = findEntryPathFromRow(element);
    const entry = sortedRemoteEntries.value.find((item) => item.path === path);
    if (!entry) return;
    const box = element.getBoundingClientRect();
    if (rectsIntersect(bandRect, { left: box.left, top: box.top, right: box.right, bottom: box.bottom }))
      hit.push(entry);
  });
  setRemoteSelection(hit, hit[0]?.path || "");
}

function paneSideAtPoint(clientX: number, clientY: number): PaneSide | null {
  const remoteBox = remotePaneRef.value?.getBoundingClientRect();
  if (
    remoteBox &&
    clientX >= remoteBox.left &&
    clientX <= remoteBox.right &&
    clientY >= remoteBox.top &&
    clientY <= remoteBox.bottom
  )
    return "remote";
  const localBox = localPaneRef.value?.getBoundingClientRect();
  if (
    localBox &&
    clientX >= localBox.left &&
    clientX <= localBox.right &&
    clientY >= localBox.top &&
    clientY <= localBox.bottom
  )
    return "local";
  return null;
}

function customDragLabel(side: PaneSide) {
  if (side === "local") {
    const rows = localSelectedRows.value;
    if (!rows.length) return "文件";
    if (rows.length === 1) return rows[0].name;
    return `${rows[0].name} 等 ${rows.length} 项`;
  }
  const rows = remoteSelectedRows.value;
  if (!rows.length) return "文件";
  if (rows.length === 1) return rows[0].name;
  return `${rows[0].name} 等 ${rows.length} 项`;
}

async function finishCustomDrag(
  hoverSide: PaneSide | null,
  source: PaneSide,
  localRows: LocalDirEntry[],
  remoteRows: SftpEntry[],
) {
  if (hoverSide === source || !hoverSide) return;
  if (busy.value) {
    ElMessage.warning("当前有任务进行中");
    return;
  }
  if (source === "local" && hoverSide === "remote") {
    const paths = localRows.map((item) => item.path);
    if (!paths.length) return;
    const fileEntries = await resolveLocalPaths(paths);
    await uploadEntries(fileEntries);
    return;
  }
  if (source === "remote" && hoverSide === "local") {
    if (!localPath.value || isDriveListPath(localPath.value)) {
      ElMessage.warning("请先打开本地目录");
      return;
    }
    if (!remoteRows.length) return;
    await downloadRemoteToLocalDir(remoteRows, localPath.value);
  }
}

function onPointerMove(event: MouseEvent) {
  if (!pointerTracking || !pointerSide) return;
  const distance = Math.hypot(event.clientX - pointerStartX, event.clientY - pointerStartY);
  if (distance < RUBBER_THRESHOLD && !customDragActive && !rubberBand.value.active) return;
  didPointerDrag = true;

  if (pointerRowPath) {
    customDragActive = true;
    suppressNativeDrag = false;
    if (rubberBand.value.active) rubberBand.value = { ...rubberBand.value, active: false };
    customDrag.value = {
      source: pointerSide,
      label: customDragLabel(pointerSide),
      x: event.clientX,
      y: event.clientY,
    };
    const hoverSide = paneSideAtPoint(event.clientX, event.clientY);
    remoteDragOver.value = pointerSide === "local" && hoverSide === "remote";
    localDragOver.value = pointerSide === "remote" && hoverSide === "local";
    return;
  }

  const pane = pointerSide === "local" ? localPaneRef.value : remotePaneRef.value;
  const insidePane = (() => {
    if (!pane) return false;
    const box = pane.getBoundingClientRect();
    return (
      event.clientX >= box.left &&
      event.clientX <= box.right &&
      event.clientY >= box.top &&
      event.clientY <= box.bottom
    );
  })();
  if (!insidePane) return;
  suppressNativeDrag = true;
  if (!rubberBand.value.active) {
    rubberBand.value = {
      active: true,
      side: pointerSide,
      startX: pointerStartX,
      startY: pointerStartY,
      currentX: event.clientX,
      currentY: event.clientY,
    };
    if (!event.ctrlKey && !event.metaKey) {
      if (pointerSide === "local") clearLocalSelection();
      else clearRemoteSelection();
    }
  } else {
    rubberBand.value = {
      ...rubberBand.value,
      currentX: event.clientX,
      currentY: event.clientY,
    };
  }
  updateRubberSelection(pointerSide);
}

async function onPointerUp(event: MouseEvent) {
  if (!pointerTracking) return;
  pointerTracking = false;
  window.removeEventListener("mousemove", onPointerMove);
  window.removeEventListener("mouseup", onPointerUp);

  const dragSource = pointerSide;
  const wasCustomDrag = customDragActive;
  let hoverSide = paneSideAtPoint(event.clientX, event.clientY);
  if (!hoverSide && remoteDragOver.value) hoverSide = "remote";
  if (!hoverSide && localDragOver.value) hoverSide = "local";
  const snapshotLocalRows = [...localSelectedRows.value];
  const snapshotRemoteRows = [...remoteSelectedRows.value];

  if (
    deferSingleSelect &&
    !didPointerDrag &&
    !rubberBand.value.active &&
    !wasCustomDrag
  ) {
    if (deferSingleSelect.side === "local") {
      const entry = localEntries.value.find((item) => item.path === deferSingleSelect!.path);
      if (entry) setLocalSelection([entry], entry.path);
    } else {
      const entry = remoteEntries.value.find((item) => item.path === deferSingleSelect!.path);
      if (entry) setRemoteSelection([entry], entry.path);
    }
  }
  deferSingleSelect = null;
  rubberBand.value = { ...rubberBand.value, active: false };
  customDragActive = false;
  customDrag.value = null;
  remoteDragOver.value = false;
  localDragOver.value = false;
  pointerRowPath = "";
  pointerSide = null;
  pointerFromSelected = false;
  didPointerDrag = false;
  requestAnimationFrame(() => {
    suppressNativeDrag = false;
  });

  if (wasCustomDrag && dragSource)
    try {
      await finishCustomDrag(hoverSide, dragSource, snapshotLocalRows, snapshotRemoteRows);
    } catch (error) {
      ElMessage.error(String(error));
    }
}

function onTableWrapMouseDown(side: PaneSide, event: MouseEvent) {
  if (event.button !== 0) return;
  const target = event.target as HTMLElement | null;
  if (target?.closest("button, input, a, .el-button, .el-table__header-wrapper")) return;
  hideContextMenu();
  const rowElement = findRowElement(event.target);
  const rowPath = findEntryPathFromRow(rowElement);
  pointerTracking = true;
  pointerSide = side;
  pointerStartX = event.clientX;
  pointerStartY = event.clientY;
  didPointerDrag = false;
  suppressNativeDrag = false;
  pointerRowPath = rowPath;
  customDragActive = false;
  customDrag.value = null;
  if (rowPath) {
    if (side === "local") {
      const entry = sortedLocalEntries.value.find((item) => item.path === rowPath);
      pointerFromSelected = localSelectedRows.value.some((item) => item.path === rowPath);
      if (entry) applyLocalClick(entry, event);
    } else {
      const entry = sortedRemoteEntries.value.find((item) => item.path === rowPath);
      pointerFromSelected = remoteSelectedRows.value.some((item) => item.path === rowPath);
      if (entry) applyRemoteClick(entry, event);
    }
  } else {
    pointerFromSelected = false;
    deferSingleSelect = null;
    if (!event.ctrlKey && !event.metaKey && !event.shiftKey) {
      if (side === "local") clearLocalSelection();
      else clearRemoteSelection();
    }
  }
  window.addEventListener("mousemove", onPointerMove);
  window.addEventListener("mouseup", onPointerUp);
}

function hideContextMenu() {
  contextMenu.value.visible = false;
}

function openContextMenu(side: PaneSide, event: MouseEvent, entry: SftpEntry | LocalDirEntry | null) {
  event.preventDefault();
  event.stopPropagation();
  if (entry) {
    if (side === "local") {
      const selected = localSelectedRows.value.some((item) => item.path === entry.path);
      if (!selected) setLocalSelection([entry as LocalDirEntry], entry.path);
    } else {
      const selected = remoteSelectedRows.value.some((item) => item.path === entry.path);
      if (!selected) setRemoteSelection([entry as SftpEntry], entry.path);
    }
  }
  contextMenu.value = {
    visible: true,
    x: event.clientX,
    y: event.clientY,
    side,
    entry,
  };
}

function onTableContextMenu(side: PaneSide, event: MouseEvent) {
  event.preventDefault();
  skipNextGlobalClick = true;
  const rowElement = findRowElement(event.target);
  const rowPath = findEntryPathFromRow(rowElement);
  if (!rowPath) {
    openContextMenu(side, event, null);
    return;
  }
  if (side === "local") {
    const entry = sortedLocalEntries.value.find((item) => item.path === rowPath) || null;
    openContextMenu("local", event, entry);
    return;
  }
  const entry = sortedRemoteEntries.value.find((item) => item.path === rowPath) || null;
  openContextMenu("remote", event, entry);
}

const contextHasEntry = computed(() => Boolean(contextMenu.value.entry));
const contextIsDir = computed(() => Boolean(contextMenu.value.entry?.isDir));

function joinRemotePath(basePath: string, relativePath: string) {
  const normalizedBase = basePath.replace(/\\/g, "/").replace(/\/+$/, "") || "/";
  const normalizedRelative = relativePath.replace(/\\/g, "/").replace(/^\/+/, "");
  if (normalizedBase === "/") return `/${normalizedRelative}`;
  return `${normalizedBase}/${normalizedRelative}`;
}

function fileNameFromPath(filePath: string) {
  return filePath.replace(/\\/g, "/").split("/").filter(Boolean).pop() || "upload.bin";
}

function joinLocalPath(directory: string, fileName: string) {
  const normalizedDir = directory.replace(/[\\/]+$/, "");
  const separator = directory.includes("\\") || /^[A-Za-z]:/.test(directory) ? "\\" : "/";
  return `${normalizedDir}${separator}${fileName}`;
}

function createTransferId() {
  return `up-${Date.now()}-${Math.random().toString(16).slice(2, 8)}`;
}

async function beginUploadSession(fileCount: number) {
  activeTransferId = createTransferId();
  uploadFileCount.value = fileCount;
  uploadFileIndex.value = 0;
  uploadCurrentFilePercent.value = 0;
  uploadTransferred.value = 0;
  uploadTotal.value = 0;
  uploadFileName.value = "";
  uploadVisible.value = true;
  busy.value = true;
  await nextTick();
}

async function uploadEntries(fileEntries: LocalFileEntry[]) {
  if (!fileEntries.length) {
    ElMessage.warning("没有可上传的文件（已跳过系统或占用中的文件）");
    return;
  }
  await beginUploadSession(fileEntries.length);
  let successCount = 0;
  const failedNames: string[] = [];
  try {
    for (let index = 0; index < fileEntries.length; index++) {
      const entry = fileEntries[index];
      const remoteFile = joinRemotePath(remotePath.value, entry.relativePath);
      uploadFileIndex.value = index + 1;
      uploadFileName.value = entry.relativePath;
      await nextTick();
      try {
        await api.sftpUpload(
          props.serverId,
          entry.localPath,
          remoteFile,
          activeTransferId,
          index + 1,
          fileEntries.length
        );
        successCount += 1;
        uploadCurrentFilePercent.value = 100;
      } catch {
        failedNames.push(entry.relativePath);
      }
    }
    if (failedNames.length && successCount)
      ElMessage.warning(`已上传 ${successCount} 个文件，跳过 ${failedNames.length} 个系统或占用中的文件`);
    else if (failedNames.length) ElMessage.error(`上传失败：${failedNames[0]}`);
    else ElMessage.success(`已上传 ${successCount} 个文件`);
    await loadRemoteDir(remotePath.value);
  } finally {
    busy.value = false;
    uploadVisible.value = false;
    activeTransferId = "";
  }
}

/** 将本地路径解析为可上传条目：目录则递归收集，文件则单条 */
async function resolveLocalPaths(paths: string[]): Promise<LocalFileEntry[]> {
  const resolved: LocalFileEntry[] = [];
  for (const localFilePath of paths) {
    try {
      const collected = await api.sftpCollectLocalFiles(localFilePath);
      resolved.push(...collected);
    } catch {
      resolved.push({ localPath: localFilePath, relativePath: fileNameFromPath(localFilePath) });
    }
  }
  return resolved.filter((item) => item.localPath && item.relativePath);
}

/** 上传左侧选中项到右侧当前目录 */
async function uploadSelectedLocal() {
  const targets = localSelectedRows.value.length
    ? localSelectedRows.value
    : localSelectedPath.value
      ? localEntries.value.filter((item) => item.path === localSelectedPath.value)
      : [];
  if (!targets.length) {
    ElMessage.warning("请先在左侧选择要上传的文件或文件夹");
    return;
  }
  if (!props.serverId) {
    ElMessage.warning("请先选择服务器");
    return;
  }
  if (busy.value) {
    ElMessage.warning("当前有任务进行中");
    return;
  }
  const paths = targets.map((item) => item.path);
  const fileEntries = await resolveLocalPaths(paths);
  await uploadEntries(fileEntries);
}

function getRemoteActionTargets(entry?: SftpEntry) {
  if (entry) {
    const inSelection = remoteSelectedRows.value.some((item) => item.path === entry.path);
    if (inSelection && remoteSelectedRows.value.length) return [...remoteSelectedRows.value];
    return [entry];
  }
  if (remoteSelectedRows.value.length) return [...remoteSelectedRows.value];
  if (remoteSelectedPath.value) {
    const found = remoteEntries.value.find((item) => item.path === remoteSelectedPath.value);
    if (found) return [found];
  }
  return [];
}

function getLocalActionTargets(entry?: LocalDirEntry) {
  if (entry) {
    const inSelection = localSelectedRows.value.some((item) => item.path === entry.path);
    if (inSelection && localSelectedRows.value.length) return [...localSelectedRows.value];
    return [entry];
  }
  if (localSelectedRows.value.length) return [...localSelectedRows.value];
  if (localSelectedPath.value) {
    const found = localEntries.value.find((item) => item.path === localSelectedPath.value);
    if (found) return [found];
  }
  return [];
}

/** 下载远端文件到指定本地目录（跳过目录并提示） */
async function downloadRemoteToLocalDir(targets: SftpEntry[], saveDirectory: string) {
  const files = targets.filter((item) => !item.isDir);
  const skippedDirCount = targets.length - files.length;
  if (!files.length) {
    ElMessage.warning("目录无法直接下载，请选择文件");
    return;
  }
  if (skippedDirCount > 0) ElMessage.info(`已跳过 ${skippedDirCount} 个目录`);

  busy.value = true;
  try {
    for (const target of files) {
      const dest = joinLocalPath(saveDirectory, target.name);
      await api.sftpDownload(props.serverId, target.path, dest);
    }
    ElMessage.success(`已下载 ${files.length} 个文件`);
    await loadLocalDir(localPath.value);
  } catch (error) {
    ElMessage.error(String(error));
  } finally {
    busy.value = false;
  }
}

async function downloadEntry(entry?: SftpEntry) {
  const targets = getRemoteActionTargets(entry);
  if (!targets.length) {
    ElMessage.warning("请先选择文件");
    return;
  }
  const files = targets.filter((item) => !item.isDir);
  const skippedDirCount = targets.length - files.length;
  if (!files.length) {
    ElMessage.warning("目录无法直接下载，请选择文件");
    return;
  }
  if (skippedDirCount > 0) ElMessage.info(`已跳过 ${skippedDirCount} 个目录`);

  // 有当前本地目录时直接下到左侧，更贴近 Termius
  if (localPath.value) {
    await downloadRemoteToLocalDir(targets, localPath.value);
    return;
  }

  busy.value = true;
  try {
    if (files.length === 1) {
      const target = files[0];
      const dest = await saveDialog({
        title: "保存到本地",
        defaultPath: target.name,
      });
      if (!dest) return;
      await api.sftpDownload(props.serverId, target.path, dest);
      ElMessage.success("下载完成");
      return;
    }

    const saveDirectory = await openDialog({
      directory: true,
      multiple: false,
      title: "选择保存目录",
    });
    if (!saveDirectory || Array.isArray(saveDirectory)) return;
    for (const target of files) {
      const dest = joinLocalPath(saveDirectory, target.name);
      await api.sftpDownload(props.serverId, target.path, dest);
    }
    ElMessage.success(`已下载 ${files.length} 个文件`);
  } catch (error) {
    ElMessage.error(String(error));
  } finally {
    busy.value = false;
  }
}

async function createRemoteFolder() {
  const { value } = await ElMessageBox.prompt("请输入新目录名", "新建文件夹", {
    confirmButtonText: "创建",
    cancelButtonText: "取消",
    inputPattern: /^[^\\/:*?"<>|]+$/,
    inputErrorMessage: "目录名不合法",
  }).catch(() => ({ value: "" }));
  if (!value) return;
  const remoteDir = remotePath.value.endsWith("/")
    ? `${remotePath.value}${value}`
    : `${remotePath.value}/${value}`;
  busy.value = true;
  try {
    await api.sftpMkdir(props.serverId, remoteDir);
    ElMessage.success("已创建");
    await loadRemoteDir(remotePath.value);
  } catch (error) {
    ElMessage.error(String(error));
  } finally {
    busy.value = false;
  }
}

async function removeRemoteSelected(entry?: SftpEntry) {
  const targets = getRemoteActionTargets(entry);
  if (!targets.length) {
    ElMessage.warning("请先选择要删除的项");
    return;
  }
  const preview =
    targets.length === 1
      ? `确认删除「${targets[0].name}」？${targets[0].isDir ? "（目录将递归删除）" : ""}`
      : `确认删除选中的 ${targets.length} 项？（目录将递归删除）`;
  await ElMessageBox.confirm(preview, "删除确认", { type: "warning" });
  busy.value = true;
  try {
    for (const target of targets) await api.sftpRemove(props.serverId, target.path);
    ElMessage.success(targets.length === 1 ? "已删除" : `已删除 ${targets.length} 项`);
    await loadRemoteDir(remotePath.value);
  } catch (error) {
    ElMessage.error(String(error));
  } finally {
    busy.value = false;
  }
}

async function renameRemoteSelected(entry?: SftpEntry) {
  const target =
    entry ||
    remoteEntries.value.find((item) => item.path === remoteSelectedPath.value) ||
    remoteSelectedRows.value[0];
  if (!target) {
    ElMessage.warning("请先选择要重命名的项");
    return;
  }
  const { value } = await ElMessageBox.prompt("请输入新名称", "重命名", {
    confirmButtonText: "确定",
    cancelButtonText: "取消",
    inputValue: target.name,
    inputPattern: /^[^\\/:*?"<>|]+$/,
    inputErrorMessage: "名称不合法",
  }).catch(() => ({ value: "" }));
  if (!value || value === target.name) return;
  const parent = target.path.replace(/\\/g, "/").replace(/\/+$/, "");
  const slashIndex = parent.lastIndexOf("/");
  const parentDir = slashIndex <= 0 ? "/" : parent.slice(0, slashIndex);
  const toPath = parentDir === "/" ? `/${value}` : `${parentDir}/${value}`;
  busy.value = true;
  try {
    await api.sftpRename(props.serverId, target.path, toPath);
    ElMessage.success("已重命名");
    await loadRemoteDir(remotePath.value);
  } catch (error) {
    ElMessage.error(String(error));
  } finally {
    busy.value = false;
  }
}

async function viewRemoteEntry(entry?: SftpEntry) {
  const target = entry || getRemoteActionTargets()[0];
  if (!target) {
    ElMessage.warning("请先选择要查看的项");
    return;
  }
  await ElMessageBox.alert(
    [
      `名称：${target.name}`,
      `路径：${target.path}`,
      `类型：${target.isDir ? "目录" : "文件"}`,
      `大小：${formatSize(target.size, target.isDir)}`,
      `修改时间：${formatTime(target.mtime)}`,
    ].join("\n"),
    "查看属性",
    { confirmButtonText: "关闭" }
  );
}

function openLocalEntry(entry?: LocalDirEntry) {
  const target = entry || getLocalActionTargets()[0];
  if (!target) return;
  if (target.isDir) loadLocalDir(target.path);
}

function openRemoteEntry(entry?: SftpEntry) {
  const target = entry || getRemoteActionTargets()[0];
  if (!target) return;
  if (target.isDir) loadRemoteDir(target.path);
  else downloadEntry(target);
}

function cycleSortKey(side: PaneSide) {
  const order: SortKey[] = ["name", "size", "mtime"];
  if (side === "local") {
    const index = order.indexOf(localSortKey.value);
    localSortKey.value = order[(index + 1) % order.length];
    ElMessage.success(
      `本地排序：${localSortKey.value === "name" ? "名称" : localSortKey.value === "size" ? "大小" : "修改时间"}`
    );
    return;
  }
  const index = order.indexOf(remoteSortKey.value);
  remoteSortKey.value = order[(index + 1) % order.length];
  ElMessage.success(
    `远端排序：${remoteSortKey.value === "name" ? "名称" : remoteSortKey.value === "size" ? "大小" : "修改时间"}`
  );
}

function setSortKey(side: PaneSide, key: SortKey) {
  if (side === "local") localSortKey.value = key;
  else remoteSortKey.value = key;
  hideContextMenu();
}

async function onContextCommand(command: string) {
  const side = contextMenu.value.side;
  const entry = contextMenu.value.entry;
  hideContextMenu();
  if (side === "local") {
    const localEntry = entry as LocalDirEntry | null;
    if (command === "open") openLocalEntry(localEntry || undefined);
    else if (command === "upload") await uploadSelectedLocal();
    else if (command === "refresh") await loadLocalDir(localPath.value);
    else if (command === "sort-name") setSortKey("local", "name");
    else if (command === "sort-size") setSortKey("local", "size");
    else if (command === "sort-mtime") setSortKey("local", "mtime");
    else if (command === "sort-cycle") cycleSortKey("local");
    else if (command === "shortcut-public") await addShortcutFromEntry("local", "public", localEntry);
    else if (command === "shortcut-dedicated") await addShortcutFromEntry("local", "dedicated", localEntry);
    else if (command === "mkdir" || command === "rename" || command === "remove")
      ElMessage.info("本地新建/重命名/删除请使用系统资源管理器");
    return;
  }
  const remoteEntry = entry as SftpEntry | null;
  if (command === "open") openRemoteEntry(remoteEntry || undefined);
  else if (command === "download") await downloadEntry(remoteEntry || undefined);
  else if (command === "mkdir") await createRemoteFolder();
  else if (command === "rename") await renameRemoteSelected(remoteEntry || undefined);
  else if (command === "remove") await removeRemoteSelected(remoteEntry || undefined);
  else if (command === "view") await viewRemoteEntry(remoteEntry || undefined);
  else if (command === "shortcut-public") await addShortcutFromEntry("remote", "public", remoteEntry);
  else if (command === "shortcut-dedicated") await addShortcutFromEntry("remote", "dedicated", remoteEntry);
  else if (command === "refresh") await loadRemoteDir(remotePath.value);
  else if (command === "sort-name") setSortKey("remote", "name");
  else if (command === "sort-size") setSortKey("remote", "size");
  else if (command === "sort-mtime") setSortKey("remote", "mtime");
  else if (command === "sort-cycle") cycleSortKey("remote");
}

async function isPointInsideElement(element: HTMLElement | null, position: { x: number; y: number }) {
  if (!element) return false;
  const scaleFactor = await getCurrentWindow().scaleFactor();
  const rect = element.getBoundingClientRect();
  const points = [
    { x: position.x, y: position.y },
    { x: position.x / scaleFactor, y: position.y / scaleFactor },
  ];
  return points.some(
    (point) =>
      point.x >= rect.left &&
      point.x <= rect.right &&
      point.y >= rect.top &&
      point.y <= rect.bottom,
  );
}

async function handleOsDroppedPaths(paths: string[]) {
  if (!props.serverId) {
    ElMessage.warning("请先选择服务器");
    return;
  }
  if (busy.value) {
    ElMessage.warning("当前有任务进行中");
    return;
  }
  if (!paths.length) return;
  try {
    const fileEntries = await resolveLocalPaths(paths);
    await uploadEntries(fileEntries);
  } catch (error) {
    ElMessage.error(String(error));
  }
}

/** HTML 拖拽兜底：读取 Tauri 注入的 path */
async function collectFromDataTransfer(dataTransfer: DataTransfer): Promise<string[]> {
  const paths: string[] = [];
  const items = dataTransfer.items;
  if (items?.length) {
    for (let index = 0; index < items.length; index++) {
      const item = items[index];
      if (item.kind !== "file") continue;
      const file = item.getAsFile() as (File & { path?: string }) | null;
      const injectedPath = file?.path;
      if (injectedPath) paths.push(injectedPath);
    }
    if (paths.length) return paths;
    if (items.length) {
      ElMessage.warning("当前环境无法获取本地路径，请通过右键「上传到远端」操作");
      return [];
    }
  }
  for (let index = 0; index < dataTransfer.files.length; index++) {
    const file = dataTransfer.files[index] as File & { path?: string };
    if (file.path) paths.push(file.path);
  }
  return paths;
}

function hasDragType(dataTransfer: DataTransfer | null, mime: string) {
  if (!dataTransfer) return false;
  return Array.from(dataTransfer.types).includes(mime);
}

function hasOsFiles(dataTransfer: DataTransfer | null) {
  if (!dataTransfer) return false;
  return Array.from(dataTransfer.types).includes("Files");
}

/** —— 内部 HTML5 拖拽：本地 → 远端 —— */
function onLocalRowDragStart(event: DragEvent, row: LocalDirEntry) {
  if (rubberBand.value.active) {
    event.preventDefault();
    return;
  }
  if (!event.dataTransfer) return;
  internalDragSource = "local";
  const selected = localSelectedRows.value.some((item) => item.path === row.path)
    ? localSelectedRows.value
    : [row];
  if (!localSelectedRows.value.some((item) => item.path === row.path))
    setLocalSelection(selected, row.path);
  const paths = selected.map((item) => item.path);
  event.dataTransfer.setData(LOCAL_DRAG_MIME, JSON.stringify(paths));
  event.dataTransfer.setData("text/plain", paths.join("\n"));
  event.dataTransfer.effectAllowed = "copy";
  deferSingleSelect = null;
  didPointerDrag = true;
}

function onLocalRowDragEnd() {
  internalDragSource = null;
  remoteDragOver.value = false;
  htmlRemoteDragDepth = 0;
}

function onRemoteRowDragStart(event: DragEvent, row: SftpEntry) {
  if (rubberBand.value.active) {
    event.preventDefault();
    return;
  }
  if (!event.dataTransfer) return;
  internalDragSource = "remote";
  const selected = remoteSelectedRows.value.some((item) => item.path === row.path)
    ? remoteSelectedRows.value
    : [row];
  if (!remoteSelectedRows.value.some((item) => item.path === row.path))
    setRemoteSelection(selected, row.path);
  const payload = selected.map((item) => ({
    name: item.name,
    path: item.path,
    isDir: item.isDir,
  }));
  event.dataTransfer.setData(REMOTE_DRAG_MIME, JSON.stringify(payload));
  event.dataTransfer.setData("text/plain", payload.map((item) => item.path).join("\n"));
  event.dataTransfer.effectAllowed = "copy";
  deferSingleSelect = null;
  didPointerDrag = true;
}

function onRemoteRowDragEnd() {
  internalDragSource = null;
  localDragOver.value = false;
  htmlLocalDragDepth = 0;
}

function canAcceptLocalDrop(transfer: DataTransfer | null) {
  return internalDragSource === "local" || hasDragType(transfer, LOCAL_DRAG_MIME) || hasOsFiles(transfer);
}

function canAcceptRemoteDrop(transfer: DataTransfer | null) {
  return internalDragSource === "remote" || hasDragType(transfer, REMOTE_DRAG_MIME);
}

function onRemotePaneDragEnter(event: DragEvent) {
  if (!canAcceptLocalDrop(event.dataTransfer)) return;
  event.preventDefault();
  htmlRemoteDragDepth += 1;
  remoteDragOver.value = true;
}

function onRemotePaneDragOver(event: DragEvent) {
  if (!canAcceptLocalDrop(event.dataTransfer)) return;
  event.preventDefault();
  if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
  remoteDragOver.value = true;
}

function onRemotePaneDragLeave(event: DragEvent) {
  if (!canAcceptLocalDrop(event.dataTransfer)) return;
  event.preventDefault();
  htmlRemoteDragDepth = Math.max(0, htmlRemoteDragDepth - 1);
  if (htmlRemoteDragDepth === 0) remoteDragOver.value = false;
}

async function onRemotePaneDrop(event: DragEvent) {
  event.preventDefault();
  htmlRemoteDragDepth = 0;
  remoteDragOver.value = false;
  if (!event.dataTransfer) return;

  const localJson = event.dataTransfer.getData(LOCAL_DRAG_MIME);
  if (localJson) {
    try {
      const paths = JSON.parse(localJson) as string[];
      if (!Array.isArray(paths) || !paths.length) return;
      if (busy.value) {
        ElMessage.warning("当前有任务进行中");
        return;
      }
      const fileEntries = await resolveLocalPaths(paths);
      await uploadEntries(fileEntries);
    } catch (error) {
      ElMessage.error(String(error));
    }
    return;
  }

  // 系统资源管理器拖入（Tauri 原生不可用时的 HTML 兜底）
  if (useTauriDragDrop) return;
  if (!hasOsFiles(event.dataTransfer)) return;
  const paths = await collectFromDataTransfer(event.dataTransfer);
  if (paths.length) await handleOsDroppedPaths(paths);
}

function onLocalPaneDragEnter(event: DragEvent) {
  if (!canAcceptRemoteDrop(event.dataTransfer)) return;
  event.preventDefault();
  htmlLocalDragDepth += 1;
  localDragOver.value = true;
}

function onLocalPaneDragOver(event: DragEvent) {
  if (!canAcceptRemoteDrop(event.dataTransfer)) return;
  event.preventDefault();
  if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
  localDragOver.value = true;
}

function onLocalPaneDragLeave(event: DragEvent) {
  if (!canAcceptRemoteDrop(event.dataTransfer)) return;
  event.preventDefault();
  htmlLocalDragDepth = Math.max(0, htmlLocalDragDepth - 1);
  if (htmlLocalDragDepth === 0) localDragOver.value = false;
}

async function onLocalPaneDrop(event: DragEvent) {
  event.preventDefault();
  htmlLocalDragDepth = 0;
  localDragOver.value = false;
  if (!event.dataTransfer) return;
  const remoteJson = event.dataTransfer.getData(REMOTE_DRAG_MIME);
  if (!remoteJson) return;
  try {
    const payload = JSON.parse(remoteJson) as Array<{ name: string; path: string; isDir: boolean }>;
    if (!Array.isArray(payload) || !payload.length) return;
    if (!localPath.value || isDriveListPath(localPath.value)) {
      ElMessage.warning("请先打开本地目录");
      return;
    }
    if (busy.value) {
      ElMessage.warning("当前有任务进行中");
      return;
    }
    const asEntries: SftpEntry[] = payload.map((item) => ({
      name: item.name,
      path: item.path,
      isDir: item.isDir,
      size: 0,
      mtime: 0,
    }));
    await downloadRemoteToLocalDir(asEntries, localPath.value);
  } catch (error) {
    ElMessage.error(String(error));
  }
}

function onGlobalClick() {
  if (skipNextGlobalClick) {
    skipNextGlobalClick = false;
    return;
  }
  hideContextMenu();
}

defineExpose({
  getCurrentPath: () => remotePath.value,
  openPath: (path: string) => loadRemoteDir(path),
});

watch(
  () => [props.serverId, props.active] as const,
  async ([serverId, active]) => {
    if (!active || !serverId) return;
    await loadShortcuts();
    if (remoteReady.value || remoteLoading.value) return;
    await loadRemoteDir(remotePath.value || "/");
  },
  { immediate: true },
);

onMounted(async () => {
  await loadLocalDir("");
  await loadShortcuts();
  window.addEventListener("click", onGlobalClick);

  progressUnlisten = await listen<SftpProgressPayload>("sftp-progress", (event) => {
    if (activeTransferId && event.payload.transferId !== activeTransferId) return;
    uploadFileName.value = event.payload.fileName;
    uploadTransferred.value = event.payload.transferred;
    uploadTotal.value = event.payload.total;
    uploadFileIndex.value = event.payload.fileIndex;
    uploadFileCount.value = event.payload.fileCount;
    if (event.payload.total > 0)
      uploadCurrentFilePercent.value = Math.min(
        100,
        Math.round((event.payload.transferred / event.payload.total) * 100)
      );
    else if (event.payload.done) uploadCurrentFilePercent.value = 100;
  });

  try {
    dragDropUnlisten = await getCurrentWebview().onDragDropEvent(async (event) => {
      if (!props.active) {
        remoteDragOver.value = false;
        return;
      }
      const payload = event.payload;
      if (payload.type === "enter" || payload.type === "over") {
        const inside = await isPointInsideElement(remotePaneRef.value, payload.position);
        remoteDragOver.value = inside;
        return;
      }
      if (payload.type === "leave") {
        remoteDragOver.value = false;
        return;
      }
      if (payload.type === "drop") {
        const inside = await isPointInsideElement(remotePaneRef.value, payload.position);
        remoteDragOver.value = false;
        if (inside) await handleOsDroppedPaths(payload.paths);
      }
    });
    useTauriDragDrop = true;
  } catch {
    // 非 Tauri 环境忽略原生拖放监听，回退到 HTML drop
  }
});

onUnmounted(async () => {
  window.removeEventListener("click", onGlobalClick);
  window.removeEventListener("mousemove", onPointerMove);
  window.removeEventListener("mouseup", onPointerUp);
  if (progressUnlisten) progressUnlisten();
  if (dragDropUnlisten) dragDropUnlisten();
});
</script>

<template>
  <div class="sftp-panel">
    <div class="sftp-header">
      <div class="sftp-title">
        <span class="sftp-badge">SFTP</span>
        <span class="pane-label">本地</span>
        <span class="pane-divider">|</span>
        <span class="sftp-server">{{ serverName || "未选择服务器" }}</span>
      </div>
    </div>

    <div class="dual-panes">
      <!-- 左侧：本地 -->
      <div
        ref="localPaneRef"
        class="pane local-pane"
        :class="{ 'is-dragover': localDragOver }"
        @dragenter="onLocalPaneDragEnter"
        @dragover="onLocalPaneDragOver"
        @dragleave="onLocalPaneDragLeave"
        @drop="onLocalPaneDrop"
      >
        <div class="pane-toolbar">
          <span class="pane-title">本地</span>
          <div class="pane-toolbar-actions">
            <el-button size="small" :icon="ArrowUp" :disabled="!canGoLocalParent" @click="goLocalParent">
              上级
            </el-button>
            <el-button size="small" @click="loadLocalDir(LOCAL_DRIVES_PATH)">磁盘</el-button>
            <el-button size="small" :icon="Refresh" :loading="localLoading" @click="loadLocalDir(localPath)">
              刷新
            </el-button>
          </div>
        </div>
        <div class="shortcut-bar">
          <div
            v-for="group in localShortcutGroups"
            :key="'local-' + group.scope"
            class="shortcut-group"
          >
            <span class="shortcut-group-label">{{ group.label }}</span>
            <button
              v-for="shortcut in group.paths"
              :key="group.scope + shortcut"
              type="button"
              class="shortcut-chip"
              :class="{ 'is-dedicated': group.scope === 'dedicated' }"
              :title="shortcut"
              @click="openShortcut('local', shortcut)"
            >
              <span class="shortcut-chip-label">{{ shortcutLabel(shortcut) }}</span>
              <span
                class="shortcut-chip-remove"
                title="删除指定"
                @click.stop="removeShortcut('local', group.scope, shortcut)"
              >×</span>
            </button>
            <button
              type="button"
              class="shortcut-add"
              :title="group.scope === 'public' ? '添加当前路径为公共指定' : '添加当前路径为专属指定'"
              @click="addCurrentShortcut('local', group.scope)"
            >
              <el-icon><Plus /></el-icon>
            </button>
          </div>
        </div>
        <div class="sftp-pathbar">
          <div class="crumbs">
            <button
              v-for="(crumb, index) in localBreadcrumbs"
              :key="crumb.path + crumb.label"
              type="button"
              class="crumb"
              @click="loadLocalDir(crumb.path)"
            >
              <span v-if="index > 0" class="crumb-sep">\</span>
              {{ crumb.label }}
            </button>
          </div>
          <form class="path-form" @submit.prevent="enterLocalPath">
            <input v-model="localPathInput" class="path-input" spellcheck="false" />
            <el-button size="small" native-type="submit">转到</el-button>
          </form>
        </div>
        <div
          ref="localTableWrapRef"
          class="table-wrap"
          @mousedown="onTableWrapMouseDown('local', $event)"
          @contextmenu.capture.prevent="onTableContextMenu('local', $event)"
        >
          <el-table
            ref="localTableRef"
            v-loading="localLoading"
            :data="sortedLocalEntries"
            height="100%"
            class="sftp-table"
            row-key="path"
            :row-class-name="getLocalRowClassName"
            @row-dblclick="onLocalRowDblClick"
          >
            <el-table-column label="名称" min-width="180">
              <template #default="{ row }">
                <div
                  class="cell-drag"
                  :data-path="row.path"
                >
                  <div class="name-cell">
                    <el-icon :class="row.isDir ? 'icon-dir' : 'icon-file'">
                      <Folder v-if="row.isDir" />
                      <Document v-else />
                    </el-icon>
                    <span class="entry-name">{{ row.name }}</span>
                  </div>
                </div>
              </template>
            </el-table-column>
            <el-table-column label="大小" width="90">
              <template #default="{ row }">
                <div
                  class="cell-drag"
                  :data-path="row.path"
                >
                  {{ formatSize(row.size, row.isDir) }}
                </div>
              </template>
            </el-table-column>
            <el-table-column label="修改时间" width="140">
              <template #default="{ row }">
                <div
                  class="cell-drag"
                  :data-path="row.path"
                >
                  {{ formatTime(row.mtime) }}
                </div>
              </template>
            </el-table-column>
          </el-table>
        </div>
        <div v-if="localDragOver" class="drop-overlay">
          <div class="drop-overlay-box">释放以下载到此处</div>
        </div>
      </div>

      <!-- 右侧：远端 -->
      <div
        ref="remotePaneRef"
        class="pane remote-pane"
        :class="{ 'is-dragover': remoteDragOver }"
        @dragenter="onRemotePaneDragEnter"
        @dragover="onRemotePaneDragOver"
        @dragleave="onRemotePaneDragLeave"
        @drop="onRemotePaneDrop"
      >
        <div class="pane-toolbar">
          <span class="pane-title">{{ serverName || "远端" }}</span>
          <div class="pane-toolbar-actions">
            <el-button size="small" :icon="ArrowUp" :disabled="remotePath === '/'" @click="goRemoteParent">
              上级
            </el-button>
            <el-button size="small" :icon="Refresh" :loading="remoteLoading" @click="loadRemoteDir(remotePath)">
              刷新
            </el-button>
          </div>
        </div>
        <div class="shortcut-bar">
          <div
            v-for="group in remoteShortcutGroups"
            :key="'remote-' + group.scope"
            class="shortcut-group"
          >
            <span class="shortcut-group-label">{{ group.label }}</span>
            <button
              v-for="shortcut in group.paths"
              :key="group.scope + shortcut"
              type="button"
              class="shortcut-chip"
              :class="{ 'is-dedicated': group.scope === 'dedicated' }"
              :title="shortcut"
              @click="openShortcut('remote', shortcut)"
            >
              <span class="shortcut-chip-label">{{ shortcutLabel(shortcut) }}</span>
              <span
                class="shortcut-chip-remove"
                title="删除指定"
                @click.stop="removeShortcut('remote', group.scope, shortcut)"
              >×</span>
            </button>
            <button
              type="button"
              class="shortcut-add"
              :title="group.scope === 'public' ? '添加当前路径为公共指定' : '添加当前路径为专属指定'"
              @click="addCurrentShortcut('remote', group.scope)"
            >
              <el-icon><Plus /></el-icon>
            </button>
          </div>
        </div>
        <div class="sftp-pathbar">
          <div class="crumbs">
            <button
              v-for="(crumb, index) in remoteBreadcrumbs"
              :key="crumb.path"
              type="button"
              class="crumb"
              @click="loadRemoteDir(crumb.path)"
            >
              <span v-if="index > 0" class="crumb-sep">/</span>
              {{ crumb.label === "/" ? "root" : crumb.label }}
            </button>
          </div>
          <form class="path-form" @submit.prevent="enterRemotePath">
            <input v-model="remotePathInput" class="path-input" spellcheck="false" />
            <el-button size="small" native-type="submit">转到</el-button>
          </form>
        </div>
        <div
          ref="remoteTableWrapRef"
          class="table-wrap"
          @mousedown="onTableWrapMouseDown('remote', $event)"
          @contextmenu.capture.prevent="onTableContextMenu('remote', $event)"
        >
          <el-table
            ref="remoteTableRef"
            v-loading="remoteLoading || busy"
            :data="sortedRemoteEntries"
            height="100%"
            class="sftp-table"
            row-key="path"
            :row-class-name="getRemoteRowClassName"
            @row-dblclick="onRemoteRowDblClick"
          >
            <el-table-column label="名称" min-width="180">
              <template #default="{ row }">
                <div
                  class="cell-drag"
                  :data-path="row.path"
                >
                  <div class="name-cell">
                    <el-icon :class="row.isDir ? 'icon-dir' : 'icon-file'">
                      <Folder v-if="row.isDir" />
                      <Document v-else />
                    </el-icon>
                    <span class="entry-name">{{ row.name }}</span>
                  </div>
                </div>
              </template>
            </el-table-column>
            <el-table-column label="大小" width="90">
              <template #default="{ row }">
                <div
                  class="cell-drag"
                  :data-path="row.path"
                >
                  {{ formatSize(row.size, row.isDir) }}
                </div>
              </template>
            </el-table-column>
            <el-table-column label="修改时间" width="140">
              <template #default="{ row }">
                <div
                  class="cell-drag"
                  :data-path="row.path"
                >
                  {{ formatTime(row.mtime) }}
                </div>
              </template>
            </el-table-column>
          </el-table>
        </div>
        <div v-if="remoteDragOver" class="drop-overlay">
          <div class="drop-overlay-box">释放以上传</div>
        </div>
      </div>
    </div>

    <Teleport to="body">
      <div
        v-if="rubberBand.active"
        class="rubber-band"
        :style="rubberBandStyle"
      />
      <div
        v-if="customDrag"
        class="sftp-drag-ghost"
        :style="{ left: `${customDrag.x}px`, top: `${customDrag.y}px` }"
      >
        {{ customDrag.label }}
      </div>
      <div
        v-if="contextMenu.visible"
        class="ctx-menu"
        :style="{ left: `${contextMenu.x}px`, top: `${contextMenu.y}px` }"
        @click.stop
        @contextmenu.prevent
      >
        <template v-if="contextMenu.side === 'local'">
          <button
            v-if="contextHasEntry && contextIsDir"
            type="button"
            class="ctx-item"
            @click="onContextCommand('open')"
          >打开</button>
          <button
            v-if="contextHasEntry"
            type="button"
            class="ctx-item"
            @click="onContextCommand('upload')"
          >上传到远端</button>
          <button type="button" class="ctx-item" @click="onContextCommand('shortcut-public')">添加为公共指定</button>
          <button type="button" class="ctx-item" @click="onContextCommand('shortcut-dedicated')">添加为专属指定</button>
          <button type="button" class="ctx-item" @click="onContextCommand('refresh')">刷新</button>
          <div class="ctx-sep" />
          <div class="ctx-label">排序</div>
          <button type="button" class="ctx-item" @click="onContextCommand('sort-name')">名称</button>
          <button type="button" class="ctx-item" @click="onContextCommand('sort-size')">大小</button>
          <button type="button" class="ctx-item" @click="onContextCommand('sort-mtime')">修改时间</button>
        </template>
        <template v-else>
          <button
            v-if="contextHasEntry && contextIsDir"
            type="button"
            class="ctx-item"
            @click="onContextCommand('open')"
          >打开</button>
          <button
            v-if="contextHasEntry"
            type="button"
            class="ctx-item"
            @click="onContextCommand('download')"
          >下载到左侧</button>
          <button type="button" class="ctx-item" @click="onContextCommand('mkdir')">新建文件夹</button>
          <button
            v-if="contextHasEntry"
            type="button"
            class="ctx-item"
            @click="onContextCommand('rename')"
          >重命名</button>
          <button
            v-if="contextHasEntry"
            type="button"
            class="ctx-item danger"
            @click="onContextCommand('remove')"
          >删除</button>
          <button
            v-if="contextHasEntry"
            type="button"
            class="ctx-item"
            @click="onContextCommand('view')"
          >查看</button>
          <button
            type="button"
            class="ctx-item"
            @click="onContextCommand('shortcut-public')"
          >添加为公共指定</button>
          <button
            type="button"
            class="ctx-item"
            @click="onContextCommand('shortcut-dedicated')"
          >添加为专属指定</button>
          <button type="button" class="ctx-item" @click="onContextCommand('refresh')">刷新</button>
          <div class="ctx-sep" />
          <div class="ctx-label">排序</div>
          <button type="button" class="ctx-item" @click="onContextCommand('sort-name')">名称</button>
          <button type="button" class="ctx-item" @click="onContextCommand('sort-size')">大小</button>
          <button type="button" class="ctx-item" @click="onContextCommand('sort-mtime')">修改时间</button>
        </template>
      </div>
    </Teleport>

    <div v-if="uploadVisible" class="upload-progress">
      <div class="upload-progress-head">
        <span>上传中：{{ uploadFileName }}</span>
        <span>{{ uploadPercent }}%</span>
      </div>
      <el-progress
        :percentage="uploadPercent"
        :stroke-width="10"
      />
      <div class="upload-progress-size">{{ uploadSizeText }}</div>
    </div>
  </div>
</template>

<style scoped>
.sftp-panel {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--app-bg, #0f1218);
  color: var(--app-text, #d7dde8);
  position: relative;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  overflow: hidden;
}

.sftp-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 12px;
  border-bottom: 1px solid var(--app-border, #2a3344);
  background: var(--app-panel, #151a22);
  flex-wrap: wrap;
}

.sftp-title {
  display: flex;
  align-items: center;
  gap: 10px;
}

.sftp-badge {
  display: inline-flex;
  align-items: center;
  height: 22px;
  padding: 0 8px;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: 6px;
  background: var(--app-accent-dim, rgba(61, 214, 140, 0.15));
  color: var(--app-accent, #3dd68c);
  font-size: 12px;
  font-weight: 600;
}

.pane-label {
  font-size: 13px;
  color: var(--app-text, #d7dde8);
}

.pane-divider {
  color: #5a6578;
}

.sftp-server {
  font-size: 13px;
  color: var(--app-muted, #8b95a8);
}

.dual-panes {
  display: flex;
  flex: 1;
  min-height: 0;
  gap: 8px;
  padding: 8px;
}

.pane {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  background: var(--app-panel, #151a22);
  position: relative;
  overflow: hidden;
}

.pane.is-dragover {
  outline: 2px dashed var(--app-accent, #3dd68c);
  outline-offset: -4px;
}

.pane-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 8px 10px;
  border-bottom: 1px solid var(--app-border, #2a3344);
  background: var(--app-panel-2, #1a2130);
  flex-shrink: 0;
}

.pane-title {
  font-size: 12px;
  font-weight: 600;
  color: var(--app-muted, #8b95a8);
  letter-spacing: 0.04em;
  text-transform: uppercase;
}

.pane-toolbar-actions {
  display: flex;
  gap: 6px;
}

.shortcut-bar {
  display: flex;
  flex-direction: column;
  align-items: stretch;
  gap: 4px;
  padding: 6px 10px;
  border-bottom: 1px solid var(--app-border, #2a3344);
  background: var(--app-panel-2, #1a2130);
  flex-shrink: 0;
}

.shortcut-group {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
}

.shortcut-group-label {
  flex-shrink: 0;
  width: 32px;
  font-size: 11px;
  color: var(--app-muted, #8b95a8);
}

.shortcut-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  max-width: 160px;
  height: 24px;
  padding: 0 6px 0 8px;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: 6px;
  background: var(--app-bg, #0f1218);
  color: var(--app-muted, #8b95a8);
  font-size: 12px;
  cursor: pointer;
}

.shortcut-chip:hover {
  color: var(--app-accent, #3dd68c);
  border-color: var(--app-accent, #3dd68c);
}

.shortcut-chip.is-dedicated {
  border-color: rgba(61, 214, 140, 0.35);
}

.shortcut-chip-label {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.shortcut-chip-remove {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 14px;
  height: 14px;
  border-radius: 4px;
  color: #5a6578;
  line-height: 1;
}

.shortcut-chip-remove:hover {
  color: #ff6b6b;
  background: rgba(255, 107, 107, 0.12);
}

.shortcut-add {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  border: 1px dashed var(--app-border, #2a3344);
  border-radius: 6px;
  background: transparent;
  color: var(--app-muted, #8b95a8);
  cursor: pointer;
}

.shortcut-add:hover {
  color: var(--app-accent, #3dd68c);
  border-color: var(--app-accent, #3dd68c);
}

.sftp-pathbar {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 10px;
  border-bottom: 1px solid var(--app-border, #2a3344);
  background: var(--app-bg, #0f1218);
  flex-wrap: wrap;
  flex-shrink: 0;
}

.crumbs {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 2px;
  flex: 1;
  min-width: 120px;
}

.crumb {
  border: 1px solid transparent;
  background: transparent;
  color: var(--app-muted, #8b95a8);
  cursor: pointer;
  font-size: 12px;
  padding: 2px 6px;
  border-radius: 6px;
}

.crumb:hover {
  color: var(--app-accent, #3dd68c);
  background: var(--app-accent-dim, rgba(61, 214, 140, 0.1));
  border-color: var(--app-border, #2a3344);
}

.crumb-sep {
  margin-right: 2px;
  color: #5a6578;
}

.path-form {
  display: flex;
  gap: 6px;
  align-items: center;
}

.path-input {
  width: min(220px, 28vw);
  height: 28px;
  border-radius: var(--app-radius, 8px);
  border: 1px solid var(--app-border, #2a3344);
  background: var(--app-panel, #151a22);
  color: var(--app-text, #d7dde8);
  padding: 0 8px;
  font-family: Consolas, monospace;
  font-size: 12px;
  outline: none;
}

.path-input:focus {
  border-color: var(--app-accent, #3dd68c);
}

.table-wrap {
  flex: 1;
  min-height: 0;
  position: relative;
  overflow: hidden;
}

.sftp-table {
  height: 100%;
}

.sftp-table :deep(.el-table__inner-wrapper::before) {
  display: none;
}

.sftp-table :deep(.el-table__body tr.hover-row > td.el-table__cell) {
  background-color: rgba(61, 214, 140, 0.08) !important;
}

.sftp-table :deep(.el-table__body tr.is-row-selected > td.el-table__cell) {
  background-color: rgba(61, 214, 140, 0.18) !important;
}

.sftp-table :deep(.el-table__body tr.is-hidden-entry > td.el-table__cell) {
  opacity: 0.55;
}

.sftp-table :deep(.el-table__body tr.is-hidden-entry .entry-name) {
  color: #6b7385;
}

.sftp-table :deep(.el-table__body td.el-table__cell) {
  padding: 0 !important;
}

.sftp-table :deep(.el-table__body .cell) {
  padding: 0 !important;
}

.sftp-table :deep(.el-table__row) {
  user-select: none;
}

.cell-drag {
  display: flex;
  align-items: center;
  gap: 8px;
  min-height: 36px;
  padding: 0 8px;
  cursor: grab;
  user-select: none;
  width: 100%;
  box-sizing: border-box;
}

.cell-drag:active {
  cursor: grabbing;
}

.name-cell {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  width: 100%;
}

.entry-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.icon-dir {
  color: #6aa8ff;
  flex-shrink: 0;
}

.icon-file {
  color: var(--app-muted, #8b95a8);
  flex-shrink: 0;
}

.rubber-band {
  position: fixed;
  z-index: 3990;
  border: 1px solid rgba(61, 214, 140, 0.85);
  background: rgba(61, 214, 140, 0.15);
  pointer-events: none;
}

.sftp-drag-ghost {
  position: fixed;
  z-index: 3995;
  pointer-events: none;
  padding: 6px 10px;
  border-radius: 6px;
  border: 1px solid var(--app-accent, #3dd68c);
  background: var(--app-panel, #151a22);
  color: var(--app-accent, #3dd68c);
  font-size: 12px;
  font-weight: 600;
  transform: translate(12px, 12px);
  box-shadow: 0 8px 20px rgba(0, 0, 0, 0.35);
}

.ctx-menu {
  position: fixed;
  z-index: 4000;
  min-width: 168px;
  padding: 6px 0;
  border: 1px solid var(--app-border, #2a3344);
  border-radius: 8px;
  background: var(--app-panel, #151a22);
  box-shadow: 0 10px 28px rgba(0, 0, 0, 0.4);
}

.ctx-item {
  display: block;
  width: 100%;
  border: none;
  background: transparent;
  color: var(--app-text, #d7dde8);
  text-align: left;
  font-size: 13px;
  padding: 7px 14px;
  cursor: pointer;
}

.ctx-item:hover {
  background: rgba(61, 214, 140, 0.12);
  color: var(--app-accent, #3dd68c);
}

.ctx-item.danger:hover {
  background: rgba(255, 107, 107, 0.12);
  color: #ff6b6b;
}

.ctx-sep {
  height: 1px;
  margin: 4px 0;
  background: var(--app-border, #2a3344);
}

.ctx-label {
  padding: 4px 14px 2px;
  font-size: 11px;
  color: #5a6578;
}

.drop-overlay {
  position: absolute;
  inset: 0;
  z-index: 30;
  display: flex;
  align-items: center;
  justify-content: center;
  pointer-events: none;
  background: rgba(15, 18, 24, 0.72);
}

.drop-overlay-box {
  padding: 18px 28px;
  border: 2px dashed var(--app-accent, #3dd68c);
  border-radius: var(--app-radius-lg, 10px);
  color: var(--app-accent, #3dd68c);
  font-size: 16px;
  font-weight: 600;
  letter-spacing: 0.04em;
  background: rgba(61, 214, 140, 0.08);
}

.upload-progress {
  position: absolute;
  left: 16px;
  right: 16px;
  bottom: 16px;
  padding: 12px 14px;
  border-radius: var(--app-radius-lg, 10px);
  background: rgba(21, 26, 34, 0.96);
  border: 1px solid var(--app-border, #2a3344);
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.35);
  z-index: 40;
}

.upload-progress-head {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 8px;
  font-size: 13px;
}

.upload-progress-size {
  margin-top: 6px;
  font-size: 12px;
  color: var(--app-muted, #8b95a8);
  font-family: Consolas, monospace;
}
</style>
