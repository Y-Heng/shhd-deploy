<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { ArrowRight, Close, Delete, EditPen, Folder, Loading, Plus, Search } from '@element-plus/icons-vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import { api } from '../api'
import { bus, OPEN_SSH_EVENT, syncActiveSshServerIds, syncConnectingSshServerIds } from '../bus'
import SftpPanel from '../components/SftpPanel.vue'
import GripDots from '../components/GripDots.vue'
import { dropPlaceByX, elementFromPointIgnoringDrag, isDropPlaceholder } from '../composables/groupedDragSort'
import { createSshTerminal } from '../sshTerminal'
import { sftpTransfer } from '../composables/useSftpTransfer'
import type { AppConfig, QuickCommand, ServerConfig, TermClosedPayload, TermDataPayload } from '../types'

interface TermSession {
  uiId: string
  sessionId: string
  serverId: string
  title: string
  terminal: Terminal
  fitAddon: FitAddon
  closed: boolean
  connecting: boolean
  popoutLabel: string
  panelMode: MainPanelMode
}

type RightPanelMode = 'snippets' | 'hidden'
type MainPanelMode = 'terminal' | 'sftp'

const emptyQuickCommand = (): QuickCommand => ({
  id: '',
  name: '',
  command: '',
  group: '常用'
})

const config = ref<AppConfig | null>(null)
const sessions = ref<TermSession[]>([])
const activeSessionId = ref('')
const connectPopoverVisible = ref(false)
const draggingSessionId = ref('')
const dropHint = ref<{ sessionId: string; place: 'before' | 'after' } | null>(null)
let lastPointerX = 0
let lastPointerY = 0
let suppressNextTabClick = false

const visibleSessions = computed(() => sessions.value.filter(session => !session.popoutLabel))

const contextMenuVisible = ref(false)
const contextMenuX = ref(0)
const contextMenuY = ref(0)

const commandDialogVisible = ref(false)
const isNewCommand = ref(true)
const editCommand = ref<QuickCommand>(emptyQuickCommand())

const snippetSearch = ref('')
const expandedGroups = ref<Set<string>>(new Set())
const rightPanelMode = ref<RightPanelMode>('snippets')
const sftpPanelMap = new Map<
  string,
  {
    getCurrentPath: () => string
    openPath?: (path: string) => void
  }
>()
const terminalObservers = new Map<string, ResizeObserver>()

const unlisteners: UnlistenFn[] = []

const snippetSidebarVisible = computed(
  () => rightPanelMode.value === 'snippets' && activePanelMode.value === 'terminal'
)
const activePanelMode = computed(() => getActiveSession()?.panelMode ?? 'terminal')

const quickCommands = computed(() => config.value?.quickCommands ?? [])

const existingCommandGroups = computed(() => {
  const names = new Set<string>()
  for (const item of quickCommands.value) if (item.group) names.add(item.group)
  return Array.from(names).sort()
})

const filteredGroupedSnippets = computed(() => {
  const keyword = snippetSearch.value.trim().toLowerCase()
  const groups = new Map<string, QuickCommand[]>()
  for (const item of quickCommands.value) {
    if (keyword) {
      const haystack = `${item.name} ${item.command} ${item.group ?? ''}`.toLowerCase()
      if (!haystack.includes(keyword)) continue
    }
    const groupName = item.group || '未分组'
    if (!groups.has(groupName)) groups.set(groupName, [])
    groups.get(groupName)!.push(item)
  }
  return Array.from(groups.entries()).map(([groupName, commands]) => ({
    groupName,
    commands,
    count: commands.length
  }))
})

const groupedServers = computed(() => {
  const groups = new Map<string, ServerConfig[]>()
  for (const server of config.value?.servers ?? []) {
    const groupName = server.group || '未分组'
    if (!groups.has(groupName)) groups.set(groupName, [])
    groups.get(groupName)!.push(server)
  }
  return groups
})

function ensureQuickCommands() {
  if (!config.value) return
  if (!config.value.quickCommands) config.value.quickCommands = []
}

async function reloadConfig() {
  config.value = await api.getConfig()
  ensureQuickCommands()
  // 默认展开全部分组（搜索时也会看到内容）
  if (expandedGroups.value.size === 0) {
    for (const item of config.value.quickCommands ?? []) expandedGroups.value.add(item.group || '未分组')
  }
}

function getActiveSession(): TermSession | undefined {
  return sessions.value.find(item => item.uiId === activeSessionId.value)
}

function syncSessionFlags() {
  const serverIds = new Set<string>()
  const connectingIds = new Set<string>()
  for (const session of sessions.value) {
    if (session.connecting) connectingIds.add(session.serverId)
    if (!session.closed) serverIds.add(session.serverId)
  }
  syncActiveSshServerIds(Array.from(serverIds))
  syncConnectingSshServerIds(Array.from(connectingIds))
}

async function copySelection() {
  const text = selectedTerminalText()
  if (!text) {
    ElMessage.warning('没有选中文本，未复制')
    return
  }
  try {
    await navigator.clipboard.writeText(text)
    const preview = text.length > 48 ? `${text.slice(0, 48)}…` : text
    ElMessage.success(`已复制：${preview}`)
  } catch {
    ElMessage.error('复制失败，请检查剪贴板权限')
  }
}

function selectedTerminalText(): string {
  const session = getActiveSession()
  if (!session) return ''
  return (session.terminal.getSelection() || '').replace(/\u00a0/g, ' ').trim()
}

async function pasteFromClipboard() {
  const session = getActiveSession()
  if (!session || session.closed) return
  try {
    const text = await navigator.clipboard.readText()
    if (!text) return
    session.terminal.paste(text)
    session.terminal.focus()
  } catch {
    ElMessage.error('粘贴失败，请检查剪贴板权限')
  }
}

function bindTerminalClipboard(terminal: Terminal) {
  terminal.attachCustomKeyEventHandler(event => {
    if (event.type !== 'keydown') return true
    const key = event.key.toLowerCase()
    const withCtrl = event.ctrlKey || event.metaKey

    if (withCtrl && event.shiftKey && key === 'c') {
      event.preventDefault()
      copySelection()
      return false
    }
    if (withCtrl && event.shiftKey && key === 'v') {
      event.preventDefault()
      pasteFromClipboard()
      return false
    }
    if (withCtrl && !event.shiftKey && key === 'c') {
      if (selectedTerminalText()) {
        event.preventDefault()
        copySelection()
        return false
      }
    }
    if (withCtrl && !event.shiftKey && key === 'v') {
      event.preventDefault()
      pasteFromClipboard()
      return false
    }
    return true
  })
}

