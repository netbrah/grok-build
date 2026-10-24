"""Bounded local protocol fixtures for HP-0; never a live provider adapter."""
import copy
from dataclasses import dataclass, field
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import re
import threading

MAX_BODY = 4 * 1024 * 1024
MAX_EVENTS = 16384
DIALECTS = ('Messages', 'Responses-Codex', 'Responses-Other')
CAPTURE_FIELDS = frozenset(('model', 'stream', 'messages', 'input', 'tools', 'tool_choice',
    'store', 'max_tokens', 'max_output_tokens', 'temperature', 'top_p', 'system', 'thinking',
    'reasoning', 'parallel_tool_calls', 'type', 'id', 'object', 'output', 'status', 'content',
    'role', 'stop_reason', 'stop_sequence', 'usage', 'text', 'error', 'created_at', 'completed_at',
    'incomplete_details', 'container', 'stop_details', 'context_management', 'provider_specific_fields',
    'output_config', 'stop_sequences', 'top_k', 'stream_options', 'reasoning_effort', 'message', 'code', 'param', 'instructions', 'include'))


class ProtocolError(ValueError):
    pass


def require(condition, code):
    if not condition:
        raise ProtocolError(code)


def screen(value):
    if isinstance(value, dict):
        for key, child in value.items():
            require(isinstance(key, str), 'field-type')
            require(not re.search(r'(?i)(auth(?:orization|entication|_cache|$)|api[_-]?key|password|bearer[_-]?token|credential|identity|headers|binding_debug|^env$|^config$|cache.*(?:key|hash))', key), 'excluded-field')
            screen(child)
    elif isinstance(value, list):
        for child in value:
            screen(child)
    elif isinstance(value, str):
        require(not re.search(r'sk-[a-z0-9]{16,}|<synthetic-secret>|^litellm_enc:.', value, re.I | re.S), 'excluded-content')
    else:
        require(value is None or type(value) in (bool, int, float), 'value-type')


RESPONSE_REQUEST_FIELDS = frozenset(('background', 'conversation', 'include', 'input', 'instructions',
    'max_output_tokens', 'max_tool_calls', 'metadata', 'model', 'parallel_tool_calls', 'previous_response_id',
    'prompt', 'prompt_cache_key', 'prompt_cache_retention', 'reasoning', 'safety_identifier', 'service_tier',
    'store', 'stream', 'stream_options', 'temperature', 'text', 'tool_choice', 'tools', 'top_logprobs', 'top_p',
    'truncation', 'context_management', 'moderation', 'prompt_cache_options'))
MESSAGE_REQUEST_FIELDS = frozenset(('model', 'messages', 'max_tokens', 'system', 'tools', 'tool_choice',
    'temperature', 'top_p', 'top_k', 'stream', 'stop_sequences', 'thinking', 'output_config', 'metadata', 'mcp_servers',
    'context_management', 'reasoning_effort', 'store', 'stream_options'))


def project_request(value, path):
    """Return capture-safe fields and the omitted top-level field count."""
    require(path in ('/v1/messages', '/codex/v1/responses', '/other/v1/responses'), 'route')
    fields = MESSAGE_REQUEST_FIELDS if path == '/v1/messages' else RESPONSE_REQUEST_FIELDS
    require(isinstance(value, dict) and set(value) <= fields, 'request-allowlist')
    captured = {key: copy.deepcopy(child) for key, child in value.items() if key in CAPTURE_FIELDS}
    screen(map_opaque(captured, False))
    return captured, len(value) - len(captured)


OPAQUE_FIELDS = frozenset((('reasoning', 'encrypted_content'), ('compaction', 'encrypted_content'),
                           ('redacted_thinking', 'data'), ('provider_specific_fields', 'thought_signature'),
                           ('provider_specific_fields', 'thought_signatures'),
                           ('thinking', 'signature'), ('signature_delta', 'signature')))
SIGNATURE_PREFIX_FIELD = 'hp8r_signature_prefix'
OPAQUE_ITEM_PATHS = frozenset((('input', '*'), ('output', '*'), ('content', '*'),
    ('messages', '*', 'content', '*'), ('items', '*'), ('history', '*'), ('history', '*', 'content', '*'),
    ('*', 'item'), ('*', 'response', 'output', '*'), ('*', 'content_block'), ('*', 'message', 'content', '*')))
OPAQUE_ROOT_FIELDS = dict.fromkeys(('input', 'output', 'content', 'system', 'messages', 'items', 'history'), 'protocol')
OPAQUE_ROOT_FIELDS.update(response='root', message='root', item='protocol', part='protocol',
                          content_block='protocol', delta='delta', provider_specific_fields='provider_specific_fields')
OPAQUE_PROTOCOL_CHILDREN = {'message': ('content',), 'reasoning': ('content', 'summary'),
                            'tool_search_tool_search_result': ('tool_references',)}
