<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { api } from '../api'

// embedded 为 true 时隐藏大标题（嵌在设置弹窗里）
const { embedded = false } = defineProps<{
  embedded?: boolean
}>()

const activeSections = ref(['start'])
const mcpPort = ref(17423)

onMounted(async () => {
  const config = await api.getConfig()
  mcpPort.value = config.mcp.port
})
</script>

<template>
  <div class="help-view" :class="{ embedded }">
    <div v-if="!embedded" class="view-header">
      <h2>使用说明</h2>
    </div>
    <el-collapse v-model="activeSections">
      <el-collapse-item title="一、这是什么 / 快速开始" name="start">
        <div class="help-body">
          <p>本工具把日常运维的四件事集中到一个软件里：<b>SSH 隧道</b>、<b>后端部署</b>（Windows 双机负载）、<b>前端部署</b>（nginx 静态资源）、<b>Docker 部署</b>，另附 SSH 终端、SFTP 文件管理和一键远程桌面。所有流量走 SSH 加密，内网 Windows 服务器经 Linux 跳板机访问。</p>
          <p><b>第一次使用（三步）：</b></p>
          <ol>
            <li>找同事把配置导给你：对方在「设置 → 导出配置」生成 JSON 文件，你在「设置 → 导入配置」一键导入（包含服务器、隧道、部署映射全部内容）。</li>
            <li>到「服务器」页，逐台点<b>「测试连接」</b>确认连通（第一次连接会自动记录服务器指纹）。</li>
            <li>确认配置里的<b>本地路径</b>（后端产物目录、前端 dist 目录）改成你自己电脑上的实际路径（「后端部署 → 项目配置」和「前端部署 → 编辑」里改）。</li>
          </ol>
        </div>
      </el-collapse-item>

      <el-collapse-item title="二、服务器与分组" name="servers">
        <div class="help-body">
          <ul>
            <li>服务器可按分组管理；双击分组名称即可改名（该组下全部条目的分组字段会一起更新）。</li>
            <li>「未分组」不能改名；若要归入某组，请编辑条目并选择或新建分组。</li>
            <li><b>Windows 服务器端口填 SSH 端口（默认 22）</b>，不是远程桌面 3389。测试连接走 SSH，需已开启 OpenSSH Server。</li>
          </ul>
        </div>
      </el-collapse-item>

      <el-collapse-item title="三、隧道（连生产 MySQL / Redis 等）" name="tunnel">
        <div class="help-body">
          <p>
            隧道 = 把服务器内网的端口映射到你本机端口。比如「生产MySQL」隧道开启后，本机连
            <code>127.0.0.1:13306</code> 就等于连上了生产库。
          </p>
          <ul>
            <li>开关即用；断线会<b>自动重连</b>，卡片上能看到活跃连接数和重连次数。</li>
            <li>常用隧道可勾选「自动启动」，软件打开即接通。</li>
            <li>本地端口被占用会立即报错，换个端口即可。</li>
            <li>隧道同样支持分组，双击分组名称即可改名。</li>
          </ul>
        </div>
      </el-collapse-item>

      <el-collapse-item title="四、SSH 终端 / SFTP / 远程桌面" name="terminal">
        <div class="help-body">
          <ul>
            <li><b>SSH 终端</b>：选服务器 → 新建会话，支持多标签。连 Windows 服务器默认进 cmd，输入 <code>powershell</code> 回车可切换。</li>
            <li><b>SFTP 文件管理</b>：双栏浏览本地与远端；支持拖拽上传/下载；右键菜单可新建、重命名、删除、下载等。</li>
            <li><b>路径同步</b>：SSH 终端里 <code>cd</code> 切换目录后，同一会话的 SFTP 远端路径会跟着更新；在 SFTP 进入目录也会尽量与终端工作目录对齐。</li>
            <li><b>远程桌面</b>：在「服务器」页 Windows 行点「远程桌面」。分辨率在编辑服务器时设置，连接时直接使用；经跳板机建隧道并拉起 mstsc。你只需在 mstsc 窗口输入 Windows 账号密码。</li>
          </ul>
        </div>
      </el-collapse-item>

      <el-collapse-item title="五、后端部署（重点）" name="backend">
        <div class="help-body">
          <p><b>三种部署方式：</b></p>
          <ul>
            <li><b>上传并立即替换</b>：完整流程一步到位（校验产物 → 压缩上传 → 同步备机 → 备份 → 滚动替换 → 健康检查）。</li>
            <li><b>仅上传到中转</b>：只把包传到服务器中转目录（如 <code>D:\code\sites\devlop\20260812-功能名\</code>），<b>不动线上</b>。适合白天先传包。</li>
            <li><b>从中转替换</b>：低峰期把已中转的包替换到线上，秒级完成。在「发布历史」找到「待替换」记录点<b>执行替换</b>即可。</li>
          </ul>
          <p><b>安全机制（自动，无需操心）：</b></p>
          <ul>
            <li>产物超过 24 小时会警告，防止发旧包。</li>
            <li>滚动发布：主服务器替换并健康检查通过后才动备机，线上始终有一台在服务。</li>
            <li>替换前 bin 自动备份；「附加备份」勾选后还会把整个应用目录复制成 <code>目录名-日期</code>。</li>
            <li>替换/回滚可自定义停止、启动脚本。默认提供 <b>IIS</b>（只停物理路径匹配到的站点或程序池，不停整个 IIS）和 <b>Java</b>（只停对应 Windows 服务）两套方案，可在项目配置里改。</li>
            <li>出问题到「发布历史」点<b>回滚</b>，一键恢复替换前的 bin。回滚成功后原记录会标成「已回滚」，并新增一条「回滚完成」记录。</li>
          </ul>
          <p><b>推荐节奏</b>：本地发布产物 → 「仅上传到中转」→ 通知相关人 → 低峰「执行替换」→ 验证 → 有问题立即回滚。</p>
        </div>
      </el-collapse-item>

      <el-collapse-item title="六、前端部署 / Docker 部署" name="frontend">
        <div class="help-body">
          <ul>
            <li><b>前端部署</b>：本地 dist 打包后一次上传到服务器再解压，避免逐文件 SFTP。项目按<b>开发环境 / 正式环境</b>分组，顶部切换环境后再部署，避免发错。直接替换或从中转替换时会做发布前快照，可在「发布历史」点<b>回滚</b>恢复。Windows / Linux 都能部署。同样支持「仅上传到中转」和「从中转替换」。</li>
            <li><b>Docker 部署</b>：点「执行」按配置顺序跑 compose 命令，输出实时回显。目标可分组，双击分组名称即可改名。</li>
            <li>新增前端项目：「前端部署 → 添加项目」，填本地 dist 目录和服务器 nginx 目录即可。</li>
          </ul>
        </div>
      </el-collapse-item>

      <el-collapse-item title="七、AI 接入（MCP）" name="mcp">
        <div class="help-body">
          <p>
            开启后，Cursor 等 AI 客户端可以直接调用本工具：AI 构建完包 → 调用
            <code>backend_deploy</code>/<code>frontend_deploy</code> 上传部署 → 轮询 <code>get_task_status</code> 拿结果。
          </p>
          <p><b>接入步骤：</b></p>
          <ol>
            <li>「设置 → MCP 服务」打开开关，选权限级别，保存。</li>
            <li>复制接入配置，粘贴到 Cursor 的 <code>mcp.json</code>：</li>
          </ol>
          <pre class="help-code">
{
  "mcpServers": {
    "shhd-deploy": { "url": "http://127.0.0.1:{{ mcpPort }}/mcp" }
  }
}</pre
          >
          <p><b>权限级别：</b></p>
          <ul>
            <li><b>只读</b>：AI 只能查配置、发布历史、任务状态。</li>
            <li><b>仅中转（推荐）</b>：AI 能传包到中转，但<b>替换线上必须人在软件里点</b>，兼顾效率与安全。</li>
            <li><b>完全访问</b>：AI 可替换线上、回滚、Docker 部署，谨慎开启。</li>
          </ul>
          <p>示例提示词：「发布 client 项目并上传到中转：先执行发布脚本，然后用 shhd-deploy 的 backend_deploy（mode=stage，releaseName=今天日期-本次功能名），最后轮询任务结果告诉我。」</p>
          <p class="help-note">服务只监听本机 127.0.0.1，局域网内其他电脑无法访问。</p>
        </div>
      </el-collapse-item>

      <el-collapse-item title="八、常见问题" name="faq">
        <div class="help-body">
          <ul>
            <li><b>连不上 Windows 服务器</b>：确认已开启 OpenSSH Server，且端口填的是 <b>22</b>（不是 RDP 的 3389）。管理员 PowerShell：<code>Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0</code> 后 <code>Start-Service sshd</code>。</li>
            <li><b>提示"主机密钥指纹变化"</b>：说明服务器重装过或存在风险。确认无误后删除配置目录下 known_hosts.json 中对应记录。</li>
            <li><b>隧道端口被占用</b>：换个本地端口，或找到占用进程关掉。</li>
            <li><b>替换提示文件被占用</b>：到「项目配置」填入 IIS 或 Java 停止/启动脚本。IIS 方案只停本项目站点/程序池；Java 方案请改成实际 Windows 服务名。SSH 账号需要有管理对应服务的权限。</li>
            <li><b>健康检查一直失败</b>：到「后端部署 → 项目配置」核对健康检查地址（必须是服务器本机可访问的 localhost 地址）。</li>
            <li><b>日期备份目录越积越多</b>：目前不自动清理，请定期手动删除服务器上的 <code>目录名-日期</code> 旧备份。</li>
            <li><b>换电脑</b>：旧电脑导出配置 → 新电脑导入，再把配置里的本地路径改成新电脑的路径。</li>
            <li><b>想改分组名</b>：在服务器 / 隧道 / Docker 部署页双击分组名称；「未分组」请通过编辑条目指定新分组。</li>
          </ul>
        </div>
      </el-collapse-item>
    </el-collapse>
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
.help-view :deep(.el-collapse-item__header) {
  padding: 0 16px;
}
.help-view :deep(.el-collapse-item__wrap) {
  padding: 0;
}
.help-view :deep(.el-collapse-item__content) {
  padding: 12px 16px 16px;
}
.help-body {
  font-size: 13px;
  line-height: 1.9;
  color: var(--el-text-color-regular);
  padding: 0;
}
.help-body code {
  background: var(--app-bg, #0f1218);
  border: 1px solid var(--app-border, #2a3344);
  padding: 1px 6px;
  border-radius: 6px;
  font-family: Consolas, Menlo, monospace;
}
.help-code {
  background: var(--app-bg, #0f1218);
  border: 1px solid var(--app-border, #2a3344);
  border-radius: var(--app-radius, 8px);
  padding: 10px 14px;
  font-family: Consolas, Menlo, monospace;
  font-size: 12px;
  line-height: 1.6;
}
.help-note {
  color: var(--el-text-color-secondary);
  font-size: 12px;
}
</style>
