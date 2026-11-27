"""在 WSL 中用真实终端和手工 v2 动态库逐个验证输入失败边界。"""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time

cancellation_requested = False


def request_cancellation(_signum, _frame):
    global cancellation_requested
    cancellation_requested = True


signal.signal(signal.SIGTERM, request_cancellation)
signal.signal(signal.SIGINT, request_cancellation)

root=Path(sys.argv[1]).resolve()
assert 'microsoft' in os.uname().release.lower()
attempt=sys.argv[2] if len(sys.argv)>2 else 'manual-v2'
deployment=root/'runs'/('cache-ffi-deployment-'+attempt)
records=[]
for mode in ('wrong-type','missing-data','preconsumed-config','data-drop-failure'):
    if cancellation_requested:
        raise InterruptedError('测试协调程序收到取消请求')
    state=root/'runs'/('data-boundary-'+mode+('-'+attempt if attempt!='manual-v2' else ''))
    state.mkdir(exist_ok=False)
    with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as allocation:
        allocation.bind(('127.0.0.1',0))
        port=allocation.getsockname()[1]
    environment=dict(os.environ,LD_LIBRARY_PATH=str(deployment),TMPDIR=str(state),PI_FFI_DATA_FAULT=mode)
    command=[str(deployment/'cache-ffi-host'),str(state),str(deployment),str(port),mode]
    with (state/'host.log').open('wb') as output:
        child=subprocess.Popen(command,env=environment,stdout=output,stderr=subprocess.STDOUT,start_new_session=True)
        try:
            deadline=time.monotonic()+40
            while child.poll() is None:
                if cancellation_requested:
                    raise InterruptedError('测试协调程序收到取消请求')
                if time.monotonic()>=deadline:
                    raise subprocess.TimeoutExpired(command,40)
                time.sleep(0.05)
        finally:
            if child.poll() is None:
                try:
                    os.killpg(child.pid,signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid,signal.SIGKILL)
                    child.wait()
            record={'mode':mode,'pid':child.pid,'exit_code':child.returncode,'command':command}
            (state/'result.json').write_text(json.dumps(record,indent=2)+'\n')
    text=(state/'host.log').read_text()
    assert child.returncode==0,(mode,text[-5000:])
    assert f'host data_boundary={mode} rejected=true hook_calls=0 replay_stable=true' in text
    count=1 if mode=='data-drop-failure' else 0
    assert f'cleanup_drop_count={count}' in text
    records.append(record)
    print(json.dumps({'mode':mode,'passed':True,'pid':child.pid}),flush=True)
(root/'runs'/('data-boundaries-'+attempt+'-verdict.json')).write_text(json.dumps({'passed':True,'cases':records},indent=2)+'\n')