OPAQUE_IDENTIFIER_FIELDS = frozenset(('id', 'call_id', 'item_id', 'response_id', 'tool_use_id', 'tool_call_id'))


def opaque_child_role(value, role, name):
    # Follow protocol grammar edges; never re-enter through arbitrary tool/schema payloads.
    kind = value.get('type', 'message')
    if (role in ('root', 'protocol') and name in OPAQUE_IDENTIFIER_FIELDS) or (role == 'tool_call' and name == 'id'):
        return 'identifier'
    if role == 'root':
        return OPAQUE_ROOT_FIELDS.get(name)
    if role == 'protocol' and isinstance(kind, str):
        if (kind, name) in OPAQUE_FIELDS:
            return 'opaque'
        if name in OPAQUE_PROTOCOL_CHILDREN.get(kind, ()):
            return 'protocol'
        if kind == 'function_call' and name == 'provider_specific_fields':
            return 'provider_specific_fields'
        if kind == 'message' and name == 'tool_calls':
            return 'tool_call'
    if role == 'provider_specific_fields' and (role, name) in OPAQUE_FIELDS:
        return 'opaque'
    if role == 'delta' and kind == 'signature_delta' and (kind, name) in OPAQUE_FIELDS:
        return 'opaque'
    return None


OPAQUE_IDENTIFIER_MARKER = '__thought__'
OPAQUE_IDENTIFIER_PATHS = frozenset(
    (*path, name) for path in OPAQUE_ITEM_PATHS for name in ('id', 'call_id', 'tool_use_id', 'tool_call_id')
) | frozenset((('id',), ('*', 'response', 'id'), ('*', 'message', 'id'),
    ('*', 'item_id'), ('*', 'call_id'), ('*', 'response_id'),
    ('messages', '*', 'tool_calls', '*', 'id'),
    ('messages', '*', 'tool_call_id'),
    ('records', 'lifecycle', 'waiting_clients', '*', 'call_id'), ('lifecycle', 'waiting_clients', '*', 'call_id'),
    ('accepted_clients', '*', 'call_id'), ('delivered_clients', '*', 'call_id')))


def opaque_identifier(value, path, role=None):
    if not isinstance(value, str) or (role != 'identifier' and path not in OPAQUE_IDENTIFIER_PATHS):
        return False
    prefix, marker, payload = value.partition(OPAQUE_IDENTIFIER_MARKER)
    return bool(prefix and marker and payload)


def captured_identifier(value):
    if isinstance(value, str):
        return bool(value)
    return (isinstance(value, dict) and set(value) == {'scheme_prefix', 'sha256', 'byte_length'}
            and value['scheme_prefix'] == OPAQUE_IDENTIFIER_MARKER
            and isinstance(value['sha256'], str) and re.fullmatch('[a-f0-9]{64}', value['sha256']) is not None
            and type(value['byte_length']) is int and value['byte_length'] > len(OPAQUE_IDENTIFIER_MARKER))


def opaque_value(value, role):
    return isinstance(value, str) and role == 'opaque'


def map_opaque(value, project, context=None, key=None, path=(), role='root'):
    identifier = opaque_identifier(value, path, role)
    if identifier or opaque_value(value, role):
        if not project:
            return ''
        raw = value.encode('utf-8')
        return {'scheme_prefix': OPAQUE_IDENTIFIER_MARKER if identifier else 'litellm_enc:' if value.startswith('litellm_enc:') and key != 'signature' else None,
                'sha256': hashlib.sha256(raw).hexdigest(), 'byte_length': len(raw)}
    if isinstance(value, dict):
        kind = value.get('type', context)
        return {name: map_opaque(child, project, 'provider_specific_fields' if name == 'provider_specific_fields' else kind, name, (*path, name), opaque_child_role(value, role, name))
                for name, child in value.items()}
    if isinstance(value, list):
        return [map_opaque(child, project, context, key, (*path, '*'), role) for child in value]
    return value


def project_signature_prefixes(events, captured):
    # Prefixes use only bytes observed so far; fragment projections remain separate.
    states = {}
    for event, target in zip(events, captured):
        kind, index = event.get('type'), event.get('index')
        if kind == 'message_start':
            states.clear()
        elif kind == 'content_block_start' and event.get('content_block', {}).get('type') == 'thinking':
            require(type(index) is int, 'signature-block-index')
            initial = event['content_block'].get('signature', '')
            states[index] = [hashlib.sha256(initial.encode()), len(initial.encode())] if isinstance(initial, str) else None
        elif kind == 'content_block_delta' and event.get('delta', {}).get('type') == 'signature_delta':
            require(type(index) is int and index in states, 'signature-without-block')
            delta = event['delta']
            fragment = delta.get('signature')
            if isinstance(fragment, str):
                require(states[index] is not None and SIGNATURE_PREFIX_FIELD not in delta, 'signature-prefix-source')
                raw = fragment.encode('utf-8')
                state = states[index]
                state[0].update(raw)
                state[1] += len(raw)
                target['delta'][SIGNATURE_PREFIX_FIELD] = {'scheme_prefix': None, 'sha256': state[0].hexdigest(), 'byte_length': state[1]}
            else:
                require(isinstance(delta.get(SIGNATURE_PREFIX_FIELD), dict), 'signature-prefix-missing')
                states[index] = None
        elif kind == 'content_block_stop' and type(index) is int:
            states.pop(index, None)
    return captured


