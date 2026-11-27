#!/usr/bin/env python3
"""常驻真实进程集成测试（real-process integration test）；仅在 WSL 串行执行。"""
import argparse
import contextlib
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time

sys.dont_write_bytecode = True
from prepare import verify_p2p_source

HERE = Path(__file__).resolve().parent
PROJECT = HERE.parents[1]
CACHE = PROJECT / 'target/cache-ffi'
cancellation_requested = False


def request_cancellation(_signum, _frame):
    global cancellation_requested
    cancellation_requested = True


signal.signal(signal.SIGTERM, request_cancellation)
signal.signal(signal.SIGINT, request_cancellation)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def source_identity():
    """记录实际源码；仅遍历相邻库的源码目录，不读取其证据或输出目录。"""
    paths = [PROJECT / 'Cargo.toml']
    for directory in (PROJECT / 'crates', HERE, CACHE / 'inputs',
                      PROJECT.parent / 'pi_result', PROJECT.parent / 'pi_task_pool',
                      PROJECT.parent / 'pi-async-rt_v1.0/crates',
                      PROJECT.parent / 'pi-clock/crates'):
        for current, directories, names in os.walk(directory):
            directories[:] = [name for name in directories if name not in
                               ('target', '.git', '.codex', '.agents', '__pycache__',
                                'docs', 'agent-skills', '.agent_guide')]
            for name in names:
                path = Path(current) / name
                if (directory == CACHE / 'inputs' or path.suffix in ('.rs', '.py')
                        or name in ('Cargo.toml', 'Cargo.lock', 'dependencies.json')):
                    paths.append(path)
    paths += [PROJECT.parent / 'pi_result/Cargo.toml',
              PROJECT.parent / 'pi-async-rt_v1.0/Cargo.toml',
              PROJECT.parent / 'pi-clock/Cargo.toml',
              CACHE / 'cargo/config.toml', CACHE / 'prepared.json', CACHE / 'vendor.tar.gz']
    for directory in (HERE,) + tuple(HERE.parents):
        for name in ('config', 'config.toml'):
            configuration = directory / '.cargo' / name
            if configuration.is_file():
                paths.append(configuration)
    records = {str(path): digest(path) for path in sorted(set(paths))}
    compiler = subprocess.check_output(['rustc', '-vV'], text=True)
    value = {'files': records, 'compiler': compiler}
    value['cargo'] = subprocess.check_output(['cargo', '-V'], text=True)
    value['sha256'] = hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()
    return value


def command(args, log, timeout, environment=None):
    """保存真实退出结果；超时只结束本入口启动的进程组（process group）。"""
    started = datetime.datetime.now(datetime.timezone.utc).isoformat()
    if cancellation_requested:
        raise InterruptedError('测试入口收到取消请求')
    timed_out = False
    with log.open('wb') as output:
        child = subprocess.Popen(args, cwd=HERE, env=environment, stdout=output,
                                 stderr=subprocess.STDOUT, start_new_session=True)
        try:
            deadline = time.monotonic() + timeout
            while child.poll() is None:
                if cancellation_requested:
                    break
                if time.monotonic() >= deadline:
                    timed_out = True
                    break
                time.sleep(0.05)
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait()
    record = {'command': args, 'cwd': str(HERE), 'pid': child.pid,
              'exit_code': child.returncode, 'timed_out': timed_out,
              'cancellation_requested': cancellation_requested, 'started_utc': started,
              'finished_utc': datetime.datetime.now(datetime.timezone.utc).isoformat()}
    log.with_suffix('.json').write_text(json.dumps(record, indent=2) + '\n')
    if timed_out or cancellation_requested or child.returncode:
        raise RuntimeError('测试命令失败；原始日志：' + str(log))