function hideContextMenu() {
  contextMenuVisible.value = false
}

function onTerminalContextMenu(event: MouseEvent) {
  event.preventDefault()
  contextMenuX.value = event.clientX
  contextMenuY.value = event.clientY
  contextMenuVisible.value = true
}

function onContextCopy() {
  hideContextMenu()
  copySelection()
}

function onContextPaste() {
  hideContextMenu()
  pasteFromClipboard()
}

function fitActiveTerminal() {
  const session = getActiveSession()
  if (!session || session.panelMode !== 'terminal' || session.popoutLabel) return
  const container = document.getElementById(`term-${session.uiId}`)
  if (!container || container.clientWidth < 40 || container.clientHeight < 40) return
  try {
    session.fitAddon.fit()
  } catch {
    // 容器可能暂时不可见
  }
}

function observeTerminalSize(session: TermSession, container: HTMLElement) {
  terminalObservers.get(session.uiId)?.disconnect()
  const observer = new ResizeObserver(() => {
    if (session.uiId !== activeSessionId.value) return
    if (session.panelMode !== 'terminal' || session.popoutLabel) return
    if (container.clientWidth < 40 || container.clientHeight < 40) return
    try {
      session.fitAddon.fit()
    } catch {
      // 容器可能暂时不可见
    }
  })
  observer.observe(container)
  terminalObservers.set(session.uiId, observer)
}

async function openSessionForServer(serverId: string) {
  if (sessions.value.some(item => item.serverId === serverId && item.connecting)) return
  connectPopoverVisible.value = false
  if (!config.value) await reloadConfig()
  const server = config.value?.servers.find(item => item.id === serverId)
  if (!server) {
    ElMessage.error('找不到服务器配置')
    return
  }

  const uiId = `pending-${Date.now()}-${Math.random().toString(16).slice(2)}`
  const { terminal, fitAddon } = createSshTerminal()
  bindTerminalClipboard(terminal)
  terminal.onData(data => {
    const current = sessions.value.find(item => item.uiId === uiId)
    if (!current || current.closed || current.connecting || !current.sessionId) return
    api.terminalWrite(current.sessionId, data)
  })
  terminal.onResize(({ cols, rows }) => {
    const current = sessions.value.find(item => item.uiId === uiId)
    if (!current?.sessionId || current.connecting) return
    api.terminalResize(current.sessionId, cols, rows)
  })

  const session: TermSession = {
    uiId,
    sessionId: '',
    serverId: server.id,
    title: server.name,
    terminal,
    fitAddon,
    closed: false,
    connecting: true,
    popoutLabel: '',
    panelMode: 'terminal'
  }
  sessions.value.push(session)
  activeSessionId.value = uiId
  syncSessionFlags()
  await nextTick()
  const container = document.getElementById(`term-${uiId}`)
  if (container) {
    terminal.open(container)
    observeTerminalSize(session, container)
    fitAddon.fit()
  }
  terminal.write(`\x1b[32m正在连接 ${server.name}（${server.host}:${server.port}）…\x1b[0m\r\n`)

  try {
    const sessionId = await api.terminalOpen(server.id, 120, 30)
    session.sessionId = sessionId
    session.connecting = false
    syncSessionFlags()
    await nextTick()
    fitAddon.fit()
    terminal.focus()
  } catch (error) {
    ElMessage.error(String(error))
    terminal.dispose()
    terminalObservers.get(uiId)?.disconnect()
    terminalObservers.delete(uiId)
    sessions.value = sessions.value.filter(item => item.uiId !== uiId)
    if (activeSessionId.value === uiId) activeSessionId.value = visibleSessions.value[visibleSessions.value.length - 1]?.uiId ?? ''
    syncSessionFlags()
  }
}

function onOpenSshRequest(payload: unknown) {
  const serverId = String(payload ?? '')
  if (!serverId) return
  openSessionForServer(serverId)
}

async function closeSession(uiId: string) {
  const index = sessions.value.findIndex(item => item.uiId === uiId)
  if (index < 0) return
  const session = sessions.value[index]
  const serverId = session.serverId
  if (session.popoutLabel) {
    const popped = await WebviewWindow.getByLabel(session.popoutLabel)
    await popped?.close()
  }
  if (session.sessionId) await api.terminalClose(session.sessionId)
  session.terminal.dispose()
  sessions.value.splice(index, 1)
  syncSessionFlags()
  sftpPanelMap.delete(uiId)
  terminalObservers.get(uiId)?.disconnect()
  terminalObservers.delete(uiId)
  const stillUsed = sessions.value.some(item => item.serverId === serverId)
  if (!stillUsed) await api.sftpDisconnect(serverId)
  if (activeSessionId.value === uiId) activeSessionId.value = visibleSessions.value[visibleSessions.value.length - 1]?.uiId ?? ''
  await nextTick()
  fitActiveTerminal()
}

async function activateSession(uiId: string) {
  if (suppressNextTabClick) {
    suppressNextTabClick = false
    return
  }
  if (activeSessionId.value === uiId) return
  activeSessionId.value = uiId
  await nextTick()
  fitActiveTerminal()
  const session = getActiveSession()
  if (session && !session.connecting) session.terminal.focus()
}

function onTabGripPointerDown(uiId: string, event: PointerEvent) {
  if (event.button !== 0) return
  event.preventDefault()
  event.stopPropagation()
  draggingSessionId.value = uiId
  lastPointerX = event.clientX
  lastPointerY = event.clientY
  suppressNextTabClick = true
  const grip = event.currentTarget as HTMLElement
  if (grip.setPointerCapture) grip.setPointerCapture(event.pointerId)
  window.addEventListener('pointermove', onTabPointerMove)
  window.addEventListener('pointerup', onTabPointerUp)
}

