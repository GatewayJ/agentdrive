"""Exercise the shipped daemon and CLI with a private temporary state directory."""
import argparse
import json
import os
from pathlib import Path
import socket
import queue
import threading
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser()
parser.add_argument('--bin-dir', default='target/debug')
args = parser.parse_args()
exe = '.exe' if os.name == 'nt' else ''
binaries = Path(args.bin_dir).resolve()
with tempfile.TemporaryDirectory(prefix='agentfs-cli-') as temporary:
    root = Path(temporary)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    endpoint = f'http://127.0.0.1:{port}/mcp'
    subprocess.run([str(binaries / ('agentfsd' + exe)), 'init', '--directory', str(root), '--listen', f'127.0.0.1:{port}'], check=True, capture_output=True, text=True)
    environment = os.environ.copy()
    environment.pop('AGENTFS_TOKEN', None)
    environment.pop('AGENTFS_TOKEN_FILE', None)
    for token_arguments in ([], ['--token-file', str(root / 'missing-token')]):
        result = subprocess.run([str(binaries / ('agentfs' + exe)), '--endpoint', endpoint, *token_arguments, 'dashboard'], env=environment, capture_output=True, text=True, timeout=10)
        assert result.returncode == 0, (result.stdout, result.stderr)
        assert result.stdout.strip() == f'http://127.0.0.1:{port}/'
    config_path = root / 'agentfs.json'
    config = json.loads(config_path.read_text())
    assert not {'allowed_origins', 'cache', 'remote'} & config.keys(), config
    for cache, expected in [
        ({'max_bytes': 1024, 'target_bytes': 1024}, 'cache target must be below its limit'),
        ({'max_byte': 1024}, 'unknown field'),
    ]:
        config['cache'] = cache
        config_path.write_text(json.dumps(config))
        invalid = subprocess.run([str(binaries / ('agentfsd' + exe)), 'serve', '--config', str(config_path)], capture_output=True, text=True, timeout=10)
        assert invalid.returncode == 1 and expected in invalid.stderr, invalid.stderr
    config['cache'] = {'max_bytes': 3 * 1024 ** 3}
    config_path.write_text(json.dumps(config))
    client = [str(binaries / ('agentfs' + exe)), '--endpoint', endpoint, '--token-file', str(root / 'token')]
    log_path = root / 'daemon.log'
    log = log_path.open('w')
    daemon = None

    def start():
        process = subprocess.Popen([str(binaries / ('agentfsd' + exe)), 'serve', '--config', str(root / 'agentfs.json')], stdout=log, stderr=log)
        for _ in range(100):
            if process.poll() is not None:
                raise AssertionError(log_path.read_text())
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=0.1):
                    return process
            except OSError:
                time.sleep(0.05)
        process.kill()
        raise AssertionError('daemon did not start')

    def call(tool, request, request_id=None, success=True):
        command = client + ['call', tool, '--json', json.dumps(request)]
        if request_id:
            command += ['--request-id', request_id]
        result = subprocess.run(command, capture_output=True, text=True, timeout=30)
        if success:
            assert result.returncode == 0, (result.stdout, result.stderr, log_path.read_text())
        else:
            assert result.returncode == 2, (result.stdout, result.stderr)
        return json.loads(result.stdout)

    try:
        daemon = start()
        tools = json.loads(subprocess.check_output(client + ['tools'], text=True, timeout=30))
        assert len(tools['tools']) == 32
        names = {tool['name'] for tool in tools['tools']}
        assert 'revision_files' in names and 'merge_validate' not in names
        commit_schema = next(tool['inputSchema'] for tool in tools['tools'] if tool['name'] == 'revision_commit')
        assert commit_schema['$defs']['CommitKind']['enum'] == ['commit', 'checkpoint'], commit_schema
        created = call('workspace_create', {'name': 'cli-integration'}, 'create')
        assert created['local_saved']
        workspace = created['result']['workspace']['id']
        branch = call('workspace_status', {'workspace': workspace})['branches'][0]
        guard = {'branch': branch['id'], 'authority_epoch': branch['authority_epoch'], 'generation': branch['generation'], 'expected_head': branch['formal_head']}
        for kind in ('commit', 'checkpoint'):
            saved = call('revision_commit', {'guard': guard, 'kind': kind}, 'save-' + kind)
            assert saved['local_saved'], saved
        for kind in ('autosave', 'candidate'):
            rejected = call('revision_commit', {'guard': guard, 'kind': kind}, 'invalid-' + kind, success=False)
            assert rejected['error']['code'] == 'INVALID_ARGUMENT', rejected
        proxy = subprocess.Popen(client + ['mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, text=True, bufsize=1)
        responses = queue.Queue()
        def receive():
            for line in proxy.stdout:
                responses.put(json.loads(line))
        threading.Thread(target=receive, daemon=True).start()
        def send(message):
            proxy.stdin.write(json.dumps({'jsonrpc': '2.0', **message}) + '\n')
            proxy.stdin.flush()
        try:
            send({'id': 1, 'method': 'initialize', 'params': {'protocolVersion': '2025-06-18', 'capabilities': {}, 'clientInfo': {'name': 'agentfs-smoke', 'version': '1'}}})
            assert responses.get(timeout=15)['id'] == 1
            send({'method': 'notifications/initialized'})
            send({'id': 2, 'method': 'tools/list', 'params': {}})
            assert len(responses.get(timeout=15)['result']['tools']) == 32
            send({'id': 3, 'method': 'tools/call', 'params': {'name': 'workspace_status', 'arguments': {'workspace': workspace}}})
            assert responses.get(timeout=15)['result']['structuredContent']['workspace']['id'] == workspace
            send({'id': 4, 'method': 'tools/call', 'params': {'name': 'revision_files', 'arguments': {'workspace': workspace, 'revision': branch['formal_head']}}})
            assert '/' in responses.get(timeout=15)['result']['structuredContent']['entries']
        finally:
            proxy.stdin.close()
            try:
                proxy.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proxy.kill()
                proxy.wait()
            proxy.stdout.close()
        source = root / 'exchange' / 'source'
        source.mkdir()
        (source / 'directory').mkdir()
        payload = bytes(range(251)) * 8193
        (source / 'directory' / 'data').write_bytes(payload)
        (source / 'empty').write_bytes(b'')
        (source / 'pages').mkdir()
        for index in range(205):
            (source / 'pages' / f'{index:03}.txt').write_text(str(index))
        modified_ns = 1_700_000_000_000_000_000
        os.utime(source / 'directory' / 'data', ns=(modified_ns, modified_ns))
        os.utime(source / 'directory', ns=(modified_ns, modified_ns))
        if os.name != 'nt':
            (source / 'directory' / 'data').chmod(0o640)
        imported = call('workspace_import', {'workspace': workspace, 'source': str(source)}, 'import')
        revision = imported['result']['branch']['formal_head']
        destination = root / 'exchange' / 'export'
        exported = call('workspace_export', {'workspace': workspace, 'revision': revision, 'destination': str(destination)}, 'export')
        assert exported['local_saved']
        assert (destination / 'directory' / 'data').read_bytes() == payload
        assert (destination / 'empty').read_bytes() == b''
        assert (destination / 'directory' / 'data').stat().st_mtime_ns == modified_ns
        assert (destination / 'directory').stat().st_mtime_ns == modified_ns
        if os.name != 'nt':
            assert (destination / 'directory' / 'data').stat().st_mode & 0o777 == 0o640
        duplicate = call('workspace_export', {'workspace': workspace, 'revision': revision, 'destination': str(destination)}, 'export')
        assert duplicate['id'] == exported['id']
        denied = call('workspace_import', {'workspace': workspace, 'source': str(root)}, 'outside', success=False)
        assert denied['error']['code'] == 'PERMISSION_DENIED'
        page = call('revision_files', {'workspace': workspace, 'revision': revision, 'path': '/pages'})
        assert len(page['entries']) == 200 and page['next'] is not None
        following = call('revision_files', {'workspace': workspace, 'revision': revision, 'path': '/pages', 'after': page['next']})
        assert len(following['entries']) == 6 and following['next'] is None
        assert not set(page['entries']) & set(following['entries'])
        target = next(item for item in call('workspace_status', {'workspace': workspace})['branches'] if item['id'] == branch['id'])
        guard = {'branch': target['id'], 'authority_epoch': target['authority_epoch'], 'generation': target['generation'], 'expected_head': target['formal_head'], 'mutation_seq': target['mutation_seq']}
        prepared = call('merge_prepare', {'workspace': workspace, 'source': revision, 'target': guard}, 'prepare-merge')['result']['candidate']
        edited = call('merge_resolve', {'merge': prepared['id'], 'candidate': prepared['revision'], 'resolutions': [
            {'choice': 'replace', 'path': '/notes.md', 'content': list('AI 合并说明'.encode()), 'mode': 420},
            {'choice': 'copy', 'path': '/image.bin', 'revision': revision, 'source_path': '/directory/data'},
            {'choice': 'delete', 'path': '/empty'},
        ]}, 'edit-merge')['result']['candidate']
        assert edited['revision'] != prepared['revision'] and not edited['conflicts']
        applied = call('merge_apply', {'merge': edited['id'], 'candidate': edited['revision'], 'target': guard}, 'apply-merge')
        assert applied['local_saved'] and not applied.get('error'), applied
        merged = applied['result']['revision']
        merged_directory = root / 'exchange' / 'merged'
        call('workspace_export', {'workspace': workspace, 'revision': merged, 'destination': str(merged_directory)}, 'export-merge')
        assert (merged_directory / 'image.bin').read_bytes() == payload
        assert (merged_directory / 'notes.md').read_text(encoding='utf-8') == 'AI 合并说明'
        assert not (merged_directory / 'empty').exists()
        merge_revision = next(item for item in call('revision_list', {'workspace': workspace}) if item['id'] == merged)
        assert merge_revision['parents'] == [target['formal_head'], revision]
        # The process restart also exercises WAL recovery and stable operation identities.
        daemon.terminate()
        daemon.wait(timeout=30)
        config['cache'] = {'max_bytes': 3 * 1024 ** 3, 'target_bytes': 1536 * 1024 ** 2, 'min_free_bytes': 64 * 1024 ** 2}
        config.update(allowed_origins=[], remote=None)
        config_path.write_text(json.dumps(config))
        daemon = start()
        recovered = call('operation_by_request', {'request_id': 'import'})
        assert recovered['id'] == imported['id']
        assert call('revision_read', {'workspace': workspace, 'revision': revision, 'path': '/directory/data', 'offset': 173, 'size': 251})['bytes'] == list(payload[173:424])
        status = call('workspace_status', {'workspace': workspace})
        assert status['workspace']['id'] == workspace
        print('CLI, stdio MCP, daemon, import/export metadata, authorization and restart recovery passed')
    finally:
        if daemon and daemon.poll() is None:
            daemon.terminate()
            try:
                daemon.wait(timeout=30)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait()
        log.close()
