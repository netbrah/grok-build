"""Offline production-format provider and containment contracts."""
import http.client
import json
import os
from pathlib import Path
import tempfile
import threading
import unittest

from smoke.redteam.hp8r import loopback_provider as provider


def message_stream(blocks, stop='end_turn'):
    events = [{'type': 'message_start', 'message': {
        'id': 'msg_fixture', 'type': 'message', 'role': 'assistant', 'model': 'fixture-M',
        'content': [], 'stop_reason': None, 'stop_sequence': None,
        'usage': {'input_tokens': 1, 'output_tokens': 0}}}]
    for index, block in enumerate(blocks):
        start = dict(block)
        input_ = start.pop('input', None)
        if input_ is not None:
            start['input'] = {}
        events.append({'type': 'content_block_start', 'index': index, 'content_block': start})
        if input_ is not None:
            text = json.dumps(input_, separators=(',', ':'))
            for part in (text[:3], text[3:]):
                events.append({'type': 'content_block_delta', 'index': index,
                               'delta': {'type': 'input_json_delta', 'partial_json': part}})
        events.append({'type': 'content_block_stop', 'index': index})
    events.extend([{'type': 'message_delta', 'delta': {'stop_reason': stop, 'stop_sequence': None},
                    'usage': {'output_tokens': 3}}, {'type': 'message_stop'}])
    return b''.join(('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n').encode() for event in events)