function onTabPointerMove(event: PointerEvent) {
  if (!draggingSessionId.value) return
  lastPointerX = event.clientX
  lastPointerY = event.clientY
  const hit = elementFromPointIgnoringDrag(event.clientX, event.clientY)
  if (isDropPlaceholder(hit)) return
  const tab = hit instanceof Element ? hit.closest('.session-tab') : null
  if (!(tab instanceof HTMLElement) || !tab.dataset.sessionId) {
    const bar = document.querySelector('.session-tabs')
    const lastSession = visibleSessions.value[visibleSessions.value.length - 1]
    if (bar && hit instanceof Node && bar.contains(hit) && lastSession && lastSession.uiId !== draggingSessionId.value) dropHint.value = { sessionId: lastSession.uiId, place: 'after' }
    return
  }
  const sessionId = tab.dataset.sessionId
  if (sessionId === draggingSessionId.value) {
    dropHint.value = null
    return
  }
  dropHint.value = { sessionId, place: dropPlaceByX(event.clientX, tab) }
}

async function onTabPointerUp() {
  window.removeEventListener('pointermove', onTabPointerMove)
  window.removeEventListener('pointerup', onTabPointerUp)
  const fromId = draggingSessionId.value
  const hint = dropHint.value
  draggingSessionId.value = ''
  dropHint.value = null
  if (!fromId) return
  const tabBar = document.querySelector('.session-tabs')
  const hit = document.elementFromPoint(lastPointerX, lastPointerY)
  const outsideBar = !(tabBar instanceof Element && hit instanceof Node && tabBar.contains(hit))
  if (outsideBar) {
    await popoutSession(fromId)
    return
  }
  if (!hint || fromId === hint.sessionId) return
  const list = [...sessions.value]
  const fromIndex = list.findIndex(item => item.uiId === fromId)
  if (fromIndex < 0) return
  const [moving] = list.splice(fromIndex, 1)
  let toIndex = list.findIndex(item => item.uiId === hint.sessionId)
  if (toIndex < 0) return
  if (hint.place === 'after') toIndex += 1
  list.splice(toIndex, 0, moving)
  sessions.value = list
}

async function popoutSession(uiId: string) {
  const session = sessions.value.find(item => item.uiId === uiId)
  if (!session || session.connecting || session.closed || !session.sessionId || session.popoutLabel) return
  const label = `term-${session.sessionId}`
  const params = new URLSearchParams({
    sessionId: session.sessionId,
    title: session.title,
    serverId: session.serverId
  })
  const webview = new WebviewWindow(label, {
    url: `/#/popout-term?${params.toString()}`,
    title: `${session.title} - SSH`,
    width: 980,
    height: 640,
    minWidth: 640,
    minHeight: 420,
    focus: true
  })
  session.popoutLabel = label
  if (activeSessionId.value === uiId) {
    const remain = visibleSessions.value.find(item => item.uiId !== uiId)
    activeSessionId.value = remain?.uiId ?? ''
  }
  const dockBack = () => {
    session.popoutLabel = ''
    if (!visibleSessions.value.some(item => item.uiId === activeSessionId.value)) activeSessionId.value = session.uiId
    nextTick(() => fitActiveTerminal())
  }
  await webview.once('tauri://error', event => {
    session.popoutLabel = ''
    ElMessage.error(`弹出窗口失败：${event.payload ?? event}`)
  })
  await webview.once('tauri://destroyed', dockBack)
}

function isTabDropHint(sessionId: string, place: 'before' | 'after') {
  return dropHint.value?.sessionId === sessionId && dropHint.value.place === place
}

function toggleGroup(groupName: string) {
  const next = new Set(expandedGroups.value)
  if (next.has(groupName)) next.delete(groupName)
  else next.add(groupName)
  expandedGroups.value = next
}

function isGroupExpanded(groupName: string) {
  // 有搜索词时全部展开，便于浏览结果
  if (snippetSearch.value.trim()) return true
  return expandedGroups.value.has(groupName)
}

function runQuickCommand(item: QuickCommand) {
  const session = getActiveSession()
  if (!session) {
    ElMessage.warning('请先打开 SSH 会话')
    return
  }
  if (session.closed || session.connecting) {
    ElMessage.warning(session.connecting ? '正在连接，请稍候' : '当前会话已断开')
    return
  }
  // Run：发送命令并回车执行
  const payload = item.command.endsWith('\n') ? item.command : `${item.command}\n`
  session.terminal.paste(payload)
  session.terminal.focus()
}

function pasteQuickCommand(item: QuickCommand) {
  const session = getActiveSession()
  if (!session) {
    ElMessage.warning('请先打开 SSH 会话')
    return
  }
  if (session.closed || session.connecting) {
    ElMessage.warning(session.connecting ? '正在连接，请稍候' : '当前会话已断开')
    return
  }
  // Paste：只粘贴不回车，便于改参数
  const text = item.command.replace(/\n$/, '')
  session.terminal.paste(text)
  session.terminal.focus()
}

function openAddCommand(presetGroup?: string) {
  isNewCommand.value = true
  editCommand.value = emptyQuickCommand()
  if (presetGroup) editCommand.value.group = presetGroup
  commandDialogVisible.value = true
}

function openEditCommand(item: QuickCommand) {
  isNewCommand.value = false
  editCommand.value = { ...item }
  commandDialogVisible.value = true
}

async function saveCommand() {
  if (!config.value) return
  ensureQuickCommands()
  if (!editCommand.value.name.trim()) {
    ElMessage.warning('请填写名称')
    return
  }
  if (!editCommand.value.command.trim()) {
    ElMessage.warning('请填写命令内容')
    return
  }

  const groupName = editCommand.value.group?.trim() || null
  const saved: QuickCommand = {
    id: editCommand.value.id || `qc-${Date.now()}`,
    name: editCommand.value.name.trim(),
    command: editCommand.value.command,
    group: groupName
  }

  const list = config.value.quickCommands!
  if (isNewCommand.value) list.push(saved)
  else {
    const index = list.findIndex(item => item.id === saved.id)
    if (index >= 0) list[index] = saved
    else list.push(saved)
  }

  expandedGroups.value = new Set(expandedGroups.value).add(groupName || '未分组')
  await api.saveConfig(config.value)
  commandDialogVisible.value = false
  ElMessage.success('已保存')
}

