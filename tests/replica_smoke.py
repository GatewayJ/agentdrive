"""Verify two shipped daemons against a disposable, existing S3 test bucket."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import uuid

parser = argparse.ArgumentParser()
parser.add_argument('--bin-dir', default='target/debug')
args = parser.parse_args()
binaries = Path(args.bin_dir).resolve()
exe = '.exe' if os.name == 'nt' else ''
endpoint = os.environ['AGENTDRIVE_TEST_S3_ENDPOINT']
prefix = 'daemon-tests/' + str(uuid.uuid4())

class Daemon:
    def __init__(self, root):
        self.root = root
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        subprocess.run([str(binaries / ('agentdrived' + exe)), 'init', '--directory', str(root), '--listen', f'127.0.0.1:{port}'], capture_output=True, text=True, check=True)
        config_path = root / 'agentdrive.json'
        config = json.loads(config_path.read_text())
        config['remote'] = {'bucket': 'agentdrive-integration', 'prefix': prefix, 'region': 'us-east-1', 'endpoint': endpoint, 'allow_http': True}
        config_path.write_text(json.dumps(config))
        self.client = [str(binaries / ('agentdrive' + exe)), '--endpoint', f'http://127.0.0.1:{port}/mcp', '--token-file', str(root / 'token')]
        self.log = (root / 'daemon.log').open('w')
        self.process = subprocess.Popen([str(binaries / ('agentdrived' + exe)), 'serve', '--config', str(config_path)], stdout=self.log, stderr=self.log)
        for _ in range(100):
            if self.process.poll() is not None:
                raise AssertionError((root / 'daemon.log').read_text())
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=0.1):
                    return
            except OSError:
                time.sleep(0.05)
        self.stop()
        raise AssertionError('daemon startup timed out')

    def call(self, tool, request, request_id=None):
        command = self.client + ['call', tool, '--json', json.dumps(request)]
        if request_id:
            command += ['--request-id', request_id]
        output = subprocess.run(command, capture_output=True, text=True, timeout=120)
        assert output.returncode == 0, (output.stdout, output.stderr, (self.root / 'daemon.log').read_text())
        return json.loads(output.stdout)

    def stop(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.log.close()

with tempfile.TemporaryDirectory(prefix='agentdrive-replica-') as temporary:
    daemons = []
    try:
        for name in ['one', 'two']:
            daemons.append(Daemon(Path(temporary) / name))
        first, second = daemons
        workspace = first.call('workspace_create', {'name': 'replica-integration'}, 'create')['result']['workspace']['id']
        source = first.root / 'exchange' / 'source'
        source.mkdir()
        payload = bytes(range(251)) * 84000
        (source / 'multipart').write_bytes(payload)
        imported = first.call('workspace_import', {'workspace': workspace, 'source': str(source)}, 'import')
        branch = imported['result']['branch']
        revision = branch['formal_head']
        first.call('sync', {'workspace': workspace, 'direction': 'push'}, 'push')
        receipt = first.call('operation_get', {'operation': imported['id']})
        assert receipt['remote_confirmed'], receipt
        second.call('sync', {'workspace': workspace, 'direction': 'pull'}, 'pull')
        destination = second.root / 'exchange' / 'export'
        second.call('workspace_export', {'workspace': workspace, 'revision': revision, 'destination': str(destination)}, 'export')
        assert hashlib.sha256((destination / 'multipart').read_bytes()).digest() == hashlib.sha256(payload).digest()
        status = second.call('workspace_status', {'workspace': workspace})
        current = next(item for item in first.call('workspace_status', {'workspace': workspace})['branches'] if item['id'] == branch['id'])
        guard = {'branch': current['id'], 'authority_epoch': current['authority_epoch'], 'generation': current['generation'], 'expected_head': current['formal_head'], 'mutation_seq': current['mutation_seq']}
        transfer = first.call('ownership_transfer', {'guard': guard, 'target': status['location']}, 'transfer')
        assert transfer['remote_confirmed'], transfer
        second.call('sync', {'workspace': workspace, 'direction': 'pull'}, 'adopt')
        adopted = next(item for item in second.call('workspace_status', {'workspace': workspace})['branches'] if item['id'] == branch['id'])
        assert adopted['owner'] == status['location'] and adopted['state'] == 'writable', adopted
        assert adopted['authority_epoch'] == current['authority_epoch'] + 1
        if status['capabilities']['platform_mount'] in ('linux-fuse', 'windows-winfsp', 'macos-macfuse'):
            mounted_workspace = first.call('workspace_create', {'name': 'location-mounts'}, 'mount-create')['result']['workspace']['id']
            mount_path = first.root / 'mounts' / 'shared'
            original = first.call('mount_prepare', {'workspace': mounted_workspace, 'path': str(mount_path)}, 'mount-root')['result']['binding']
            (mount_path / 'shared.txt').write_text('published root content')
            saved = first.call('mount_save', {'mount': original['id'], 'expected_binding_generation': original['generation'], 'durability': 'remote'}, 'save-root')
            assert saved['remote_confirmed'], saved
            third = Daemon(Path(temporary) / 'three')
            daemons.append(third)
            branches = {original['branch']}
            for remote_daemon in (second, third):
                remote_path = remote_daemon.root / 'mounts' / 'shared'
                request = {'workspace': mounted_workspace, 'path': str(remote_path)}
                mounted = remote_daemon.call('mount_prepare', request, 'mount-location')['result']['binding']
                assert mounted['branch'] not in branches, mounted
                branches.add(mounted['branch'])
                assert (remote_path / 'shared.txt').read_text() == 'published root content'
                local_status = remote_daemon.call('workspace_status', {'workspace': mounted_workspace})
                assert local_status['selected_branch'] == mounted['branch'], local_status
                local_branch = next(item for item in local_status['branches'] if item['id'] == mounted['branch'])
                assert local_branch['owner'] == local_status['location'], local_branch
                assert local_branch['source_branch'] == original['branch'], local_branch
                assert local_branch['base_revision'] == saved['result']['revision'], local_branch
                repeated = remote_daemon.call('mount_prepare', request, 'mount-again')['result']['binding']
                assert repeated == mounted, (repeated, mounted)
                (remote_path / 'local.txt').write_text(local_status['location'])
                assert not (mount_path / 'local.txt').exists()
            print('Three-location native mounts, automatic S3 import, isolated branches and repeated binding reuse passed')
        print('Two-daemon S3 replication, multipart content, remote receipts and ownership transfer passed')
    finally:
        for daemon in reversed(daemons):
            daemon.stop()
