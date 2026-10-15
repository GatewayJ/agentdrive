"""通过运行中的 daemon 验证文件热更新、设置保存与重启恢复。"""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('--bin-dir', default='target/debug')
args = parser.parse_args()
executable = Path(args.bin_dir).resolve() / ('agentfsd.exe' if os.name == 'nt' else 'agentfsd')

with tempfile.TemporaryDirectory(prefix='agentfs-config-') as temporary:
    root = Path(temporary)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        address = f'127.0.0.1:{listener.getsockname()[1]}'
    subprocess.run([str(executable), 'init', '--directory', str(root), '--listen', address], check=True, capture_output=True)
    path = root / 'agentfs.json'
    original = json.loads(path.read_text())
    token = (root / 'token').read_text().strip()
    endpoint = f'http://{address}/api/settings'
    daemon = None
    log = (root / 'daemon.log').open('w')

    def request(method='GET', payload=None, authenticated=True):
        headers = {'Authorization': f'Bearer {token}'} if authenticated else {}
        data = None
        if payload is not None:
            headers['Content-Type'] = 'application/json'
            data = json.dumps(payload).encode()
        try:
            response = urllib.request.urlopen(urllib.request.Request(endpoint, data=data, headers=headers, method=method), timeout=5)
        except urllib.error.HTTPError as error:
            return error.code, error.read().decode()
        with response:
            body = response.read().decode()
            assert token not in body
            return response.status, json.loads(body)

    def wait_for(predicate):
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline:
            assert daemon.poll() is None, (root / 'daemon.log').read_text()
            try:
                status, data = request()
                if status == 200 and predicate(data):
                    return data
            except (urllib.error.URLError, ConnectionError):
                pass
            time.sleep(.1)
        raise AssertionError('configuration did not reach the expected state')

    def start():
        return subprocess.Popen([str(executable), 'serve', '--config', str(path)], stdin=subprocess.DEVNULL, stdout=log, stderr=log)

    def replace_config(config):
        pending = root / 'agentfs.pending.json'
        pending.write_text(json.dumps(config))
        os.replace(pending, path)

    try:
        daemon = start()
        initial = wait_for(lambda data: data['reload_error'] is None)
        assert request(authenticated=False)[0] == 401
        configured = dict(original, cache={'max_bytes': 3 * 1024 ** 3}, allowed_origins=['https://example.com'])
        replace_config(configured)
        loaded = wait_for(lambda data: data['runtime']['cache']['max_bytes'] == 3 * 1024 ** 3)
        assert loaded['runtime']['allowed_origins'] == ['https://example.com']
        path.write_text('{')
        failed = wait_for(lambda data: data['reload_error'] is not None)
        assert failed['revision'] == loaded['revision']
        assert failed['runtime'] == loaded['runtime']
        replace_config(configured)
        wait_for(lambda data: data['reload_error'] is None)
        replace_config(dict(configured, data_dir=str(root / 'different-state')))
        failed = wait_for(lambda data: data['reload_error'] and 'restart required' in data['reload_error'])
        assert failed['startup']['data_dir'] == original['data_dir']
        replace_config(configured)
        recovered = wait_for(lambda data: data['reload_error'] is None)
        runtime = recovered['runtime']
        runtime['cache'] = {'max_bytes': 128 * 1024 ** 2}
        update = {'revision': recovered['revision'], 'runtime': runtime}
        status, saved = request('PUT', update)
        assert status == 200, saved
        assert request('PUT', update)[0] == 409
        assert saved['runtime']['cache'] == runtime['cache']
        assert json.loads(path.read_text())['cache'] == runtime['cache']
        assert daemon.poll() is None
        daemon.terminate()
        daemon.wait(timeout=15)
        daemon = start()
        restarted = wait_for(lambda data: data['runtime'] == saved['runtime'])
        assert restarted['revision'] == saved['revision']
        print('configuration file reload, error recovery, HTTP save and restart checks passed')
    finally:
        if daemon is not None and daemon.poll() is None:
            daemon.terminate()
            try:
                daemon.wait(timeout=15)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=10)
        log.close()
