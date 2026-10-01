import json
import os
import sys

root = os.path.dirname(__file__)

def record(value):
    with open(os.path.join(root, 'calls'), 'a') as log:
        log.write(value + '\n')

def result(request, body):
    print(json.dumps({'id': request['id'], 'result': body}), flush=True)

def reject(request, message):
    print(json.dumps({'id': request['id'], 'error': {'code': -32600, 'message': message}}), flush=True)

def notify(method, params):
    print(json.dumps({'method': method, 'params': params}), flush=True)

record('spawn ' + str(os.getpid()))
record('args ' + ' '.join(sys.argv[1:]))
if sys.argv[-1] != 'app-server':
    raise SystemExit('expected persistent app-server')
initialized = False
turn_count = 0
for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method')
    params = request.get('params', {})
    if method == 'initialize':
        record('initialize')
        assert params['clientInfo']['name'] == 'lumvise-assistant-session'
        if os.path.exists(os.path.join(root, 'stall_init')):
            continue
        if os.path.exists(os.path.join(root, 'reject_init')):
            reject(request, 'bad client')
        else:
            result(request, {'userAgent': 'fake'})
    elif method == 'initialized':
        initialized = True
    elif method in ('thread/start', 'thread/resume'):
        assert initialized
        assert params['sandbox'] == 'read-only'
        assert params['approvalPolicy'] == 'never'
        record(method + ' ' + params.get('threadId', 'new'))
        if params.get('threadId') == 'stale':
            reject(request, 'no rollout found for thread id stale')
        elif params.get('threadId') == 'denied':
            reject(request, 'permission denied')
        else:
            result(request, {'thread': {'id': 'thread-1'}})
    elif method == 'turn/start':
        prompt = params['input'][0]['text']
        assert params['input'][0]['type'] == 'text'
        assert params['threadId'] == 'thread-1'
        record('turn/start thread-1')
        if 'stall' in prompt:
            record('waiting')
            continue
        if 'crash' in prompt:
            raise SystemExit(3)
        if 'rpc-error' in prompt:
            reject(request, 'turn rejected')
            continue
        turn_count += 1
        turn_id = 'turn-' + str(turn_count)
        content = '' if 'tool-only' in prompt else 'final answer'
        if 'early-events' not in prompt:
            result(request, {'turn': {'id': turn_id, 'status': 'inProgress', 'items': []}})
        scope = {'threadId': 'thread-1', 'turnId': turn_id}
        notify('item/agentMessage/delta', {**scope, 'threadId': 'unrelated', 'delta': 'wrong thread'})
        notify('item/agentMessage/delta', {**scope, 'turnId': 'old', 'delta': 'wrong turn'})
        if content:
            notify('item/agentMessage/delta', {**scope, 'itemId': 'message', 'delta': 'final '})
            notify('item/agentMessage/delta', {**scope, 'itemId': 'message', 'delta': 'answer'})
            notify('item/completed', {**scope, 'item': {'type': 'agentMessage', 'id': 'message', 'text': content}})
        status = 'failed' if 'fail-turn' in prompt else 'completed'
        notify('turn/completed', {'threadId': 'thread-1', 'turn': {'id': turn_id, 'status': status, 'items': [], 'error': {'message': 'provider failure'} if status == 'failed' else None}})
        if 'early-events' in prompt:
            result(request, {'turn': {'id': turn_id, 'status': 'inProgress', 'items': []}})