def capture_preflight(value, event=False):
    if __package__:
        from .validate import ReceiptError, capture_http_body, capture_stream_event
    else:
        from validate import ReceiptError, capture_http_body, capture_stream_event
    masked = map_opaque(value, False)
    screen(masked)
    try:
        if event:
            for item in masked:
                capture_stream_event(item)
        else:
            capture_http_body(masked)
    except ReceiptError as error:
        raise ProtocolError('capture-allowlist') from error
    captured = map_opaque(value, True)
    return project_signature_prefixes(value, captured) if event else captured


def capture_json(path, value):
    require(isinstance(value, dict) and set(value) <= CAPTURE_FIELDS, 'capture-allowlist')
    captured = capture_preflight(value)
    raw = (json.dumps(captured, ensure_ascii=False, allow_nan=False) + '\n').encode()
    require(len(raw) <= MAX_BODY, 'capture-cap')
    with Path(path).open('xb') as stream:
        stream.write(raw)
    return hashlib.sha256(raw).hexdigest()


RESPONSE_CAPTURE_FIELDS = frozenset((
    'id', 'object', 'created_at', 'completed_at', 'model', 'status', 'output', 'usage', 'error',
    'incomplete_details', 'max_output_tokens', 'parallel_tool_calls', 'reasoning', 'store',
    'temperature', 'text', 'tool_choice', 'tools', 'top_p', 'provider_specific_fields', 'instructions'))


RESPONSE_ACCEPT_FIELDS = RESPONSE_REQUEST_FIELDS | frozenset((
    'id', 'object', 'created_at', 'completed_at', 'output', 'status', 'usage', 'error', 'incomplete_details',
    'frequency_penalty', 'presence_penalty', 'tool_usage', 'content_filters', 'user', 'provider_specific_fields',
    'type', 'code', 'message', 'param'))


def project_response_event(event):
    if event.get('type', '').startswith('response.') and 'response' in event:
        response = event['response']
        require(isinstance(response, dict) and set(response) <= RESPONSE_ACCEPT_FIELDS, 'response-shape')
        captured = {key: copy.deepcopy(child) for key, child in response.items() if key in RESPONSE_CAPTURE_FIELDS}
        return {**event, 'response': captured}
    return event


def _sse_records(raw):
    require(isinstance(raw, bytes) and 0 < len(raw) <= MAX_BODY, 'stream-cap')
    try:
        text = raw.decode('utf-8').replace('\r\n', '\n')
        require(text.endswith('\n\n'), 'truncated-frame')
        events = []
        for frame in text.split('\n\n'):
            if not frame or all(not line or line.startswith(':') for line in frame.splitlines()):
                continue
            names = [line[6:].strip() for line in frame.splitlines() if line.startswith('event:')]
            data = '\n'.join(line[5:].lstrip(' ') for line in frame.splitlines() if line.startswith('data:'))
            require(len(names) <= 1 and bool(data), 'malformed-frame')
            if data == '[DONE]':
                events.append({'type': '[DONE]'})
                continue
            event = json.loads(data)
            require(isinstance(event, dict) and (isinstance(event.get('type'), str) or 'error' in event), 'event-type')
            require(not names or names == [event.get('type', 'error')], 'event-name')
            event = project_response_event(event)
            events.append(event)
        require(0 < len(events) <= MAX_EVENTS, 'event-cap')
        return events
    except (UnicodeError, json.JSONDecodeError) as error:
        raise ProtocolError('malformed-stream') from error


def sse_events(raw):
    return capture_preflight(_sse_records(raw), event=True)


def capture_sse(path, raw):
    events = sse_events(raw)
    projected = b''.join(('data: ' + json.dumps(event, ensure_ascii=False, allow_nan=False) + '\n\n').encode() for event in events)
    require(len(projected) <= MAX_BODY, 'capture-cap')
    with Path(path).open('xb') as stream:
        stream.write(projected)
    return hashlib.sha256(projected).hexdigest()


def screen_wire(raw):
    try:
        value = _sse_records(raw) if raw.lstrip().startswith((b'event:', b'data:', b':')) else json.loads(raw)
    except (ProtocolError, ValueError, UnicodeError):
        screen(raw.decode('utf-8'))
        return
    if isinstance(value, dict) and (value.get('object') == 'response' or 'output' in value):
        value = project_response_event({'type': 'response.completed', 'response': value})['response']
    screen(map_opaque(value, False))


