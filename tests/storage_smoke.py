"""验证运行期间切换存储、凭据保存、历史读取和配置失败恢复。"""
import argparse
import copy
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

parser = argparse.ArgumentParser()
parser.add_argument('--bin-dir', default='target/debug')
args = parser.parse_args()
binaries = Path(args.bin_dir).resolve()
exe = '.exe' if os.name == 'nt' else ''
endpoint = os.environ['AGENTFS_TEST_S3_ENDPOINT']

with tempfile.TemporaryDirectory(prefix='agentfs-storage-') as temporary:
    root = Path(temporary)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    subprocess.run([str(binaries / ('agentfsd' + exe)), 'init', '--directory', str(root), '--listen', f'127.0.0.1:{port}'], check=True, capture_output=True)
    config_path = root / 'agentfs.json'
    token = (root / 'token').read_text().strip()
    client = [str(binaries / ('agentfs' + exe)), '--endpoint', f'http://127.0.0.1:{port}/mcp', '--token-file', str(root / 'token')]
    log = (root / 'daemon.log').open('w')
    daemon = None

    def settings(payload=None):
        request = urllib.request.Request(f'http://127.0.0.1:{port}/api/settings',
            data=json.dumps(payload).encode() if payload else None,
            headers={'Authorization': f'Bearer {token}', 'Content-Type': 'application/json'})
        request.method = 'PUT' if payload else 'GET'
        try:
            with urllib.request.urlopen(request, timeout=180) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, json.load(error)

    def save(current, runtime):
        status, response = settings({'revision': current['revision'], 'runtime': runtime})
        assert status == 200, response
        return response

    def call(tool, request):
        command = client + ['call', tool, '--json', json.dumps(request)]
        if tool != 'workspace_status':
            command += ['--request-id', str(uuid.uuid4())]
        result = subprocess.run(command, capture_output=True, text=True, timeout=120)
        assert result.returncode == 0, (tool, result.stderr, result.stdout)
        result = json.loads(result.stdout)
        assert not result.get('error'), result
        return result

    def start():
        process = subprocess.Popen([str(binaries / ('agentfsd' + exe)), 'serve', '--config', str(config_path)], stdout=log, stderr=log)
        for _ in range(100):
            try:
                if settings()[0] == 200:
                    return process
            except OSError:
                pass
            if process.poll() is not None:
                raise AssertionError((root / 'daemon.log').read_text())
            time.sleep(0.05)
        process.terminate()
        process.wait(timeout=20)
        raise AssertionError('daemon startup timed out')

    try:
        daemon = start()
        pid = daemon.pid
        workspace = call('workspace_create', {'name': 'storage-switch'})['result']['workspace']['id']
        source = root / 'exchange' / 'source'
        source.mkdir()
        payload = bytes(range(256)) * 4096
        (source / 'image.bin').write_bytes(payload)
        branch = call('workspace_import', {'workspace': workspace, 'source': str(source)})['result']['branch']
        revision = branch['formal_head']
        current = settings()[1]
        runtime = copy.deepcopy(current['runtime'])
        runtime['remote'] = {'enabled': True, 'bucket': 'agentfs-integration', 'prefix': f'storage-tests/{uuid.uuid4()}', 'region': 'us-east-1', 'endpoint': endpoint, 'allow_http': True,
            'credentials': {'access_key_id': os.environ['AWS_ACCESS_KEY_ID'], 'secret_access_key': os.environ['AWS_SECRET_ACCESS_KEY']}}
        current = save(current, runtime)
        assert os.environ['AWS_SECRET_ACCESS_KEY'] not in json.dumps(current)
        assert call('workspace_status', {'workspace': workspace})['capabilities']['remote_configured']
        call('sync', {'workspace': workspace, 'direction': 'push'})
        # 失败的切换保留配置文件、配置版本和当前连接。
        before = config_path.read_bytes()
        invalid = copy.deepcopy(current['runtime'])
        invalid['remote']['bucket'] = f'missing-{uuid.uuid4()}'
        status, _ = settings({'revision': current['revision'], 'runtime': invalid})
        assert status != 200
        assert config_path.read_bytes() == before
        assert settings()[1]['revision'] == current['revision']
        runtime = copy.deepcopy(current['runtime'])
        runtime['remote']['enabled'] = False
        current = save(current, runtime)
        assert not call('workspace_status', {'workspace': workspace})['capabilities']['remote_configured']
        destination = root / 'exchange' / 'export'
        call('workspace_export', {'workspace': workspace, 'revision': revision, 'destination': str(destination)})
        assert (destination / 'image.bin').read_bytes() == payload
        # 直接编辑配置文件也使用同一条存储切换路径。
        config = json.loads(config_path.read_text())
        config['remote']['enabled'] = True
        pending = root / 'pending.json'
        pending.write_text(json.dumps(config))
        os.replace(pending, config_path)
        for _ in range(200):
            current = settings()[1]
            if current['runtime']['remote']['enabled']:
                break
            time.sleep(0.05)
        assert current['runtime']['remote']['enabled'], current
        assert daemon.pid == pid and daemon.poll() is None
        assert call('workspace_status', {'workspace': workspace})['capabilities']['remote_configured']
        daemon.terminate()
        daemon.wait(timeout=30)
        daemon = start()
        assert settings()[1]['runtime']['remote']['credentials']['secret_access_key'] == ''
        destination = root / 'exchange' / 'after-restart'
        call('workspace_export', {'workspace': workspace, 'revision': revision, 'destination': str(destination)})
        assert (destination / 'image.bin').read_bytes() == payload
        print('Live storage switching, saved credentials, failed-switch recovery, file reload and restart passed')
    finally:
        if daemon and daemon.poll() is None:
            daemon.terminate()
            try:
                daemon.wait(timeout=30)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait()
        log.close()
