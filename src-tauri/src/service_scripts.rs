use crate::config::BackendProject;

/// IIS：只停物理路径匹配到的网站或应用程序池，不停 W3SVC / 不做 iisreset
pub const IIS_STOP_SCRIPT: &str = r#"# IIS 方案：只停本项目对应的网站或应用程序池，不会停止整个 IIS
$appDir = '{appDir}'.TrimEnd('\')
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
    $sitePhys = [Environment]::ExpandEnvironmentVariables([string]$site.physicalPath).TrimEnd('\')
    if ($appDir -ieq $sitePhys) {
      $siteName = [string]$site.Name
      $poolName = [string]$site.applicationPool
      $matchedKind = 'site'
      break
    }
    foreach ($app in @(Get-WebApplication -Site $site.Name -ErrorAction SilentlyContinue)) {
      $appPhys = [Environment]::ExpandEnvironmentVariables([string]$app.PhysicalPath).TrimEnd('\')
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
"#;

pub const IIS_START_SCRIPT: &str = r#"# 先去掉 app_offline，再启动本项目对应的程序池/网站
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
"#;

/// Java：只停本项目对应的 Windows 服务（界面「填入 Java 方案」使用同一份脚本）
#[allow(dead_code)]
pub const JAVA_STOP_SCRIPT: &str = r#"# Java 方案：只停本项目的 Windows 服务，请把下一行改成实际服务名
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
"#;

#[allow(dead_code)]
pub const JAVA_START_SCRIPT: &str = r#"if ($serviceName -and $serviceName -ne '请填写Windows服务名') {
  try {
    Start-Service -Name $serviceName
    $svc = Get-Service -Name $serviceName
    try { $svc.WaitForStatus('Running', [TimeSpan]::FromSeconds(45)) } catch {}
    Write-Output ('已启动服务 ' + $serviceName + ' 状态=' + (Get-Service -Name $serviceName).Status)
  } catch { Write-Output ('启动服务失败: ' + $_) }
}
"#;

fn escape_ps(value: &str) -> String {
    value.trim_end_matches('\\').replace('\'', "''")
}

fn fill_placeholders(script: &str, app_dir: &str, app_bin: &str, project_name: &str) -> String {
    script
        .replace("{appDir}", &escape_ps(app_dir))
        .replace("{appBin}", &escape_ps(app_bin))
        .replace("{projectName}", &escape_ps(project_name))
}

/// 解析项目停止/启动脚本；旧配置未写脚本且开启了 stopIisBeforeReplace 时套用 IIS 方案
pub fn resolve_scripts(project: &BackendProject) -> (String, String) {
    let app_bin = project.remote_app_dir.trim_end_matches('\\').to_string();
    let fill = |script: &str| {
        fill_placeholders(script, &project.remote_app_dir, &app_bin, &project.name)
    };
    if !project.stop_script.trim().is_empty() || !project.start_script.trim().is_empty() {
        return (fill(&project.stop_script), fill(&project.start_script));
    }
    if project.stop_iis_before_replace {
        return (fill(IIS_STOP_SCRIPT), fill(IIS_START_SCRIPT));
    }
    (String::new(), String::new())
}

/// 停止脚本 → 替换 → 启动脚本（启动放 finally，替换失败也会拉起来）
pub fn wrap_with_service_scripts(stop_script: &str, start_script: &str, inner_script: &str) -> String {
    let mut script = String::from("$ErrorActionPreference = 'Continue'\n$actionExit = 0\n");
    let has_wrap = !stop_script.trim().is_empty() || !start_script.trim().is_empty();
    if has_wrap {
        script.push_str("try {\n");
        if !stop_script.trim().is_empty() {
            script.push_str(stop_script);
            script.push('\n');
        }
        script.push_str(inner_script);
        script.push_str("\n} finally {\n");
        if !start_script.trim().is_empty() {
            script.push_str(start_script);
            script.push('\n');
        }
        script.push_str("}\n");
    } else {
        script.push_str(inner_script);
        script.push('\n');
    }
    script.push_str("if ($null -eq $actionExit) { $actionExit = 0 }\nexit $actionExit\n");
    script
}