@dataclass(frozen=True)
class ProviderFailure:
    http_status: int | None
    failures: tuple = field(repr=False)
    events: tuple = field(default=(), repr=False)
    body: object = field(default=None, repr=False)

    def as_dict(self):
        return {'outcome': 'TYPED-FAILURE', 'http_status': self.http_status,
                'failures': copy.deepcopy(list(self.failures)), 'events': copy.deepcopy(list(self.events)),
                'body': copy.deepcopy(self.body)}


def provider_failure(events, http_status=None):
    failures = []
    for index, event in enumerate(events):
        kind = event.get('type')
        error = event.get('error')
        if kind == 'response.failed':
            error = event.get('response', {}).get('error')
        if error is not None or kind in ('error', 'response.failed'):
            failure = {'event_index': index, 'error': copy.deepcopy(error if error is not None else event)}
            if 'type' in event:
                failure['event_type'] = kind
            if 'sequence_number' in event:
                failure['sequence_number'] = event['sequence_number']
            failures.append(failure)
    if failures:
        return ProviderFailure(http_status, tuple(failures), tuple(copy.deepcopy(events)))
    return None


def decode_sse(raw, dialect, http_status=None):
    require(dialect in DIALECTS, 'dialect')
    events = sse_events(raw)
    failure = provider_failure(events, http_status)
    if failure is not None:
        return failure
    if http_status is not None and http_status >= 400:
        return ProviderFailure(http_status, ({'event_index': None, 'error': None},), tuple(events))
    return _messages(events) if dialect == 'Messages' else _responses(events)


def decode_http(raw, dialect, http_status=None):
    require(dialect in DIALECTS, 'dialect')
    require(http_status is None or type(http_status) is int and 100 <= http_status <= 599, 'http-status')
    failed_status = http_status is not None and http_status >= 400
    require(isinstance(raw, bytes) and len(raw) <= MAX_BODY and (raw or failed_status), 'response-cap')
    if raw.lstrip().startswith((b'data:', b'event:', b':')):
        return decode_sse(raw, dialect, http_status)
    try:
        value = json.loads(raw)
    except (ValueError, UnicodeError) as error:
        if not failed_status:
            raise ProtocolError('malformed-response') from error
        try:
            value = raw.decode('utf-8')
        except UnicodeError as invalid:
            raise ProtocolError('unscreenable-error-body') from invalid
    if failed_status and not isinstance(value, dict):
        screen(value)
        return ProviderFailure(http_status, ({'event_index': None, 'error': copy.deepcopy(value)},), body=value)
    require(isinstance(value, dict), 'response-shape')
    require(set(value) <= (CAPTURE_FIELDS if dialect == 'Messages' else RESPONSE_ACCEPT_FIELDS), 'response-allowlist')
    fields = CAPTURE_FIELDS if dialect == 'Messages' else RESPONSE_CAPTURE_FIELDS | {'type', 'code', 'message', 'param'}
    captured = capture_preflight({key: child for key, child in value.items() if key in fields})
    if http_status is not None and http_status >= 400 or captured.get('error') is not None or captured.get('type') == 'error':
        return ProviderFailure(http_status, ({'event_index': None, 'error': copy.deepcopy(captured.get('error', captured))},), body=captured)
    field_name = 'content' if dialect == 'Messages' else 'output'
    require(isinstance(captured.get(field_name), list), 'response-output')
    stop = captured.get('stop_reason' if dialect == 'Messages' else 'status')
    require(isinstance(stop, str) and bool(stop), 'response-terminal')
    return {'blocks': captured[field_name], 'stop_reason': stop, 'usage': captured.get('usage', {})}


def decode_sse_chunks(chunks, dialect):
    raw = bytearray()
    for chunk in chunks:
        require(isinstance(chunk, bytes) and bool(chunk), 'stream-chunk')
        require(len(raw) + len(chunk) <= MAX_BODY, 'stream-cap')
        raw.extend(chunk)
    return decode_sse(bytes(raw), dialect)


