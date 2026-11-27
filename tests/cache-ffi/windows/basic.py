#!/usr/bin/env python3
"""原生 Windows x64/MSVC 基本验证（smoke test），由独立客户端（client）验证一次实际缓存业务。"""
from contextlib import ExitStack
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import traceback

HERE = Path(__file__).resolve().parent
FIXTURE = HERE.parent
PROJECT = FIXTURE.parents[1]
INPUTS = PROJECT / 'target/cache-ffi'
CACHE = PROJECT / 'target/cache-ffi-windows'
TARGET = 'x86_64-pc-windows-msvc'
sys.dont_write_bytecode = True
sys.path.insert(0, str(FIXTURE))
from prepare import archive_digest, fixed_cargo_config, unpack_fixed, verify_p2p_source


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')


def rustc(*arguments):
    return subprocess.check_output(['rustc', *arguments], cwd=HERE, encoding='utf-8').strip()


def prepare_build(scratch, output, report):
    """隔离 Windows 构建（build）缓存，校验原版本依赖与同一构建的系统库（standard library）。"""
    identity = rustc('-vV')
    report['rustc'] = identity
    if f'host: {TARGET}' not in identity or not re.search(r'^release: .*?-nightly$', identity, re.MULTILINE):
        raise RuntimeError('请在能运行 Windows x64/MSVC Rust nightly 的终端执行')
    expected = json.loads((FIXTURE / 'dependencies.json').read_text(encoding='utf-8'))
    prepared = INPUTS / 'prepared.json'
    if not prepared.is_file() or json.loads(prepared.read_text(encoding='utf-8')) != expected:
        raise RuntimeError('当前目录缺少已准备的固定依赖（target/cache-ffi/prepared.json）')
    verify_p2p_source(expected, INPUTS / 'inputs/upstream' / f'pi_p2p-{expected["p2p"]}')
    unpack_fixed(INPUTS / 'vendor.tar.gz', expected['vendor_archive_sha256'], ('vendor',), scratch)
    cargo_home = scratch / 'cargo'
    cargo_home.mkdir()
    (cargo_home / 'config.toml').write_text(fixed_cargo_config(scratch / 'vendor'), encoding='utf-8')

    target_dir = CACHE / 'build'
    environment = dict(os.environ, CARGO_HOME=str(cargo_home), CARGO_PROFILE_DEV_DEBUG='0',
                       CARGO_ENCODED_RUSTFLAGS='-C\x1fprefer-dynamic')
    command = ['cargo', 'build', '--offline', '--locked', '--target', TARGET,
               '--target-dir', str(target_dir)]
    report['stage'] = 'build'
    report['build_command'] = command
    print('正在构建 Windows x64/MSVC 制品（artifact）', flush=True)
    with (output / 'build.log').open('wb') as log:
        built = subprocess.run(command, cwd=HERE, env=environment, stdout=log, stderr=subprocess.STDOUT)
    report['build_exit_code'] = built.returncode
    built.check_returncode()
    return target_dir / TARGET / 'debug'


def deploy(built, deployment, report):
    """把实际宿主、插件和同次构建的共享适配库（adapter library）放在同一部署目录。"""
    libdir = Path(rustc('--print', 'target-libdir', '--target', TARGET))
    sysroot = Path(rustc('--print', 'sysroot'))
    candidates = {}
    for directory in (built, built / 'deps', libdir, libdir.parent / 'bin', sysroot / 'bin'):
        for path in directory.glob('*std-*.dll'):
            if re.fullmatch(r'(?:lib)?std-[a-fA-F0-9]+\.dll', path.name, re.IGNORECASE):
                candidates[(path.name.lower(), archive_digest(path))] = path
    if len(candidates) != 1:
        raise RuntimeError(f'应找到一份当前构建配套的标准库 DLL，实际找到 {list(candidates.values())}')
    standard, = candidates.values()
    adapter = built / 'deps/pi_ffi_adapters.dll'
    if not adapter.is_file():
        adapter = built / 'pi_ffi_adapters.dll'
    for source in (built / 'cache-ffi-host.exe', built / 'cache-native-probe.exe',
                   built / 'cache_ffi_plugin.dll', adapter, standard):
        shutil.copy2(source, deployment)
    (deployment / 'build-id').write_text(
        report['rustc'] + '\nCargo.lock sha256=' + archive_digest(HERE / 'Cargo.lock') + '\n', encoding='utf-8')
    report['artifacts'] = {path.name: archive_digest(path) for path in sorted(deployment.iterdir())}