def artifacts(form, identity, output, no_build, build_inputs):
    destination = CACHE / 'artifacts' / form
    record_path = destination / 'record.json'
    if record_path.exists():
        previous = json.loads(record_path.read_text())
        if previous['source_identity'] == identity['sha256'] and all(
                (destination / name).is_file() and digest(destination / name) == checksum
                for name, checksum in previous['files'].items()):
            print('复用已核对制品（reuse verified artifacts）：' + form, flush=True)
            return destination, False
    if no_build:
        raise RuntimeError('缺少与当前源码一致的制品，请先不带 --no-build 运行：' + form)
    vendor_link = CACHE / 'vendor'
    if not vendor_link.exists():
        scratch = Path(build_inputs.enter_context(tempfile.TemporaryDirectory(prefix='pi-ffi-cache-build-')))
        with tarfile.open(CACHE / 'vendor.tar.gz', 'r:gz') as archive:
            members = archive.getmembers()
            for member in members:
                path = Path(member.name)
                if (path.is_absolute() or '..' in path.parts or not path.parts
                        or path.parts[0] != 'vendor' or not (member.isfile() or member.isdir())):
                    raise RuntimeError('固定依赖缓存成员不符合准备范围')
            archive.extractall(scratch, members=members)
        vendor_link.symlink_to(scratch / 'vendor', target_is_directory=True)
        build_inputs.callback(vendor_link.unlink)
        (output / 'build-inputs.json').write_text(json.dumps({'archive_sha256': digest(CACHE / 'vendor.tar.gz'),
            'temporary_input_directory': str(scratch), 'compiled_artifact_directory': str(CACHE / 'build')}, indent=2) + '\n')
    environment = dict(os.environ, CARGO_HOME=str(CACHE / 'cargo'), CARGO_BUILD_JOBS='4',
                       CARGO_PROFILE_DEV_DEBUG='0', RUSTFLAGS='-C prefer-dynamic')
    args = ['cargo', 'build', '--offline', '--locked', '--target-dir', str(CACHE / 'build'), '-p', 'cache-ffi']
    if form != 'manual':
        args += ['--features', 'macro-exports' if form == 'macro' else 'optional-runtime']
    command(args, output / ('build-' + form + '.log'), 600, environment)
    destination.mkdir(parents=True, exist_ok=True)
    if record_path.exists():
        for name in json.loads(record_path.read_text())['files']:
            path = destination / name
            if path.is_file():
                path.unlink()
    target = CACHE / 'build/debug'
    standard_directory = Path(subprocess.check_output(['rustc', '--print', 'target-libdir'], text=True).strip())
    standard, = standard_directory.glob('libstd-*.so')
    sources = [target / 'cache-ffi-host', target / 'cache-native-probe',
               target / 'libcache_ffi_plugin.so', target / 'deps/libpi_ffi_adapters.so', standard]
    for source in sources:
        shutil.copy2(source, destination / source.name)
    (destination / 'build-id').write_text(identity['compiler'] + '\nCargo.lock sha256=' + digest(HERE / 'Cargo.lock') + '\n')
    checksums = {path.name: digest(path) for path in destination.iterdir()
                 if path.is_file() and path.name != 'record.json'}
    record_path.write_text(json.dumps({'source_identity': identity['sha256'], 'files': checksums}, indent=2) + '\n')
    return destination, True


