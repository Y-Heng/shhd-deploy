/** 与后端 service_scripts.rs 保持一致，部署时替换 {appDir} {appBin} {projectName} */

export const IIS_STOP_SCRIPT = `# IIS 方案：只停本项目对应的网站或应用程序池，不会停止整个 IIS
$appDir = '{appDir}'.TrimEnd('\\')
$offlineFile = Join-Path $appDir 'app_offline.htm'
$siteName = $null
$poolName = $null
$matchedKind = $null
try { Import-Module WebAdministration -ErrorAction Stop } catch {
  Write-Output '未能加载 WebAdministration 模块，无法按站点停止 IIS'
  $actionExit = 5
}
if ($actionExit -eq 0) {
  if (-not (Test-Path -LiteralPath $appDir)) { New-Item -ItemType Directory -Path $appDir -Force | Out-Null }
  foreach ($site in Get-Website) {
    $sitePhys = [Environment]::ExpandEnvironmentVariables([string]$site.physicalPath).TrimEnd('\\')
    if ($appDir -ieq $sitePhys) {
      $siteName = [string]$site.Name
      $poolName = [string]$site.applicationPool
      $matchedKind = 'site'
      break
    }
    foreach ($app in @(Get-WebApplication -Site $site.Name -ErrorAction SilentlyContinue)) {
      $appPhys = [Environment]::ExpandEnvironmentVariables([string]$app.PhysicalPath).TrimEnd('\\')
      if ($appDir -ieq $appPhys) {
        $siteName = [string]$site.Name
        $poolName = [string]$app.applicationPool
        $matchedKind = 'app'
        break
      }
    }
    if ($matchedKind) { break }
  }
  if (-not $poolName) {
    Write-Output ('未找到物理路径为 ' + $appDir + ' 的 IIS 站点或应用，请核对接目录或改脚本')
    $actionExit = 5
  } else {
    Set-Content -LiteralPath $offlineFile -Value '<html><body>deploying</body></html>' -Encoding ASCII
    Write-Output ('匹配到 IIS ' + $matchedKind + ' 站点=' + $siteName + ' 程序池=' + $poolName)
    if ($matchedKind -eq 'site' -and $siteName) {
      $siteState = [string](Get-WebsiteState -Name $siteName).Value
      if ($siteState -ne 'Stopped') {
        Stop-Website -Name $siteName
        Write-Output ('已停止网站 ' + $siteName)
      }
    }
    $poolState = [string](Get-WebAppPoolState -Name $poolName).Value
    if ($poolState -ne 'Stopped') {
      Stop-WebAppPool -Name $poolName
      $deadline = (Get-Date).AddSeconds(45)
      do {
        Start-Sleep -Seconds 1
        $poolState = [string](Get-WebAppPoolState -Name $poolName).Value
      } while ($poolState -ne 'Stopped' -and (Get-Date) -lt $deadline)
      Write-Output ('已停止应用程序池 ' + $poolName + ' 状态=' + $poolState)
    } else {
      Write-Output ('应用程序池 ' + $poolName + ' 已是停止状态')
    }
  }
}
`;

export const IIS_START_SCRIPT = `# 先去掉 app_offline，再启动本项目对应的程序池/网站
if ($offlineFile -and (Test-Path -LiteralPath $offlineFile)) {
  Remove-Item -LiteralPath $offlineFile -Force -ErrorAction SilentlyContinue
  Write-Output '已移除 app_offline.htm'
}
if ($poolName) {
  try {
    $poolState = [string](Get-WebAppPoolState -Name $poolName).Value
    if ($poolState -ne 'Started') {
      Start-WebAppPool -Name $poolName
      $deadline = (Get-Date).AddSeconds(30)
      do {
        Start-Sleep -Seconds 1
        $poolState = [string](Get-WebAppPoolState -Name $poolName).Value
      } while ($poolState -ne 'Started' -and (Get-Date) -lt $deadline)
    }
    Write-Output ('已启动应用程序池 ' + $poolName + ' 状态=' + $poolState)
  } catch { Write-Output ('启动应用程序池失败: ' + $_) }
}
if ($matchedKind -eq 'site' -and $siteName) {
  try {
    $siteState = [string](Get-WebsiteState -Name $siteName).Value
    if ($siteState -ne 'Started') { Start-Website -Name $siteName }
    Write-Output ('已启动网站 ' + $siteName)
  } catch { Write-Output ('启动网站失败: ' + $_) }
}
`;

export const JAVA_STOP_SCRIPT = `# Java 方案：只停本项目的 Windows 服务，请把下一行改成实际服务名
$serviceName = '请填写Windows服务名'
if (-not $serviceName -or $serviceName -eq '请填写Windows服务名') {
  Write-Output '请把停止脚本中的服务名改成实际的 Windows 服务名'
  $actionExit = 5
} else {
  $svc = Get-Service -Name $serviceName -ErrorAction SilentlyContinue
  if (-not $svc) {
    Write-Output ('找不到 Windows 服务: ' + $serviceName)
    $actionExit = 5
  } elseif ($svc.Status -ne 'Stopped') {
    Stop-Service -Name $serviceName -Force
    try { $svc.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(45)) } catch {}
    Write-Output ('已停止服务 ' + $serviceName + ' 状态=' + (Get-Service -Name $serviceName).Status)
  } else {
    Write-Output ('服务 ' + $serviceName + ' 已是停止状态')
  }
}
`;

export const JAVA_START_SCRIPT = `if ($serviceName -and $serviceName -ne '请填写Windows服务名') {
  try {
    Start-Service -Name $serviceName
    $svc = Get-Service -Name $serviceName
    try { $svc.WaitForStatus('Running', [TimeSpan]::FromSeconds(45)) } catch {}
    Write-Output ('已启动服务 ' + $serviceName + ' 状态=' + (Get-Service -Name $serviceName).Status)
  } catch { Write-Output ('启动服务失败: ' + $_) }
}
`;