async function removeCommand(item: QuickCommand) {
  if (!config.value) return
  await ElMessageBox.confirm(`确认删除命令「${item.name}」？`, '删除确认', {
    type: 'warning'
  })
  ensureQuickCommands()
  config.value.quickCommands = config.value.quickCommands!.filter(entry => entry.id !== item.id)
  await api.saveConfig(config.value)
  ElMessage.success('已删除')
}

function toggleSnippetSidebar() {
  if (activePanelMode.value === 'sftp') return
  rightPanelMode.value = rightPanelMode.value === 'snippets' ? 'hidden' : 'snippets'
  nextTick(() => fitActiveTerminal())
}

function bindSftpPanel(sessionId: string, element: unknown) {
  if (element && typeof element === 'object' && 'getCurrentPath' in element) sftpPanelMap.set(sessionId, element as { getCurrentPath: () => string; openPath?: (path: string) => void })
  else sftpPanelMap.delete(sessionId)
}

function activeSftpPanel() {
  return sftpPanelMap.get(activeSessionId.value) ?? null
}

function openSftpPanel() {
  const session = getActiveSession()
  if (!session) {
    ElMessage.warning('请先打开终端会话')
    connectPopoverVisible.value = true
    return
  }
  if (session.connecting) {
    ElMessage.warning('正在连接，请稍候')
    return
  }
  if (session.panelMode === 'sftp') return
  session.panelMode = 'sftp'
  nextTick(() => {
    const cwd = inferSshCwd()
    if (!cwd) return
    const panel = activeSftpPanel()
    if (!panel) return
    if (normalizeRemotePath(panel.getCurrentPath()) === normalizeRemotePath(cwd)) return
    panel.openPath?.(cwd)
  })
}

function openTerminalPanel() {
  const session = getActiveSession()
  if (!session) return
  if (session.panelMode === 'terminal') {
    session.terminal.focus()
    return
  }
  const path = activeSftpPanel()?.getCurrentPath?.() || '/'
  session.panelMode = 'terminal'
  nextTick(() => {
    fitActiveTerminal()
    syncTerminalToSftpPath(path)
    session.terminal.focus()
  })
}

/** 将 shell 提示符中的 ~ 展开为绝对路径 */
function expandShellHome(path: string, username: string): string {
  if (path === '~') return username === 'root' ? '/root' : `/home/${username}`
  if (path.startsWith('~/')) {
    const home = username === 'root' ? '/root' : `/home/${username}`
    return `${home}${path.slice(1)}`
  }
  return path
}

/** 仅接受绝对路径；提示符 \W 往往只有末级目录名（如 nginx），不能当 SFTP 路径 */
function isAbsoluteRemotePath(path: string) {
  const trimmed = path.trim()
  if (!trimmed) return false
  if (trimmed.startsWith('/') || trimmed === '~' || trimmed.startsWith('~/')) return true
  return /^[A-Za-z]:[\\/]?/.test(trimmed)
}

function normalizeRemotePath(path: string) {
  const trimmed = path.trim().replace(/\\/g, '/')
  if (!trimmed || trimmed === '/') return '/'
  return trimmed.replace(/\/+$/, '')
}

