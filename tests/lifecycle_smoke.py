"""Run lifecycle bridges with actual daemon processes and a native filesystem."""
import argparse
from contextlib import closing
import json
from pathlib import Path
import queue
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('--bin-dir', default='target/debug')
args = parser.parse_args()
binaries = Path(args.bin_dir).resolve()
exe = '.exe' if sys.platform == 'win32' else ''
fixture = Path(__file__).parent.resolve() / 'fixtures' / 'lifecycle_agent.py'
with tempfile.TemporaryDirectory(prefix='agentfs-lifecycle-') as temporary:
    root = Path(temporary)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    subprocess.run([str(binaries / ('agentfsd' + exe)), 'init', '--directory', str(root), '--listen', f'127.0.0.1:{port}'], check=True, capture_output=True)
    token = (root / 'token').read_text().strip()
    command = [str(binaries / ('agentfs' + exe)), '--endpoint', base + '/mcp', '--token-file', str(root / 'token')]
    log = (root / 'test.log').open('w+')
    daemon = None
    bridges = []

    def start_daemon():
        process = subprocess.Popen([str(binaries / ('agentfsd' + exe)), 'serve', '--config', str(root / 'agentfs.json')], stdout=log, stderr=log)
        for _ in range(100):
            if process.poll() is not None:
                raise AssertionError((root / 'test.log').read_text())
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.1):
                    return process
            except OSError:
                time.sleep(.05)
        raise AssertionError('daemon startup timed out')

    def call(tool, request, identity=None):
        arguments = command + ['call', tool, '--json', json.dumps(request)]
        if identity:
            arguments += ['--request-id', identity]
        result = subprocess.run(arguments, text=True, capture_output=True, timeout=30)
        assert result.returncode == 0, (result.stdout, result.stderr, (root / 'test.log').read_text())
        return json.loads(result.stdout)

    def api(path, body=None):
        headers = {'Authorization': 'Bearer ' + token}
        if body is not None:
            headers['Content-Type'] = 'application/json'
        request = urllib.request.Request(base + path, None if body is None else json.dumps(body).encode(), headers)
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)

    def spawn(protocol, workspace):
        arguments = command + ['integrate', '--protocol', protocol, '--workspace', workspace, '--mount-path', str(root / 'mounts' / protocol), '--', sys.executable, str(fixture), protocol]
        process = subprocess.Popen(arguments, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, text=True, bufsize=1)
        bridges.append(process)
        messages = queue.Queue()
        def read():
            for line in process.stdout:
                messages.put(json.loads(line))
            messages.put(None)
        threading.Thread(target=read, daemon=True).start()
        def send(message):
            process.stdin.write(json.dumps({'jsonrpc': '2.0', **message}) + '\n')
            process.stdin.flush()
        def wait(predicate):
            for _ in range(100):
                try:
                    message = messages.get(timeout=30)
                except queue.Empty:
                    raise AssertionError((root / 'test.log').read_text())
                assert message is not None, (process.poll(), (root / 'test.log').read_text())
                if predicate(message):
                    return message
            raise AssertionError('expected response was not received')
        send({'id': 1, 'method': 'initialize', 'params': {}})
        wait(lambda message: message.get('id') == 1)
        send({'id': 2, 'method': 'session/new' if protocol == 'acp' else 'thread/start', 'params': {'cwd': '/invalid/unmanaged-directory', 'mcpServers': []}})
        wait(lambda message: message.get('id') == 2)
        return process, send, wait

    def records(workspace):
        config = json.loads((root / 'agentfs.json').read_text())
        with closing(sqlite3.connect(str(Path(config['data_dir']) / 'meta.db'))) as database:
            return [record for row in database.execute("SELECT data FROM operations WHERE workspace=?", (workspace,)) if (record := json.loads(row[0]))["name"] == "mount_save"]

    try:
        daemon = start_daemon()
        for protocol in ('acp', 'codex-app-server'):
            workspace = call('workspace_create', {'name': protocol}, 'create-' + protocol)['result']['workspace']['id']
            process, send, wait = spawn(protocol, workspace)
            shared = next(item for item in call('workspace_status', {'workspace': workspace})['mounts'] if item['state'] == 'ready')
            peer, _, _ = spawn(protocol, workspace)
            peer.stdin.close()
            peer.wait(timeout=30)
            assert peer.returncode == 0, (root / 'test.log').read_text()
            connected = call('workspace_status', {'workspace': workspace})
            assert len(connected['branches']) == 1 and len(connected['mounts']) == 1, connected
            assert connected['mounts'][0] == shared, connected
            method = 'session/prompt' if protocol == 'acp' else 'turn/start'
            def prompt(identity, action):
                params = {'sessionId': 'session-1', 'prompt': [{'type': 'text', 'text': action}]} if protocol == 'acp' else {'threadId': 'thread-1', 'input': [{'type': 'text', 'text': action}]}
                return {'id': identity, 'method': method, 'params': params}
            def done(identity):
                if protocol == 'acp':
                    return wait(lambda message: message.get('id') == identity)
                return wait(lambda message: message.get('method') == 'turn/completed' and message['params']['turn']['id'] == f'turn-{identity - 2}')
            request = prompt(3, 'completed')
            send(request)
            done(3)
            current = call('workspace_status', {'workspace': workspace})
            assert len(current['branches']) == 1, current
            branch = current['branches'][0]
            assert call('revision_read', {'workspace': workspace, 'revision': branch['autosave_head'] or branch['formal_head'], 'path': '/result.txt', 'offset': 0, 'size': 100})['bytes'] == list(b'completed:1')
            send(request)
            wait(lambda message: message.get('id') == 3)
            assert len(records(workspace)) == 1, records(workspace)
            send(prompt(4, 'hold'))
            wait(lambda message: message.get('method') == 'fixture/holding')
            if protocol == 'acp':
                deadline = time.monotonic() + 12
                while True:
                    status = call('workspace_status', {'workspace': workspace})
                    if status['branches'][0]['autosave_head'] != branch['autosave_head']:
                        break
                    assert time.monotonic() < deadline, status
                    time.sleep(.25)
            send({'method': 'session/cancel', 'params': {'sessionId': 'session-1'}} if protocol == 'acp' else {'id': 20, 'method': 'turn/interrupt', 'params': {'threadId': 'thread-1', 'turnId': 'turn-2'}})
            done(4)
            send(prompt(5, 'failed'))
            done(5)
            if protocol == 'codex-app-server':
                send({'id': 21, 'method': 'thread/fork', 'params': {'threadId': 'thread-1', 'cwd': '/invalid/unmanaged-directory'}})
                forked = wait(lambda message: message.get('id') == 21)
                assert Path(forked['result']['thread']['cwd']).samefile(root / 'mounts' / protocol)
                send({'id': 22, 'method': 'review/start', 'params': {'threadId': 'thread-1', 'delivery': 'detached', 'target': {'type': 'custom', 'instructions': 'completed'}}})
                wait(lambda message: message.get('method') == 'turn/completed' and message['params']['threadId'] == 'review-thread')
            process.stdin.close()
            process.wait(timeout=30)
            assert process.returncode == 0, (root / 'test.log').read_text()
            assert len(records(workspace)) == (3 if protocol == 'acp' else 4), records(workspace)
            active_mount = next(item for item in call('workspace_status', {'workspace': workspace})['mounts'] if item['state'] == 'ready')
            assert active_mount['path'] == str(root / 'mounts' / protocol)
            process, send, wait = spawn(protocol, workspace)
            assert len(call('workspace_status', {'workspace': workspace})['branches']) == 1
            send(prompt(6, 'exit'))
            process.wait(timeout=30)
            assert process.returncode != 0
            assert (root / 'mounts' / protocol / 'result.txt').read_text().startswith('exit:')
            previous_saves = {record['id'] for record in records(workspace)}
            process, send, wait = spawn(protocol, workspace)
            send(prompt(10, 'exit-success'))
            process.wait(timeout=30)
            assert process.returncode == 0, (root / 'test.log').read_text()
            interrupted = [record for record in records(workspace) if record['id'] not in previous_saves]
            assert len(interrupted) == 1 and interrupted[0]['local_saved'], interrupted
            process, send, wait = spawn(protocol, workspace)
            send(prompt(7, 'completed'))
            if protocol == 'acp':
                wait(lambda message: message.get('id') == 7)
            else:
                wait(lambda message: message.get('method') == 'turn/completed')
            process.stdin.close()
            process.wait(timeout=30)
            assert process.returncode == 0, (root / 'test.log').read_text()
            assert len(call('workspace_status', {'workspace': workspace})['branches']) == 1
            if protocol == 'acp':
                process, send, wait = spawn(protocol, workspace)
                send(prompt(8, 'hold'))
                wait(lambda message: message.get('method') == 'fixture/holding')
                daemon.terminate()
                daemon.wait(timeout=30)
                process.terminate()
                process.wait(timeout=30)
                daemon = start_daemon()
                process, send, wait = spawn(protocol, workspace)
                assert len(call('workspace_status', {'workspace': workspace})['branches']) == 1
                send(prompt(9, 'completed'))
                wait(lambda message: message.get('id') == 9)
                process.stdin.close()
                process.wait(timeout=30)
                assert process.returncode == 0, (root / 'test.log').read_text()
            if protocol == 'acp':
                branch = call('workspace_status', {'workspace': workspace})['branches'][0]
                active = next(item for item in call('workspace_status', {'workspace': workspace})['mounts'] if item['state'] == 'ready')
                call('mount_unmount', {'mount': active['id'], 'generation': active['generation']}, 'release-shared')
                mounted = subprocess.run(command + ['mount', '--workspace', workspace, str(root / 'mounts' / 'explicit')], capture_output=True, text=True, timeout=30)
                assert mounted.returncode == 0, (mounted.stdout, mounted.stderr)
                created = subprocess.run(command + ['branch', 'create', '--workspace', workspace, '--name', 'feature'], capture_output=True, text=True, timeout=30)
                assert created.returncode == 0, (created.stdout, created.stderr)
                feature = json.loads(created.stdout)['result']['branch']
                assert feature['source_branch'] == branch['id']
                switched = subprocess.run(command + ['branch', 'switch', '--workspace', workspace, '--branch', feature['id']], capture_output=True, text=True, timeout=30)
                assert switched.returncode == 0, (switched.stdout, switched.stderr)
                receipt = json.loads(switched.stdout)
                assert receipt['phase'] == 'complete' and receipt['mount_ready'], receipt
                assert (root / 'mounts' / 'explicit' / 'result.txt').read_text().startswith('completed:')
                assert call('workspace_status', {'workspace': workspace})['selected_branch'] == feature['id']
            dashboard = api('/api/workspaces/' + workspace)
            assert dashboard['revisions'] and dashboard['status']['selected_branch']
        print('ACP and app-server persistence, cancellation, failures, autosave, duplicate requests, process interruption, daemon restart, reconnect, CLI branch switching and dashboard queries passed')
    finally:
        for process in bridges:
            if process.poll() is None:
                process.kill()
                process.wait()
            for stream in (process.stdin, process.stdout):
                if stream and not stream.closed:
                    stream.close()
        if daemon and daemon.poll() is None:
            daemon.terminate()
            daemon.wait(timeout=30)
        log.close()
