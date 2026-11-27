#!/usr/bin/env python3
"""一次性准备固定测试依赖（fixed test dependencies）；缓存保留在本仓 target 中。"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import sys
import tarfile
import tempfile

HERE = Path(__file__).resolve().parent
PROJECT = HERE.parents[1]
CACHE = PROJECT / 'target/cache-ffi'


def archive_digest(path):
    checksum = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            checksum.update(block)
    return checksum.hexdigest()


def fixed_cargo_config(vendor):
    """为离线构建（offline build）指向同一份已准备的依赖源码。"""
    return (
        '[registries.yn]\nindex = "http://ser.yinengyun.com:10082/tech/crates-io.git"\n'
        '[source.crates-io]\nreplace-with = "fixed-inputs"\n'
        '[source.yn-fixed]\nregistry = "http://ser.yinengyun.com:10082/tech/crates-io.git"\n'
        'replace-with = "fixed-inputs"\n[source.fixed-inputs]\ndirectory = '
        + json.dumps(vendor.as_posix(), ensure_ascii=False) + '\n'
    )


def unpack_fixed(source, checksum, roots, destination):
    if not source.is_file() or archive_digest(source) != checksum:
        raise RuntimeError(f'固定依赖包缺失或摘要不匹配：{source}')
    with tarfile.open(source, 'r:gz') as archive:
        members = archive.getmembers()
        for member in members:
            path = PurePosixPath(member.name)
            if (path.is_absolute() or '..' in path.parts or not path.parts
                    or path.parts[0] not in roots or not (member.isfile() or member.isdir())):
                raise RuntimeError('固定依赖包成员不符合准备范围：' + member.name)
        archive.extractall(destination, members=members)


def verify_p2p_source(expected, directory):
    """核对提交（commit）中全部文件的路径及内容。"""
    files = []
    if directory.is_symlink() or not directory.is_dir():
        raise RuntimeError(f'固定 P2P 源码目录缺失或不匹配：{directory}')
    for path in directory.rglob('*'):
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            raise RuntimeError(f'固定 P2P 源码包含非普通文件：{path}')
        if path.is_file():
            files.append((path.relative_to(directory).as_posix(), archive_digest(path)))
    checksum = hashlib.sha256(json.dumps(sorted(files), separators=(',', ':')).encode()).hexdigest()
    if checksum != expected['p2p_source']['tree_sha256']:
        raise RuntimeError(f'固定 P2P 源码摘要不匹配：{directory}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', type=Path,
                        default=PROJECT / 'docs/evidence/EV-0018/cache-ffi-dependencies.tar.gz')
    parser.add_argument('--p2p-archive', type=Path,
                        default=PROJECT / 'docs/evidence/EV-0023/pi_p2p-1.3.22.tar.gz')
    args = parser.parse_args()
    if sys.platform != 'linux' or 'microsoft' not in os.uname().release.lower():
        raise RuntimeError('此测试入口只允许在 WSL 中准备和运行')
    expected = json.loads((HERE / 'dependencies.json').read_text())
    p2p_name = f'pi_p2p-{expected["p2p"]}'
    p2p_source = CACHE / 'inputs/upstream' / p2p_name
    prepared = CACHE / 'prepared.json'
    cargo = CACHE / 'cargo'
    reuse_base = (cargo / 'config.toml').is_file()
    if reuse_base:
        record = json.loads(prepared.read_text()) if prepared.is_file() else None
        previous = {key: value for key, value in expected.items() if key != 'p2p_source'}
        previous['p2p'] = '1.3.18'
        if (record not in (expected, previous) or not (CACHE / 'inputs').is_dir()
                or not (CACHE / 'vendor.tar.gz').is_file()
                or archive_digest(CACHE / 'vendor.tar.gz') != expected['vendor_archive_sha256']):
            raise RuntimeError('现有依赖缓存与固定准备记录不一致，请先检查保留现场')
        if record == expected:
            verify_p2p_source(expected, p2p_source)
            print('常驻依赖已准备；记录匹配（persistent dependencies ready）')
            return
    elif (CACHE / 'inputs').exists() or (CACHE / 'vendor.tar.gz').exists():
        raise RuntimeError('存在未完成的准备目录，请先检查保留现场；不覆盖已有输入')

    CACHE.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='prepare-', dir=CACHE) as temporary:
        staging = Path(temporary)
        has_p2p_source = p2p_source.exists() or p2p_source.is_symlink()
        if has_p2p_source:
            verify_p2p_source(expected, p2p_source)
        else:
            unpack_fixed(args.p2p_archive, expected['p2p_source']['archive_sha256'], (p2p_name,), staging)
            verify_p2p_source(expected, staging / p2p_name)
        if not reuse_base:
            unpack_fixed(args.archive, expected['archive_sha256'], ('inputs', 'vendor.tar.gz'), staging)
            if archive_digest(staging / 'vendor.tar.gz') != expected['vendor_archive_sha256']:
                raise RuntimeError('固定公共依赖压缩包摘要不匹配')
            shutil.move(str(staging / 'inputs'), str(CACHE / 'inputs'))
            shutil.move(str(staging / 'vendor.tar.gz'), str(CACHE / 'vendor.tar.gz'))
            cargo.mkdir(exist_ok=True)
            (cargo / 'config.toml').write_text(fixed_cargo_config(CACHE / 'vendor'), encoding='utf-8')
        if not has_p2p_source:
            (staging / p2p_name).rename(p2p_source)
        ready = staging / 'prepared.json'
        ready.write_text(json.dumps(expected, indent=2) + '\n')
        ready.replace(prepared)
    print('固定依赖准备完成（fixed dependencies ready）：' + str(CACHE))


if __name__ == '__main__':
    main()