def response_stream(items):
    events = [{'type': 'response.created', 'response': {'id': 'resp_fixture', 'object': 'response',
                'status': 'in_progress', 'output': []}}]
    for index, item in enumerate(items):
        events.extend([{'type': 'response.output_item.added', 'output_index': index, 'item': item},
                       {'type': 'response.output_item.done', 'output_index': index, 'item': item}])
    events.append({'type': 'response.completed', 'response': {'id': 'resp_fixture',
                   'object': 'response', 'status': 'completed', 'output': items,
                   'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}}})
    return b''.join(('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n').encode() for event in events)


class ProviderContractTests(unittest.TestCase):
    def test_messages_delta_order_and_mixed_handoff(self):
        blocks = [{'type': 'server_tool_use', 'id': 'srv_1', 'name': 'tool_search_tool_bm25',
                   'input': {'query': 'weather'}}, {'type': 'text', 'text': 'between'},
                  {'type': 'tool_use', 'id': 'client_1', 'name': 'read_status', 'input': {'name': 'sky'}}]
        result = provider.decode_sse(message_stream(blocks, 'tool_use'), 'Messages')
        self.assertEqual(result['blocks'], blocks)
        self.assertEqual(result['stop_reason'], 'tool_use')

    def test_responses_codex_and_non_codex_preserve_optional_ids(self):
        items = [{'type': 'tool_search_call', 'id': 'ts_1', 'call_id': 'client_1',
                  'execution': 'client', 'status': 'completed', 'arguments': {'query': 'status'}},
                 {'type': 'tool_search_output', 'call_id': 'client_1', 'execution': 'client',
                  'status': 'completed', 'tools': []}]
        for dialect in ('Responses-Codex', 'Responses-Other'):
            with self.subTest(dialect=dialect):
                self.assertEqual(provider.decode_sse(response_stream(items), dialect)['blocks'], items)

    def test_truncated_malformed_and_duplicate_streams_are_not_success(self):
        valid = message_stream([{'type': 'text', 'text': 'hello'}])
        invalid = [valid.rsplit(b'event: message_stop', 1)[0], b'event: message_start\ndata: {\n\n',
                   valid + b'event: message_stop\ndata: {"type":"message_stop"}\n\n']
        for raw in invalid:
            with self.subTest(raw_length=len(raw)):
                with self.assertRaises(provider.ProtocolError):
                    provider.decode_sse(raw, 'Messages')

    def test_pause_zero_blocks_repeated_suffix_and_continuation_cap(self):
        state = provider.PauseState()
        state.accept(provider.decode_sse(message_stream([{'type': 'text', 'text': 'one'}], 'pause_turn'), 'Messages'))
        state.accept(provider.decode_sse(message_stream([], 'pause_turn'), 'Messages'))
        state.accept(provider.decode_sse(message_stream([{'type': 'text', 'text': 'two'}], 'pause_turn'), 'Messages'))
        self.assertEqual(state.blocks, [{'type': 'text', 'text': 'one'}, {'type': 'text', 'text': 'two'}])
        with self.assertRaisesRegex(provider.ProtocolError, 'continuation-cap'):
            state.accept(provider.decode_sse(message_stream([], 'pause_turn'), 'Messages'))

    def test_http_attempt_budget_counts_retry_compaction_and_continuation(self):
        budget = provider.AttemptBudget('single')
        for reason in ('initial', 'retry', 'compaction', 'continuation'):
            budget.take(reason)
        with self.assertRaisesRegex(provider.ProtocolError, 'attempt-cap'):
            budget.take('retry')
        self.assertEqual([x['reason'] for x in budget.attempts], ['initial', 'retry', 'compaction', 'continuation'])
        roundtrip = provider.AttemptBudget('roundtrip')
        for _ in range(12):
            roundtrip.take('initial')
        with self.assertRaises(provider.ProtocolError):
            roundtrip.take('continuation')

    def test_request_disk_session_barriers_and_cancellation(self):
        gate = provider.Barriers(('A-request', 'disk-ack', 'B-installed'))
        observed = threading.Event()
        thread = threading.Thread(target=lambda: (gate.wait('B-installed'), observed.set()))
        thread.start()
        gate.signal('A-request')
        gate.signal('disk-ack')
        self.assertFalse(observed.is_set())
        gate.signal('B-installed')
        thread.join(2)
        self.assertFalse(thread.is_alive())
        self.assertTrue(observed.is_set())
        gate.cancel()
        with self.assertRaisesRegex(provider.ProtocolError, 'cancelled'):
            gate.wait('disk-ack')

    def test_loopback_records_actual_ordered_bodies_without_headers(self):
        body = {'model': 'fixture-M', 'stream': True, 'messages': [{'role': 'user', 'content': 'ping'}]}
        raw = message_stream([{'type': 'text', 'text': 'pong'}])
        with provider.LoopbackProvider([{'path': '/v1/messages', 'body': body, 'response': raw}], 'single') as server:
            conn = http.client.HTTPConnection('127.0.0.1', server.port, timeout=2)
            try:
                conn.request('POST', '/v1/messages', json.dumps(body), {'Authorization': 'synthetic-local-only'})
                response = conn.getresponse()
                self.assertEqual(response.status, 200)
                self.assertEqual(response.read(), raw)
            finally:
                conn.close()
            self.assertEqual(server.requests, [body])
            self.assertEqual(len(server.attempts), 1)
            self.assertNotIn('Authorization', json.dumps(server.attempts))

    def test_read_only_mcp_validates_id_arguments_and_permission(self):
        stub = provider.ReadOnlyMCP({'read_status': {'sky': 'clear'}})
        result = stub.call({'jsonrpc': '2.0', 'id': 7, 'method': 'tools/call',
                            'params': {'name': 'read_status', 'arguments': {'name': 'sky'}}})
        self.assertEqual(result, {'jsonrpc': '2.0', 'id': 7, 'result': {'content': [{'type': 'text', 'text': 'clear'}], 'isError': False}})
        with self.assertRaisesRegex(provider.ProtocolError, 'permission'):
            stub.call({'jsonrpc': '2.0', 'id': 8, 'method': 'tools/call',
                       'params': {'name': 'write_status', 'arguments': {'name': 'sky'}}})

    def test_before_write_excludes_sentinels_and_credential_derived_hashes(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            for index, payload in enumerate(({'text': 'sK-' + 'a' * 20}, {'cache_identity_hash': 'a' * 64},
                            {'headers': {'Authorization': 'synthetic-local-only'}}, {'text': '<synthetic-secret>'}, {'input': {'cache_identity_hash': 'a' * 64}})):
                dest = Path(temp) / f'capture-{index}.json'
                with self.subTest(keys=list(payload)):
                    with self.assertRaises(provider.ProtocolError):
                        provider.capture_json(dest, payload)
                    self.assertFalse(dest.exists())


    def test_cli_requires_absolute_binary_and_l2_authorization(self):
        import contextlib
        import io
        from smoke.redteam.hp8r import run
        base = ['--layer', 'L2', '--case', 'fixture', '--manifest', 'manifest.json',
                '--bin', '/bin/false', '--out', '/not-written']
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                run.parse_args(base)
            invalid = base[:]
            invalid[7] = 'relative-bin'
            with self.assertRaises(SystemExit):
                run.parse_args(invalid + ['--authorization', 'approval.json'])
        args = run.parse_args(base + ['--authorization', 'approval.json'])
        self.assertEqual((args.layer, args.case, str(args.bin)), ('L2', 'fixture', '/bin/false'))

    def test_acp_transport_matches_ids_and_excludes_ambient_environment(self):
        import sys
        from smoke.redteam.hp8r import run
        source = 'import json,sys,os\nfor line in sys.stdin:\n r=json.loads(line)\n if "id" not in r: continue\n print(json.dumps({"jsonrpc":"2.0","id":r["id"],"result":{"method":r["method"],"ambient_absent":"APEX_LLM_PROXY_KEY" not in os.environ}}),flush=True)'
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            with run.AcpProcess([sys.executable, '-u', '-c', source], Path(temp) / 'home') as client:
                for method in ('initialize', 'session/new', 'session/load', 'session/set_model', 'session/prompt'):
                    result = client.call(method, {})
                    self.assertEqual(result['result'], {'method': method, 'ambient_absent': True})
                self.assertIsNone(client.call('session/cancel', {}))
            self.assertIsNotNone(client.process.returncode)
        self.assertEqual(set(run.isolated_environment('/isolated')), {
            'HOME', 'GROK_HOME', 'TMPDIR', 'PATH', 'PYTHONDONTWRITEBYTECODE', 'GROK_AUTH_EXPIRED',
            'OPENAI_API_KEY', 'ANTHROPIC_API_KEY', 'NO_PROXY', 'OTEL_SDK_DISABLED'})

    def test_http_fault_retry_and_over_limit_attempt_remain_visible(self):
        body = {'model': 'fixture-M', 'messages': [], 'stream': True}
        steps = [{'path': '/v1/messages', 'body': body, 'status': status,
                  'reason': reason, 'response': b'{"error":"synthetic-fault"}'}
                 for status, reason in ((503, 'initial'), (401, 'retry'), (429, 'compaction'), (500, 'continuation'))]
        with provider.LoopbackProvider(steps, 'single') as server:
            statuses = []
            for _ in range(5):
                conn = http.client.HTTPConnection('127.0.0.1', server.port, timeout=2)
                try:
                    conn.request('POST', '/v1/messages', json.dumps(body))
                    response = conn.getresponse()
                    statuses.append(response.status)
                    response.read()
                finally:
                    conn.close()
            self.assertEqual(statuses, [503, 401, 429, 500, 400])
            self.assertEqual(len(server.attempts), 5)
            self.assertEqual(len(server.budget.attempts), 4)
            self.assertEqual(len(server.errors), 1)

    def test_http_barrier_cancellation_is_bounded(self):
        body = {'model': 'fixture-M', 'messages': [], 'stream': True}
        barriers = provider.Barriers(('arrived', 'disk-ack'))
        status = []
        with provider.LoopbackProvider([{'path': '/v1/messages', 'body': body,
                'signal': 'arrived', 'wait': 'disk-ack', 'response': message_stream([])}], 'single', barriers) as server:
            def request():
                conn = http.client.HTTPConnection('127.0.0.1', server.port, timeout=3)
                try:
                    conn.request('POST', '/v1/messages', json.dumps(body))
                    response = conn.getresponse()
                    status.append(response.status)
                    response.read()
                finally:
                    conn.close()
            thread = threading.Thread(target=request)
            thread.start()
            barriers.wait('arrived')
            self.assertEqual(server.responses, [])
            barriers.cancel()
            thread.join(3)
            self.assertFalse(thread.is_alive())
            self.assertEqual(status, [400])
            self.assertEqual(server.responses, [])



    def test_mcp_dispatch_work_is_bounded(self):
        stub = provider.ReadOnlyMCP({'read_status': {'sky': 'clear'}})
        for identifier in range(128):
            stub.call({'jsonrpc': '2.0', 'id': identifier, 'method': 'tools/call',
                       'params': {'name': 'read_status', 'arguments': {'name': 'sky'}}})
        with self.assertRaisesRegex(provider.ProtocolError, 'mcp-call-cap'):
            stub.call({'jsonrpc': '2.0', 'id': 128, 'method': 'tools/call',
                       'params': {'name': 'read_status', 'arguments': {'name': 'sky'}}})
        self.assertEqual(len(stub.calls), 128)


def test_cli_report_writer_screens_before_creating_output(self):
    import contextlib
    import io
    import sys
    from smoke.redteam.hp8r import run
    case = 'case-<synthetic-secret>'
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        source = Path(temp) / 'manifest.json'
        source.write_text(json.dumps({'cases': {case: {'layers': ['L0'], 'execution': 'tooling-validation'}}}))
        destination = Path(temp) / 'not-created'
        with contextlib.redirect_stdout(io.StringIO()):
            result = run.main(['--layer', 'L0', '--case', case, '--manifest', str(source),
                               '--bin', sys.executable, '--out', str(destination)])
        self.assertEqual(result, 2)
        self.assertFalse(destination.exists())


ProviderContractTests.test_cli_report_writer_screens_before_creating_output = test_cli_report_writer_screens_before_creating_output


def test_acp_cancel_is_an_idless_notification(self):
    import sys
    from smoke.redteam.hp8r import run
    source = '''import json,sys
for line in sys.stdin:
    request=json.loads(line)
    if "id" in request:
        print(json.dumps({"jsonrpc":"2.0","id":request["id"],"result":{"method":request["method"],"had_id":True}}),flush=True)
'''
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        with run.AcpProcess([sys.executable, '-u', '-c', source], Path(temp) / 'home') as client:
            self.assertIsNone(client.call('session/cancel', {'sessionId': 'fixture-session'}))
            self.assertEqual(client.call('session/load', {})['result'], {'method': 'session/load', 'had_id': True})


def test_acp_held_prompt_interleaves_install_ack_and_notification_cancel(self):
    import sys
    from smoke.redteam.hp8r import run
    source = '''import json,sys
held=None
order=[]
def emit(value):
    print(json.dumps(value),flush=True)
for line in sys.stdin:
    request=json.loads(line)
    method=request["method"]
    order.append(method)
    if method=="session/prompt":
        held=request["id"]
        emit({"jsonrpc":"2.0","method":"session/update","params":{"state":"prompt-held"}})
    elif method=="session/set_model":
        emit({"jsonrpc":"2.0","id":request["id"],"result":{"installed":"fixture-B","held_prompt":held}})
    elif method=="session/load":
        emit({"jsonrpc":"2.0","id":request["id"],"result":{"loaded":"fixture-session"}})
    elif method=="session/cancel":
        emit({"jsonrpc":"2.0","method":"session/update","params":{"cancel_keys":sorted(request)}})
        emit({"jsonrpc":"2.0","id":held,"result":{"stopReason":"cancelled","order":order}})
'''
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        with run.AcpProcess([sys.executable, '-u', '-c', source], Path(temp) / 'home') as client:
            prompt = client.send('session/prompt', {'sessionId': 'fixture-session', 'prompt': []})
            install = client.send('session/set_model', {'sessionId': 'fixture-session', 'modelId': 'fixture-B'})
            load = client.send('session/load', {'sessionId': 'fixture-session'})
            self.assertEqual(client.receive(load), {'jsonrpc': '2.0', 'id': load, 'result': {'loaded': 'fixture-session'}})
            self.assertEqual(client.receive(install), {'jsonrpc': '2.0', 'id': install,
                                                     'result': {'installed': 'fixture-B', 'held_prompt': prompt}})
            self.assertIsNone(client.call('session/cancel', {'sessionId': 'fixture-session'}))
            self.assertEqual(client.receive(prompt), {'jsonrpc': '2.0', 'id': prompt, 'result': {
                'stopReason': 'cancelled', 'order': ['session/prompt', 'session/set_model', 'session/load', 'session/cancel']}})
            self.assertEqual(client.events, [
                {'jsonrpc': '2.0', 'method': 'session/update', 'params': {'state': 'prompt-held'}},
                {'jsonrpc': '2.0', 'method': 'session/update', 'params': {'cancel_keys': ['jsonrpc', 'method', 'params']}}])
            self.assertEqual(client.pending, {})


def test_acp_pending_requests_are_bounded(self):
    import sys
    from smoke.redteam.hp8r import run
    source = 'import sys\nfor line in sys.stdin:\n pass\n'
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        with run.AcpProcess([sys.executable, '-u', '-c', source], Path(temp) / 'home') as client:
            for _ in range(128):
                client.send('session/prompt', {})
            with self.assertRaisesRegex(provider.ProtocolError, 'acp-pending-cap'):
                client.send('session/prompt', {})


def test_acp_foreign_reverse_truncated_and_timeout_responses_are_rejected(self):
    import sys
    from smoke.redteam.hp8r import run
    scripts = {
        'acp-foreign-id-or-reverse-request': 'import json,sys\nr=json.loads(sys.stdin.readline()); print(json.dumps({"jsonrpc":"2.0","id":999,"result":{}}),flush=True)',
        'acp-reverse-request': 'import json,sys\nr=json.loads(sys.stdin.readline()); print(json.dumps({"jsonrpc":"2.0","id":r["id"],"method":"session/update","params":{}}),flush=True)',
        'acp-truncated': 'import sys\nsys.stdin.readline(); sys.stdout.write("{"); sys.stdout.flush()',
        'acp-timeout': 'import sys\nfor line in sys.stdin:\n pass\n'}
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        for index, (reason, source) in enumerate(scripts.items()):
            with self.subTest(reason=reason):
                with run.AcpProcess([sys.executable, '-u', '-c', source], Path(temp) / str(index), timeout=0.2) as client:
                    with self.assertRaisesRegex(provider.ProtocolError, reason):
                        client.call('session/prompt', {})


ProviderContractTests.test_acp_cancel_is_an_idless_notification = test_acp_cancel_is_an_idless_notification
ProviderContractTests.test_acp_held_prompt_interleaves_install_ack_and_notification_cancel = test_acp_held_prompt_interleaves_install_ack_and_notification_cancel
ProviderContractTests.test_acp_pending_requests_are_bounded = test_acp_pending_requests_are_bounded
ProviderContractTests.test_acp_foreign_reverse_truncated_and_timeout_responses_are_rejected = test_acp_foreign_reverse_truncated_and_timeout_responses_are_rejected


def responses_text_events():
    import copy
    items = [
        {'type': 'reasoning', 'id': 'rs_fixture', 'summary': [
            {'type': 'summary_text', 'text': 'Check α. \nKeep order.'}, {'type': 'summary_text', 'text': 'Second summary.'}]},
        {'type': 'message', 'id': 'msg_fixture', 'role': 'assistant', 'status': 'completed', 'content': [
            {'type': 'output_text', 'text': 'Hello π. \nExact  spacing.', 'annotations': []},
            {'type': 'output_text', 'text': 'Second part!', 'annotations': []}]}]
    response = {'id': 'resp_fragments', 'object': 'response', 'created_at': 1234567890,
                'model': 'fixture-R', 'status': 'in_progress', 'output': []}
    events = [{'type': 'response.created', 'response': copy.deepcopy(response)}]
    for output_index, item in enumerate(items):
        summary = item['type'] == 'reasoning'
        field, index_field = ('summary', 'summary_index') if summary else ('content', 'content_index')
        part_event = 'response.reasoning_summary_part' if summary else 'response.content_part'
        text_event = 'response.reasoning_summary_text' if summary else 'response.output_text'
        initial = copy.deepcopy(item)
        initial[field] = []
        if 'status' in initial:
            initial['status'] = 'in_progress'
        events.append({'type': 'response.output_item.added', 'output_index': output_index, 'item': initial})
        for part_index, part in enumerate(item[field]):
            address = {'item_id': item['id'], 'output_index': output_index, index_field: part_index}
            initial_part = {**copy.deepcopy(part), 'text': ''}
            events.append({'type': part_event + '.added', **address, 'part': initial_part})
            text = part['text']
            for fragment in (text[:3], text[3:8], text[8:]):
                delta = {'type': text_event + '.delta', **address, 'delta': fragment}
                if not summary:
                    delta['logprobs'] = []
                events.append(delta)
            done = {'type': text_event + '.done', **address, 'text': text}
            if not summary:
                done['logprobs'] = []
            events.extend([done, {'type': part_event + '.done', **address, 'part': copy.deepcopy(part)}])
        events.append({'type': 'response.output_item.done', 'output_index': output_index, 'item': copy.deepcopy(item)})
    response.update(status='completed', output=copy.deepcopy(items), usage={'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2})
    events.append({'type': 'response.completed', 'response': response})
    return events, items


def encode_response_events(events):
    return b''.join(('event: ' + event['type'] + '\ndata: ' + json.dumps({**event, 'sequence_number': index},
                   ensure_ascii=False) + '\n\n').encode() for index, event in enumerate(events))


def response_delta_fault(name):
    import copy
    events, items = responses_text_events()
    summary = 'summary' in name
    prefix = 'response.reasoning_summary_text' if summary else 'response.output_text'
    indices = [i for i, event in enumerate(events) if event['type'] == prefix + '.delta']
    if name.startswith('missing_all_'):
        events = [event for event in events if event['type'] != prefix + '.delta']
    elif name.startswith('missing_') and name not in ('missing_part', 'missing_text_done'):
        del events[indices[0]]
    elif name.startswith('reordered_') and name != 'reordered_parts':
        events[indices[0]], events[indices[1]] = events[indices[1]], events[indices[0]]
    elif name.startswith('conflicting_') and name != 'conflicting_part':
        events[indices[0]]['delta'] = 'conflict'
    elif name == 'missing_part':
        events = [event for event in events if not (event['type'] == 'response.content_part.added' and event['content_index'] == 0)]
    elif name == 'missing_text_done':
        events = [event for event in events if event['type'] != 'response.output_text.done']
    elif name == 'reordered_parts':
        next(event for event in events if event['type'] == 'response.content_part.added')['content_index'] = 1
    elif name == 'conflicting_part':
        next(event for event in events if event['type'] == 'response.content_part.done')['part']['text'] = 'conflict'
    elif name == 'wrong_item_id':
        events[indices[0]]['item_id'] = 'other-item'
    elif name in ('terminal_text', 'terminal_summary'):
        index, field = (0, 'summary') if summary else (1, 'content')
        changed = copy.deepcopy(items[index])
        changed[field][0]['text'] = 'terminal conflicts with deltas'
        next(event for event in events if event['type'] == 'response.output_item.done' and event['output_index'] == index)['item'] = changed
        events[-1]['response']['output'][index] = copy.deepcopy(changed)
    return encode_response_events(events)


def response_http_chunks(raw, route):
    body = {'model': 'fixture-R', 'store': False, 'stream': True, 'input': [], 'tools': []}
    with provider.LoopbackProvider([{'path': route, 'body': body, 'response': raw, 'chunk_bytes': 17}], 'single') as server:
        connection = http.client.HTTPConnection('127.0.0.1', server.port, timeout=2)
        try:
            connection.request('POST', route, json.dumps(body))
            response = connection.getresponse()
            if response.status != 200:
                raise AssertionError('unexpected HTTP status')
            chunks = []
            while chunk := response.read(1):
                chunks.append(chunk)
        finally:
            connection.close()
        if server.requests != [body] or server.responses != [raw] or len(server.attempts) != 1 or server.errors:
            raise AssertionError('HTTP capture/order/attempt mismatch')
    return chunks


def responses_routes():
    from smoke.redteam.hp8r.test_validate import manifest
    cases = manifest()['cases']
    return [('Responses-' + cases[name]['route']['dialect'], cases[name]['route']['endpoint']['path'])
            for name in ('tooling-f', 'tooling-f-other')]


RESPONSE_DELTA_REJECTIONS = (
    'missing_text', 'missing_all_text', 'reordered_text', 'conflicting_text',
    'missing_summary', 'missing_all_summary', 'reordered_summary', 'conflicting_summary',
    'missing_part', 'missing_text_done', 'reordered_parts', 'conflicting_part',
    'wrong_item_id', 'terminal_text', 'terminal_summary')


def response_delta_rejection_test(name):
    def test(self):
        raw = response_delta_fault(name)
        for dialect, route in responses_routes():
            with self.subTest(dialect=dialect, fault=name):
                chunks = response_http_chunks(raw, route)
                self.assertEqual(b''.join(chunks), raw)
                with self.assertRaises(provider.ProtocolError):
                    provider.decode_sse(b''.join(chunks), dialect)
    return test


def test_responses_http_fragmented_text_parts_and_reasoning_match_completed_items(self):
    events, items = responses_text_events()
    raw = encode_response_events(events)
    for dialect, route in responses_routes():
        with self.subTest(dialect=dialect):
            chunks = response_http_chunks(raw, route)
            self.assertEqual(b''.join(chunks), raw)
            self.assertIn(b'\xcf', chunks)
            self.assertEqual(provider.decode_sse_chunks(iter(chunks), dialect), {
                'blocks': items, 'stop_reason': 'completed', 'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}})
            self.assertEqual(provider.sse_events(raw), [{**event, 'sequence_number': index} for index, event in enumerate(events)])


def test_responses_chunk_accumulation_is_bounded_and_truncation_is_not_success(self):
    events, _ = responses_text_events()
    raw = encode_response_events(events)
    with self.assertRaisesRegex(provider.ProtocolError, 'stream-cap'):
        provider.decode_sse_chunks([raw, b':' + b'x' * provider.MAX_BODY + b'\n\n'], 'Responses-Codex')
    for bad in (raw[:-1], raw.rsplit(b'event: response.completed', 1)[0]):
        with self.subTest(length=len(bad)):
            with self.assertRaises(provider.ProtocolError):
                provider.decode_sse_chunks([bad[:17], bad[17:]], 'Responses-Other')


for _name in RESPONSE_DELTA_REJECTIONS:
    setattr(ProviderContractTests, 'test_responses_http_reject_' + _name, response_delta_rejection_test(_name))
ProviderContractTests.test_responses_http_fragmented_text_parts_and_reasoning_match_completed_items = test_responses_http_fragmented_text_parts_and_reasoning_match_completed_items
ProviderContractTests.test_responses_chunk_accumulation_is_bounded_and_truncation_is_not_success = test_responses_chunk_accumulation_is_bounded_and_truncation_is_not_success


def function_argument_events():
    import copy
    items = [{'type': 'function_call', 'id': 'fc_' + str(index), 'call_id': 'call_' + str(index),
              'name': 'read_fixture_' + str(index), 'arguments': json.dumps({'city': city}, ensure_ascii=False),
              'status': 'completed'} for index, city in enumerate(('Paris π', 'Berlin β'))]
    events = [{'type': 'response.created', 'response': {'id': 'resp_arguments', 'object': 'response',
               'status': 'in_progress', 'output': []}}]
    for index, item in enumerate(items):
        events.append({'type': 'response.output_item.added', 'response_id': 'resp_arguments', 'output_index': index,
                       'item': {**item, 'arguments': '', 'status': 'in_progress'}})
    for start, end in ((0, 4), (4, 9), (9, None)):
        for index, item in enumerate(items):
            events.append({'type': 'response.function_call_arguments.delta', 'response_id': 'resp_arguments',
                           'item_id': item['id'], 'output_index': index, 'delta': item['arguments'][start:end]})
    for index, item in enumerate(items):
        events.extend([
            {'type': 'response.function_call_arguments.done', 'response_id': 'resp_arguments',
             'item_id': item['id'], 'output_index': index, 'arguments': item['arguments']},
            {'type': 'response.output_item.done', 'response_id': 'resp_arguments', 'output_index': index, 'item': copy.deepcopy(item)}])
    events.append({'type': 'response.completed', 'response': {'id': 'resp_arguments', 'object': 'response',
                   'status': 'completed', 'output': copy.deepcopy(items),
                   'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}}})
    return events, items


FUNCTION_ARGUMENT_REJECTIONS = (
    'delta_item', 'done_item', 'delta_response', 'done_response', 'missing_item',
    'changed_call_id', 'changed_name', 'missing_fragment', 'missing_all_fragments',
    'missing_done', 'reordered_fragments', 'conflicting_done', 'conflicting_item',
    'delta_after_done', 'duplicate_done', 'nonfunction_arguments')


def function_argument_fault(name):
    import copy
    events, _ = function_argument_events()
    deltas = [event for event in events if event['type'] == 'response.function_call_arguments.delta' and event['output_index'] == 0]
    done = next(event for event in events if event['type'] == 'response.function_call_arguments.done')
    item_done = next(event for event in events if event['type'] == 'response.output_item.done')
    if name in ('delta_item', 'done_item'):
        (deltas[0] if name == 'delta_item' else done)['item_id'] = 'fc_1'
    elif name in ('delta_response', 'done_response'):
        (deltas[0] if name == 'delta_response' else done)['response_id'] = 'resp_other'
    elif name == 'missing_item':
        del deltas[0]['item_id']
    elif name in ('changed_call_id', 'changed_name', 'conflicting_item'):
        field = {'changed_call_id': 'call_id', 'changed_name': 'name', 'conflicting_item': 'arguments'}[name]
        item_done['item'][field] = 'changed'
        events[-1]['response']['output'][0][field] = 'changed'
    elif name == 'missing_fragment':
        events.remove(deltas[0])
    elif name == 'missing_all_fragments':
        events = [event for event in events if event not in deltas]
    elif name == 'missing_done':
        events.remove(done)
    elif name == 'reordered_fragments':
        first, second = events.index(deltas[0]), events.index(deltas[1])
        events[first], events[second] = events[second], events[first]
    elif name == 'conflicting_done':
        done['arguments'] = '{}'
    elif name == 'delta_after_done':
        events.insert(events.index(done) + 1, {**deltas[0], 'delta': ''})
    elif name == 'duplicate_done':
        events.insert(events.index(done) + 1, copy.deepcopy(done))
    elif name == 'nonfunction_arguments':
        next(event for event in events if event['type'] == 'response.output_item.added')['item']['type'] = 'tool_search_call'
        item_done['item']['type'] = 'tool_search_call'
        events[-1]['response']['output'][0]['type'] = 'tool_search_call'
    return encode_response_events(events)


def function_arguments_test(dialect, route, fault=None):
    def test(self):
        events, items = function_argument_events()
        if fault is not None:
            raw = function_argument_fault(fault)
            chunks = response_http_chunks(raw, route)
            self.assertEqual(b''.join(chunks), raw)
            with self.assertRaises(provider.ProtocolError):
                provider.decode_sse_chunks(chunks, dialect)
        else:
            for response_ids in (True, False):
                if not response_ids:
                    for event in events:
                        event.pop('response_id', None)
                raw = encode_response_events(events)
                chunks = response_http_chunks(raw, route)
                self.assertEqual(b''.join(chunks), raw)
                self.assertEqual(provider.decode_sse_chunks(chunks, dialect), {
                    'blocks': items, 'stop_reason': 'completed', 'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}})
    return test


for _dialect, _route in responses_routes():
    _suffix = _dialect.removeprefix('Responses-').lower()
    setattr(ProviderContractTests, 'test_function_arguments_fragmented_' + _suffix,
            function_arguments_test(_dialect, _route))
    for _fault in FUNCTION_ARGUMENT_REJECTIONS:
        setattr(ProviderContractTests, 'test_function_arguments_reject_' + _fault + '_' + _suffix,
                function_arguments_test(_dialect, _route, _fault))


def production_request_fixture(route):
    if route == '/v1/messages':
        captured = {'model': 'fixture-M', 'stream': True, 'max_tokens': 2048,
                    'messages': [{'role': 'user', 'content': [{'type': 'text', 'text': 'fixture ping'}]}],
                    'system': [{'type': 'text', 'text': 'fixture instructions'}],
                    'thinking': {'type': 'enabled', 'budget_tokens': 1024}, 'tools': []}
        body = {**captured, 'metadata': {'user_id': 'hidden-user-' + 'b' * 64}}
        raw = message_stream([{'type': 'text', 'text': 'fixture pong'}])
    else:
        captured = {'model': 'fixture-R', 'store': False, 'stream': True, 'max_output_tokens': 64,
                    'input': [{'type': 'message', 'role': 'user', 'content': [{'type': 'input_text', 'text': 'fixture ping'}]}],
                    'reasoning': {'effort': 'high', 'summary': 'concise'}, 'tools': [],
                    'temperature': 0.4, 'top_p': 1.0, 'tool_choice': 'auto', 'parallel_tool_calls': False}
        body = {**captured, 'prompt_cache_key': 'hidden-cache-' + 'a' * 64}
        raw = response_stream([])
    return body, captured, raw


def production_request_test(route):
    def test(self):
        import copy
        from unittest import mock
        body, captured, raw = production_request_fixture(route)
        original = copy.deepcopy(body)
        scripted = copy.deepcopy(body)
        if 'prompt_cache_key' in scripted:
            scripted['prompt_cache_key'] = 'different-script-session'
        else:
            scripted['metadata']['user_id'] = 'different-script-user'
        steps = [{'path': route, 'body': expected, 'response': raw} for expected in (captured, scripted)]
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
                with provider.LoopbackProvider(steps, 'single') as server:
                    for _ in steps:
                        connection = http.client.HTTPConnection('127.0.0.1', server.port, timeout=2)
                        try:
                            connection.request('POST', route, json.dumps(body), {'Authorization': 'synthetic-local-only'})
                            response = connection.getresponse()
                            self.assertEqual(response.status, 200)
                            self.assertEqual(response.read(), raw)
                        finally:
                            connection.close()
                    self.assertEqual(server.requests, [captured, captured])
                    self.assertEqual([attempt.get('omitted_fields') for attempt in server.attempts], [1, 1])
                    self.assertEqual(server.errors, [])
                    destination = Path(temp) / 'capture.json'
                    provider.capture_json(destination, server.requests[0])
                    self.assertEqual(json.loads(destination.read_text()), captured)
                self.assertTrue(hashed.called)
                for call in hashed.call_args_list:
                    self.assertNotIn(b'hidden-cache-', call.args[0])
                    self.assertNotIn(b'hidden-user-', call.args[0])
            self.assertEqual(body, original)
    return test


for _suffix, _route in (('codex', '/codex/v1/responses'), ('other', '/other/v1/responses'), ('messages', '/v1/messages')):
    setattr(ProviderContractTests, 'test_production_request_is_projected_before_capture_' + _suffix, production_request_test(_route))


def test_malformed_cli_inputs_have_distinct_invalid_exits(self):
    import subprocess
    import sys
    scripts = Path(__file__).parent
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        root = Path(temp)
        cases = [('run.py', '{"cases": []}'), ('run.py', '[' * 2000 + '0' + ']' * 2000),
                 ('validate.py', '[' * 2000 + '0' + ']' * 2000)]
        for index, (script, raw) in enumerate(cases):
            with self.subTest(script=script, index=index):
                manifest_path = root / f'malformed-{index}.json'
                manifest_path.write_text(raw)
                args = [sys.executable, str(scripts / script), '--manifest', str(manifest_path)]
                destination = root / f'not-created-{index}'
                if script == 'run.py':
                    args += ['--layer', 'L0', '--case', 'fixture', '--bin', sys.executable, '--out', str(destination)]
                else:
                    args += ['--report', str(destination)]
                result = subprocess.run(args, capture_output=True, timeout=3)
                self.assertEqual(result.returncode, 2)
                response = json.loads(result.stdout)
                self.assertIn(response['reason'], ('TypeError', 'RecursionError', 'manifest-shape'))
                self.assertFalse(destination.exists())
                self.assertNotIn(b'Traceback', result.stderr)


def test_http_disconnect_and_partial_body_are_not_complete_streams(self):
    body = {'model': 'fixture-M', 'messages': [], 'stream': True}
    raw = message_stream([{'type': 'text', 'text': 'partial fixture'}])
    for fault in ({'disconnect': True}, {'truncate_after': 17}, {'truncate_after': len(raw) - 1}):
        with self.subTest(fault=fault):
            step = {'path': '/v1/messages', 'body': body, 'response': raw, **fault}
            with provider.LoopbackProvider([step], 'single') as server:
                connection = http.client.HTTPConnection('127.0.0.1', server.port, timeout=2)
                try:
                    connection.request('POST', '/v1/messages', json.dumps(body))
                    if fault.get('disconnect'):
                        with self.assertRaises(http.client.RemoteDisconnected):
                            connection.getresponse()
                        self.assertEqual(server.responses, [])
                    else:
                        response = connection.getresponse()
                        self.assertEqual(response.status, 200)
                        with self.assertRaises(http.client.IncompleteRead) as error:
                            response.read()
                        partial = error.exception.partial
                        self.assertEqual(partial, raw[:fault['truncate_after']])
                        self.assertEqual(server.responses, [partial])
                        with self.assertRaises(provider.ProtocolError):
                            provider.decode_sse(partial, 'Messages')
                finally:
                    connection.close()
                self.assertEqual(len(server.attempts), 1)
                self.assertEqual(server.requests, [body])


ProviderContractTests.test_malformed_cli_inputs_have_distinct_invalid_exits = test_malformed_cli_inputs_have_distinct_invalid_exits
ProviderContractTests.test_http_disconnect_and_partial_body_are_not_complete_streams = test_http_disconnect_and_partial_body_are_not_complete_streams


def http_capture_fixture(route):
    import copy
    body, _, raw = production_request_fixture(route)
    body = copy.deepcopy(body)
    schema = {'type': 'object', 'properties': {
        'model_cache': {'type': 'object', 'properties': {'digest': {'type': 'string'}}}}}
    payload = {'model_cache': {'digest': 'declared-tool-payload'}, 'value': ['first', 'second']}
    if route == '/v1/messages':
        body['tools'] = [{'name': 'read_fixture', 'description': 'Read-only fixture.', 'input_schema': schema,
                          'cache_control': {'type': 'ephemeral'}}]
        body['messages'].append({'role': 'assistant', 'content': [
            {'type': 'tool_use', 'id': 'tool_payload', 'name': 'read_fixture', 'input': payload}]})
        body['system'][0]['cache_control'] = {'type': 'ephemeral'}
    else:
        body['tools'] = [{'type': 'function', 'name': 'read_fixture', 'parameters': schema, 'strict': False}]
        body['input'].append({'type': 'function_call_output', 'call_id': 'call_payload', 'output': payload})
    return body, raw


def http_capture_test(route, location=None):
    def test(self):
        import copy
        from unittest import mock
        body, raw = http_capture_fixture(route)
        if location is not None:
            items = body['messages' if route == '/v1/messages' else 'input']
            target = {'item': items[0], 'content': items[0]['content'][0], 'tool': body['tools'][0],
                      'options': body['thinking' if route == '/v1/messages' else 'reasoning']}[location]
            target['model_cache'] = {'digest': 'a' * 64}
        expected = {key: copy.deepcopy(value) for key, value in body.items() if key in provider.CAPTURE_FIELDS}
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            with provider.LoopbackProvider([{'path': route, 'body': body, 'response': raw}], 'single') as server:
                connection = http.client.HTTPConnection('127.0.0.1', server.port, timeout=2)
                try:
                    connection.request('POST', route, json.dumps(body), {'Authorization': 'synthetic-local-only'})
                    response = connection.getresponse()
                    self.assertEqual(response.status, 200)
                    self.assertEqual(response.read(), raw)
                finally:
                    connection.close()
                self.assertEqual(server.requests, [expected])
                self.assertEqual(server.errors, [])
                destination = Path(temp) / 'capture.json'
                hash_fn = provider.hashlib.sha256
                with mock.patch.object(provider.hashlib, 'sha256', wraps=hash_fn) as hashed:
                    if location is not None:
                        with self.assertRaisesRegex(provider.ProtocolError, 'allowlist'):
                            provider.capture_json(destination, server.requests[0])
                        self.assertFalse(destination.exists())
                        hashed.assert_not_called()
                    else:
                        digest = provider.capture_json(destination, server.requests[0])
                        self.assertEqual(json.loads(destination.read_text()), expected)
                        self.assertEqual(digest, hash_fn(destination.read_bytes()).hexdigest())
                        self.assertEqual(hashed.call_count, 1)
                        self.assertNotIn(b'hidden-cache-', hashed.call_args.args[0])
                        self.assertNotIn(b'hidden-user-', hashed.call_args.args[0])
    return test


for _suffix, _route in (('codex', '/codex/v1/responses'), ('other', '/other/v1/responses'), ('messages', '/v1/messages')):
    setattr(ProviderContractTests, 'test_http_capture_preserves_declared_payloads_' + _suffix, http_capture_test(_route))
    for _location in ('item', 'content', 'tool', 'options'):
        setattr(ProviderContractTests, 'test_http_capture_rejects_nested_' + _location + '_' + _suffix,
                http_capture_test(_route, _location))


RESPONSE_ENVELOPE_CAPTURE_FIELDS = frozenset((
    'id', 'object', 'created_at', 'completed_at', 'model', 'status', 'output', 'usage', 'error',
    'incomplete_details', 'max_output_tokens', 'parallel_tool_calls', 'reasoning', 'store',
    'temperature', 'text', 'tool_choice', 'tools', 'top_p'))


def production_response_events(payload='text', metadata_phase='both', key_is_null=False):
    import copy
    events, items = responses_text_events() if payload == 'text' else function_argument_events()
    for event in events:
        if 'response' not in event:
            continue
        event['model'] = 'fixture-R'
        response = event['response']
        response.update(created_at=1234567890, model='fixture-R', parallel_tool_calls=False,
                        temperature=0.4, tool_choice='auto', tools=[], top_p=1.0, max_output_tokens=64,
                        reasoning={'effort': 'high', 'summary': 'auto'},
                        text={'format': {'type': 'text'}, 'verbosity': 'medium'}, truncation='disabled',
                        store=False, background=False, frequency_penalty=0.0, presence_penalty=0.0,
                        service_tier='default', tool_usage={}, top_logprobs=0, error=None, incomplete_details=None)
        if event['type'] == 'response.completed':
            response.update(completed_at=1234567891, content_filters=[])
            response['usage'].update(cost=0.0, input_tokens_details={'cached_tokens': 0},
                                     output_tokens_details={'reasoning_tokens': 0})
        if metadata_phase in ('both', event['type'].removeprefix('response.')):
            response.update(prompt_cache_key=None if key_is_null else 'hidden-response-key-' + 'a' * 64,
                            prompt_cache_retention='in_memory',
                            metadata={'tenant': 'hidden-response-metadata'},
                            safety_identifier='hidden-response-safety', user='hidden-response-user')
    expected_events = []
    for index, event in enumerate(events):
        expected = copy.deepcopy(event)
        expected['sequence_number'] = index
        if 'response' in expected:
            expected['response'] = {key: value for key, value in expected['response'].items()
                                    if key in RESPONSE_ENVELOPE_CAPTURE_FIELDS}
        expected_events.append(expected)
    return events, items, expected_events


def production_response_test(dialect, route, phase='both'):
    def test(self):
        from unittest import mock
        for payload in ('text', 'arguments'):
            for key_is_null in (False, True):
                with self.subTest(payload=payload, key_is_null=key_is_null, phase=phase):
                    events, items, expected_events = production_response_events(payload, phase, key_is_null)
                    raw = encode_response_events(events)
                    chunks = response_http_chunks(raw, route)
                    self.assertTrue(b''.join(chunks) == raw, 'transport changed the production-shaped frames')
                    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
                        with mock.patch.object(provider, 'screen', wraps=provider.screen) as screened:
                            try:
                                decoded = provider.decode_sse_chunks(chunks, dialect)
                                projected = provider.sse_events(raw)
                            except provider.ProtocolError as error:
                                self.fail('production response envelope rejected before assembly: ' + str(error))
                            expected_result = {'blocks': items, 'stop_reason': 'completed',
                                               'usage': expected_events[-1]['response']['usage']}
                            self.assertTrue(decoded == expected_result, 'decoded payload, identity, order, or usage changed')
                            self.assertTrue(projected == expected_events, 'event capture projection changed payload or retained metadata')
                            envelopes = [event['response'] for event in projected if 'response' in event]
                            with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
                                for index, envelope in enumerate(envelopes):
                                    destination = Path(temp) / f'response-{index}.json'
                                    try:
                                        provider.capture_json(destination, envelope)
                                    except provider.ProtocolError as error:
                                        self.fail('projected response capture rejected: ' + str(error))
                                    self.assertTrue(json.loads(destination.read_text()) == envelope,
                                                    'captured envelope differs from the safe projection')
                                    self.assertNotIn(b'hidden-response-', destination.read_bytes())
                                self.assertEqual(hashed.call_count, len(envelopes))
                                self.assertTrue(all(b'hidden-response-' not in call.args[0]
                                                    for call in hashed.call_args_list), 'excluded envelope value reached a digest')
                            self.assertTrue(all('hidden-response-' not in json.dumps(call.args[0])
                                                for call in screened.call_args_list), 'excluded envelope value reached screening')
    return test


def retained_response_secret_test(dialect):
    def test(self):
        for fault in ('value', 'field'):
            events, _, _ = production_response_events(metadata_phase='none')
            if fault == 'value':
                events[-1]['response']['model'] = 'sK-' + 'b' * 20
                expected = 'excluded-content'
            else:
                events[-1]['response']['output'][0]['authorization'] = 'synthetic-local-only'
                expected = 'excluded-field'
            with self.subTest(fault=fault):
                with self.assertRaisesRegex(provider.ProtocolError, expected):
                    provider.decode_sse(encode_response_events(events), dialect)
    return test


for _dialect, _route in responses_routes():
    _suffix = _dialect.removeprefix('Responses-').lower()
    for _phase in ('both', 'created', 'completed'):
        setattr(ProviderContractTests, 'test_production_response_envelopes_' + _phase + '_' + _suffix,
                production_response_test(_dialect, _route, _phase))
    setattr(ProviderContractTests, 'test_retained_response_secrets_are_rejected_' + _suffix,
            retained_response_secret_test(_dialect))


def native_search_declaration():
    # responses.rs::tool_search_declaration_entry, client/Omit/default-limit=8.
    return {'type': 'tool_search', 'execution': 'client',
            'description': '# Tool discovery\n\nSearches over deferred tool metadata with BM25 and exposes matching tools for the next model call.\n\nSome of the tools may not have been provided to you upfront, and you should use this tool (`tool_search`) to search for the required tools. For MCP tool discovery, always use `tool_search` instead of `list_mcp_resources` or `list_mcp_resource_templates`.',
            'parameters': {'type': 'object', 'properties': {
                'limit': {'type': 'number', 'description': 'Maximum number of tools to return. Defaults to 8.'},
                'query': {'type': 'string', 'description': 'Search query for deferred tools.'}},
                'required': ['query'], 'additionalProperties': False}}


def native_capture_fixture(family, presence='value'):
    import copy
    tool = {'type': 'function', 'name': 'read_fixture', 'description': 'Read-only fixture.',
            'parameters': {'type': 'object', 'properties': {'name': {'type': 'string'}},
                           'required': ['name'], 'additionalProperties': False}, 'strict': True, 'defer_loading': True}
    if family in ('allowed_callers', 'output_schema') and presence != 'absent':
        value = ['programmatic', 'direct'] if family == 'allowed_callers' else {
            'type': 'object', 'properties': {'model_cache': {'type': 'string'}, 'value': {'type': 'integer'}},
            'required': ['value', 'model_cache'], 'additionalProperties': False}
        tool[family] = None if presence == 'null' else value
    items = [
        {'type': 'tool_search_call', 'id': 'ts_fixture', 'call_id': 'call_fixture', 'execution': 'client',
         'status': 'completed', 'arguments': {'query': 'read fixture', 'limit': 8}},
        {'type': 'tool_search_output', 'call_id': 'call_fixture', 'execution': 'client',
         'status': 'completed', 'tools': [copy.deepcopy(tool), {**copy.deepcopy(tool), 'name': 'read_second'}]}]
    if family == 'created_by' and presence != 'absent':
        for index, item in enumerate(items):
            item['created_by'] = None if presence == 'null' else 'actor_fixture_' + str(index)
    declaration = native_search_declaration() if family in ('declaration', 'keyless') else copy.deepcopy(tool)
    body = {'model': 'fixture-R', 'store': False, 'stream': family != 'keyless',
            'input': copy.deepcopy(items), 'tools': [declaration]}
    if family == 'keyless':
        frozen = json.loads((Path(__file__).parent / 'corpus/fixtures/responses-keyless.json').read_text())
        body['input'] = copy.deepcopy(frozen['output'])
        return body, (json.dumps(frozen, ensure_ascii=False) + '\n').encode(), [frozen]
    envelopes = [{'id': 'resp_fixture', 'object': 'response', 'status': 'in_progress', 'output': [], 'tools': [copy.deepcopy(declaration)]},
                 {'id': 'resp_fixture', 'object': 'response', 'status': 'completed', 'output': items, 'tools': [copy.deepcopy(declaration)],
                  'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}}]
    events = [{'type': 'response.created', 'response': envelopes[0]}]
    for index, item in enumerate(items):
        events.extend([{'type': 'response.output_item.added', 'output_index': index, 'item': item},
                       {'type': 'response.output_item.done', 'output_index': index, 'item': item}])
    events.append({'type': 'response.completed', 'response': envelopes[1]})
    return body, encode_response_events(events), envelopes


def native_http_exchange(test, route, body, raw):
    import copy
    original = copy.deepcopy(body)
    with provider.LoopbackProvider([{'path': route, 'body': body, 'response': raw, 'chunk_bytes': 17}], 'single') as server:
        connection = http.client.HTTPConnection('127.0.0.1', server.port, timeout=2)
        try:
            connection.request('POST', route, json.dumps(body), {'Authorization': 'synthetic-local-only'})
            response = connection.getresponse()
            test.assertEqual(response.status, 200)
            received = response.read()
        finally:
            connection.close()
        test.assertEqual(received, raw)
        test.assertEqual(server.requests, [original])
        test.assertEqual(server.responses, [raw])
        test.assertEqual(len(server.attempts), 1)
        test.assertEqual(server.errors, [])
        test.assertEqual(body, original)
        return server.requests[0], received


def assert_native_capture(test, destination, value):
    import copy
    from unittest import mock
    original = copy.deepcopy(value)
    hash_fn = provider.hashlib.sha256
    with mock.patch.object(provider.hashlib, 'sha256', wraps=hash_fn) as hashed:
        try:
            digest = provider.capture_json(destination, value)
        except provider.ProtocolError as error:
            test.fail('production-shaped native capture rejected: ' + str(error))
        test.assertEqual(json.loads(destination.read_text()), original)
        test.assertEqual(destination.read_bytes(), (json.dumps(original, ensure_ascii=False, allow_nan=False) + '\n').encode())
        test.assertEqual(digest, hash_fn(destination.read_bytes()).hexdigest())
        test.assertEqual(hashed.call_count, 1)
    test.assertEqual(value, original)


def native_http_capture_test(dialect, route, family, direction):
    def test(self):
        presences = ('absent', 'null', 'value') if family in ('created_by', 'allowed_callers', 'output_schema') else ('value',)
        for presence in presences:
            with self.subTest(family=family, direction=direction, presence=presence):
                body, raw, expected_envelopes = native_capture_fixture(family, presence)
                if family == 'keyless':
                    frozen = expected_envelopes[0]
                    created = {'id': 'resp_keyless_fixture', 'object': 'response', 'status': 'in_progress', 'output': []}
                    terminal = {'id': 'resp_keyless_fixture', **frozen}
                    events = [{'type': 'response.created', 'response': created}]
                    for index, item in enumerate(frozen['output']):
                        events.extend([{'type': 'response.output_item.added', 'output_index': index, 'item': item},
                                       {'type': 'response.output_item.done', 'output_index': index, 'item': item}])
                    events.append({'type': 'response.incomplete', 'response': terminal})
                    raw = encode_response_events(events)
                    expected_envelopes = [created, terminal]
                    body['stream'] = True
                request, received = native_http_exchange(self, route, body, raw)
                projected = provider.sse_events(received)
                envelopes = [event['response'] for event in projected if 'response' in event]
                self.assertEqual(envelopes, expected_envelopes)
                decoded = provider.decode_sse(received, dialect)
                self.assertEqual(decoded, {'blocks': expected_envelopes[-1]['output'],
                                          'stop_reason': expected_envelopes[-1]['status'],
                                          'usage': expected_envelopes[-1].get('usage', {})})
                values = [request] if direction == 'request' else envelopes
                with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
                    for index, value in enumerate(values):
                        assert_native_capture(self, Path(temp) / f'native-{index}.json', value)
    return test


def messages_thinking_capture_test(direction):
    def test(self):
        for presence in ('absent', 'empty', 'value'):
            with self.subTest(direction=direction, signature=presence):
                thinking = {'type': 'thinking', 'thinking': 'Inspect the fixture π.'}
                start = {'type': 'thinking', 'thinking': ''}
                if presence != 'absent':
                    thinking['signature'] = 'fixture-signature' if presence == 'value' else ''
                    start['signature'] = ''
                blocks = [thinking, {'type': 'text', 'text': 'Use the read-only fixture.'},
                          {'type': 'tool_use', 'id': 'thinking_tool', 'name': 'read_fixture', 'input': {'name': 'sky'}}]
                events = [{'type': 'message_start', 'message': {
                    'id': 'msg_thinking', 'type': 'message', 'role': 'assistant', 'model': 'fixture-M',
                    'content': [], 'stop_reason': None, 'stop_sequence': None,
                    'usage': {'input_tokens': 1, 'output_tokens': 0}}},
                    {'type': 'content_block_start', 'index': 0, 'content_block': start}]
                for text in ('Inspect the ', 'fixture π.'):
                    events.append({'type': 'content_block_delta', 'index': 0,
                                   'delta': {'type': 'thinking_delta', 'thinking': text}})
                if presence == 'value':
                    events.append({'type': 'content_block_delta', 'index': 0,
                                   'delta': {'type': 'signature_delta', 'signature': thinking['signature']}})
                events.append({'type': 'content_block_stop', 'index': 0})
                for index, block in enumerate(blocks[1:], 1):
                    events.extend([{'type': 'content_block_start', 'index': index, 'content_block': block},
                                   {'type': 'content_block_stop', 'index': index}])
                events.extend([{'type': 'message_delta', 'delta': {'stop_reason': 'tool_use', 'stop_sequence': None},
                                'usage': {'output_tokens': 3}}, {'type': 'message_stop'}])
                raw = b''.join(('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n').encode() for event in events)
                body = {'model': 'fixture-M', 'max_tokens': 2048, 'stream': True, 'tools': [],
                        'thinking': {'type': 'enabled', 'budget_tokens': 1024},
                        'messages': [{'role': 'user', 'content': 'Inspect the fixture.'},
                                     {'role': 'assistant', 'content': blocks},
                                     {'role': 'user', 'content': [{'type': 'tool_result', 'tool_use_id': 'thinking_tool', 'content': 'clear'}]}]}
                request, received = native_http_exchange(self, '/v1/messages', body, raw)
                decoded = provider.decode_sse(received, 'Messages')
                expected_blocks = json.loads(json.dumps(blocks))
                if presence != 'absent':
                    expected_blocks[0]['signature'] = signature_projection(thinking['signature'])
                self.assertEqual(decoded, {'blocks': expected_blocks, 'stop_reason': 'tool_use',
                                           'usage': {'input_tokens': 1, 'output_tokens': 3}})
                response = {**events[0]['message'], 'content': decoded['blocks'],
                            'stop_reason': decoded['stop_reason'], 'usage': decoded['usage']}
                with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
                    value = request if direction == 'request' else response
                    expected = json.loads(json.dumps(value))
                    if direction == 'request' and presence != 'absent':
                        expected['messages'][1]['content'][0]['signature'] = signature_projection(thinking['signature'])
                    destination = Path(temp) / 'thinking.json'
                    provider.capture_json(destination, value)
                    self.assertEqual(json.loads(destination.read_bytes()), expected)
                    self.assertEqual(destination.read_bytes(), (json.dumps(expected, ensure_ascii=False, allow_nan=False) + '\n').encode())
    return test


def native_metadata_rejection_test(dialect, route, location):
    def test(self):
        from unittest import mock
        body, raw, _ = native_capture_fixture('declaration')
        request, received = native_http_exchange(self, route, body, raw)
        envelope = provider.sse_events(received)[-1]['response']
        model_cache = {'model_cache': {'digest': 'a' * 64}}
        if location == 'declaration':
            request['tools'][0].update(model_cache)
            value = request
        elif location == 'created_by':
            envelope['output'][0]['created_by'] = model_cache
            value = envelope
        elif location == 'allowed_callers':
            envelope['output'][1]['tools'][0]['allowed_callers'] = [model_cache]
            value = envelope
        else:
            envelope['output'][1]['tools'][0].update(model_cache)
            value = envelope
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            destination = Path(temp) / 'rejected.json'
            with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
                with self.assertRaisesRegex(provider.ProtocolError, 'capture-allowlist'):
                    provider.capture_json(destination, value)
                self.assertFalse(destination.exists())
                hashed.assert_not_called()
    return test


for _dialect, _route in responses_routes():
    _suffix = _dialect.removeprefix('Responses-').lower()
    for _family in ('declaration', 'created_by', 'allowed_callers', 'output_schema', 'keyless'):
        for _direction in ('request', 'response'):
            setattr(ProviderContractTests, 'test_native_http_capture_' + _family + '_' + _direction + '_' + _suffix,
                    native_http_capture_test(_dialect, _route, _family, _direction))
    for _location in ('declaration', 'created_by', 'allowed_callers', 'returned_tool'):
        setattr(ProviderContractTests, 'test_native_http_capture_rejects_metadata_' + _location + '_' + _suffix,
                native_metadata_rejection_test(_dialect, _route, _location))
for _direction in ('request', 'response'):
    setattr(ProviderContractTests, 'test_native_http_capture_thinking_' + _direction + '_messages',
            messages_thinking_capture_test(_direction))


def test_native_request_report_writer_preserves_protocol_fields(self):
    from smoke.redteam.hp8r import validate
    for family in ('declaration', 'created_by', 'allowed_callers', 'output_schema', 'keyless'):
        with self.subTest(family=family):
            body, _, _ = native_capture_fixture(family)
            with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
                destination = Path(temp) / 'report'
                try:
                    inventory = validate.write_screened_artifacts(destination, {'wire/req-001.json': body})
                except validate.ReceiptError as error:
                    self.fail('production-shaped report capture rejected: ' + str(error))
                written = destination / 'wire/req-001.json'
                self.assertEqual(json.loads(written.read_text()), body)
                self.assertEqual(inventory, [{'path': 'wire/req-001.json', 'sha256': validate.sha(written.read_bytes())}])


ProviderContractTests.test_native_request_report_writer_preserves_protocol_fields = test_native_request_report_writer_preserves_protocol_fields


def test_corpus_derived_native_capture_fields(self):
    import copy
    from smoke.redteam.hp8r import validate
    tool = {'type': 'function', 'name': 'read_fixture', 'parameters': {'type': 'object'},
            'strict': True, 'defer_loading': None, 'allowed_callers': None,
            'output_schema': None, 'format': None, 'tools': None}
    values = [
        {'model': 'fixture-R', 'input': [{'type': 'compaction', 'id': 'compaction_1',
                                        'encrypted_content': 'opaque-compaction'}], 'tools': [tool], 'store': False},
        {'id': 'resp_fixture', 'object': 'response', 'status': 'completed', 'model': 'fixture-R',
         'output': [{'type': 'function_call', 'id': 'fn_1', 'call_id': 'call_1', 'name': 'read_fixture',
                     'namespace': None, 'caller': None, 'arguments': '{}', 'status': 'completed',
                     'provider_specific_fields': {'thought_signature': 'opaque-thought'}}],
         'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2,
                   'input_tokens_details': {'cached_tokens': 0, 'cache_write_tokens': 0, 'audio_tokens': None, 'text_tokens': None},
                   'output_tokens_details': {'reasoning_tokens': 0, 'audio_tokens': None, 'text_tokens': None}},
         'tools': [tool]},
        {'id': 'msg_fixture', 'type': 'message', 'role': 'assistant', 'model': 'fixture-M',
         'content': [{'type': 'tool_search_tool_result', 'tool_use_id': 'srv_1',
                      'content': {'type': 'tool_search_tool_search_result', 'tool_references': [{'type': 'tool_reference', 'tool_name': 'read_fixture'}]}}],
         'container': None, 'stop_details': None, 'context_management': None, 'stop_reason': 'end_turn',
         'usage': {'input_tokens': 1, 'output_tokens': 1, 'output_tokens_details': {'thinking_tokens': 1}}}]
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        for index, value in enumerate(values):
            original = copy.deepcopy(value)
            try:
                provider.capture_json(Path(temp) / f'capture-{index}.json', value)
            except provider.ProtocolError as error:
                self.fail('corpus-derived capture rejected: ' + str(error))
            captured = json.loads((Path(temp) / f'capture-{index}.json').read_text())
            self.assertEqual(value, original)
            validate.capture_http_body(captured)
            self.assertEqual(captured.get('usage'), value.get('usage'))
            self.assertEqual(captured.get('tools'), value.get('tools'))


def test_opaque_capture_projects_before_any_hash_or_write(self):
    from unittest import mock
    ciphertext = 'litellm_enc:opaque-' + 'sK-' + 'a' * 20
    value = {'model': 'fixture-R', 'input': [{'type': 'reasoning', 'id': 'rs_1', 'summary': [],
                                           'encrypted_content': ciphertext}], 'store': False}
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        destination = Path(temp) / 'opaque.json'
        try:
            provider.capture_json(destination, value)
        except provider.ProtocolError as error:
            self.fail('declared opaque ciphertext rejected: ' + str(error))
        captured = json.loads(destination.read_text())
        expected = {'scheme_prefix': 'litellm_enc:', 'sha256': provider.hashlib.sha256(ciphertext.encode()).hexdigest(),
                    'byte_length': len(ciphertext.encode())}
        self.assertTrue(captured['input'][0]['encrypted_content'] == expected, 'ciphertext projection changed')
        self.assertTrue(ciphertext.encode() not in destination.read_bytes(), 'raw ciphertext persisted')
        value['input'].append({'type': 'message', 'role': 'user', 'content': [{'type': 'input_text', 'text': '<synthetic-secret>'}]})
        rejected = Path(temp) / 'secret.json'
        with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
            with self.assertRaises(provider.ProtocolError):
                provider.capture_json(rejected, value)
            hashed.assert_not_called()
            self.assertFalse(rejected.exists())


def test_structured_search_result_metadata_is_rejected_before_hash(self):
    from unittest import mock
    for location in ('result', 'reference'):
        content = {'type': 'tool_search_tool_search_result', 'tool_references': [{'type': 'tool_reference', 'tool_name': 'read_fixture'}]}
        target = content if location == 'result' else content['tool_references'][0]
        target['model_cache'] = {'digest': 'a' * 64}
        value = {'type': 'message', 'content': [{'type': 'tool_search_tool_result', 'tool_use_id': 'srv_1', 'content': content}]}
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            destination = Path(temp) / 'rejected.json'
            with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
                with self.assertRaisesRegex(provider.ProtocolError, 'capture-allowlist'):
                    provider.capture_json(destination, value)
                hashed.assert_not_called()
                self.assertFalse(destination.exists())


ProviderContractTests.test_corpus_derived_native_capture_fields = test_corpus_derived_native_capture_fields
ProviderContractTests.test_opaque_capture_projects_before_any_hash_or_write = test_opaque_capture_projects_before_any_hash_or_write
ProviderContractTests.test_structured_search_result_metadata_is_rejected_before_hash = test_structured_search_result_metadata_is_rejected_before_hash


def test_recorded_provider_errors_are_typed_unsuccessful(self):
    import copy
    error = {'type': None, 'code': '500', 'message': 'synthetic upstream failure', 'param': ''}
    for dialect in provider.DIALECTS:
        start = ({'type': 'message_start', 'message': {'type': 'message', 'role': 'assistant', 'content': []}}
                 if dialect == 'Messages' else {'type': 'response.created', 'response': {'id': 'resp_failed', 'object': 'response', 'output': [], 'status': 'in_progress'}})
        for tagged in (False, True):
            failure = {'error': copy.deepcopy(error)}
            if tagged:
                failure['type'] = 'error'
            events = [start, failure]
            raw = b''.join(('data: ' + json.dumps(event) + '\n\n').encode() for event in events)
            try:
                decoded = provider.decode_sse(raw, dialect)
            except provider.ProtocolError as rejected:
                self.fail('captured provider failure discarded: ' + str(rejected))
            self.assertEqual(type(decoded).__name__, 'ProviderFailure')
            self.assertEqual(decoded.http_status, None)
            self.assertEqual(decoded.events, tuple(events))
            self.assertEqual(decoded.failures, ({'event_index': 1, 'error': error, **({'event_type': 'error'} if tagged else {})},))
            self.assertEqual(decoded.as_dict()['outcome'], 'TYPED-FAILURE')
            self.assertNotIn('stop_reason', decoded.as_dict())


def test_partial_sequence_and_empty_logprobs_preserve_every_event(self):
    import copy
    events, items = responses_text_events()
    for event in events:
        if event['type'] == 'response.output_item.done':
            event['sequence_number'] = event['output_index'] + 1
            if event['item']['type'] == 'message':
                for part in event['item']['content']:
                    part['logprobs'] = None
    events.insert(1, {'type': 'keepalive'})
    raw = b''.join(('data: ' + json.dumps(event, ensure_ascii=False) + '\n\n').encode() for event in events)
    for dialect in ('Responses-Codex', 'Responses-Other'):
        try:
            decoded = provider.decode_sse(raw, dialect)
        except provider.ProtocolError as error:
            self.fail('deployed-proxy completion rejected: ' + str(error))
        self.assertEqual(decoded['blocks'], items)
        self.assertEqual(provider.sse_events(raw), events)
        conflicting = copy.deepcopy(events)
        conflicting[-1]['response']['output'][1]['content'][0]['text'] = 'changed terminal text'
        broken = b''.join(('data: ' + json.dumps(event) + '\n\n').encode() for event in conflicting)
        with self.assertRaises(provider.ProtocolError):
            provider.decode_sse(broken, dialect)


ProviderContractTests.test_recorded_provider_errors_are_typed_unsuccessful = test_recorded_provider_errors_are_typed_unsuccessful
ProviderContractTests.test_partial_sequence_and_empty_logprobs_preserve_every_event = test_partial_sequence_and_empty_logprobs_preserve_every_event


def test_schema_union_and_nullable_generate_summary_are_preserved(self):
    value = {'model': 'fixture-R', 'input': [], 'store': False,
             'reasoning': {'effort': 'high', 'generate_summary': None},
             'tools': [{'type': 'function', 'name': 'read_fixture', 'parameters': {
                 'type': ['object', 'null'], 'properties': {'name': {'type': ['string', 'null']}}}}]}
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        destination = Path(temp) / 'union.json'
        try:
            provider.capture_json(destination, value)
        except (provider.ProtocolError, TypeError) as error:
            self.fail('recorded schema union/nullable field rejected: ' + type(error).__name__)
        self.assertEqual(json.loads(destination.read_text()), value)


def test_http_status_and_multiple_stream_failures_are_preserved(self):
    error = {'type': 'rate_limit_error', 'code': None, 'message': 'synthetic throttling', 'param': None,
             'provider_specific_fields': {'error': 'synthetic upstream detail'}}
    for dialect in provider.DIALECTS:
        for status in (400, 429, 500, 503):
            raw = json.dumps({'error': error}).encode()
            try:
                result = provider.decode_http(raw, dialect, status)
            except provider.ProtocolError as rejected:
                self.fail('recorded HTTP error detail discarded: ' + str(rejected))
            self.assertEqual(type(result).__name__, 'ProviderFailure')
            self.assertEqual(result.http_status, status)
            self.assertEqual(result.body, {'error': error})
            self.assertEqual(result.failures, ({'event_index': None, 'error': error},))
        events = [{'type': 'error', 'sequence_number': 7, 'error': error}, {'error': {**error, 'code': '500'}}]
        raw = b''.join(('data: ' + json.dumps(event) + '\n\n').encode() for event in events)
        result = provider.decode_http(raw, dialect, 200)
        self.assertEqual(result.http_status, 200)
        self.assertEqual(result.events, tuple(events))
        self.assertEqual(result.failures, ({'event_index': 0, 'event_type': 'error', 'sequence_number': 7, 'error': error},
                                          {'event_index': 1, 'error': {**error, 'code': '500'}}))
        for body in (b'{', b'', b'upstream unavailable'):
            try:
                raw_failure = provider.decode_http(body, dialect, 500)
            except provider.ProtocolError as rejected:
                self.fail('HTTP failure body discarded: ' + str(rejected))
            self.assertIsInstance(raw_failure, provider.ProviderFailure)
            self.assertEqual(raw_failure.body, body.decode())
            self.assertEqual(raw_failure.failures, ({'event_index': None, 'error': body.decode()},))


def test_report_ciphertext_projection_preflights_all_artifacts(self):
    from unittest import mock
    from smoke.redteam.hp8r import validate
    ciphertext = 'litellm_enc:report-' + 'sK-' + 'a' * 20
    body = {'model': 'fixture-R', 'store': False, 'stream': True, 'tools': [],
            'input': [{'type': 'reasoning', 'id': 'rs_1', 'summary': [], 'encrypted_content': ciphertext}]}
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        root = Path(temp) / 'report'
        try:
            validate.write_screened_artifacts(root, {'wire/req-001.json': body})
        except (validate.ReceiptError, provider.ProtocolError) as error:
            self.fail('report writer rejected declared ciphertext: ' + str(error))
        written = (root / 'wire/req-001.json').read_bytes()
        self.assertTrue(ciphertext.encode() not in written, 'raw ciphertext persisted')
        projected = json.loads(written)
        self.assertEqual(projected['input'][0]['encrypted_content']['sha256'], provider.hashlib.sha256(ciphertext.encode()).hexdigest())
        validate.write_screened_artifacts(Path(temp) / 'again', {'wire/req-001.json': projected})
        self.assertEqual((Path(temp) / 'again/wire/req-001.json').read_bytes(), written)
        bad = {**body, 'input': [{'type': 'message', 'role': 'user', 'content': [{'type': 'input_text', 'text': '<synthetic-secret>'}]}]}
        with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
            with self.assertRaises((validate.ReceiptError, provider.ProtocolError)):
                validate.write_screened_artifacts(Path(temp) / 'rejected', {'wire/req-001.json': body, 'wire/req-002.json': bad})
            hashed.assert_not_called()
            self.assertFalse((Path(temp) / 'rejected').exists())


ProviderContractTests.test_schema_union_and_nullable_generate_summary_are_preserved = test_schema_union_and_nullable_generate_summary_are_preserved
ProviderContractTests.test_http_status_and_multiple_stream_failures_are_preserved = test_http_status_and_multiple_stream_failures_are_preserved
ProviderContractTests.test_report_ciphertext_projection_preflights_all_artifacts = test_report_ciphertext_projection_preflights_all_artifacts


def read_corpus_capture(path):
    from smoke.redteam.hp8r import validate
    path = Path(path)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 32 * 1024 * 1024:
        raise ValueError('corpus-source-bound')
    raw = path.read_bytes()
    text = raw.decode('utf-8')
    status = None
    if path.suffix == '.jsonl':
        rows = [validate.json_bytes(line) for line in text.splitlines() if line.strip()]
        frames = [row for row in rows if isinstance(row, dict) and isinstance(row.get('frame'), str)]
        if frames:
            metadata = [row for row in rows if not isinstance(row, dict) or not isinstance(row.get('frame'), str)]
            if any(not isinstance(row, dict) or set(row) != {'headers', 'n', 'status', 'ts'} for row in metadata):
                raise ValueError('unknown-capture-wrapper')
            if any(set(row) - {'frame', 'frame_index', 'n', 'ts'} for row in frames) or len(metadata) > 1:
                raise ValueError('unknown-capture-wrapper')
            if metadata:
                status = metadata[0]['status']
                if type(status) is not int or not 100 <= status <= 599:
                    raise ValueError('capture-http-status')
            if not frames[0]['frame'].lstrip().startswith(('event:', 'data:', ':')):
                raw = ''.join(row['frame'] for row in frames).encode()
                try:
                    value = validate.json_bytes(raw)
                except validate.ReceiptError:
                    return {'kind': 'text', 'value': raw.decode(), 'raw': raw, 'status': status}
                return {'kind': 'json', 'value': value, 'raw': raw, 'status': status}
            raw = ''.join(row['frame'] if row['frame'].endswith('\n\n') else row['frame'] + '\n\n' for row in frames).encode()
        else:
            raw = b''.join(('data: ' + json.dumps(row, ensure_ascii=False) + '\n\n').encode() for row in rows)
    elif not text.lstrip().startswith(('event:', 'data:', ':')):
        value = validate.json_bytes(raw)
        if isinstance(value, dict) and 'body' in value:
            status = next((value[key] for key in ('status', 'status_code') if type(value.get(key)) is int), None)
            value = value['body']
            if isinstance(value, str):
                if value.lstrip().startswith(('event:', 'data:', ':')):
                    raw = value.encode()
                else:
                    try:
                        value = validate.json_bytes(value.encode())
                    except validate.ReceiptError:
                        return {'kind': 'text', 'value': value, 'raw': value.encode(), 'status': status}
                    return {'kind': 'json', 'value': value, 'raw': json.dumps(value, ensure_ascii=False).encode(), 'status': status}
            else:
                return {'kind': 'json', 'value': value, 'raw': json.dumps(value, ensure_ascii=False).encode(), 'status': status}
        else:
            return {'kind': 'json', 'value': value, 'raw': raw, 'status': status}
    events = []
    for frame in raw.decode().replace('\r\n', '\n').split('\n\n'):
        data = '\n'.join(line[5:].lstrip(' ') for line in frame.splitlines() if line.startswith('data:'))
        if data:
            events.append({'type': '[DONE]'} if data == '[DONE]' else validate.json_bytes(data.encode()))
    return {'kind': 'sse', 'value': events, 'raw': raw, 'status': status}


def corpus_projection(capture, family, policy):
    import copy
    value = copy.deepcopy(capture['value'])
    if capture['kind'] == 'text':
        provider.screen(value)
        return value, []
    if capture['kind'] == 'sse':
        for event in value:
            if isinstance(event, dict) and isinstance(event.get('response'), dict):
                event['response'] = {key: child for key, child in event['response'].items() if key in policy['response_fields']}
    else:
        fields = policy['request_fields'] if family.endswith('-request') or family.startswith('Messages') else policy['response_fields'] + ['type', 'code', 'message', 'param']
        value = {key: child for key, child in value.items() if key in fields}
    opaque = []
    fields = {tuple(item) for item in policy['opaque_fields']}
    root_fields = policy['opaque_root_fields']
    protocol_children = policy['opaque_protocol_children']
    identifier_fields = set(policy['opaque_identifier_fields'])
    def child_role(parent, role, name):
        kind = parent.get('type', 'message')
        if (role in ('root', 'protocol') and name in identifier_fields) or (role == 'tool_call' and name == 'id'):
            return 'identifier'
        if role == 'root':
            return root_fields.get(name)
        if role == 'protocol' and isinstance(kind, str):
            if (kind, name) in fields:
                return 'opaque'
            if name in protocol_children.get(kind, ()):
                return 'protocol'
            if kind == 'function_call' and name == 'provider_specific_fields':
                return 'provider_specific_fields'
            if kind == 'message' and name == 'tool_calls':
                return 'tool_call'
        if role == 'provider_specific_fields' and (role, name) in fields:
            return 'opaque'
        if role == 'delta' and kind == 'signature_delta' and (kind, name) in fields:
            return 'opaque'
        return None
    identifier_paths = {tuple(item) for item in policy['opaque_identifier_paths']}
    identifier_marker = policy['opaque_identifier_marker']
    def identifier(child, path, role):
        return isinstance(child, str) and (role == 'identifier' or path in identifier_paths) and all(child.partition(identifier_marker))
    def visit(child, context=None, key=None, path=(), role='root'):
        if identifier(child, path, role) or (isinstance(child, str) and role == 'opaque'):
            opaque.append((path, child))
            return ''
        if isinstance(child, dict):
            kind = child.get('type', context)
            return {name: visit(item, 'provider_specific_fields' if name == 'provider_specific_fields' else kind, name, (*path, name), child_role(child, role, name)) for name, item in child.items()}
        if isinstance(child, list):
            return [visit(item, context, key, (*path, '*'), role) for item in child]
        return child
    from smoke.redteam.hp8r import validate
    masked = visit(value)
    provider.screen(masked)
    if capture['kind'] == 'sse':
        for event in masked:
            validate.capture_stream_event(event)
    else:
        validate.capture_http_body(masked)
    def projected(child, context=None, key=None, path=(), role='root'):
        if identifier(child, path, role) or (isinstance(child, str) and role == 'opaque'):
            return {'scheme_prefix': identifier_marker if identifier(child, path, role) else 'litellm_enc:' if child.startswith('litellm_enc:') and key != 'signature' else None,
                    'sha256': provider.hashlib.sha256(child.encode()).hexdigest(), 'byte_length': len(child.encode())}
        if isinstance(child, dict):
            kind = child.get('type', context)
            return {name: projected(item, 'provider_specific_fields' if name == 'provider_specific_fields' else kind, name, (*path, name), child_role(child, role, name)) for name, item in child.items()}
        if isinstance(child, list):
            return [projected(item, context, key, (*path, '*'), role) for item in child]
        return child
    result = projected(value)
    if capture['kind'] == 'sse' and 'signature_prefix_field' in policy:
        prefixes = {}
        for original, captured in zip(value, result):
            kind, index = original.get('type'), original.get('index')
            if kind == 'message_start':
                prefixes.clear()
            elif kind == 'content_block_start' and original.get('content_block', {}).get('type') == 'thinking':
                raw = original['content_block'].get('signature', '').encode()
                prefixes[index] = [provider.hashlib.sha256(raw), len(raw)]
            elif kind == 'content_block_delta' and original.get('delta', {}).get('type') == 'signature_delta':
                raw = original['delta']['signature'].encode()
                state = prefixes[index]
                state[0].update(raw)
                state[1] += len(raw)
                captured['delta'][policy['signature_prefix_field']] = {'scheme_prefix': None, 'sha256': state[0].hexdigest(), 'byte_length': state[1]}
            elif kind == 'content_block_stop':
                prefixes.pop(index, None)
    return result, opaque


def corpus_error_records(capture, projected):
    if capture['kind'] == 'text':
        return [{'event_index': None, 'error': projected}] if capture['status'] is not None and capture['status'] >= 400 else []
    if capture['kind'] == 'json':
        if capture['status'] is not None and capture['status'] >= 400 or projected.get('error') is not None or projected.get('type') == 'error':
            return [{'event_index': None, 'error': projected.get('error', projected)}]
        return []
    errors = []
    for index, event in enumerate(projected):
        kind = event.get('type')
        error = event.get('error') if kind != 'response.failed' else event.get('response', {}).get('error')
        if error is not None or kind in ('error', 'response.failed'):
            errors.append({'event_index': index, 'error': error if error is not None else event,
                           **({'event_type': kind} if 'type' in event else {}),
                           **({'sequence_number': event['sequence_number']} if 'sequence_number' in event else {})})
    if not errors and capture['status'] is not None and capture['status'] >= 400:
        errors.append({'event_index': None, 'error': None})
    return errors


def corpus_structure_class(capture, family, projected):
    if corpus_error_records(capture, projected):
        return 'TYPED-FAILURE', 'recorded-provider-error'
    if family.endswith('-request'):
        if family.startswith('Responses') and projected.get('store') is not False:
            return 'UNCLASSIFIED', 'request-store-false-absent'
        return 'SUCCESS', 'request-protocol'
    if capture['kind'] == 'json':
        field = 'content' if family.startswith('Messages') else 'output'
        stop = projected.get('stop_reason' if family.startswith('Messages') else 'status')
        if not isinstance(projected.get(field), list) or not isinstance(stop, str):
            return 'UNCLASSIFIED', 'nonterminal-or-placeholder-envelope'
        return 'SUCCESS', 'response-envelope'
    events = projected
    if family.startswith('Messages'):
        if not any(event.get('type') == 'message_stop' for event in events):
            return 'UNCLASSIFIED', 'stream-without-terminal-or-provider-error'
        return 'SUCCESS', 'message-terminal'
    starts = [event['response'] for event in events if event.get('type') == 'response.created']
    terminals = [event['response'] for event in events if event.get('type') in ('response.completed', 'response.incomplete')]
    if not terminals:
        return 'UNCLASSIFIED', 'stream-without-terminal-or-provider-error'
    if len(starts) != 1 or len(terminals) != 1 or starts[0].get('id') != terminals[0].get('id'):
        return 'LEGACY-CORRUPTION', 'response-identity-or-terminal-count'
    added = [event for event in events if event.get('type') == 'response.output_item.added']
    done = [event for event in events if event.get('type') == 'response.output_item.done']
    indexes = [event.get('output_index') for event in added]
    completed = [event.get('output_index') for event in done]
    if indexes != list(range(len(added))) or sorted(completed) != indexes:
        return 'LEGACY-CORRUPTION', 'missing-duplicate-or-noncontiguous-item-index'
    by_index = {event['output_index']: event['item'] for event in done}
    output = terminals[0].get('output')
    if not isinstance(output, list) or len(output) != len(added):
        return 'LEGACY-CORRUPTION', 'terminal-output-cardinality'
    for index, event in enumerate(added):
        initial, final, terminal = event['item'], by_index[index], output[index]
        for key in ('type', 'id', 'call_id', 'name'):
            if (key in initial) != (key in final) or initial.get(key) != final.get(key):
                return 'LEGACY-CORRUPTION', 'added-completed-item-identity'
            if (key in final) != (key in terminal) or final.get(key) != terminal.get(key):
                return 'LEGACY-CORRUPTION', 'completed-terminal-item-identity'
        if ('status' in final) != ('status' in terminal) or final.get('status') != terminal.get('status'):
            return 'LEGACY-CORRUPTION', 'completed-terminal-status-disagreement'
        if final.get('encrypted_content') != terminal.get('encrypted_content'):
            return 'UNCLASSIFIED', 'opaque-completion-and-terminal-bytes-differ'
    numbers = [event['sequence_number'] for event in events if 'sequence_number' in event and event.get('type') != '[DONE]']
    if any(type(number) is not int for number in numbers) or any(a >= b for a, b in zip(numbers, numbers[1:])):
        return 'LEGACY-CORRUPTION', 'recorded-sequence-regression'
    return 'SUCCESS', 'response-terminal'


def exercise_corpus_case(test, row, policy, root):
    from smoke.redteam.hp8r import validate
    capture = read_corpus_capture(row['path'])
    family = row['family']
    projected, opaque = corpus_projection(capture, family, policy)
    test.assertEqual(validate.sha(validate.canonical(projected)), row['projected_sha256'])
    expected_class, reason = corpus_structure_class(capture, family, projected)
    test.assertEqual((expected_class, reason), (row['classification'], row['reason']))
    dialects = provider.DIALECTS if family == 'Provider-error' else ('Messages',) if family.startswith('Messages') else ('Responses-Codex', 'Responses-Other')
    for dialect in dialects:
        route = '/v1/messages' if dialect == 'Messages' else '/codex/v1/responses' if dialect == 'Responses-Codex' else '/other/v1/responses'
        request_case = family.endswith('-request')
        body = capture['value'] if request_case else ({'model': 'fixture-M', 'messages': [], 'stream': True} if dialect == 'Messages' else
                                                     {'model': 'fixture-R', 'input': [], 'tools': [], 'store': False, 'stream': True})
        raw = (message_stream([]) if dialect == 'Messages' else response_stream([])) if request_case else capture['raw']
        status = capture['status'] or 200
        step = {'path': route, 'body': body, 'response': raw, 'status': status}
        with provider.LoopbackProvider([step], 'single') as server:
            connection = http.client.HTTPConnection('127.0.0.1', server.port, timeout=10)
            try:
                connection.request('POST', route, json.dumps(body, ensure_ascii=False).encode('utf-8'), {'Authorization': 'synthetic-local-only'})
                response = connection.getresponse()
                received = response.read()
                if request_case and expected_class == 'UNCLASSIFIED' and response.status == 400:
                    test.assertEqual(server.requests, [])
                    test.assertEqual(len(server.attempts), 1)
                    continue
                test.assertEqual(response.status, status)
                test.assertTrue(received == raw, 'HTTP changed corpus response bytes')
            finally:
                connection.close()
            test.assertEqual(len(server.attempts), 1)
            test.assertEqual(server.errors, [])
            destination = root / (row['id'] + '-' + dialect + '.json')
            if request_case:
                provider.capture_json(destination, server.requests[0])
                written = destination.read_bytes()
                captured = json.loads(written)
                assert_request_identifiers(test, capture['value'], captured, written)
                for _, ciphertext in opaque:
                    test.assertTrue(not ciphertext or ciphertext.encode() not in written, 'raw request ciphertext persisted')
                test.assertTrue(captured == projected, 'request capture lost a retained corpus field')
                continue
            if capture['kind'] == 'sse':
                provider.capture_sse(destination, received)
                test.assertTrue(provider.sse_events(destination.read_bytes()) == projected, 'SSE capture lost event order, fields or optional presence')
            else:
                captured_body = {'error': projected} if capture['kind'] == 'text' else projected
                provider.capture_json(destination, captured_body)
                test.assertTrue(json.loads(destination.read_text()) == captured_body, 'response capture changed projected fields')
            for _, ciphertext in opaque:
                test.assertTrue(not ciphertext or ciphertext.encode() not in destination.read_bytes(), 'raw ciphertext persisted')
            if expected_class in ('LEGACY-CORRUPTION', 'UNCLASSIFIED'):
                with test.assertRaises(provider.ProtocolError) as rejected:
                    provider.decode_http(received, dialect, capture['status'])
                if row['id'] in LEGACY_CORRUPTION_REASONS:
                    test.assertEqual(str(rejected.exception), LEGACY_CORRUPTION_REASONS[row['id']],
                                     'legacy corruption rejection reason changed')
            else:
                result = provider.decode_http(received, dialect, capture['status'])
                if expected_class == 'TYPED-FAILURE':
                    test.assertIsInstance(result, provider.ProviderFailure)
                    test.assertTrue(result.failures == tuple(corpus_error_records(capture, projected)), 'typed failure error or position changed')
                    test.assertEqual(result.http_status, capture['status'])
                    test.assertTrue((result.events == tuple(projected) if capture['kind'] == 'sse' else result.body == projected), 'typed failure discarded recorded evidence')
                    test.assertNotIn('stop_reason', result.as_dict())
                else:
                    test.assertNotIsInstance(result, provider.ProviderFailure)
                    test.assertIsInstance(result, dict)


def corpus_index():
    from smoke.redteam.hp8r.test_validate import manifest
    from smoke.redteam.hp8r import validate
    binding = manifest()['capture_corpus']
    raw = Path(binding['path']).read_bytes()
    if validate.sha(raw) != binding['sha256']:
        raise AssertionError('frozen corpus index changed')
    return validate.json_bytes(raw)


def test_approved_root_capture_corpus(self):
    index = corpus_index()
    recorded_raw = Path(index['discovery']['path']).read_bytes()
    self.assertEqual(provider.hashlib.sha256(recorded_raw).hexdigest(), index['discovery']['sha256'])
    recorded = json.loads(recorded_raw)
    required = {row['path'] for row in recorded['inventory'] if row.get('family')} | set(index['additional_http_errors'])
    self.assertTrue(set(index['additional_http_errors']) <= {row['path'] for row in recorded['inventory']})
    self.assertEqual({row['path'] for row in index['records']}, required)
    self.assertTrue(required)
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temporary:
        proof_root = Path(os.environ.get('HP8R_TEST_ROOT', temporary))
        with (proof_root / 'corpus-execution.jsonl').open('x') as proof:
            for row in index['records']:
                with self.subTest(path=row['path']):
                    result = {'id': row['id'], 'path': row['path'], 'classification': row['classification'], 'projected_sha256': row['projected_sha256']}
                    try:
                        with tempfile.TemporaryDirectory(dir=proof_root) as temp:
                            exercise_corpus_case(self, row, index['policy'], Path(temp))
                        result['verified'] = True
                    except Exception:
                        result['verified'] = False
                        raise
                    finally:
                        proof.write(json.dumps(result, sort_keys=True) + '\n')
                        proof.flush()



def test_corpus_ciphertext_witness(self):
    index = corpus_index()
    row = next(row for row in index['records'] if row['id'] == index['ciphertext_witness'])
    capture = read_corpus_capture(row['path'])
    try:
        provider.capture_preflight(provider.project_request(capture['value'], '/codex/v1/responses')[0])
    except provider.ProtocolError as rejected:
        self.fail('corpus ciphertext screen rejected: ' + str(rejected))
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        exercise_corpus_case(self, row, index['policy'], Path(temp))


ProviderContractTests.test_approved_root_capture_corpus = test_approved_root_capture_corpus
ProviderContractTests.test_corpus_ciphertext_witness = test_corpus_ciphertext_witness


def test_capture_reader_separates_http_metadata_from_provider_frames(self):
    from unittest import mock
    error = {'error': {'code': '500', 'type': None, 'message': 'synthetic failure', 'param': None}}
    metadata = {'headers': {'Authorization': '<synthetic-secret>', 'cache_identity_hash': 'a' * 64}, 'n': 3, 'status': 500, 'ts': 'fixture'}
    frame = {'frame': 'data: ' + json.dumps(error), 'n': 4, 'ts': 'fixture'}
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        source = Path(temp) / 'wrapped.jsonl'
        source.write_text(json.dumps(metadata) + '\n' + json.dumps(frame) + '\n')
        with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
            capture = read_corpus_capture(source)
            self.assertEqual(capture['status'], 500)
            self.assertEqual(capture['value'], [error])
            hashed.assert_not_called()
            destination = Path(temp) / 'events.sse'
            provider.capture_sse(destination, capture['raw'])
            self.assertNotIn(b'<synthetic-secret>', destination.read_bytes())
            self.assertTrue(all(b'<synthetic-secret>' not in call.args[0] for call in hashed.call_args_list))
        failure = provider.decode_http(capture['raw'], 'Responses-Codex', capture['status'])
        self.assertIsInstance(failure, provider.ProviderFailure)
        self.assertEqual(failure.http_status, 500)
        self.assertEqual(failure.events, (error,))
        json_source = Path(temp) / 'json-error.jsonl'
        json_source.write_text(json.dumps(metadata) + '\n' + json.dumps({**frame, 'frame': json.dumps(error)}) + '\n')
        captured_body = read_corpus_capture(json_source)
        self.assertEqual(captured_body['kind'], 'json')
        self.assertEqual(captured_body['value'], error)
        self.assertEqual(captured_body['raw'], json.dumps(error).encode())
        self.assertEqual(provider.decode_http(captured_body['raw'], 'Messages', captured_body['status']).body, error)


ProviderContractTests.test_capture_reader_separates_http_metadata_from_provider_frames = test_capture_reader_separates_http_metadata_from_provider_frames


def test_reasoning_text_without_content_part_frames_is_reconciled(self):
    import copy
    item = {'type': 'reasoning', 'id': 'rs_text', 'status': 'completed', 'summary': [],
            'content': [{'type': 'reasoning_text', 'text': 'First second.'}]}
    events = [
        {'type': 'response.created', 'response': {'id': 'resp_text', 'object': 'response', 'status': 'in_progress', 'output': []}},
        {'type': 'response.output_item.added', 'output_index': 0, 'item': {**item, 'status': 'in_progress', 'content': []}},
        {'type': 'response.reasoning_text.delta', 'output_index': 0, 'content_index': 0, 'item_id': 'rs_text', 'delta': 'First '},
        {'type': 'response.reasoning_text.delta', 'output_index': 0, 'content_index': 0, 'item_id': 'rs_text', 'delta': 'second.'},
        {'type': 'response.reasoning_text.done', 'output_index': 0, 'content_index': 0, 'item_id': 'rs_text', 'text': 'First second.'},
        {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
        {'type': 'response.completed', 'response': {'id': 'resp_text', 'object': 'response', 'status': 'completed', 'output': [item]}}]
    for dialect in ('Responses-Codex', 'Responses-Other'):
        try:
            result = provider.decode_sse(encode_response_events(events), dialect)
        except provider.ProtocolError as error:
            self.fail('recorded reasoning-text family rejected: ' + str(error))
        self.assertEqual(result['blocks'], [item])
        for fault in ('missing', 'identity', 'text', 'duplicate'):
            changed = copy.deepcopy(events)
            if fault == 'missing':
                del changed[2]
            elif fault == 'identity':
                changed[2]['item_id'] = 'other'
            elif fault == 'text':
                changed[-2]['item']['content'][0]['text'] = 'different'
            else:
                changed.insert(5, copy.deepcopy(changed[4]))
            with self.assertRaises(provider.ProtocolError):
                provider.decode_sse(encode_response_events(changed), dialect)


ProviderContractTests.test_reasoning_text_without_content_part_frames_is_reconciled = test_reasoning_text_without_content_part_frames_is_reconciled


def test_reasoning_content_mirrors_only_observed_summary_text(self):
    import copy
    events, _ = responses_text_events()
    for event in events:
        if event.get('type') == 'response.output_item.done' and event['item']['type'] == 'reasoning':
            event['item']['content'] = [{'type': 'reasoning_text', 'text': part['text']} for part in event['item']['summary']]
    completed = next(event['item'] for event in events if event.get('type') == 'response.output_item.done' and event['item']['type'] == 'reasoning')
    events[-1]['response']['output'][0]['content'] = copy.deepcopy(completed['content'])
    for dialect in ('Responses-Codex', 'Responses-Other'):
        try:
            result = provider.decode_sse(encode_response_events(events), dialect)
        except provider.ProtocolError as error:
            self.fail('exact observed summary mirror rejected: ' + str(error))
        self.assertEqual(result['blocks'], events[-1]['response']['output'])
        changed = copy.deepcopy(events)
        for event in changed:
            if event.get('type') == 'response.output_item.done' and event['item']['type'] == 'reasoning':
                event['item']['content'][0]['text'] = 'unobserved reasoning'
        changed[-1]['response']['output'][0]['content'][0]['text'] = 'unobserved reasoning'
        with self.assertRaises(provider.ProtocolError):
            provider.decode_sse(encode_response_events(changed), dialect)


ProviderContractTests.test_reasoning_content_mirrors_only_observed_summary_text = test_reasoning_content_mirrors_only_observed_summary_text


def test_opaque_lookalike_in_tool_payload_remains_excluded(self):
    from unittest import mock
    value = {'model': 'fixture-R', 'store': False, 'input': [
        {'type': 'tool_search_call', 'execution': 'client', 'call_id': 'call_1',
         'arguments': {'type': 'reasoning', 'encrypted_content': 'litellm_enc:' + 'sK-' + 'a' * 20}}]}
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        destination = Path(temp) / 'excluded.json'
        with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
            with self.assertRaisesRegex(provider.ProtocolError, 'excluded-content'):
                provider.capture_json(destination, value)
            hashed.assert_not_called()
            self.assertFalse(destination.exists())


ProviderContractTests.test_opaque_lookalike_in_tool_payload_remains_excluded = test_opaque_lookalike_in_tool_payload_remains_excluded


def nonstreaming_projection_test(dialect, route):
    def test(self):
        import copy
        from unittest import mock
        events, _, expected_events = production_response_events('arguments')
        value = copy.deepcopy(events[-1]['response'])
        expected = copy.deepcopy(expected_events[-1]['response'])
        ciphertext = 'litellm_enc:nonstream-' + 'sK-' + 'a' * 20
        opaque = {'scheme_prefix': 'litellm_enc:',
                  'sha256': provider.hashlib.sha256(ciphertext.encode()).hexdigest(),
                  'byte_length': len(ciphertext.encode())}
        value['provider_specific_fields'] = {'thought_signatures': [ciphertext]}
        expected['provider_specific_fields'] = {'thought_signatures': [opaque]}
        raw = json.dumps(value, ensure_ascii=False).encode()
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            with mock.patch.object(provider, 'screen', wraps=provider.screen) as screened:
                with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
                    try:
                        received = b''.join(response_http_chunks(raw, route))
                        decoded = provider.decode_http(received, dialect, 200)
                        envelope = provider.project_response_event({'type': 'response.completed', 'response': value})['response']
                        destination = Path(temp) / 'response.json'
                        provider.capture_json(destination, envelope)
                    except provider.ProtocolError as error:
                        self.fail('non-streaming response rejected before safe projection: ' + str(error))
                    self.assertTrue(received == raw, 'non-streaming wire bytes changed')
                    self.assertTrue(json.loads(destination.read_text()) == expected, 'safe envelope capture changed retained fields')
                    self.assertTrue(decoded == {'blocks': expected['output'], 'stop_reason': 'completed', 'usage': expected['usage']},
                                    'non-streaming decoder changed output')
                    self.assertTrue(ciphertext.encode() not in destination.read_bytes(), 'raw ciphertext persisted')
                    self.assertTrue(all('hidden-response-' not in json.dumps(call.args[0]) and ciphertext not in json.dumps(call.args[0])
                                        for call in screened.call_args_list), 'excluded non-streaming value reached screening')
                    self.assertTrue(all(b'hidden-response-' not in call.args[0] for call in hashed.call_args_list),
                                    'excluded non-streaming metadata reached a digest')
    return test


def test_nonstreaming_projection_still_screens_retained_fields(self):
    import copy
    from unittest import mock
    events, _, _ = production_response_events('arguments')
    for fault in ('secret', 'nested_metadata'):
        value = copy.deepcopy(events[-1]['response'])
        if fault == 'secret':
            value['output'][0]['arguments'] = '<synthetic-secret>'
        else:
            value['output'][0]['authorization'] = 'synthetic-local-only'
        raw = json.dumps(value).encode()
        with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
            with self.assertRaises(provider.ProtocolError):
                provider.screen_wire(raw)
            hashed.assert_not_called()


for _dialect, _route in responses_routes():
    _suffix = _dialect.removeprefix('Responses-').lower()
    setattr(ProviderContractTests, 'test_nonstreaming_response_projection_' + _suffix,
            nonstreaming_projection_test(_dialect, _route))
ProviderContractTests.test_nonstreaming_projection_still_screens_retained_fields = test_nonstreaming_projection_still_screens_retained_fields


def opaque_identifier_fixture():
    import copy
    signature = 'synthetic-thought-π'
    events, items = function_argument_events()
    replacements = {item[key]: item[key] + '__thought__' + signature for item in items for key in ('id', 'call_id')}
    def substitute(value):
        if isinstance(value, str):
            return replacements.get(value, value)
        if isinstance(value, dict):
            return {key: substitute(child) for key, child in value.items()}
        if isinstance(value, list):
            return [substitute(child) for child in value]
        return value
    events, items = substitute(events), substitute(items)
    body = {'model': 'fixture-R', 'store': False, 'stream': True, 'tools': [], 'input': []}
    for item in items:
        body['input'].extend([copy.deepcopy(item), {'type': 'function_call_output', 'call_id': item['call_id'], 'output': 'fixture-result'}])
    return events, items, body, signature


def opaque_identifier_capture_test(dialect, route):
    def test(self):
        from smoke.redteam.hp8r import validate
        events, items, body, signature = opaque_identifier_fixture()
        raw = encode_response_events(events)
        try:
            received = b''.join(response_http_chunks(raw, route))
        except provider.ProtocolError as error:
            self.fail('opaque identifier HTTP capture rejected: ' + str(error))
        self.assertTrue(received == raw, 'opaque identifier changed on the wire')
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            request_path, stream_path = Path(temp) / 'request.json', Path(temp) / 'response.sse'
            try:
                provider.capture_json(request_path, body)
                provider.capture_sse(stream_path, received)
            except provider.ProtocolError as error:
                self.fail('opaque identifier capture rejected: ' + str(error))
            written = request_path.read_bytes() + stream_path.read_bytes()
            self.assertTrue(signature.encode() not in written, 'raw signature-bearing identifier persisted')
            request = json.loads(request_path.read_text())
            for index, item in enumerate(items):
                call, result = request['input'][index * 2:index * 2 + 2]
                self.assertTrue(call['call_id'] == result['call_id'], 'opaque identifier call/result pairing changed')
                for key in ('id', 'call_id'):
                    original = item[key].encode()
                    expected = {'scheme_prefix': '__thought__', 'sha256': provider.hashlib.sha256(original).hexdigest(),
                                'byte_length': len(original)}
                    self.assertTrue(call[key] == expected, 'opaque identifier projection is not exact')
            try:
                captured_events = provider.sse_events(stream_path.read_bytes())
                decoded = provider.decode_http(received, dialect, 200)
                replay = provider.decode_http(stream_path.read_bytes(), dialect, 200)
            except provider.ProtocolError as error:
                self.fail('opaque identifier item join rejected: ' + str(error))
            self.assertTrue(decoded == replay, 'opaque identifier replay equality changed')
            self.assertTrue(decoded['blocks'] == request['input'][::2], 'opaque identifier stream/request identity changed')
            for event in captured_events:
                if 'item_id' in event:
                    self.assertTrue(event['item_id'] == decoded['blocks'][event['output_index']]['id'],
                                    'opaque identifier event/item pairing changed')
            report = Path(temp) / 'report'
            validate.write_screened_artifacts(report, {'wire/req-001.json': body})
            self.assertTrue(json.loads((report / 'wire/req-001.json').read_text()) == request,
                            'report and HTTP identifier projections differ')
            again = Path(temp) / 'again.json'
            provider.capture_json(again, request)
            self.assertTrue(again.read_bytes() == request_path.read_bytes(), 'opaque identifier projection is not idempotent')
    return test


for _dialect, _route in responses_routes():
    _suffix = _dialect.removeprefix('Responses-').lower()
    setattr(ProviderContractTests, 'test_opaque_identifier_capture_pairing_' + _suffix,
            opaque_identifier_capture_test(_dialect, _route))


def test_opaque_identifier_preflight_keeps_nonderived_fields_screened(self):
    import copy
    from unittest import mock
    from smoke.redteam.hp8r import validate
    _, _, original, _ = opaque_identifier_fixture()
    for fault in ('retained_secret', 'unknown_metadata', 'payload_lookalike', 'ordinary_id_secret'):
        body = copy.deepcopy(original)
        if fault == 'retained_secret':
            body['input'][0]['arguments'] = '<synthetic-secret>'
        elif fault == 'unknown_metadata':
            body['input'][0]['model_cache'] = {'digest': 'a' * 64}
        elif fault == 'payload_lookalike':
            body['input'][1]['output'] = {'id': 'call_1__thought__' + 'sK-' + 'a' * 20}
        else:
            body['input'][0]['id'] = 'sK-' + 'a' * 20
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            destination, report = Path(temp) / 'excluded.json', Path(temp) / 'report'
            with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
                with self.assertRaises(provider.ProtocolError):
                    provider.capture_json(destination, body)
                with self.assertRaises((provider.ProtocolError, validate.ReceiptError)):
                    validate.write_screened_artifacts(report, {'wire/req-001.json': original, 'wire/req-002.json': body})
                hashed.assert_not_called()
                self.assertFalse(destination.exists())
                self.assertFalse(report.exists())


def test_opaque_identifier_projection_shape_is_validated(self):
    import copy
    from unittest import mock
    _, _, original, _ = opaque_identifier_fixture()
    projected = {'scheme_prefix': '__thought__', 'sha256': 'a' * 64, 'byte_length': 25}
    for fault in ('scheme', 'hash', 'length', 'extra'):
        body = copy.deepcopy(original)
        value = copy.deepcopy(projected)
        if fault == 'scheme':
            value['scheme_prefix'] = 'unrecognized'
        elif fault == 'hash':
            value['sha256'] = 'not-a-digest'
        elif fault == 'length':
            value['byte_length'] = True
        else:
            value['unrecognized'] = 'metadata'
        body['input'][0]['id'] = value
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            destination = Path(temp) / 'excluded.json'
            with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
                with self.assertRaisesRegex(provider.ProtocolError, 'capture-allowlist'):
                    provider.capture_json(destination, body)
                hashed.assert_not_called()
                self.assertFalse(destination.exists())


ProviderContractTests.test_opaque_identifier_preflight_keeps_nonderived_fields_screened = test_opaque_identifier_preflight_keeps_nonderived_fields_screened
ProviderContractTests.test_opaque_identifier_projection_shape_is_validated = test_opaque_identifier_projection_shape_is_validated


LEGACY_CORRUPTION_REASONS = {
    'capture-05111': 'terminal-order',
    'capture-08636': 'response-part-index',
    'capture-08893': 'block-order',
}


def legacy_rejection_contract_test(identifier, dialect):
    def test(self):
        from unittest import mock
        index = corpus_index()
        row = next(row for row in index['records'] if row['id'] == identifier)
        self.assertEqual(row['classification'], 'LEGACY-CORRUPTION')
        capture = read_corpus_capture(row['path'])
        with self.assertRaises(provider.ProtocolError) as rejected:
            provider.decode_http(capture['raw'], dialect, capture['status'])
        self.assertEqual(str(rejected.exception), LEGACY_CORRUPTION_REASONS[identifier],
                         'legacy corruption rejection reason changed')
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            exercise_corpus_case(self, row, index['policy'], Path(temp))
        decode = provider.decode_http
        def changed_reason(raw, selected, status):
            try:
                return decode(raw, selected, status)
            except provider.ProtocolError as error:
                if selected == dialect:
                    raise provider.ProtocolError('r12-wrong-reason') from error
                raise
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            with mock.patch.object(provider, 'decode_http', side_effect=changed_reason):
                with self.assertRaisesRegex(AssertionError, 'legacy corruption rejection reason changed'):
                    exercise_corpus_case(self, row, index['policy'], Path(temp))
    return test


for _identifier in LEGACY_CORRUPTION_REASONS:
    for _dialect in ('Responses-Codex', 'Responses-Other'):
        _suffix = _identifier.removeprefix('capture-') + '_' + _dialect.removeprefix('Responses-').lower()
        setattr(ProviderContractTests, 'test_legacy_corruption_recorded_reason_' + _suffix,
                legacy_rejection_contract_test(_identifier, _dialect))


def request_identifier_occurrences(body):
    for message_index, message in enumerate(body.get('messages', [])):
        value = message.get('tool_call_id')
        if isinstance(value, str) and all(value.partition('__thought__')):
            yield ('messages', message_index, 'tool_call_id'), value
        for call_index, call in enumerate(message.get('tool_calls', [])):
            value = call.get('id')
            if isinstance(value, str) and all(value.partition('__thought__')):
                yield ('messages', message_index, 'tool_calls', call_index, 'id'), value


def assert_request_identifiers(test, original, captured, written):
    occurrences = list(request_identifier_occurrences(original))
    projected = []
    for path, identifier in occurrences:
        value = captured
        for part in path:
            value = value[part]
        test.assertTrue(identifier.encode() not in written, 'raw request signature-bearing identifier persisted')
        projected.append(value)
    for index, (_, identifier) in enumerate(occurrences):
        for other, (_, candidate) in enumerate(occurrences):
            test.assertTrue((identifier == candidate) == (projected[index] == projected[other]),
                            'request identifier call/result pairing changed')
        expected = {'scheme_prefix': '__thought__',
                    'sha256': provider.hashlib.sha256(identifier.encode()).hexdigest(),
                    'byte_length': len(identifier.encode())}
        test.assertTrue(projected[index] == expected, 'request identifier projection is not exact')


def request_identifier_fixture():
    identifiers = ['call_alpha__thought__synthetic-request-π-A', 'call_beta__thought__synthetic-request-π-B']
    calls = [{'type': 'function', 'id': value, 'function': {'name': 'read_fixture', 'arguments': '{}'}}
             for value in [*identifiers, 'ordinary_call']]
    return {'model': 'fixture-M', 'stream': True, 'messages': [
        {'role': 'assistant', 'tool_calls': calls},
        *[{'role': 'tool', 'tool_call_id': value, 'content': 'fixture result'} for value in [*identifiers, 'ordinary_call']],
        {'role': 'assistant', 'content': 'between'},
        {'role': 'assistant', 'tool_calls': [calls[0]]},
        {'role': 'tool', 'tool_call_id': identifiers[0], 'content': 'replayed fixture result'}]}


def request_identifier_capture_test(writer):
    def test(self):
        import copy
        from smoke.redteam.hp8r import validate
        body = request_identifier_fixture()
        original = copy.deepcopy(body)
        raw = message_stream([{'type': 'text', 'text': 'fixture completion'}])
        route = '/v1/messages'
        with provider.LoopbackProvider([{'path': route, 'body': body, 'response': raw}], 'single') as server:
            connection = http.client.HTTPConnection('127.0.0.1', server.port, timeout=2)
            try:
                connection.request('POST', route, json.dumps(body, ensure_ascii=False).encode(),
                                   {'Authorization': 'synthetic-local-only'})
                response = connection.getresponse()
                self.assertEqual(response.status, 200, 'request identifier HTTP capture rejected')
                self.assertTrue(response.read() == raw, 'request identifier HTTP response changed')
            finally:
                connection.close()
            self.assertEqual(server.errors, [])
            self.assertEqual(len(server.attempts), 1)
            self.assertTrue(server.requests == [original], 'request identifier HTTP body changed')
            with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
                if writer == 'http':
                    destination = Path(temp) / 'request.json'
                    provider.capture_json(destination, server.requests[0])
                else:
                    from unittest import mock
                    destination = Path(temp) / 'report'
                    with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
                        with self.assertRaisesRegex(validate.ReceiptError, 'artifact-allowlist'):
                            validate.write_screened_artifacts(destination, {'wire/req-001.json': server.requests[0]})
                        hashed.assert_not_called()
                        self.assertFalse(destination.exists())
                    return
                written = destination.read_bytes()
                captured = json.loads(written)
                assert_request_identifiers(self, original, captured, written)
                expected = copy.deepcopy(original)
                for path, identifier in request_identifier_occurrences(original):
                    owner = expected
                    for part in path[:-1]:
                        owner = owner[part]
                    owner[path[-1]] = {'scheme_prefix': '__thought__',
                        'sha256': provider.hashlib.sha256(identifier.encode()).hexdigest(),
                        'byte_length': len(identifier.encode())}
                self.assertTrue(captured == expected, 'request capture changed order, fields, or optional presence')
                again = Path(temp) / 'again.json'
                provider.capture_json(again, captured)
                self.assertTrue(json.loads(again.read_bytes()) == captured, 'request identifier replay changed')
                self.assertTrue(body == original, 'request identifier capture mutated caller input')
    return test


for _writer in ('http', 'report'):
    _name = 'test_request_opaque_identifier_capture_http' if _writer == 'http' else 'test_request_identifier_report_schema_remains_closed'
    setattr(ProviderContractTests, _name, request_identifier_capture_test(_writer))


def test_request_opaque_identifier_corpus_witnesses(self):
    index = corpus_index()
    witnesses, occurrences = [], 0
    for row in index['records']:
        if not row['family'].endswith('-request'):
            continue
        capture = read_corpus_capture(row['path'])
        identifiers = list(request_identifier_occurrences(capture['value']))
        if not identifiers:
            continue
        witnesses.append(row['id'])
        occurrences += len(identifiers)
        with self.subTest(corpus_id=row['id']):
            with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
                exercise_corpus_case(self, row, index['policy'], Path(temp))
                destination = Path(temp) / (row['id'] + '-Messages.json')
                assert_request_identifiers(self, capture['value'], json.loads(destination.read_bytes()), destination.read_bytes())
    self.assertEqual((len(witnesses), occurrences), (10, 30))


def test_request_identifier_preflight_screens_nonopaque_fields(self):
    import copy
    from unittest import mock
    original = request_identifier_fixture()
    for fault in ('retained_secret', 'unknown_metadata', 'ordinary_identifier', 'payload_lookalike'):
        body = copy.deepcopy(original)
        if fault == 'retained_secret':
            body['messages'][1]['content'] = '<synthetic-secret>'
        elif fault == 'unknown_metadata':
            body['messages'][0]['tool_calls'][0]['unrecognized'] = {'cache_identity_hash': 'a' * 64}
        elif fault == 'ordinary_identifier':
            body['messages'][0]['tool_calls'][0]['id'] = 'sK-' + 'a' * 20
        else:
            body['messages'][0]['tool_calls'][0]['function']['arguments'] = json.dumps({'id': 'call_payload__thought__' + 'sK-' + 'a' * 20})
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            destination = Path(temp) / 'excluded.json'
            with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
                with self.assertRaises(provider.ProtocolError):
                    provider.project_request(body, '/v1/messages')
                with self.assertRaises(provider.ProtocolError):
                    provider.capture_json(destination, body)
                hashed.assert_not_called()
                self.assertFalse(destination.exists())


def test_request_identifier_projection_shape_is_validated(self):
    import copy
    from unittest import mock
    for site in ('call', 'result'):
        for fault in ('scheme', 'hash', 'length', 'extra'):
            body = request_identifier_fixture()
            projection = {'scheme_prefix': '__thought__', 'sha256': 'a' * 64, 'byte_length': 25}
            if fault == 'scheme':
                projection['scheme_prefix'] = 'unrecognized'
            elif fault == 'hash':
                projection['sha256'] = 'not-a-digest'
            elif fault == 'length':
                projection['byte_length'] = True
            else:
                projection['unrecognized'] = 'metadata'
            if site == 'call':
                body['messages'][0]['tool_calls'][0]['id'] = copy.deepcopy(projection)
            else:
                body['messages'][1]['tool_call_id'] = copy.deepcopy(projection)
            with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
                destination = Path(temp) / 'excluded.json'
                with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
                    with self.assertRaisesRegex(provider.ProtocolError, 'capture-allowlist'):
                        provider.capture_json(destination, body)
                    hashed.assert_not_called()
                    self.assertFalse(destination.exists())


ProviderContractTests.test_request_opaque_identifier_corpus_witnesses = test_request_opaque_identifier_corpus_witnesses
ProviderContractTests.test_request_identifier_preflight_screens_nonopaque_fields = test_request_identifier_preflight_screens_nonopaque_fields
ProviderContractTests.test_request_identifier_projection_shape_is_validated = test_request_identifier_projection_shape_is_validated




def thinking_signature_fixture(initial='opaque-initial-π', fragments=None):
    fragments = ['opaque-fragment-A-λ', 'opaque-fragment-B-δ'] if fragments is None else fragments
    start = {'type': 'thinking', 'thinking': 'Inspect the read-only fixture.'}
    if initial is not None:
        start['signature'] = initial
    block = dict(start)
    if initial is not None or fragments:
        block['signature'] = (initial or '') + ''.join(fragments)
    response = {'id': 'msg_signature', 'type': 'message', 'role': 'assistant', 'model': 'fixture-M',
                'content': [block], 'stop_reason': 'end_turn', 'usage': {'input_tokens': 1, 'output_tokens': 2}}
    request = {'model': 'fixture-M', 'max_tokens': 1024, 'stream': True,
               'messages': [{'role': 'assistant', 'content': [block]}, {'role': 'user', 'content': 'Continue.'}]}
    events = [{'type': 'message_start', 'message': {**response, 'content': [], 'stop_reason': None}},
              {'type': 'content_block_start', 'index': 0, 'content_block': start}]
    for fragment in fragments:
        events.append({'type': 'content_block_delta', 'index': 0,
                       'delta': {'type': 'signature_delta', 'signature': fragment}})
    events.extend([{'type': 'content_block_stop', 'index': 0},
                   {'type': 'message_delta', 'delta': {'stop_reason': 'end_turn'}, 'usage': {'output_tokens': 2}},
                   {'type': 'message_stop'}])
    return request, response, events


def signature_projection(value):
    raw = value.encode('utf-8')
    return {'scheme_prefix': None, 'sha256': provider.hashlib.sha256(raw).hexdigest(), 'byte_length': len(raw)}


def test_messages_thinking_signature_paths_never_persist_raw(self):
    request, response, events = thinking_signature_fixture()
    raw = encode_response_events(events)
    received_request, received = native_http_exchange(self, '/v1/messages', request, raw)
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        request_path, response_path, stream_path = [Path(temp) / name for name in ('request.json', 'response.json', 'response.sse')]
        try:
            provider.capture_json(request_path, received_request)
            provider.capture_json(response_path, response)
            provider.capture_sse(stream_path, received)
        except provider.ProtocolError as error:
            self.fail('Messages signature capture rejected: ' + str(error))
        written = request_path.read_bytes() + response_path.read_bytes() + stream_path.read_bytes()
        signatures = [request['messages'][0]['content'][0]['signature'], events[1]['content_block']['signature'],
                      *[event['delta']['signature'] for event in events if event['type'] == 'content_block_delta']]
        self.assertTrue(all(value.encode() not in written for value in signatures), 'raw Messages signature persisted')
        expected = signature_projection(signatures[0])
        self.assertEqual(json.loads(request_path.read_bytes())['messages'][0]['content'][0]['signature'], expected)
        self.assertEqual(json.loads(response_path.read_bytes())['content'][0]['signature'], expected)
        captured_events = provider.sse_events(stream_path.read_bytes())
        self.assertEqual(captured_events[1]['content_block']['signature'], signature_projection(signatures[1]))
        deltas = [event['delta'] for event in captured_events if event['type'] == 'content_block_delta']
        self.assertEqual([delta['signature'] for delta in deltas], [signature_projection(value) for value in signatures[2:]])


def test_messages_signature_stream_replay_matches_complete_bytes(self):
    for initial, fragments in [(None, []), ('', []), (None, ['opaque-A-π', 'opaque-B-λ']),
                               ('', ['opaque-A-π', 'opaque-B-λ']), ('opaque-start-δ', ['opaque-A-π', 'opaque-B-λ'])]:
        with self.subTest(initial_present=initial is not None, fragments=len(fragments)):
            request, response, events = thinking_signature_fixture(initial, fragments)
            raw = encode_response_events(events)
            with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
                request_path, stream_path = Path(temp) / 'request.json', Path(temp) / 'stream.sse'
                try:
                    provider.capture_json(request_path, request)
                    provider.capture_sse(stream_path, raw)
                    decoded = provider.decode_http(raw, 'Messages', 200)
                    replay = provider.decode_http(stream_path.read_bytes(), 'Messages', 200)
                except provider.ProtocolError as error:
                    self.fail('Messages streamed signature rejected: ' + str(error))
                expected = json.loads(request_path.read_bytes())['messages'][0]['content']
                self.assertTrue(decoded['blocks'] == expected, 'streamed and replayed thinking signatures differ')
                self.assertEqual(replay, decoded)
                if initial is not None or fragments:
                    self.assertEqual(decoded['blocks'][0]['signature'], signature_projection((initial or '') + ''.join(fragments)))
                else:
                    self.assertNotIn('signature', decoded['blocks'][0])
                captured = provider.sse_events(stream_path.read_bytes())
                deltas = [event['delta'] for event in captured if event['type'] == 'content_block_delta']
                prefix = initial or ''
                for fragment, delta in zip(fragments, deltas):
                    prefix += fragment
                    self.assertEqual(delta.get('hp8r_signature_prefix'), signature_projection(prefix), 'signature prefix is not exact in-order bytes')
                again = Path(temp) / 'again.sse'
                provider.capture_sse(again, stream_path.read_bytes())
                self.assertEqual(again.read_bytes(), stream_path.read_bytes())


def test_messages_signature_report_capture_excludes_ciphertext(self):
    from smoke.redteam.hp8r import validate
    _, response, _ = thinking_signature_fixture()
    block = response['content'][0]
    source = {'source_class': 'synthetic-mutation', 'items': [block],
              'coordinates': [{'lineage': 'fixture-lineage', 'occurrence': 0}]}
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        destination = Path(temp) / 'report'
        try:
            validate.write_screened_artifacts(destination, {'source.json': source})
        except (validate.ReceiptError, provider.ProtocolError) as error:
            self.fail('Messages signature report rejected: ' + str(error))
        written = (destination / 'source.json').read_bytes()
        self.assertTrue(block['signature'].encode() not in written, 'raw Messages ciphertext persisted in report')
        self.assertEqual(json.loads(written)['items'][0]['signature'], signature_projection(block['signature']))


def test_messages_signature_projection_preserves_before_hash_exclusion(self):
    from unittest import mock
    import copy
    original, _, events = thinking_signature_fixture('<synthetic-secret>', ['<synthetic-secret>'])
    for fault in ('retained-secret', 'unknown-metadata', 'payload-lookalike'):
        request = copy.deepcopy(original)
        if fault == 'retained-secret':
            request['messages'][1]['content'] = '<synthetic-secret>'
        elif fault == 'unknown-metadata':
            request['messages'][0]['content'][0]['unrecognized'] = 'metadata'
        else:
            request['messages'][1]['content'] = [{'type': 'tool_result', 'tool_use_id': 'fixture-call',
                'content': {'type': 'thinking', 'signature': '<synthetic-secret>'}}]
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            destination = Path(temp) / 'excluded.json'
            with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
                with self.assertRaises(provider.ProtocolError):
                    provider.capture_json(destination, request)
                hashed.assert_not_called()
                self.assertFalse(destination.exists())
    events.insert(-1, {'type': 'ping', 'unrecognized': 'metadata'})
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        destination = Path(temp) / 'excluded.sse'
        with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
            with self.assertRaises(provider.ProtocolError):
                provider.capture_sse(destination, encode_response_events(events))
            hashed.assert_not_called()
            self.assertFalse(destination.exists())


ProviderContractTests.test_messages_thinking_signature_paths_never_persist_raw = test_messages_thinking_signature_paths_never_persist_raw
ProviderContractTests.test_messages_signature_stream_replay_matches_complete_bytes = test_messages_signature_stream_replay_matches_complete_bytes
ProviderContractTests.test_messages_signature_report_capture_excludes_ciphertext = test_messages_signature_report_capture_excludes_ciphertext
ProviderContractTests.test_messages_signature_projection_preserves_before_hash_exclusion = test_messages_signature_projection_preserves_before_hash_exclusion



def ciphertext_family_fixtures():
    import copy
    families = {
        'reasoning': ({'type': 'reasoning', 'summary': []}, ('encrypted_content',)),
        'compaction': ({'type': 'compaction'}, ('encrypted_content',)),
        'litellm_enc': ({'type': 'reasoning', 'summary': []}, ('encrypted_content',)),
        'redacted_thinking': ({'type': 'redacted_thinking'}, ('data',)),
        'thought_signature': ({'type': 'function_call', 'name': 'read_fixture', 'arguments': '{}',
                               'provider_specific_fields': {}}, ('provider_specific_fields', 'thought_signature')),
        'thinking_signature': ({'type': 'thinking', 'thinking': 'Retained text.'}, ('signature',)),
        'signature_identifier': ({'type': 'function_call', 'name': 'read_fixture', 'arguments': '{}'}, ('call_id',)),
    }
    for name, (block, path) in families.items():
        prefix = {'litellm_enc': 'litellm_enc:', 'signature_identifier': 'call_fixture__thought__'}.get(name, '')
        raw = prefix + 'R14-ciphertext-' + name + '-π'
        expected = copy.deepcopy(block)
        actual_owner, expected_owner = block, expected
        for key in path[:-1]:
            actual_owner, expected_owner = actual_owner[key], expected_owner[key]
        actual_owner[path[-1]] = raw
        expected_owner[path[-1]] = {'scheme_prefix': '__thought__' if name == 'signature_identifier' else
                                   'litellm_enc:' if name == 'litellm_enc' else None,
                                   'sha256': provider.hashlib.sha256(raw.encode()).hexdigest(),
                                   'byte_length': len(raw.encode())}
        yield name, block, expected, raw


def ciphertext_representations(block):
    import copy
    message = {'type': 'message', 'role': 'assistant', 'content': [block]}
    implicit = {'role': 'assistant', 'content': [block]}
    for key in ('input', 'messages', 'system', 'content'):
        value = {'model': 'fixture-R14', key: [implicit] if key == 'messages' else [block]}
        yield 'request_body', key, 'json', value
    for key in ('output', 'content'):
        yield 'response_body', key, 'json', {'id': 'response_fixture', key: [block]}
    for key, kind in (('response', 'response.completed'), ('message', 'message_start')):
        yield 'sse_body', key, 'sse', [{'type': kind, key: {'output' if key == 'response' else 'content': [message]}}]
    for key, kind in (('item', 'response.output_item.done'), ('content_block', 'content_block_start')):
        yield 'sse_item', key, 'sse', [{'type': kind, 'output_index' if key == 'item' else 'index': 0, key: block}]
    for kind in ('response.content_part.added', 'response.content_part.done',
                 'response.reasoning_summary_part.added', 'response.reasoning_summary_part.done'):
        yield 'sse_part', kind, 'sse', [{'type': kind, 'part': block}]
    for filename in ('source.json', 'ordered.json'):
        for name, item in (('bare', block), ('message', message), ('implicit-message', implicit)):
            source = {'source_class': 'synthetic-mutation', 'items': [item],
                      'coordinates': [{'lineage': 'fixture-lineage', 'occurrence': 0}]}
            yield 'source_records', filename + '-' + name, 'report', {filename: source}
    from smoke.redteam.hp8r.test_validate import fixture
    _, artifacts = fixture('tooling-keep')
    for filename in ('checkpoints/pre.json', 'checkpoints/post.json', 'reload/pre.json', 'reload/post.json'):
        for name, item in (('bare', block), ('message', message)):
            checkpoint = copy.deepcopy(artifacts[filename])
            checkpoint['history'] = [item]
            yield 'checkpoint_history', filename + '-' + name, 'report', {filename: checkpoint}
    nested = message
    for _ in range(4):
        nested = {'role': 'assistant', 'content': [nested]}
    wrappers = {'nested-message': nested,
                'reasoning-content': {'type': 'reasoning', 'content': [message]},
                'reasoning-summary': {'type': 'reasoning', 'summary': [message]},
                'search-references': {'type': 'tool_search_tool_search_result', 'tool_references': [message]}}
    for name, item in wrappers.items():
        yield 'nested_protocol', name, 'json', {'input': [item]}


def write_ciphertext_representation(mode, destination, value):
    from smoke.redteam.hp8r import validate
    if mode == 'json':
        provider.capture_json(destination, value)
        raw = destination.read_bytes()
        return raw, json.loads(raw)
    if mode == 'sse':
        raw = b''.join(('data: ' + json.dumps(event, ensure_ascii=False) + '\n\n').encode() for event in value)
        provider.capture_sse(destination, raw)
        raw = destination.read_bytes()
        return raw, [json.loads(line[6:]) for line in raw.splitlines() if line.startswith(b'data: ')]
    validate.write_screened_artifacts(destination, value)
    raw = b''.join((destination / name).read_bytes() for name in value)
    return raw, {name: json.loads((destination / name).read_bytes()) for name in value}


def ciphertext_representation_test(representation):
    def test(self):
        import copy
        from unittest import mock
        from smoke.redteam.hp8r import validate
        selected = 0
        for family, block, expected_block, ciphertext in ciphertext_family_fixtures():
            originals = list(ciphertext_representations(block))
            expected = list(ciphertext_representations(expected_block))
            for source, projection in zip(originals, expected):
                group, variant, mode, value = source
                if group != representation:
                    continue
                selected += 1
                with self.subTest(representation=group, variant=variant, family=family):
                    original = copy.deepcopy(value)
                    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
                        destination = Path(temp) / 'captured'
                        with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
                            try:
                                written, captured = write_ciphertext_representation(mode, destination, value)
                            except (provider.ProtocolError, validate.ReceiptError) as error:
                                self.fail('declared ciphertext representation rejected: ' + str(error))
                            self.assertTrue(ciphertext.encode() not in written, 'raw ciphertext persisted in representation')
                            self.assertTrue(captured == projection[3], 'ciphertext representation projection differs')
                            self.assertTrue(all(call.args[0] == ciphertext.encode() or ciphertext.encode() not in call.args[0]
                                                for call in hashed.call_args_list), 'raw ciphertext entered an artifact digest')
                        replay, _ = write_ciphertext_representation(mode, Path(temp) / 'replayed', captured)
                        self.assertEqual(replay, written)
                    self.assertEqual(value, original)
        self.assertGreater(selected, 0)
    return test


def test_ciphertext_body_signature_array_representations(self):
    value = 'litellm_enc:R14-body-signature-π'
    expected = {'scheme_prefix': 'litellm_enc:', 'sha256': provider.hashlib.sha256(value.encode()).hexdigest(),
                'byte_length': len(value.encode())}
    def variants(signature):
        body = {'provider_specific_fields': {'thought_signatures': [signature]}}
        return [('json', body), ('sse', [{'type': 'response.completed', 'response': body}]),
                ('sse', [{'type': 'message_start', 'message': body}])]
    for index, ((mode, original), (_, projected)) in enumerate(zip(variants(value), variants(expected))):
        with self.subTest(variant=index):
            with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
                try:
                    written, captured = write_ciphertext_representation(mode, Path(temp) / 'capture', original)
                except provider.ProtocolError as error:
                    self.fail('declared body signature representation rejected: ' + str(error))
                self.assertTrue(value.encode() not in written, 'raw body signature persisted')
                self.assertEqual(captured, projected)


def test_ciphertext_signature_delta_representation(self):
    _, _, events = thinking_signature_fixture('', ['R14-delta-A-π', 'R14-delta-B-λ'])
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        written, captured = write_ciphertext_representation('sse', Path(temp) / 'capture', events)
        prefix = ''
        for original, projected in zip(events, captured):
            if original['type'] == 'content_block_delta':
                raw = original['delta']['signature']
                prefix += raw
                self.assertTrue(raw.encode() not in written, 'raw signature delta persisted')
                self.assertEqual(projected['delta']['signature'], signature_projection(raw))
                self.assertEqual(projected['delta']['hp8r_signature_prefix'], signature_projection(prefix))


def test_ciphertext_representation_matrix_preserves_zero_digest_exclusion(self):
    from unittest import mock
    from smoke.redteam.hp8r import validate
    block = {'type': 'thinking', 'thinking': '<synthetic-secret>', 'signature': 'R14-safe-opaque'}
    for group, variant, mode, value in ciphertext_representations(block):
        with self.subTest(representation=group, variant=variant):
            with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
                destination = Path(temp) / 'excluded'
                with mock.patch.object(provider.hashlib, 'sha256', wraps=provider.hashlib.sha256) as hashed:
                    with self.assertRaises((provider.ProtocolError, validate.ReceiptError)):
                        write_ciphertext_representation(mode, destination, value)
                    hashed.assert_not_called()
                    self.assertFalse(destination.exists())


for _representation in ('request_body', 'response_body', 'sse_body', 'sse_item', 'sse_part',
                        'source_records', 'checkpoint_history', 'nested_protocol'):
    setattr(ProviderContractTests, 'test_ciphertext_representation_' + _representation,
            ciphertext_representation_test(_representation))
ProviderContractTests.test_ciphertext_body_signature_array_representations = test_ciphertext_body_signature_array_representations
ProviderContractTests.test_ciphertext_signature_delta_representation = test_ciphertext_signature_delta_representation
ProviderContractTests.test_ciphertext_representation_matrix_preserves_zero_digest_exclusion = test_ciphertext_representation_matrix_preserves_zero_digest_exclusion

if __name__ == '__main__':
    unittest.main()
