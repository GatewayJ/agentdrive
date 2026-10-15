"""Exercise standard ACP and app-server messages against a mounted directory."""
import json
import os
from pathlib import Path
import sys

protocol = sys.argv[1]
working = None
pending = None
count = 0
thread = 'thread-1'

def send(message):
    print(json.dumps(message), flush=True)

def complete(request, status, external):
    if protocol == 'acp':
        if status == 'failed':
            send({'jsonrpc': '2.0', 'id': request, 'error': {'code': -32000, 'message': 'injected failure'}})
        else:
            send({'jsonrpc': '2.0', 'id': request, 'result': {'stopReason': 'cancelled' if status == 'cancelled' else 'end_turn'}})
    else:
        message = {'method': 'turn/completed', 'params': {'threadId': thread, 'turn': {'id': external, 'status': 'interrupted' if status == 'cancelled' else status}}}
        send(message)
        send(message)

for line in sys.stdin:
    message = json.loads(line)
    method = message.get('method')
    request = message.get('id')
    params = message.get('params', {})
    if method == 'initialize':
        send({'id': request, 'result': {'protocolVersion': 1, 'agentCapabilities': {}, 'agentInfo': {'name': 'test', 'version': '1'}}})
    elif method in ('session/new', 'session/load', 'session/resume', 'thread/start', 'thread/resume', 'thread/fork'):
        working = Path(params['cwd'])
        assert working.is_absolute() and working.is_dir(), working
        if protocol == 'acp':
            send({'jsonrpc': '2.0', 'id': request, 'result': {'sessionId': 'session-1'}})
        else:
            send({'id': request, 'result': {'thread': {'id': 'thread-1', 'cwd': str(working)}}})
    elif method in ('session/prompt', 'turn/start', 'review/start'):
        count += 1
        action = params['target']['instructions'] if method == 'review/start' else (params.get('prompt') or params.get('input'))[0]['text']
        thread = 'review-thread' if method == 'review/start' else 'thread-1'
        (working / 'result.txt').write_text(f'{action}:{count}')
        external = f'turn-{count}'
        if protocol != 'acp':
            send({'method': 'turn/started', 'params': {'threadId': thread, 'turn': {'id': external, 'status': 'inProgress'}}})
            send({'id': request, 'result': {'turn': {'id': external, 'status': 'inProgress'}, **({'reviewThreadId': thread} if method == 'review/start' else {})}})
        if action == 'exit':
            os._exit(3)
        if action == 'hold':
            pending = (request, external)
            send({'method': 'fixture/holding', 'params': {}})
        else:
            complete(request, 'failed' if action == 'failed' else 'completed', external)
    elif method in ('session/cancel', 'turn/interrupt'):
        if pending:
            (working / 'cancelled.txt').write_text('saved cancellation')
            complete(pending[0], 'cancelled', pending[1])
            pending = None
        if request is not None:
            send({'id': request, 'result': {}})
    elif request is not None:
        send({'id': request, 'result': {}})