def _messages(events):
    blocks, active, fragments = [], set(), {}
    started = ended = False
    reason = None
    usage = {}
    for event in events:
        kind = event['type']
        require(not ended, 'after-terminal')
        if kind == 'ping':
            continue
        if kind == 'message_start':
            require(not started and not blocks, 'duplicate-start')
            message = event.get('message', {})
            require(message.get('type') == 'message' and message.get('role') == 'assistant' and message.get('content') == [], 'message-start')
            usage.update(message.get('usage', {}))
            started = True
            continue
        require(started, 'before-start')
        if kind == 'content_block_start':
            index = event.get('index')
            require(type(index) is int and index == len(blocks), 'block-order')
            block = event.get('content_block')
            require(isinstance(block, dict) and isinstance(block.get('type'), str), 'block-shape')
            blocks.append(copy.deepcopy(block))
            active.add(index)
            fragments[index] = ''
        elif kind == 'content_block_delta':
            index = event.get('index')
            require(index in active, 'delta-without-block')
            delta = event['delta']
            key = {'text_delta': 'text', 'thinking_delta': 'thinking', 'signature_delta': 'signature'}.get(delta.get('type'))
            if delta.get('type') == 'input_json_delta':
                fragments[index] += delta['partial_json']
            elif delta.get('type') == 'signature_delta':
                require(blocks[index].get('type') == 'thinking' and isinstance(delta.get('signature'), dict)
                        and isinstance(delta.get(SIGNATURE_PREFIX_FIELD), dict), 'signature-delta')
                blocks[index]['signature'] = copy.deepcopy(delta[SIGNATURE_PREFIX_FIELD])
            else:
                require(key is not None and isinstance(delta.get(key), str), 'delta-type')
                blocks[index][key] = blocks[index].get(key, '') + delta[key]
        elif kind == 'content_block_stop':
            index = event.get('index')
            require(index in active, 'duplicate-block-stop')
            if fragments[index]:
                try:
                    blocks[index]['input'] = json.loads(fragments[index])
                except json.JSONDecodeError as error:
                    raise ProtocolError('malformed-arguments') from error
            active.remove(index)
        elif kind == 'message_delta':
            require(not active and reason is None, 'terminal-order')
            reason = event.get('delta', {}).get('stop_reason')
            require(isinstance(reason, str) and bool(reason), 'stop-reason')
            usage.update(event.get('usage', {}))
        elif kind == 'message_stop':
            require(reason is not None and not active, 'premature-stop')
            ended = True
        else:
            raise ProtocolError('unexpected-message-event')
    require(ended, 'truncated-stream')
    return {'blocks': blocks, 'stop_reason': reason, 'usage': usage}


RESPONSE_PART_EVENTS = frozenset((
    'response.content_part.added', 'response.content_part.done', 'response.output_text.delta', 'response.output_text.done',
    'response.reasoning_summary_part.added', 'response.reasoning_summary_part.done',
    'response.reasoning_summary_text.delta', 'response.reasoning_summary_text.done'))


def _response_part(event, blocks, completed, parts):
    kind = event['type']
    index = event.get('output_index')
    require(type(index) is int and 0 <= index < len(blocks) and index not in completed, 'delta-without-block')
    require(captured_identifier(event.get('item_id')) and event['item_id'] == blocks[index].get('id'), 'response-part-item-id')
    summary = kind.startswith('response.reasoning_summary_')
    field, index_field = ('summary', 'summary_index') if summary else ('content', 'content_index')
    require(blocks[index]['type'] == ('reasoning' if summary else 'message'), 'response-part-kind')
    values = parts[index][field]
    part_index = event.get(index_field)
    require(type(part_index) is int and part_index >= 0, 'response-part-index')
    if kind.endswith('_part.added'):
        require(part_index == len(values), 'response-part-order')
        initial = event.get('part')
        require(isinstance(initial, dict) and initial.get('type') == ('summary_text' if summary else 'output_text')
                and isinstance(initial.get('text'), str), 'response-part-shape')
        values.append({'initial': copy.deepcopy(initial), 'fragments': [initial['text']], 'logprobs': [],
                       'text_done': False, 'done': False, 'incomplete': False, 'value': None})
        return
    require(part_index < len(values), 'response-part-index')
    part = values[part_index]
    require(not part['done'], 'response-part-after-done')
    if kind.endswith('.delta'):
        require(not part['text_done'] and isinstance(event.get('delta'), str), 'response-text-delta')
        part['fragments'].append(event['delta'])
        if 'logprobs' in event:
            require(isinstance(event['logprobs'], list), 'response-logprob-shape')
            part['logprobs'].extend(event['logprobs'])
    elif kind.endswith('_text.done'):
        require(not part['text_done'], 'response-text-done')
        assembled = ''.join(part['fragments'])
        require(event.get('text') == assembled, 'response-text-fidelity')
        if part['logprobs'] or 'logprobs' in event:
            require(event.get('logprobs') == part['logprobs'], 'response-logprob-fidelity')
        part.update(text=assembled, text_done=True)
    else:
        require(part['text_done'], 'response-text-done')
        expected = {**part['initial'], 'text': part['text']}
        require(completion_equal(event.get('part'), expected), 'response-part-fidelity')
        require('status' not in event or event['status'] == 'incomplete', 'response-part-status')
        part.update(value=expected, done=True, incomplete='status' in event)