/** SFTP 路径转成 Windows 本机路径：/D:/code、/D/code、D:/code → D:\code */
function sftpPathToWindowsNative(path: string): string | null {
  const trimmed = path.trim()
  if (!trimmed || trimmed === '/' || trimmed === '\\') return null
  const withSlash = trimmed.replace(/\//g, '\\')
  const driveColon = withSlash.match(/^\\?([A-Za-z]:)(.*)$/)
  if (driveColon) {
    const rest = driveColon[2]
    if (!rest) return `${driveColon[1].toUpperCase()}\\`
    return `${driveColon[1].toUpperCase()}${rest}`
  }
  const posixDrive = withSlash.match(/^\\([A-Za-z])(\\.+)?$/)
  if (posixDrive) return `${posixDrive[1].toUpperCase()}:${posixDrive[2] || '\\'}`
  return null
}

function normalizeWindowsPath(path: string) {
  return path.replace(/\//g, '\\').replace(/\\+$/, '').toUpperCase()
}

/** 从 xterm 缓冲区倒序推断当前 SSH 工作目录 */
function inferSshCwd(): string | null {
  const session = getActiveSession()
  if (!session || session.closed) return null

  const buffer = session.terminal.buffer.active
  const cursorLine = buffer.baseY + buffer.cursorY
  const scanStart = Math.max(0, cursorLine - 39)

  for (let lineIndex = cursorLine; lineIndex >= scanStart; lineIndex--) {
    const line = buffer.getLine(lineIndex)
    if (!line) continue
    const text = line.translateToString(true).trim()
    if (!text) continue

    // Linux: [user@host /path]# 或 [user@host ~]#
    const bracketMatch = text.match(/\[([^@\]]+)@[^\s\]]+\s+([^\]]+)\][#\$]\s*$/)
    if (bracketMatch) {
      const username = bracketMatch[1]
      const rawPath = bracketMatch[2].trim()
      const absolutePath = expandShellHome(rawPath, username)
      if (isAbsoluteRemotePath(absolutePath)) return absolutePath
    }

    // Linux: user@host:/path$ 或 user@host:~$
    const colonMatch = text.match(/([^@\s]+)@[^:\s]+:([^\s$#]+)[$#]\s*$/)
    if (colonMatch) {
      const username = colonMatch[1]
      const rawPath = colonMatch[2].trim()
      const absolutePath = expandShellHome(rawPath, username)
      if (isAbsoluteRemotePath(absolutePath)) return absolutePath
    }

    // Windows PowerShell: PS C:\foo>
    const psMatch = text.match(/PS\s+([A-Za-z]:\\[^>]*)>\s*$/i)
    if (psMatch) {
      const winPath = psMatch[1].trim()
      if (winPath) return winPath
    }

    // Windows CMD: C:\foo>
    const cmdMatch = text.match(/([A-Za-z]:\\[^>]*)>\s*$/)
    if (cmdMatch) {
      const winPath = cmdMatch[1].trim()
      if (winPath) return winPath
    }
  }

  return null
}

/** 根据提示符判断当前是 cmd 还是 PowerShell（用户可能手动切过） */
function inferWindowsPromptShell(): 'powershell' | 'cmd' {
  const session = getActiveSession()
  if (!session) return 'powershell'
  const buffer = session.terminal.buffer.active
  const cursorLine = buffer.baseY + buffer.cursorY
  const scanStart = Math.max(0, cursorLine - 39)
  for (let lineIndex = cursorLine; lineIndex >= scanStart; lineIndex--) {
    const text = buffer.getLine(lineIndex)?.translateToString(true).trim() ?? ''
    if (!text) continue
    if (/PS\s+[A-Za-z]:\\[^>]*>\s*$/i.test(text)) return 'powershell'
    if (/[A-Za-z]:\\[^>]*>\s*$/.test(text)) return 'cmd'
  }
  return 'powershell'
}

/** 切回 SSH 时，把工作目录切到当前 SFTP 路径 */
function syncTerminalToSftpPath(path: string) {
  const session = getActiveSession()
  if (!session || session.closed || !path) return
  const server = config.value?.servers.find(item => item.id === session.serverId)
  if (server?.os === 'windows') {
    const winPath = sftpPathToWindowsNative(path)
    if (!winPath) return
    const current = inferSshCwd()
    if (current && normalizeWindowsPath(current) === normalizeWindowsPath(winPath)) return
    if (inferWindowsPromptShell() === 'powershell') {
      const escaped = winPath.replace(/'/g, "''")
      session.terminal.paste(`Set-Location -LiteralPath '${escaped}'\n`)
      return
    }
    const escaped = winPath.replace(/"/g, '""')
    session.terminal.paste(`cd /d "${escaped}"\n`)
    return
  }
  const escaped = path.replace(/'/g, `'\\''`)
  session.terminal.paste(`cd '${escaped}'\n`)
}

watch(snippetSidebarVisible, () => {
  nextTick(() => fitActiveTerminal())
})

onMounted(async () => {
  await reloadConfig()

  unlisteners.push(
    await listen<TermDataPayload>('term-data', event => {
      const session = sessions.value.find(item => item.sessionId === event.payload.sessionId)
      if (!session) return
      const binary = atob(event.payload.data)
      const bytes = new Uint8Array(binary.length)
      for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index)
      session.terminal.write(bytes)
    }),
    await listen<TermClosedPayload>('term-closed', event => {
      const session = sessions.value.find(item => item.sessionId === event.payload.sessionId)
      if (!session || session.closed) return
      session.closed = true
      session.connecting = false
      session.terminal.write('\r\n\x1b[31m[会话已断开]\x1b[0m\r\n')
      syncSessionFlags()
    })
  )

  bus.on(OPEN_SSH_EVENT, onOpenSshRequest)
  window.addEventListener('resize', fitActiveTerminal)
  window.addEventListener('click', hideContextMenu)
})

onUnmounted(() => {
  window.removeEventListener('pointermove', onTabPointerMove)
  window.removeEventListener('pointerup', onTabPointerUp)
  for (const unlisten of unlisteners) unlisten()
  bus.off(OPEN_SSH_EVENT, onOpenSshRequest)
  window.removeEventListener('resize', fitActiveTerminal)
  window.removeEventListener('click', hideContextMenu)
  for (const observer of terminalObservers.values()) observer.disconnect()
  terminalObservers.clear()
  for (const session of sessions.value) {
    if (session.sessionId) api.terminalClose(session.sessionId)
    session.terminal.dispose()
    api.sftpDisconnect(session.serverId)
  }
  syncActiveSshServerIds([])
  syncConnectingSshServerIds([])
})
</script>

<template>
  <div class="terminal-view">
    <!-- 顶栏：会话标签 + 新建 -->
    <div class="term-topbar">
      <div class="session-tabs">
        <template v-for="session in visibleSessions" :key="session.uiId">
          <div v-if="isTabDropHint(session.uiId, 'before')" class="drop-placeholder is-tab" />
          <div
            role="button"
            tabindex="0"
            class="session-tab"
            :data-session-id="session.uiId"
            :class="{
              active: session.uiId === activeSessionId,
              closed: session.closed,
              connecting: session.connecting,
              'is-dragging': draggingSessionId === session.uiId
            }"
            :title="session.connecting ? '正在连接…' : '拖动标签排序，拖出标签栏可弹出独立窗口'"
            @click="activateSession(session.uiId)"
            @keydown.enter.prevent="activateSession(session.uiId)"
          >
            <el-icon v-if="session.connecting" class="session-loading is-loading"><Loading /></el-icon>
            <span v-else class="session-dot" />
            <span class="session-title">{{ session.title }}</span>
            <span class="drag-grip" title="拖动标签排序 / 拖出弹出" @pointerdown.stop="onTabGripPointerDown(session.uiId, $event)" @click.stop>
              <GripDots />
            </span>
            <span class="session-close" title="关闭" @click.stop="closeSession(session.uiId)">
              <el-icon :size="12"><Close /></el-icon>
            </span>
          </div>
          <div v-if="isTabDropHint(session.uiId, 'after')" class="drop-placeholder is-tab" />
        </template>

        <el-popover v-model:visible="connectPopoverVisible" placement="bottom-start" :width="320" trigger="click" popper-class="connect-popover">
          <template #reference>
            <button type="button" class="session-add" title="新建会话">
              <el-icon :size="16"><Plus /></el-icon>
            </button>
          </template>
          <div class="connect-panel">
            <div class="connect-panel-title">选择服务器</div>
            <div v-for="[groupName, servers] in groupedServers" :key="groupName" class="connect-group">
              <div class="connect-group-name">{{ groupName }}</div>
              <button v-for="server in servers" :key="server.id" type="button" class="connect-server" :disabled="sessions.some(item => item.serverId === server.id && item.connecting)" @click="openSessionForServer(server.id)">
                <span>{{ server.name }}</span>
                <span class="connect-meta">{{ server.host }}:{{ server.port }}</span>
              </button>
            </div>
            <div v-if="!config?.servers?.length" class="connect-empty">暂无服务器</div>
          </div>
        </el-popover>
      </div>

      <div class="topbar-actions">
        <button type="button" class="mode-btn" :class="{ active: activePanelMode === 'terminal' }" title="终端" @click="openTerminalPanel">SSH</button>
        <button type="button" class="mode-btn" :class="{ active: activePanelMode === 'sftp' }" title="SFTP 文件管理" @click="openSftpPanel">SFTP</button>
        <button v-if="activePanelMode !== 'sftp'" type="button" class="ghost-btn braces-btn" :class="{ active: snippetSidebarVisible }" title="常用命令" @click="toggleSnippetSidebar">
          <span class="braces-icon" aria-hidden="true">{}</span>
        </button>
      </div>
    </div>

    <div v-if="sftpTransfer.running.value && activePanelMode !== 'sftp'" class="sftp-transfer-bar">
      <div class="sftp-transfer-text">
        <strong>SFTP 上传</strong>
        <span>{{ sftpTransfer.percent.value }}%</span>
        <span class="sftp-transfer-route">{{ sftpTransfer.route.value }}</span>
        <span v-if="sftpTransfer.detail?.value">{{ sftpTransfer.detail.value }}</span>
      </div>
      <el-button size="small" type="danger" @click="sftpTransfer.cancel()">终止</el-button>
    </div>

    <div class="term-body">
      <div class="term-workspace">
        <div v-if="visibleSessions.length === 0" class="term-empty">
          <div class="term-empty-title">SSH 终端</div>
          <div class="term-empty-desc">
            {{ sessions.length ? '会话已弹出为独立窗口，关闭窗口后会回到这里' : '点击左上角 + 选择服务器，或在「服务器」页直接点击连接' }}
          </div>
          <el-button v-if="!sessions.length" type="primary" @click="connectPopoverVisible = true"> 新建会话 </el-button>
        </div>

        <div v-for="session in sessions" v-show="session.uiId === activeSessionId && !session.popoutLabel" :key="session.uiId" class="session-workspace">
          <div v-show="session.panelMode === 'terminal'" class="term-main">
            <div class="terminal-frame">
              <div v-if="session.connecting" class="term-connecting">
                <el-icon class="is-loading" :size="22"><Loading /></el-icon>
                <span>正在连接 {{ session.title }}…</span>
              </div>
              <div :id="`term-${session.uiId}`" class="terminal-container" :class="{ 'is-connecting': session.connecting }" @contextmenu="onTerminalContextMenu" />
            </div>
          </div>
          <div v-show="session.panelMode === 'sftp'" class="sftp-main">
            <SftpPanel :ref="element => bindSftpPanel(session.uiId, element)" :server-id="session.serverId" :server-name="session.title" :active="session.panelMode === 'sftp' && session.uiId === activeSessionId && !session.popoutLabel" />
          </div>
        </div>
      </div>

      <!-- 右侧 Snippets 侧栏 -->
      <aside v-show="snippetSidebarVisible" class="snippet-sidebar">
        <div class="snippet-toolbar">
          <button type="button" class="new-snippet-btn" @click="openAddCommand()">
            <el-icon><Plus /></el-icon>
            New Snippet
          </button>
          <div class="snippet-search">
            <el-icon class="search-icon"><Search /></el-icon>
            <input v-model="snippetSearch" type="text" placeholder="搜索命令…" />
          </div>
        </div>

        <div class="snippet-list">
          <div v-if="filteredGroupedSnippets.length === 0" class="snippet-empty">
            {{ snippetSearch ? '无匹配命令' : '暂无常用命令，点击上方新建' }}
          </div>

          <div v-for="group in filteredGroupedSnippets" :key="group.groupName" class="snippet-group">
            <button type="button" class="snippet-group-header" @click="toggleGroup(group.groupName)">
              <el-icon class="group-arrow" :class="{ open: isGroupExpanded(group.groupName) }">
                <ArrowRight />
              </el-icon>
              <el-icon class="group-folder"><Folder /></el-icon>
              <span class="group-name">{{ group.groupName }}</span>
              <span class="group-count">{{ group.count }}</span>
            </button>

            <div v-show="isGroupExpanded(group.groupName)" class="snippet-items">
              <div v-for="item in group.commands" :key="item.id" class="snippet-item" title="双击执行 Run" @dblclick.stop="runQuickCommand(item)">
                <div class="snippet-item-icon">{}</div>
                <div class="snippet-item-main">
                  <div class="snippet-item-top">
                    <div class="snippet-item-name">{{ item.name }}</div>
                    <div class="snippet-run-actions" @click.stop>
                      <button type="button" class="run-btn" @click="runQuickCommand(item)">Run</button>
                      <button type="button" class="paste-btn" @click="pasteQuickCommand(item)">Paste</button>
                    </div>
                  </div>
                  <div class="snippet-item-cmd">{{ item.command }}</div>
                </div>
                <div class="snippet-item-actions" @click.stop>
                  <button type="button" title="编辑" @click="openEditCommand(item)">
                    <el-icon :size="14"><EditPen /></el-icon>
                  </button>
                  <button type="button" title="删除" @click="removeCommand(item)">
                    <el-icon :size="14"><Delete /></el-icon>
                  </button>
                </div>
              </div>
            </div>
          </div>
        </div>
      </aside>
    </div>

    <!-- 右键菜单 -->
    <ul v-show="contextMenuVisible" class="term-context-menu" :style="{ left: `${contextMenuX}px`, top: `${contextMenuY}px` }" @click.stop>
      <li @click="onContextCopy">复制</li>
      <li @click="onContextPaste">粘贴</li>
    </ul>

    <el-dialog v-model="commandDialogVisible" :title="isNewCommand ? 'New Snippet' : '编辑命令'" width="520px" append-to-body>
      <el-form label-width="80px">
        <el-form-item label="名称">
          <el-input v-model="editCommand.name" placeholder="如 重启 nginx" />
        </el-form-item>
        <el-form-item label="分组">
          <el-select v-model="editCommand.group" filterable allow-create clearable default-first-option placeholder="选择或输入分组" style="width: 100%">
            <el-option v-for="groupName in existingCommandGroups" :key="groupName" :label="groupName" :value="groupName" />
          </el-select>
        </el-form-item>
        <el-form-item label="命令">
          <el-input v-model="editCommand.command" type="textarea" :rows="4" placeholder="发送到终端的内容，执行时自动补换行" />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="commandDialogVisible = false">取消</el-button>
        <el-button type="primary" @click="saveCommand">保存</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<style scoped>
.terminal-view {
  --term-bg: var(--app-bg);
  --term-panel: var(--app-panel);
  --term-panel-2: var(--app-panel-2);
  --term-border: var(--app-border);
  --term-text: var(--app-text);
  --term-muted: var(--app-muted);
  --term-accent: #3dd68c;
  --term-accent-dim: rgba(61, 214, 140, 0.15);

  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--app-bg);
  color: var(--app-text);
  border-radius: 0;
  overflow: hidden;
  border: none;
}

.term-topbar {
  display: flex;
  align-items: stretch;
  justify-content: space-between;
  height: 42px;
  background: var(--term-panel);
  border-bottom: 1px solid var(--term-border);
  flex-shrink: 0;
}

.session-tabs {
  display: flex;
  align-items: stretch;
  overflow-x: auto;
  flex: 1;
  min-width: 0;
}

.session-tab {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  max-width: 220px;
  padding: 0 8px 0 12px;
  border: none;
  border-right: 1px solid var(--term-border);
  background: transparent;
  color: var(--term-muted);
  cursor: pointer;
  font-size: 13px;
  user-select: none;
  transition:
    background 0.15s,
    color 0.15s;
}

.session-tab .drag-grip {
  color: var(--term-muted);
  margin-left: auto;
  touch-action: none;
}

.session-tab:hover {
  background: var(--term-panel-2);
  color: var(--term-text);
}

.session-tab.active {
  background: #1e1e2e;
  color: var(--term-text);
}

.session-tab.active .session-dot {
  background: var(--term-accent);
  box-shadow: 0 0 8px rgba(61, 214, 140, 0.6);
}

.session-tab.closed .session-title {
  text-decoration: line-through;
  color: #e85d5d;
}
.session-tab.is-dragging {
  opacity: 0.55;
}

.session-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #5a6578;
  flex-shrink: 0;
}

.session-loading {
  font-size: 14px;
  color: var(--term-accent);
  flex-shrink: 0;
}

.session-title {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  min-width: 0;
}

.session-close {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  border-radius: 4px;
  opacity: 0.5;
  flex-shrink: 0;
}

.session-close:hover {
  opacity: 1;
  background: rgba(255, 255, 255, 0.08);
}

.session-add {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 42px;
  border: 1px solid var(--term-border);
  background: transparent;
  color: var(--term-muted);
  cursor: pointer;
}

.session-add:hover:not(:disabled) {
  color: var(--term-accent);
  background: var(--term-accent-dim);
}

.topbar-actions {
  display: flex;
  align-items: center;
  padding: 0 8px;
  gap: 4px;
  border-left: 1px solid var(--term-border);
}

.mode-btn {
  border: 1px solid var(--term-border);
  background: transparent;
  color: var(--term-muted);
  height: 28px;
  padding: 0 10px;
  border-radius: var(--app-radius, 8px);
  cursor: pointer;
  font-size: 12px;
  font-weight: 600;
  letter-spacing: 0.5px;
}

.mode-btn:hover,
.mode-btn.active {
  color: var(--term-accent);
  background: var(--term-accent-dim);
  border-color: var(--term-accent);
}

.ghost-btn {
  border: 1px solid var(--term-border);
  background: transparent;
  color: var(--term-muted);
  width: 32px;
  height: 28px;
  border-radius: var(--app-radius, 8px);
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  justify-content: center;
}

.braces-icon {
  font-family:
    Cascadia Code,
    Consolas,
    monospace;
  font-size: 14px;
  font-weight: 700;
  letter-spacing: -1px;
  line-height: 1;
}

.ghost-btn:hover,
.ghost-btn.active {
  color: var(--term-accent);
  background: var(--term-accent-dim);
  border-color: var(--term-accent);
}

.term-body {
  display: flex;
  flex: 1;
  min-height: 0;
}

.sftp-transfer-bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 12px;
  border-bottom: 1px solid var(--app-border, #2a3344);
  background: var(--app-panel, #151a22);
}

.sftp-transfer-text {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
  font-size: 12px;
  color: var(--app-muted);
}

.sftp-transfer-text strong {
  color: var(--app-text);
  font-weight: 600;
}

.sftp-transfer-text span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.sftp-transfer-text .sftp-transfer-route {
  flex: 1;
  white-space: pre-line;
  line-height: 1.4;
  color: var(--app-text);
}

.term-workspace {
  flex: 1;
  min-width: 0;
  min-height: 0;
  display: flex;
  flex-direction: column;
  position: relative;
}

.session-workspace {
  flex: 1;
  min-width: 0;
  min-height: 0;
  display: flex;
  flex-direction: column;
  position: relative;
}

.term-main,
.sftp-main {
  flex: 1;
  min-width: 0;
  min-height: 0;
  position: relative;
  background: var(--term-bg);
}

.sftp-main {
  display: flex;
  flex-direction: column;
}

.sftp-main :deep(.sftp-panel) {
  flex: 1;
  min-height: 0;
}

.term-main {
  display: flex;
  flex-direction: column;
  background: var(--term-bg);
  box-sizing: border-box;
}

.terminal-frame {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  box-sizing: border-box;
  padding: 12px 14px;
  border: 1px solid var(--term-border);
  border-radius: 0px;
  overflow: hidden;
  background: #1e1e2e;
  position: relative;
}

.term-connecting {
  position: absolute;
  inset: 0;
  z-index: 2;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 10px;
  color: var(--term-accent);
  background: rgba(30, 30, 46, 0.88);
  font-size: 14px;
}

.terminal-container.is-connecting {
  opacity: 0.35;
}

.term-empty {
  height: 100%;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  color: var(--term-muted);
  border-bottom: 1px solid var(--term-border);
  border-left: 1px solid var(--term-border);
}

.term-empty-title {
  font-size: 22px;
  color: var(--term-text);
  font-weight: 600;
  letter-spacing: 1px;
}

.term-empty-desc {
  font-size: 13px;
  margin-bottom: 8px;
}

.terminal-container {
  flex: 1;
  min-width: 0;
  min-height: 0;
  position: relative;
}

.terminal-container :deep(.xterm) {
  height: 100%;
  padding: 0;
}

.terminal-container :deep(.xterm-viewport) {
  background-color: #1e1e2e;
}

.terminal-container :deep(.xterm-selection div) {
  background-color: rgba(74, 126, 200, 0.45) !important;
}

.snippet-sidebar {
  width: 300px;
  flex-shrink: 0;
  display: flex;
  flex-direction: column;
  background: var(--term-panel);
  border-left: 1px solid var(--term-border);
  min-height: 0;
}

.snippet-toolbar {
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  border-bottom: 1px solid var(--term-border);
}

.new-snippet-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  height: 34px;
  border: 1px dashed var(--term-border);
  border-radius: 8px;
  background: transparent;
  color: var(--term-text);
  cursor: pointer;
  font-size: 13px;
  transition:
    border-color 0.15s,
    background 0.15s,
    color 0.15s;
}

.new-snippet-btn:hover {
  border-color: var(--term-accent);
  color: var(--term-accent);
  background: var(--term-accent-dim);
}

.snippet-search {
  position: relative;
}

.snippet-search .search-icon {
  position: absolute;
  left: 10px;
  top: 50%;
  transform: translateY(-50%);
  color: var(--term-muted);
}

.snippet-search input {
  width: 100%;
  height: 34px;
  box-sizing: border-box;
  padding: 0 10px 0 32px;
  border-radius: 8px;
  border: 1px solid var(--term-border);
  background: var(--term-bg);
  color: var(--term-text);
  outline: none;
  font-size: 13px;
}

.snippet-search input:focus {
  border-color: var(--term-accent);
}

.snippet-list {
  flex: 1;
  overflow-y: auto;
  padding: 8px 0 16px;
}

.snippet-empty {
  padding: 24px 16px;
  text-align: center;
  color: var(--term-muted);
  font-size: 13px;
}

.snippet-group-header {
  width: calc(100% - 16px);
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 0 8px 6px;
  padding: 8px 10px;
  border: 1px solid var(--term-border);
  border-radius: var(--app-radius, 8px);
  background: var(--term-bg);
  color: var(--term-text);
  cursor: pointer;
  font-size: 13px;
  text-align: left;
}

.snippet-group-header:hover {
  background: var(--term-panel-2);
}

.group-arrow {
  transition: transform 0.15s;
  color: var(--term-muted);
}

.group-arrow.open {
  transform: rotate(90deg);
}

.group-folder {
  color: #6aa8ff;
}

.group-name {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: 500;
}

.group-count {
  color: var(--term-muted);
  font-size: 12px;
  min-width: 18px;
  text-align: right;
}

.snippet-items {
  padding: 0 8px 6px;
}

.snippet-item {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  padding: 10px 10px;
  margin-bottom: 6px;
  border: 1px solid var(--term-border);
  border-radius: var(--app-radius, 8px);
  background: var(--term-bg);
  cursor: default;
  transition:
    background 0.12s,
    border-color 0.12s;
}

.snippet-item:hover {
  background: var(--term-panel-2);
  border-color: #323c50;
}

.snippet-item:hover .snippet-item-actions,
.snippet-item:hover .snippet-run-actions {
  opacity: 1;
}

.snippet-item-icon {
  flex-shrink: 0;
  width: 22px;
  height: 22px;
  border-radius: 6px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  font-family:
    Cascadia Code,
    Consolas,
    monospace;
  font-size: 12px;
  font-weight: 700;
  letter-spacing: -1px;
  color: var(--term-accent);
  background: var(--term-accent-dim);
  margin-top: 1px;
}

.snippet-item-main {
  flex: 1;
  min-width: 0;
}

.snippet-item-top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  margin-bottom: 4px;
}

.snippet-item-name {
  font-size: 13px;
  font-weight: 500;
  color: var(--term-text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.snippet-run-actions {
  display: inline-flex;
  gap: 4px;
  opacity: 0;
  transition: opacity 0.12s;
  flex-shrink: 0;
}

.run-btn,
.paste-btn {
  border: none;
  border-radius: 4px;
  height: 22px;
  padding: 0 8px;
  font-size: 11px;
  font-weight: 600;
  cursor: pointer;
}

.run-btn {
  background: var(--term-accent-dim);
  color: var(--term-accent);
}

.paste-btn {
  background: rgba(106, 168, 255, 0.15);
  color: #6aa8ff;
}

.run-btn:hover {
  background: rgba(61, 214, 140, 0.28);
}

.paste-btn:hover {
  background: rgba(106, 168, 255, 0.28);
}

.snippet-item-cmd {
  font-family:
    Cascadia Code,
    Consolas,
    monospace;
  font-size: 12px;
  color: var(--term-muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.snippet-item-actions {
  display: flex;
  gap: 2px;
  opacity: 0;
  transition: opacity 0.12s;
  flex-shrink: 0;
}

.snippet-item-actions button {
  width: 26px;
  height: 26px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--term-muted);
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  justify-content: center;
}

.snippet-item-actions button:hover {
  color: var(--term-text);
  background: rgba(255, 255, 255, 0.08);
}

.connect-panel {
  max-height: 360px;
  overflow-y: auto;
}

.connect-panel-title {
  font-size: 12px;
  color: var(--term-muted);
  margin-bottom: 8px;
  padding: 0 4px;
}

.connect-group {
  margin-bottom: 10px;
}

.connect-group-name {
  font-size: 12px;
  font-weight: 600;
  color: var(--term-muted);
  padding: 4px;
}

.connect-server {
  width: 100%;
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
  padding: 8px 10px;
  margin-bottom: 6px;
  border: 1px solid var(--term-border);
  border-radius: var(--app-radius, 8px);
  background: var(--term-bg);
  color: var(--term-text);
  cursor: pointer;
  text-align: left;
}

.connect-server:hover {
  background: var(--term-accent-dim);
  border-color: var(--term-accent);
}

.connect-meta {
  font-size: 11px;
  color: var(--term-muted);
  font-family: Consolas, monospace;
}

.connect-empty {
  padding: 16px;
  text-align: center;
  color: var(--term-muted);
  font-size: 13px;
}

.term-context-menu {
  position: fixed;
  z-index: 3000;
  margin: 0;
  padding: 4px 0;
  list-style: none;
  min-width: 120px;
  background: var(--term-panel-2);
  border: 1px solid var(--term-border);
  border-radius: 8px;
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.35);
}

.term-context-menu li {
  padding: 8px 16px;
  cursor: pointer;
  font-size: 13px;
}

.term-context-menu li:hover {
  background: var(--term-accent-dim);
  color: var(--term-accent);
}
</style>
