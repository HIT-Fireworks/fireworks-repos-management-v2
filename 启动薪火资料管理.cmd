@echo off
chcp 65001 >nul
cd /d "%~dp0"
title 薪火资料管理
if not exist "薪火资料管理.exe" (
  echo 安装包不完整：缺少“薪火资料管理.exe”。
  echo 请重新下载并完整解压 ZIP 文件。
  pause
  exit /b 1
)
"%~dp0薪火资料管理.exe"
if errorlevel 1 (
  echo.
  echo 程序没有正常启动。请截图本窗口并联系维护人员。
  pause
)