def _response_reasoning_text(event, blocks, completed, parts):
    index = event.get('output_index')
    require(type(index) is int and 0 <= index < len(blocks) and index not in completed, 'delta-without-block')
    require(blocks[index].get('type') == 'reasoning', 'response-part-kind')
    require(event.get('item_id') == blocks[index].get('id') and captured_identifier(event.get('item_id')), 'response-part-item-id')
    values = parts[index]['content']
    part_index = event.get('content_index')
    require(type(part_index) is int and 0 <= part_index <= len(values), 'response-part-index')
    if part_index == len(values):
        require(event['type'].endswith('.delta') or event.get('text') == '', 'response-text-done')
        values.append({'fragments': [], 'done': False, 'incomplete': False, 'value': None})
    part = values[part_index]
    require(not part['done'], 'response-part-after-done')
    if event['type'].endswith('.delta'):
        require(isinstance(event.get('delta'), str), 'response-text-delta')
        part['fragments'].append(event['delta'])
    else:
        text = ''.join(part['fragments'])
        require(event.get('text') == text, 'response-text-fidelity')
        part.update(done=True, value={'type': 'reasoning_text', 'text': text})


def _response_arguments(event, blocks, completed, fragments, arguments_done):
    index = event.get('output_index')
    require(type(index) is int and 0 <= index < len(blocks) and index not in completed, 'delta-without-block')
    require(blocks[index].get('type') == 'function_call', 'arguments-item-kind')
    require(captured_identifier(event.get('item_id')) and event['item_id'] == blocks[index].get('id'), 'arguments-item-id')
    require(all(key not in event or event[key] == blocks[index].get(key) for key in ('call_id', 'name')), 'function-identity')
    require(index not in arguments_done, 'arguments-after-done')
    if event['type'] == 'response.function_call_arguments.delta':
        require(isinstance(event.get('delta'), str), 'arguments-delta')
        fragments[index] += event['delta']
    else:
        require(isinstance(event.get('arguments'), str) and event['arguments'] == fragments[index], 'arguments-fidelity')
        arguments_done.add(index)


def completion_equal(left, right):
    # Empty logprobs vary by event phase; both original records remain captured.
    if isinstance(left, dict) and isinstance(right, dict):
        missing = set(left) ^ set(right)
        if any(key != 'logprobs' or left.get(key, right.get(key)) not in (None, []) for key in missing):
            return False
        for key in set(left) & set(right):
            if key == 'logprobs' and left[key] in (None, []) and right[key] in (None, []):
                continue
            if not completion_equal(left[key], right[key]):
                return False
        return True
    if isinstance(left, list) and isinstance(right, list):
        return len(left) == len(right) and all(completion_equal(a, b) for a, b in zip(left, right))
    return type(left) is type(right) and left == right


def _responses(events):
    blocks, completed, fragments, parts = [], set(), {}, {}
    arguments_done = set()
    response_id = None
    terminal = None
    sequence = -1
    for event in events:
        kind = event['type']
        if kind == '[DONE]':
            require(terminal is not None, 'premature-done')
            continue
        require(terminal is None, 'after-terminal')
        if kind != 'response.created' and 'response_id' in event:
            require(response_id is not None and event['response_id'] == response_id, 'response-id')
        if 'sequence_number' in event:
            current = event.get('sequence_number')
            require(type(current) is int and current > sequence, 'response-sequence')
            sequence = current
        if kind == 'keepalive':
            continue
        if kind == 'response.created':
            require(response_id is None, 'duplicate-start')
            response_id = event.get('response', {}).get('id')
            require(captured_identifier(response_id), 'response-id')
            require('response_id' not in event or event['response_id'] == response_id, 'response-id')
        elif kind == 'response.in_progress':
            require(event.get('response', {}).get('id') == response_id, 'response-id')
        elif kind == 'response.output_item.added':
            index = event.get('output_index')
            require(response_id is not None and type(index) is int and index == len(blocks), 'block-order')
            item = event['item']
            require(isinstance(item, dict) and isinstance(item.get('type'), str), 'item-kind')
            if item['type'] == 'function_call':
                require(all(captured_identifier(item.get(key)) for key in ('id', 'call_id'))
                        and isinstance(item.get('name'), str) and bool(item['name'])
                        and isinstance(item.get('arguments'), str), 'function-identity')
            blocks.append(copy.deepcopy(item))
            fragments[index] = item['arguments'] if item['type'] == 'function_call' else ''
            parts[index] = {'content': [], 'summary': []}
        elif kind in ('response.reasoning_text.delta', 'response.reasoning_text.done'):
            _response_reasoning_text(event, blocks, completed, parts)
        elif kind in RESPONSE_PART_EVENTS:
            _response_part(event, blocks, completed, parts)
        elif kind in ('response.function_call_arguments.delta', 'response.function_call_arguments.done'):
            _response_arguments(event, blocks, completed, fragments, arguments_done)
        elif kind == 'response.output_item.done':
            index = event.get('output_index')
            require(type(index) is int and 0 <= index < len(blocks) and index not in completed, 'item-completion')
            item = event['item']
            require(('id' in item) == ('id' in blocks[index]) and item.get('id') == blocks[index].get('id'), 'item-id')
            require(item.get('type') == blocks[index].get('type') and item.get('role') == blocks[index].get('role'), 'item-kind')
            if item['type'] == 'function_call':
                require(all(key in item and item[key] == blocks[index].get(key) for key in ('call_id', 'name')), 'function-identity')
                require(index in arguments_done, 'arguments-incomplete')
                require(item.get('arguments') == fragments[index], 'arguments-fidelity')
            field = {'message': 'content', 'reasoning': 'summary'}.get(item.get('type'))
            if field is not None:
                values = parts[index][field]
                require(all(part['done'] for part in values), 'response-parts-incomplete')
                require(completion_equal(item.get(field), [part['value'] for part in values]), 'response-content-fidelity')
            if item.get('type') == 'reasoning' and ('content' in item or parts[index]['content']):
                content = parts[index]['content']
                require(all(part['done'] for part in content), 'response-parts-incomplete')
                expected_content = [part['value'] for part in content]
                if not content and item.get('content'):
                    expected_content = [{'type': 'reasoning_text', 'text': part['value']['text']} for part in parts[index]['summary']]
                require(item.get('content') == expected_content, 'response-content-fidelity')
            blocks[index] = copy.deepcopy(item)
            completed.add(index)
        elif kind in ('response.completed', 'response.incomplete'):
            terminal = event['response']
            require(terminal.get('id') == response_id and completed == set(range(len(blocks))), 'terminal-order')
            require(completion_equal(terminal.get('output'), blocks), 'output-fidelity')
            incomplete = any(part['incomplete'] for fields in parts.values() for values in fields.values() for part in values)
            require(not incomplete or terminal.get('status') == 'incomplete', 'response-part-status')
        else:
            raise ProtocolError('unexpected-response-event')
    require(terminal is not None, 'truncated-stream')
    return {'blocks': copy.deepcopy(terminal['output']), 'stop_reason': terminal.get('status'), 'usage': terminal.get('usage', {})}


