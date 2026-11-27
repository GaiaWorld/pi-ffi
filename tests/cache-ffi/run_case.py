#!/usr/bin/env python3
"""真实共享缓存插件（shared cache plugin）的标准 FFI 双进程（two-process）验证。"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import time

# 独立运行本入口时也必须逐项检查角色退出及原生进程间通信（IPC）。
if not __debug__:
    raise RuntimeError('测试需要启用 Python 断言（assertion）')

cancellation_requested = False


def request_cancellation(_signum, _frame):
    # 只记请求；在实际子进程登记后进入 finally，避免启动窗口遗漏持有。
    global cancellation_requested
    cancellation_requested = True


signal.signal(signal.SIGTERM, request_cancellation)
signal.signal(signal.SIGINT, request_cancellation)

FIXTURE = Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument('attempt')
parser.add_argument('--state-root', required=True, type=Path)
parser.add_argument('--artifacts-dir', required=True, type=Path)
parser.add_argument('--sequence', choices=('121','212'), required=True)
parser.add_argument('--form', choices=('manual','macro','optional'), required=True)
parser.add_argument('--runtime', choices=('none', 'some'), default='none')
args = parser.parse_args()
if args.runtime == 'some' and args.form != 'optional':
    parser.error('--runtime some 需要 --form optional')
ROOT = args.state_root.resolve()
assert re.fullmatch(r'[a-zA-Z0-9_-]+', args.attempt)
assert 'microsoft' in os.uname().release.lower()
state = ROOT / 'runs' / ('cache-ffi-' + args.attempt)
deployment = ROOT / 'runs' / ('cache-ffi-deployment-' + args.attempt)
state.mkdir(exist_ok=False)
deployment.mkdir(exist_ok=False)
files = [path for path in args.artifacts_dir.iterdir() if path.is_file() and path.name != 'record.json']
artifacts = {}
for source in files:
    destination = deployment / source.name
    shutil.copy2(source, destination)
    artifacts[source.name] = {'sha256': hashlib.sha256(destination.read_bytes()).hexdigest(), 'source': str(source)}
(state / 'artifacts.json').write_text(json.dumps(artifacts, indent=2) + '\n')
symbols = subprocess.run(['nm', '-D', '--defined-only', str(deployment / 'libcache_ffi_plugin.so')],
                         text=True, capture_output=True, check=True).stdout
(state / 'exports.txt').write_text(symbols)
exports = {line.split()[-1] for line in symbols.splitlines() if line.split() and line.split()[-1].startswith('pi_ffi_')}
assert exports == {'pi_ffi_metadata_v1', 'pi_ffi_initialize_v1', 'pi_ffi_initialize_v2', 'pi_ffi_close_v1'}, exports
environment = dict(os.environ, LD_LIBRARY_PATH=str(deployment), TMPDIR=str(state), PI_P2P_SERVICE_TRACE_RPC='1',
                   PI_FFI_INITIALIZATION_SEQUENCE=','.join(args.sequence), PI_FFI_TEST_RUNTIME=args.runtime)
with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as allocation:
    allocation.bind(('127.0.0.1', 0))
    port = allocation.getsockname()[1]
children = []
start = time.monotonic()
try:
    commands = [('host', [str(deployment / 'cache-ffi-host'), str(state), str(deployment), str(port)]),
                ('client', [str(deployment / 'cache-native-probe'), 'ffi-client', str(state)])]
    for role, command in commands:
        output = (state / (role + '.log')).open('wb')
        child = subprocess.Popen(command, env=environment, stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
        children.append((role, child, output, command))
        if cancellation_requested:
            raise InterruptedError('测试协调程序收到取消请求')
    assert children[0][1].pid != children[1][1].pid
    while any(child.poll() is None for _, child, _, _ in children):
        if cancellation_requested:
            raise InterruptedError('测试协调程序收到取消请求')
        assert time.monotonic() - start < 75, 'real FFI two-process test timed out'
        if any(child.poll() not in (None, 0) for _, child, _, _ in children):
            raise RuntimeError('real child failed; original outputs retained')
        time.sleep(0.05)
finally:
    for role, child, output, command in children:
        if child.poll() is None:
            try:
                os.killpg(child.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
        output.close()
    results = [{'role': role, 'pid': child.pid, 'exit_code': child.returncode, 'command': command}
               for role, child, _, command in children]
    (state / 'results.json').write_text(json.dumps(results, indent=2) + '\n')

assert all(item['exit_code'] == 0 for item in results), results
host = (state / 'host.log').read_text()
client = (state / 'client.log').read_text()
expected = [int(port) for port in (state / 'expected-ports.txt').read_text().split(',')]
peer = (state / 'host-ready').read_text().split()[1]
finished = [line for line in client.splitlines() if '[p2p-service][rpc] event=service_rpc_finish ' in line]
assert len(expected) == 105
assert len(finished) == len(expected)
for index, (line, port) in enumerate(zip(finished, expected), 1):
    assert f' service_request_id={index} ' in line
    assert f' peer={peer} ' in line
    assert f' port={port} ' in line
    for fact in (' actual_source=ipc ', ' fallback_happened=false ', ' result=ok ',
                 ' p2p_call_started=true ', ' p2p_call_finished=true '):
        assert fact in line, line
assert host.count('actual_calls=2 uuid_ok=true full_semver_ok=true side_effects=false') == 9
assert 'host ffi_assertions_complete=true actual_metadata_calls=18 input_return=true failed_init_preserved=true' in host
assert host.count('success=true input_arc_count=1 retained_mapping=true file_handles=0') == 2
assert host.count('host initialization_data non_clone=true shared_instance=true consumed_once=true released=true') == 1
for version in (1, 2):
    assert host.count(f'plugin initialization_callback_version={version} ') == args.sequence.count(str(version))
    assert host.count(f'host selected_initialization_version={version} ') == args.sequence.count(str(version))
if args.form == 'manual':
    assert host.count('plugin cross_version_conflict=true ') == 3
    assert host.count('plugin close_in_flight_busy=true') == 3

assert 'client baseline_complete=true' in client
assert client.count('client discovered_services_running=59') == 2
runtime_ids = dict(re.findall(r'^host runtime_task label=(\w+) id=(\d+) owner_verified=true$', host, re.MULTILINE))
returned_ids = dict(re.findall(r'^host runtime_after_close label=(\w+) id=(\d+) input_returned=true owner_task=true$', host, re.MULTILINE))
assert runtime_ids == returned_ids
if args.runtime == 'some':
    assert runtime_ids.keys() == {'red', 'blue', 'invalid'}
    assert len(set(runtime_ids.values())) == 3
else:
    assert not runtime_ids
verdict = {'passed': True, 'roles': results, 'ffi_plugin_loaded': True, 'initialization_versions': [int(v) for v in args.sequence], 'plugin_form': args.form, 'same_dual_artifact': True, 'non_clone_data': True, 'shared_data_instance': True, 'standard_metadata_calls': 18,
           'runtime': args.runtime, 'runtime_ids': runtime_ids,
           'successful_initialization_rounds': 2, 'failed_initialization_rounds': 1,
           'native_cache_client_business_calls': 102, 'independent_control_calls_after_close': 3,
           'native_ipc_calls': len(finished), 'fallback_calls': 0,
           'scope': 'standard FFI results, metadata, idempotency, ownership return, actual native cache client calls; plugin internal shutdown is transparent',
           'elapsed_seconds': time.monotonic() - start}
(state / 'verdict.json').write_text(json.dumps(verdict, indent=2) + '\n')
print(json.dumps(verdict))
