#!/usr/bin/env python3
"""生成包含全部引用分片的 Windows 发行包。"""
import argparse
import importlib.util
from pathlib import Path
import zipfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('validate_registry', ROOT / 'scripts/validate-registry.py')
registry = importlib.util.module_from_spec(spec)
spec.loader.exec_module(registry)


def package(executable, destination):
    registry.validate(ROOT)
    files = {ROOT / '启动薪火仓库管理.cmd': '启动薪火仓库管理.cmd', executable: '薪火仓库管理.exe'}
    for path in registry.locations(ROOT):
        store = registry.Store(path)
        store.verify()
        for dependency in store.dependencies:
            files[dependency] = dependency.relative_to(ROOT).as_posix()
    destination.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(destination, 'w', compression=zipfile.ZIP_DEFLATED, compresslevel=6) as archive:
        for path, relative in files.items():
            archive.write(path, '薪火仓库管理-Windows/' + relative)
        archive.writestr('薪火仓库管理-Windows/请先看我.txt', '完整解压后运行启动薪火仓库管理.cmd。请勿删除 .fireworks-json 隐藏目录；它保存完整课程数据。\n资料使用预设中文分类，不得自行新增根级分类。\n')
    with zipfile.ZipFile(destination) as archive:
        names = set(archive.namelist())
        assert all('薪火仓库管理-Windows/' + relative in names for relative in files.values())
        assert archive.testzip() is None
    print(f'发行包已验证：{destination}，{len(files)} 个文件')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--executable', type=Path, default=ROOT / 'repository-tui/target/release/repository-tui.exe')
    parser.add_argument('--output', type=Path, default=ROOT / 'dist/fireworks-repository-manager-windows.zip')
    args = parser.parse_args()
    package(args.executable, args.output)