class AttemptBudget:
    def __init__(self, kind):
        require(kind in ('single', 'roundtrip'), 'budget-kind')
        self.limit = 12 if kind == 'roundtrip' else 4
        self.attempts = []

    def take(self, reason):
        require(reason in ('initial', 'retry', 'compaction', 'continuation'), 'attempt-reason')
        require(len(self.attempts) < self.limit, 'attempt-cap')
        self.attempts.append({'number': len(self.attempts) + 1, 'reason': reason})


class PauseState:
    def __init__(self):
        self.blocks = []
        self.continuations = 0
        self.terminal = False

    def accept(self, response):
        require(not self.terminal, 'after-terminal')
        if response['stop_reason'] == 'pause_turn':
            require(not any(b['type'] == 'tool_use' for b in response['blocks']), 'pause-with-client-call')
            require(self.continuations < 3, 'continuation-cap')
            self.continuations += 1
        else:
            self.terminal = True
        self.blocks.extend(copy.deepcopy(response['blocks']))

    def cancel(self):
        self.terminal = True


class Barriers:
    def __init__(self, names):
        self.names = frozenset(names)
        self.seen = set()
        self.events = []
        self.condition = threading.Condition()
        self.cancelled = False

    def signal(self, name):
        with self.condition:
            require(name in self.names and name not in self.seen, 'barrier-identity')
            require(not self.cancelled, 'cancelled')
            self.seen.add(name)
            self.events.append(name)
            self.condition.notify_all()

    def wait(self, name, timeout=2):
        with self.condition:
            require(name in self.names, 'barrier-identity')
            ready = self.condition.wait_for(lambda: self.cancelled or name in self.seen, timeout)
            require(not self.cancelled, 'cancelled')
            require(ready, 'barrier-timeout')

    def cancel(self):
        with self.condition:
            self.cancelled = True
            self.condition.notify_all()


class ReadOnlyMCP:
    def __init__(self, tools):
        require(0 < len(tools) <= 128 and all(name.startswith('read_') for name in tools), 'permission')
        screen(tools)
        require(len(json.dumps(tools).encode()) <= MAX_BODY, 'mcp-catalog-cap')
        self.tools = copy.deepcopy(tools)
        self.calls = []

    def call(self, request):
        screen(request)
        require(set(request) == {'jsonrpc', 'id', 'method', 'params'} and request['jsonrpc'] == '2.0', 'mcp-request')
        require(type(request['id']) in (int, str), 'mcp-id')
        if request['method'] == 'tools/list':
            result = {'tools': [{'name': name, 'description': 'Read-only fixture.',
                       'inputSchema': {'type': 'object', 'properties': {'name': {'type': 'string'}},
                                       'required': ['name'], 'additionalProperties': False}}
                      for name in sorted(self.tools)]}
        else:
            require(request['method'] == 'tools/call', 'permission')
            params = request['params']
            require(set(params) == {'name', 'arguments'} and params['name'] in self.tools, 'permission')
            arguments = params['arguments']
            require(isinstance(arguments, dict) and set(arguments) == {'name'}, 'mcp-arguments')
            require(arguments['name'] in self.tools[params['name']], 'mcp-arguments')
            require(len(self.calls) < 128, 'mcp-call-cap')
            self.calls.append(copy.deepcopy(request))
            result = {'content': [{'type': 'text', 'text': self.tools[params['name']][arguments['name']]}], 'isError': False}
        return {'jsonrpc': '2.0', 'id': request['id'], 'result': result}