def run_pair(output, state, deployment, report):
    """启动两个不同进程（process），失败或超时时回收本次仍在运行的进程并记录退出码。"""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as allocation:
        allocation.bind(('127.0.0.1', 0))
        port = allocation.getsockname()[1]
    environment = dict(os.environ, PI_P2P_SERVICE_TRACE_RPC='1', TEMP=str(state), TMP=str(state))
    commands = [('host', [str(deployment / 'cache-ffi-host.exe'), str(state), str(deployment), str(port)]),
                ('client', [str(deployment / 'cache-native-probe.exe'), 'windows-basic', str(state)])]
    children = []
    report['stage'] = 'run'
    print('正在执行一轮 Windows 原生进程间通信（IPC）业务验证', flush=True)
    with ExitStack() as logs:
        try:
            for role, command in commands:
                log = logs.enter_context((output / f'{role}.log').open('wb'))
                child = subprocess.Popen(command, cwd=state, env=environment, stdout=log, stderr=subprocess.STDOUT)
                children.append((role, command, child))
            deadline = time.monotonic() + 75
            while any(child.poll() is None for _, _, child in children):
                if any(child.poll() not in (None, 0) for _, _, child in children):
                    raise RuntimeError('宿主（host）或客户端提前退出，请查看原始日志')
                if time.monotonic() >= deadline:
                    raise TimeoutError('双进程基本验证超过 75 秒')
                time.sleep(0.05)
        finally:
            for _, _, child in children:
                if child.poll() is None:
                    child.terminate()
                    child.wait(timeout=5)
            report['processes'] = [dict(role=role, command=command, pid=child.pid, exit_code=child.returncode)
                                   for role, command, child in children]
            for name in ('host-ready', 'expected-ports.txt', 'input-effects'):
                if (state / name).is_file():
                    shutil.copy2(state / name, output)


def verify(output, report):
    """只以进程退出码、实际 FFI 和实际传输（transport）日志判断结果。"""
    processes = report['processes']
    if len(processes) != 2 or len({item['pid'] for item in processes}) != 2 or any(item['exit_code'] for item in processes):
        raise RuntimeError(f'两个进程均须正常退出：{processes}')
    host = (output / 'host.log').read_text(encoding='utf-8', errors='replace')
    client = (output / 'client.log').read_text(encoding='utf-8', errors='replace')
    assert 'host metadata_api_phase=before-init actual_calls=2 uuid_ok=true full_semver_ok=true side_effects=false' in host
    assert 'plugin initialization_callback_version=2 non_clone_data=true' in host
    assert 'host windows_basic_complete=true actual_metadata_calls=2 input_return=true' in host
    assert 'client windows_basic_complete=true value_ok=true after_close_control=true' in client
    expected = (output / 'expected-ports.txt').read_text(encoding='utf-8').split(',')
    peer = (output / 'host-ready').read_text(encoding='utf-8').split()[1]
    completed = [line for line in client.splitlines() if '[p2p-service][rpc] event=service_rpc_finish ' in line]
    assert len(completed) == len(expected) == 4
    for index, (line, port) in enumerate(zip(completed, expected), 1):
        for fact in (f' service_request_id={index} ', f' peer={peer} ', f' port={port} ',
                     ' actual_source=ipc ', ' fallback_happened=false ', ' result=ok ',
                     ' p2p_call_started=true ', ' p2p_call_finished=true '):
            assert fact in line, line
    report.update(stage='complete', passed=True, native_ipc_calls=len(completed), fallback_calls=0)


def main():
    if sys.platform != 'win32':
        raise RuntimeError('请在 Windows 本机运行此基本验证入口')
    if not __debug__:
        raise RuntimeError('测试需要启用 Python 断言（assertion）')
    stamp = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    output = CACHE / 'results' / stamp
    output.mkdir(parents=True)
    report = {'passed': False, 'stage': 'preflight', 'target': TARGET}
    print(f'结果目录：{output}', flush=True)
    try:
        with tempfile.TemporaryDirectory(prefix='pffiw-') as temporary:
            scratch = Path(temporary)
            state = scratch / 'state'
            deployment = scratch / 'deployment'
            state.mkdir()
            deployment.mkdir()
            built = prepare_build(scratch, output, report)
            report['stage'] = 'deploy'
            deploy(built, deployment, report)
            run_pair(output, state, deployment, report)
        report['stage'] = 'verify'
        verify(output, report)
    except (Exception, KeyboardInterrupt):
        report['error'] = traceback.format_exc()
        print(report['error'], file=sys.stderr, flush=True)
    finally:
        write_json(output / 'result.json', report)
    bundle = shutil.make_archive(str(output), 'zip', root_dir=output)
    print(f'{"PASS" if report["passed"] else "FAIL"} {bundle}', flush=True)
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    sys.exit(main())