def collect_run_files(run_root, output):
    """归档运行记录及摘要；构建制品由常驻缓存保存。"""
    records = {}
    runs = run_root / 'runs'
    for path in runs.rglob('*'):
        relative = path.relative_to(run_root)
        if (not path.is_file() or 'ipc' in relative.parts
                or any(part.startswith('cache-ffi-deployment-') for part in relative.parts)
                or path.suffix == '.key'):
            continue
        target = output / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)
        checksum = digest(path)
        assert digest(target) == checksum
        records[str(relative)] = checksum
    (output / 'run-files.json').write_text(json.dumps(records, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--form', choices=('all', 'manual', 'macro', 'optional'), default='all')
    parser.add_argument('--sequence', choices=('both', '121', '212'), default='both')
    parser.add_argument('--no-build', action='store_true', help='只允许复用匹配源码的已核制品')
    parser.add_argument('--skip-data-boundaries', action='store_true', help='只运行指定双进程组合')
    args = parser.parse_args()
    if sys.platform != 'linux' or 'microsoft' not in os.uname().release.lower():
        raise RuntimeError('真实进程测试只允许在 WSL 中执行')
    if not (CACHE / 'cargo/config.toml').is_file():
        raise RuntimeError('尚未准备固定依赖，请先运行 tests/cache-ffi/prepare.py')
    expected_dependencies = json.loads((HERE / 'dependencies.json').read_text())
    if json.loads((CACHE / 'prepared.json').read_text()) != expected_dependencies:
        raise RuntimeError('固定依赖准备记录不匹配')
    if digest(CACHE / 'vendor.tar.gz') != expected_dependencies['vendor_archive_sha256']:
        raise RuntimeError('固定公共依赖压缩包摘要不匹配')
    verify_p2p_source(expected_dependencies, CACHE / 'inputs/upstream' / f'pi_p2p-{expected_dependencies["p2p"]}')
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    output = CACHE / 'results' / stamp
    output.mkdir(parents=True)
    identity = source_identity()
    (output / 'sources.json').write_text(json.dumps(identity, indent=2) + '\n')
    run_root = Path(tempfile.mkdtemp(prefix='pi-ffi-cache-run-'))
    (run_root / 'runs').mkdir()
    owner = {'created_by': str(HERE / 'run.py'), 'pid': os.getpid(), 'result_directory': str(output)}
    (run_root / 'owner.json').write_text(json.dumps(owner, indent=2) + '\n')
    print('运行数据（run data）：' + str(run_root), flush=True)
    verdicts = []
    data_boundaries = None
    builds = []
    build_inputs = contextlib.ExitStack()
    try:
        forms = ('manual', 'macro', 'optional') if args.form == 'all' else (args.form,)
        sequences = ('121', '212') if args.sequence == 'both' else (args.sequence,)
        for form in forms:
            bundle, built = artifacts(form, identity, output, args.no_build, build_inputs)
            builds.append({'form': form, 'built': built, 'artifacts': str(bundle)})
            plugin_checksums = []
            for sequence in sequences:
                attempt = form + '-' + sequence
                command([sys.executable, str(HERE / 'run_case.py'), attempt, '--state-root', str(run_root),
                         '--artifacts-dir', str(bundle), '--sequence', sequence, '--form', form],
                        output / (attempt + '.log'), 100)
                state = run_root / 'runs' / ('cache-ffi-' + attempt)
                verdicts.append(json.loads((state / 'verdict.json').read_text()))
                observed = json.loads((state / 'artifacts.json').read_text())
                plugin_checksums.append(observed['libcache_ffi_plugin.so']['sha256'])
            assert len(set(plugin_checksums)) == 1
            if form == 'manual' and not args.skip_data_boundaries:
                command([sys.executable, str(HERE / 'run_data_boundaries.py'), str(run_root), 'manual-' + sequences[0]],
                        output / 'data-boundaries.log', 180)
                data_boundaries = json.loads((run_root / 'runs' / ('data-boundaries-manual-' + sequences[0] + '-verdict.json')).read_text())
        assert source_identity()['sha256'] == identity['sha256'], '测试期间源码发生变化'
        if cancellation_requested:
            raise InterruptedError('测试入口收到取消请求')
        collect_run_files(run_root, output)
        result = {'passed': True, 'builds': builds, 'cases': verdicts,
                  'native_ipc_calls': sum(item['native_ipc_calls'] for item in verdicts),
                  'standard_metadata_calls': sum(item['standard_metadata_calls'] for item in verdicts),
                  'data_boundaries_requested': 'manual' in forms and not args.skip_data_boundaries,
                  'data_boundaries': data_boundaries,
                  'windows_executed': False, 'temporary_directory': str(run_root),
                  'archived_results': str(output), 'temporary_directory_removed': False}
        assert run_root.parent == Path(tempfile.gettempdir()).resolve()
        assert json.loads((run_root / 'owner.json').read_text()) == owner
        shutil.rmtree(run_root)
        result['temporary_directory_removed'] = not run_root.exists()
        (output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps({'passed': True, 'native_ipc_calls': result['native_ipc_calls'],
                          'standard_metadata_calls': result['standard_metadata_calls'],
                          'cargo_builds': sum(item['built'] for item in builds),
                          'temporary_directory_removed': result['temporary_directory_removed'],
                          'results': str(output)}), flush=True)
    except BaseException:
        print('失败现场保留（failure retained）：' + str(run_root), file=sys.stderr)
        raise
    finally:
        build_inputs.close()


if __name__ == '__main__':
    main()