class _Server(ThreadingHTTPServer):
    request_queue_size = 4
    daemon_threads = False

    def __init__(self, handler):
        self.slots = threading.BoundedSemaphore(2)
        super().__init__(('127.0.0.1', 0), handler)

    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.slots.release()


class LoopbackProvider:
    def __init__(self, script, budget, barriers=None):
        self.budget = AttemptBudget(budget)
        require(0 < len(script) <= self.budget.limit, 'script-cap')
        self.script = copy.deepcopy(script)
        self.barriers = barriers or Barriers(())
        self.requests, self.attempts, self.responses, self.errors = [], [], [], []
        self.lock = threading.Lock()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.0'

            def setup(self):
                super().setup()
                self.connection.settimeout(2)

            def log_message(self, *args):
                pass

            def do_POST(self):
                code, raw = 200, b''
                declared_length = None
                chunk_size = 65536
                try:
                    with owner.lock:
                        number = len(owner.attempts)
                        owner.attempts.append({'number': number + 1, 'path': self.path})
                    require(self.path in ('/v1/messages', '/codex/v1/responses', '/other/v1/responses'), 'route')
                    require(number < len(owner.script), 'unexpected-attempt')
                    step = owner.script[number]
                    require(self.path == step['path'], 'route')
                    require('Transfer-Encoding' not in self.headers, 'transfer-encoding')
                    lengths = self.headers.get_all('Content-Length') or []
                    require(len(lengths) == 1 and lengths[0].isdigit(), 'content-length')
                    length = int(lengths[0])
                    require(0 < length <= MAX_BODY, 'request-cap')
                    data = self.rfile.read(length)
                    require(len(data) == length, 'truncated-request')
                    body, omitted = project_request(json.loads(data), self.path)
                    expected, _ = project_request(step['body'], self.path)
                    require(body == expected, 'request-body')
                    if self.path.endswith('/responses'):
                        require(body.get('store') is False, 'store-false')
                    with owner.lock:
                        owner.budget.take(step.get('reason', 'initial'))
                        owner.attempts[number]['omitted_fields'] = omitted
                        owner.requests.append(body)
                    if 'signal' in step:
                        owner.barriers.signal(step['signal'])
                    if 'wait' in step:
                        owner.barriers.wait(step['wait'])
                    raw = step['response']
                    require(isinstance(raw, bytes) and len(raw) <= MAX_BODY, 'response-cap')
                    chunk = step.get('chunk_bytes', 65536)
                    require(type(chunk) is int and 1 <= chunk <= 65536, 'response-chunk-bound')
                    chunk_size = chunk
                    screen_wire(raw)
                    code = step.get('status', 200)
                    require(type(code) is int and (code == 200 or 400 <= code <= 599), 'fault-status')
                    if step.get('disconnect'):
                        self.close_connection = True
                        return
                    declared_length = len(raw)
                    cut = step.get('truncate_after', declared_length)
                    require(type(cut) is int and 0 <= cut <= declared_length, 'response-truncation-bound')
                    raw = raw[:cut]
                    with owner.lock:
                        owner.responses.append(raw)
                except (ProtocolError, ValueError, UnicodeError, KeyError, TimeoutError) as error:
                    code, raw = 400, b'{"error":"fixture-rejected"}'
                    declared_length = None
                    with owner.lock:
                        owner.errors.append(type(error).__name__)
                    owner.server.shutdown()
                try:
                    self.send_response(code)
                    self.send_header('Content-Type', 'text/event-stream' if code == 200 else 'application/json')
                    self.send_header('Content-Length', str(len(raw) if declared_length is None else declared_length))
                    self.end_headers()
                    for offset in range(0, len(raw), chunk_size):
                        self.wfile.write(raw[offset:offset + chunk_size])
                        self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError, TimeoutError):
                    with owner.lock:
                        owner.errors.append('client-disconnected')

        self.server = _Server(Handler)
        self.port = self.server.server_address[1]
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={'poll_interval': 0.02})

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.barriers.cancel()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(3)
        require(not self.thread.is_alive(), 'server-shutdown')
