"""通过 stdio MCP 验证原生挂载的一次调用切换、占用提示及重试。"""
import argparse
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import tempfile
import threading
import time

parser = argparse.ArgumentParser()
parser.add_argument('--bin-dir', default='target/debug')
args = parser.parse_args()
binaries = Path(args.bin_dir).resolve()
exe = '.exe' if os.name == 'nt' else ''
with tempfile.TemporaryDirectory(prefix='agentdrive-switch-') as temporary:
    root = Path(temporary)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    subprocess.run([str(binaries / ('agentdrived' + exe)), 'init', '--directory', str(root),
                    '--listen', f'127.0.0.1:{port}'], check=True, capture_output=True)
    config_path = root / 'agentdrive.json'
    config = json.loads(config_path.read_text(encoding='utf-8'))
    config['directory_roots'].append(str(root / 'mounts'))
    config_path.write_text(json.dumps(config))
    client = [str(binaries / ('agentdrive' + exe)), '--endpoint', f'http://127.0.0.1:{port}/mcp',
              '--token-file', str(root / 'token')]
    daemon = proxy = holder = None
    log = (root / 'daemon.log').open('w')
    responses = queue.Queue()
    sequence = 0
    calls = []

    def send(message):
        proxy.stdin.write(json.dumps({'jsonrpc': '2.0', **message}) + '\n')
        proxy.stdin.flush()

    def rpc(method, params):
        global sequence
        sequence += 1
        send({'id': sequence, 'method': method, 'params': params})
        response = responses.get(timeout=30)
        assert response.get('id') == sequence and 'error' not in response, response
        return response['result']

    def start_proxy():
        global proxy, responses
        responses = queue.Queue()
        proxy = subprocess.Popen(client + ['mcp'], cwd=root, stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=log, text=True, encoding='utf-8', bufsize=1)
        output, inbox = proxy.stdout, responses
        def receive():
            for line in output:
                inbox.put(json.loads(line))
        threading.Thread(target=receive, daemon=True).start()
        rpc('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                           'clientInfo': {'name': 'branch-switch-test', 'version': '1'}})
        send({'method': 'notifications/initialized'})

    def stop_proxy():
        proxy.stdin.close()
        try:
            proxy.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proxy.kill()
            proxy.wait()
        proxy.stdout.close()

    def call(tool, request, request_id=None, error=None):
        calls.append(tool)
        arguments = request if request_id is None else {'request_id': request_id, 'request': request}
        response = rpc('tools/call', {'name': tool, 'arguments': arguments})
        result = response['structuredContent']
        if error:
            assert response.get('isError') is True and result['error']['code'] == error, result
        else:
            assert not response.get('isError') and not result.get('error'), result
        print(tool, json.dumps(result), flush=True)
        return result

    def switch(branch, request_id, error=None):
        count = len(calls)
        result = call('branch_switch', {'workspace': workspace, 'branch': branch}, request_id, error)
        assert calls[count:] == ['branch_switch']
        if error is None:
            assert result['phase'] == 'complete' and result['mount_ready'] is True, result
        return result

    try:
        daemon = subprocess.Popen([str(binaries / ('agentdrived' + exe)), 'serve', '--config', str(config_path)], stdout=log, stderr=log)
        for _ in range(100):
            assert daemon.poll() is None, (root / 'daemon.log').read_text(encoding='utf-8')
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.1):
                    break
            except OSError:
                time.sleep(.05)
        start_proxy()
        path = root / 'mounts' / 'project'
        path.mkdir()
        (path / 'main.txt').write_text('main content')
        adopted = call('workspace_adopt_directory', {'path': str(path), 'restart_agent': True}, 'adopt')
        workspace, main = adopted['workspace'], adopted['branch']
        feature = call('branch_create', {'workspace': workspace, 'name': 'feature'}, 'fork')['branch']
        completed = switch(feature, 'to-feature')
        with (path / 'feature.txt').open('wb') as stream:
            stream.write(b'feature content')
            stream.flush()
            os.fsync(stream.fileno())
        holder = subprocess.Popen([os.sys.executable, '-c', 'import time; time.sleep(60)'], cwd=path)
        occupied = switch(main, 'to-main', error='MOUNT_BUSY')
        assert occupied['error']['retryable']
        assert 'restart the Agent' in occupied['error']['message']
        assert call('workspace_status', {'workspace': workspace})['selected_branch'] == feature
        assert (path / 'feature.txt').read_bytes() == b'feature content'
        holder.terminate()
        holder.wait(timeout=10)
        holder = None
        stop_proxy()
        start_proxy()
        retried = switch(main, 'to-main')
        assert retried['id'] == occupied['id']
        assert not (path / 'feature.txt').exists()
        assert (path / 'main.txt').read_text(encoding='utf-8') == 'main content'
        replay = switch(feature, 'to-feature')
        assert replay['id'] == completed['id']
        assert call('workspace_status', {'workspace': workspace})['selected_branch'] == main
        switch(feature, 'to-feature-again')
        assert (path / 'feature.txt').read_bytes() == b'feature content'
        assert 'mount_release' not in calls
        print('Single-call MCP branch switching, busy errors, client restart, idempotent retries and branch isolation passed')
    finally:
        if holder is not None:
            holder.terminate()
            holder.wait(timeout=10)
        if proxy is not None and proxy.poll() is None:
            stop_proxy()
        if daemon is not None:
            daemon.terminate()
            try:
                daemon.wait(timeout=15)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait()
        log.close()
