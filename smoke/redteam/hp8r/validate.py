#!/usr/bin/env python3
"""Validate HP-0 evidence without authorizing any product or live arm."""
import argparse
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import re

if __package__:
    from .loopback_provider import CAPTURE_FIELDS, MAX_BODY, ProtocolError, captured_identifier, decode_sse, map_opaque, screen, sse_events
else:
    from loopback_provider import CAPTURE_FIELDS, MAX_BODY, ProtocolError, captured_identifier, decode_sse, map_opaque, screen, sse_events

VERSIONS = {'schema_version': 'hp8r-manifest/1', 'corpus_version': 'hp8r-corpus/10',
            'assertion_version': 'hp8r-assertions/10', 'policy_version': 'RAT-20261014'}
READERS = {'local': 1, 'ordered': 1, 'origin': 1, 'projection': 1, 'ledger': 1, 'migration': 1}
SOURCE_CLASSES = ('screened-provider-capture', 'harness-authored-output', 'synthetic-mutation')
VERDICTS = ('PASS', 'FAIL', 'BLOCKED', 'UNVERIFIED')
APPROVED_ROOTS = (Path('/Users/palanisd/hts-native-declaration-evidence'),
    Path('/Users/palanisd/Projects/upstream/grok/plans/harness/hosted-tool-search/captures'),
    Path('/Users/palanisd/Projects/upstream/wt/grok-build-responses/smoke/redteam/report'),
    Path('/Users/palanisd/Projects/upstream/wt/grok-build-responses/smoke/wstream/report'))


class ReceiptError(ValueError):
    pass


def check(condition, code):
    if not condition:
        raise ReceiptError(code)


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(',', ':'), allow_nan=False).encode()


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def safe(value):
    try:
        screen(value)
    except ProtocolError as error:
        raise ReceiptError(str(error)) from error


def json_bytes(raw):
    def unique(pairs):
        obj = {}
        for key, value in pairs:
            check(key not in obj, 'duplicate-json-key')
            obj[key] = value
        return obj
    def constant(_):
        raise ReceiptError('nonfinite-json')
    try:
        return json.loads(raw, object_pairs_hook=unique, parse_constant=constant)
    except (ValueError, UnicodeError) as error:
        raise ReceiptError('invalid-json') from error


def local_file(root, name):
    check(isinstance(name, str) and bool(name), 'artifact-path')
    path = Path(name)
    check(not path.is_absolute() and '..' not in path.parts, 'artifact-path')
    root = Path(root).resolve()
    target = root / path
    check(target.resolve().is_relative_to(root), 'artifact-path')
    check(all(not (root / Path(*path.parts[:i])).is_symlink() for i in range(1, len(path.parts) + 1)), 'artifact-symlink')
    check(target.is_file() and target.stat().st_size <= MAX_BODY, 'artifact-missing-or-cap')
    return target


def validate_capture_binding(manifest):
    binding = manifest.get('capture_corpus')
    check(isinstance(binding, dict) and set(binding) == {'path', 'sha256', 'schema_version', 'field_classifications'}, 'capture-corpus-binding')
    check(binding['schema_version'] == 'hp8r-capture-corpus/4' and isinstance(binding['field_classifications'], list)
          and bool(binding['field_classifications']), 'capture-corpus-binding')
    for position, item in enumerate([binding, *binding['field_classifications']]):
        check(isinstance(item, dict) and isinstance(item.get('path'), str) and isinstance(item.get('sha256'), str), 'capture-corpus-binding')
        path = Path(item['path'])
        check(path.is_absolute() and any(path.resolve().is_relative_to(root) for root in APPROVED_ROOTS)
              and not any(part in ('.grok', 'home', 'auth', 'config') for part in path.parts), 'capture-corpus-path')
        check(path.is_file() and not path.is_symlink() and path.stat().st_size <= 32 * 1024 * 1024, 'capture-corpus-file')
        raw = path.read_bytes()
        check(not re.search(rb'sk-[a-z0-9]{16,}', raw, re.I), 'capture-corpus-screen')
        document = json_bytes(raw)
        if position == 0:
            safe(document)
            check(document.get('schema_version') == binding['schema_version'] and bool(document.get('records')), 'capture-corpus-schema')
            check(all(row.get('classification') in ('SUCCESS', 'TYPED-FAILURE', 'LEGACY-CORRUPTION', 'UNCLASSIFIED')
                      and row.get('product_credit') is False for row in document['records']), 'capture-corpus-class')
        check(sha(raw) == item['sha256'], 'capture-corpus-hash')


def validate_corpus(manifest, root):
    check(isinstance(manifest, dict), 'manifest-shape')
    check(all(manifest.get(k) == v for k, v in VERSIONS.items()), 'manifest-version')
    check(manifest.get('reader_versions') == READERS, 'reader-version')
    validate_capture_binding(manifest)
    check(manifest.get('normalization') == [], 'normalization')
    check(manifest.get('tooling_only') is True and manifest.get('released_shapes') == [], 'tooling-boundary')
    sources = manifest.get('sources')
    check(isinstance(sources, list) and bool(sources), 'source-inventory')
    seen = set()
    for source in sources:
        fields = {'id', 'path', 'source_path', 'source_class', 'sha256', 'hash_domain',
                  'transformations', 'redaction', 'raw_key_sweep_exit', 'facts', 'product_credit'}
        check(isinstance(source, dict) and set(source) == fields, 'source-fields')
        check(source['id'] not in seen, 'source-duplicate')
        seen.add(source['id'])
        check(source['source_class'] in SOURCE_CLASSES, 'source-class')
        check(isinstance(source['transformations'], list) and source['raw_key_sweep_exit'] == 1
              and source['redaction'] == 'structurally screened before write and hashing', 'source-screening')
        check(source['product_credit'] is False, 'source-credit')
        raw = local_file(root, source['path']).read_bytes()
        if source['path'].endswith('.sse'):
            try:
                events = sse_events(raw)
                decoded = decode_sse(raw, 'Messages')
            except ProtocolError as error:
                raise ReceiptError('source-stream') from error
            facts = {'model': events[0]['message']['model'],
                     'stop_reasons': [event['delta']['stop_reason'] for event in events if event['type'] == 'message_delta'],
                     'block_types': [block['type'] for block in decoded['blocks']]}
        else:
            body = json_bytes(raw)
            safe(body)
            blocks = body.get('output', body.get('content', []))
            facts = {'model': body['model'], 'protocol_types': [block['type'] for block in blocks]}
            if 'output' in body:
                facts['call_id_presence'] = ['null' if block.get('call_id', True) is None else 'other' for block in blocks]
        check(sha(raw) == source['sha256'], 'source-hash')
        check(facts == source['facts'], 'source-facts')
        original = Path(source['source_path'])
        check(original.is_absolute() and any(original.resolve().is_relative_to(base) for base in APPROVED_ROOTS), 'source-root')
        check(not any(part in ('.grok', 'home', 'auth', 'config') for part in original.parts), 'source-root')
        check(original.is_file() and original.stat().st_size <= MAX_BODY, 'source-original')
        source_raw = original.read_bytes()
        if source['transformations']:
            selected = json_bytes(source_raw)
            for transformation in source['transformations']:
                if set(transformation) == {'retain_top_level'}:
                    selected = {key: selected[key] for key in transformation['retain_top_level']}
                elif set(transformation) == {'retain_output_types'}:
                    selected['output'] = [item for item in selected['output'] if item['type'] in transformation['retain_output_types']]
                else:
                    raise ReceiptError('source-transformation')
            safe(selected)
            check(selected == json_bytes(raw), 'source-transformation')
        else:
            check(source_raw == raw, 'source-original')
    bank = manifest.get('vertex_bank_inventory')
    check(isinstance(bank, list) and bool(bank), 'bank-inventory')
    for item in bank:
        check(item.get('used_as_product_coverage') is False and item.get('source_class') == 'screened-provider-capture', 'bank-credit')
        path = Path(item['source_path'])
        check(path.is_absolute() and any(path.resolve().is_relative_to(base) for base in APPROVED_ROOTS)
              and not any(part in ('.grok', 'home', 'auth', 'config') for part in path.parts), 'bank-root')
        if item.get('status') != 'screened-historical-only':
            check(item.get('status') in ('BLOCKED', 'UNVERIFIED') and item.get('reason'), 'bank-status')
            continue
        check(path.is_file() and path.stat().st_size <= MAX_BODY, 'bank-source')
        raw = path.read_bytes()
        try:
            if item['format'] == 'SSE':
                events = sse_events(raw)
                stops = [event.get('delta', {}).get('stop_reason') for event in events if event.get('type') == 'message_delta']
            else:
                value = json_bytes(raw)
                safe(value)
                stops = [value['stop_reason']] if 'stop_reason' in value else []
        except ProtocolError as error:
            raise ReceiptError('bank-protocol') from error
        check(sha(raw) == item['sha256'] and len(raw) == item['bytes'], 'bank-hash')
        check(stops == item['stop_reasons'], 'bank-facts')
    unavailable = manifest.get('unavailable_witnesses', [])
    check(bool(unavailable) and all(item.get('status') in ('BLOCKED', 'UNVERIFIED')
                                  and bool(item.get('reason')) for item in unavailable), 'unavailable-witness')


def required_arms(case):
    return sorted(name for name, arm in case['arms'].items() if arm['applicability'] != 'N/A')


def validate_applicability(case, receipt):
    check(receipt.get('arm') in case.get('arms', {}), 'unknown-arm')
    arm = case['arms'][receipt['arm']]
    check(arm.get('applicability') != 'N/A', 'na-receipt')
    check(receipt.get('verdict') in VERDICTS, 'verdict')
    if case.get('native_schema') and 'dac96d87' in case['arms']:
        check(case['arms']['dac96d87']['applicability'] == 'N/A', 'native-old-incompatible')
    if arm.get('kind') == 'real-binary' and arm.get('applicability') == 'APPLICABLE':
        check(isinstance(arm.get('seam_reachability'), dict)
              and arm['seam_reachability'].get('observed') is True
              and arm['seam_reachability'].get('artifact')
              and arm['seam_reachability'].get('binary_sha256'), 'seam-reachability')
    if arm.get('applicability') == 'BLOCKED':
        check(receipt['verdict'] == 'BLOCKED', 'blocked-arm')


def differential(before, after):
    killed = (before.get('verdict') == 'FAIL' and after.get('verdict') == 'PASS'
              and before.get('failure_kind') == 'assertion' and bool(before.get('predicate')))
    return {'before': before['verdict'], 'after': after['verdict'], 'behavioral_kill': killed}


def validate_binary(binary):
    check(isinstance(binary, dict) and set(binary) == {'path', 'sha256', 'version', 'features', 'source_commit', 'build'}, 'binary-fields')
    safe(binary)
    path = Path(binary['path'])
    check(path.is_absolute() and '.grok' not in path.resolve().parts, 'binary-path')
    check(path.is_file() and not path.is_symlink() and os.access(path, os.X_OK), 'binary-executable')
    with path.open('rb') as stream:
        actual = hashlib.file_digest(stream, 'sha256').hexdigest()
    check(actual == binary['sha256'], 'binary-digest')
    check(isinstance(binary['version'], str) and bool(binary['version']), 'binary-version')
    check(isinstance(binary['features'], list) and all(isinstance(x, str) for x in binary['features']), 'binary-features')
    check(isinstance(binary['source_commit'], str) and re.fullmatch('[0-9a-f]{40}', binary['source_commit']), 'binary-commit')
    build = binary['build']
    check(isinstance(build, dict) and set(build) == {'phase', 'fingerprint_before', 'fingerprint_after', 'untracked_fixtures', 'command', 'exit'}, 'build-fields')
    check(build['phase'] == 'build-time', 'build-time')
    check(isinstance(build['fingerprint_before'], str) and re.fullmatch('[0-9a-f]{64}', build['fingerprint_before']), 'build-fingerprint')
    check(build['fingerprint_before'] == build['fingerprint_after'], 'build-drift')
    check(isinstance(build['command'], list) and bool(build['command']) and build['exit'] == 0, 'build-command')
    fixtures = build['untracked_fixtures']
    check(isinstance(fixtures, dict) and all(isinstance(key, str) and isinstance(value, dict)
          and set(value) == {'sha256', 'mode'} and re.fullmatch('[0-9a-f]{64}', value['sha256'])
          and type(value['mode']) is int for key, value in fixtures.items()), 'build-fixtures')


BASE_ARTIFACTS = frozenset(('source.json', 'ordered.json', 'checkpoints/pre.json', 'reload/pre.json',
    'checkpoints/post.json', 'reload/post.json', 'ledger.json', 'route-manifest.json', 'binary.json',
    'plan.json', 'bodies.json', 'attempts.jsonl', 'events.jsonl', 'observation.json', 'display.json', 'usage.json', 'authority.json'))


def validate_receipt_utc(value):
    check(isinstance(value, str) and re.fullmatch(r'[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?Z', value), 'receipt-utc')
    try:
        datetime.fromisoformat(value)
    except ValueError as error:
        raise ReceiptError('receipt-utc') from error


def validate_report(manifest, root):
    root = Path(root)
    receipt = json_bytes(local_file(root, 'receipt.json').read_bytes())
    fields = {'schema_version', 'utc', 'case', 'arm', 'layer', 'shape', 'action', 'rat_clause', 'H',
              'manifest_digest', 'reader_versions', 'assertion_version', 'corpus_version', 'policy_version',
              'source_class', 'product_credit', 'verdict', 'failed_paths', 'reason', 'artifacts', 'sha256'}
    check(isinstance(receipt, dict) and set(receipt) == fields, 'receipt-fields')
    safe(receipt)
    unsigned = {k: v for k, v in receipt.items() if k != 'sha256'}
    check(digest(unsigned) == receipt['sha256'], 'receipt-hash')
    check(receipt['schema_version'] == 'hp8r-receipt/2', 'receipt-version')
    validate_receipt_utc(receipt['utc'])
    check(receipt['manifest_digest'] == digest(manifest), 'manifest-hash')
    check(receipt['case'] in manifest['cases'], 'case')
    case = manifest['cases'][receipt['case']]
    validate_applicability(case, receipt)
    check(receipt['layer'] in case['layers'] and receipt['shape'] == case['shape'], 'layer-shape')
    check(receipt['action'] == case['action'] and receipt['rat_clause'] == case['rat_clause'], 'action')
    check(receipt['H'] == case['H'], 'H-witness')
    check(receipt['reader_versions'] == READERS and all(receipt[k] == manifest[k]
          for k in ('assertion_version', 'corpus_version', 'policy_version')), 'receipt-readers')
    check(receipt['source_class'] == 'synthetic-mutation' and receipt['product_credit'] is False
          and receipt['layer'] == 'L0', 'synthetic-credit')
    check(isinstance(receipt['failed_paths'], list) and isinstance(receipt['reason'], str), 'verdict-details')
    if receipt['verdict'] in ('PASS', 'FAIL'):
        check((receipt['verdict'] == 'PASS') == (receipt['failed_paths'] == []), 'failed-paths')
    inventory = receipt['artifacts']
    check(isinstance(inventory, list) and 0 < len(inventory) <= 128, 'artifact-inventory')
    artifacts, raw_artifacts = {}, {}
    for item in inventory:
        check(isinstance(item, dict) and set(item) == {'path', 'sha256'}, 'artifact-entry')
        name = item['path']
        check(name not in artifacts and name not in ('receipt.json', 'verdict.json'), 'duplicate-artifact')
        raw = local_file(root, name).read_bytes()
        if name.endswith('.jsonl'):
            check(raw.endswith(b'\n') or not raw, 'jsonl-truncated')
            value = [json_bytes(line) for line in raw.splitlines()]
            check(len(value) <= 4096, 'jsonl-cap')
        else:
            value = json_bytes(raw)
        artifact_shape(name, value)
        safe(value)
        check(sha(raw) == item['sha256'], 'artifact-hash')
        artifacts[name], raw_artifacts[name] = value, raw
    check(BASE_ARTIFACTS <= set(artifacts), 'missing-artifact')
    check(sum(len(raw) for raw in raw_artifacts.values()) <= 32 * 1024 * 1024, 'report-cap')
    actual = {str(p.relative_to(root)) for p in root.rglob('*') if p.is_file()}
    check(actual == set(artifacts) | {'receipt.json', 'verdict.json'}, 'unlisted-artifact')
    verdict = json_bytes(local_file(root, 'verdict.json').read_bytes())
    check(verdict == {'verdict': receipt['verdict'], 'tooling_only': True}, 'verdict-artifact')
    check(artifacts['route-manifest.json'] == case['route'], 'route')
    check(artifacts['binary.json'] == {'kind': 'synthetic-mutation', 'executed': False, 'build_provenance': None}, 'binary-kind')
    if receipt['verdict'] == 'BLOCKED':
        check(artifacts['attempts.jsonl'] == [], 'blocked-no-launch')
    validate_chain(manifest, case, receipt, artifacts, raw_artifacts)
    if 'hold.json' in artifacts:
        validate_hold(artifacts['hold.json'])
    return receipt['verdict']


def validate_chain(manifest, case, receipt, artifacts, raw_artifacts):
    source = artifacts['source.json']
    expected = manifest['synthetic_sources'][case['source_id']]['items']
    check(source.get('items') == expected and source.get('source_class') == 'synthetic-mutation', 'source-identity')
    coordinates = [{'lineage': 'fixture-lineage', 'occurrence': i} for i in range(len(expected))]
    check(source.get('coordinates') == coordinates, 'source-coordinates')
    check(artifacts['ordered.json'] == source, 'ordered-fidelity')
    if case['control'] == 'live-reload':
        validate_live_reload(case, artifacts, raw_artifacts)
        return
    pre, post = artifacts['checkpoints/pre.json'], artifacts['checkpoints/post.json']
    check(pre.get('kind') == 'synthetic-checkpoint-observation' and post.get('kind') == pre['kind'], 'checkpoint-kind')
    check(pre.get('history') == expected, 'pre-history')
    check(pre['records']['groups'] == manifest['synthetic_sources'][case['source_id']]['groups'], 'source-groups')
    check(set(pre['records']['lifecycle']['groups']) == {group['key'] for group in pre['records']['groups']}
          and set(post['records']['lifecycle']['groups']) == {group['key'] for group in post['records']['groups']}, 'lifecycle-membership')
    check(artifacts['reload/pre.json'] == pre and artifacts['reload/post.json'] == post, 'independent-reload')
    check(all(pre['records'].get(k) == 1 and post['records'].get(k) == 1
              for k in ('schema_version', 'policy_revision', 'reader_floor', 'limits_revision')), 'checkpoint-reader')
    check(not contains_field(post, {'pin_owner', 'held_input', 'occurrence_clock'}), 'ephemeral-checkpoint')
    events = artifacts['events.jsonl']
    check(isinstance(events, list) and [e.get('sequence') for e in events] == list(range(len(events))), 'event-sequence')
    kinds = [e.get('type') for e in events]
    if receipt['verdict'] == 'PASS':
        check(all(kind in kinds for kind in case['required_events']), 'missing-witness')
    check(not set(kinds) & set(case['forbidden_events']), 'forbidden-effect')
    check(len(kinds) == len(set(kinds)), 'duplicate-event')
    order = [kinds.index(kind) for kind in case['required_events'] if kind in kinds]
    check(order == sorted(order), 'event-order')
    for event in events:
        if event['type'] == 'producer':
            check(event.get('mechanism') == case['producer_mechanism']
                  and event.get('raw') == 'source.json' and event.get('decoded') == 'ordered.json'
                  and event.get('source_class') == 'synthetic-mutation', 'producer-mechanism')
        if event['type'] == 'projection_ack':
            check(event.get('generation') == post['records']['projection']['generation']
                  and event.get('checkpoint') == 'checkpoints/post.json', 'projection-ack')
    plan = artifacts['plan.json']
    check(plan.get('action') == case['action'] and plan.get('history_digest') == digest(pre['history'])
          and plan.get('binding') == case['route']['model'], 'plan')
    check(plan.get('activation') == 'synthetic-selected', 'activation')
    check(plan.get('splice') == {'dialect': case['route']['dialect'], 'selected': case['route']['dialect'] == 'Codex'}, 'splice')
    control = case['control']
    if control != 'positive':
        check(pre == post and plan.get('send_only') is True, 'no-change-records')
    else:
        check(post['records']['projection'] == {'generation': 2, 'high_water': 2, 'disposition': 'F'}
              and plan.get('send_only') is False and plan.get('generation') == 2, 'durable-projection')
    ledger = artifacts['ledger.json']
    check(ledger.get('source_digest') == digest(source) and ledger.get('active_digest') == digest(post['history']), 'ledger-digests')
    check(ledger.get('lifecycle') == post['records']['lifecycle'] and ledger.get('groups') == post['records']['groups'], 'SC2-C1-links')
    members = ledger.get('members')
    check(isinstance(members, list) and [member.get('source') for member in members] == list(range(len(expected))), 'member-conservation')
    check(all(set(member) == {'source', 'action', 'output', 'scope', 'reason'} and member['reason'] == 'fixture-policy'
              and member['scope'] == ('durable' if control == 'positive' else 'request') for member in members), 'member-witness')
    check(ledger.get('omissions') == ([0] if control == 'omission' else []), 'omission-ledger')
    target = list(expected)
    for index, member in enumerate(members):
        action = 'F' if case['action'] == 'F' and index in (0, len(expected) - 1) else 'OMIT' if control == 'omission' and index == 0 else 'KEEP'
        check(member['action'] == action, 'keep-action-witness')
        expected_index = None if action == 'OMIT' else index - (1 if control == 'omission' else 0)
        check(member['output'] == expected_index, 'output-mapping')
        if action == 'F':
            text = 'Historical discovery request; no current authority.' if index == 0 else 'Historical discovery completed; no current authority.'
            target[index] = {'type': 'message', 'role': 'assistant', 'content': [{'type': 'output_text', 'text': text}]}
    if control == 'omission':
        target = target[1:]
        check(post['records']['lifecycle']['groups'] == {'group-0': 'AbandonedPending'}
              and post['records']['lifecycle']['exchange'] == 'Terminal', 'omission-terminal')
    if control == 'positive':
        check(post['history'] == target, 'post-history')
    validate_attempts(case, receipt, artifacts, target)
    validate_observation(case, artifacts)
    validate_display(artifacts, members)
    if case['action'] == 'KEEP':
        validate_retained_backing(manifest['synthetic_sources'][case['source_id']], artifacts)
    validate_usage_and_authority(case, artifacts)



def validate_search_result(content):
    check(isinstance(content, dict), 'SC2-C1-search-result-shape')
    kind = content.get('type')
    if kind == 'tool_search_tool_search_result':
        check(set(content) == {'type', 'tool_references'} and isinstance(content['tool_references'], list), 'SC2-C1-search-result-shape')
        check(all(isinstance(reference, dict) and set(reference) == {'type', 'tool_name'}
                  and reference['type'] == 'tool_reference' and isinstance(reference['tool_name'], str)
                  and bool(reference['tool_name']) for reference in content['tool_references']), 'SC2-C1-search-result-references')
    elif kind == 'tool_search_tool_result_error':
        check({'type', 'error_code'} <= set(content) <= {'type', 'error_code', 'error_message'}, 'SC2-C1-search-result-shape')
        check(content['error_code'] in ('invalid_tool_input', 'unavailable', 'too_many_requests', 'execution_time_exceeded'), 'SC2-C1-search-result-error-code')
        check(content.get('error_message') is None or isinstance(content['error_message'], str), 'SC2-C1-search-result-error-message')
    else:
        raise ReceiptError('SC2-C1-search-result-kind')


def live_links(history):
    groups, server_calls, client_calls, results, states = [], {}, {}, {}, {}
    for item_index, message in enumerate(history):
        check(set(message) == {'role', 'content'} and message['role'] in ('assistant', 'user')
              and isinstance(message['content'], list), 'SC2-C1-message')
        for block_index, block in enumerate(message['content']):
            coordinate = {'item': item_index, 'block': block_index}
            kind = block.get('type')
            if message['role'] == 'user':
                check(kind == 'tool_result', 'SC2-C1-results-only')
            if kind == 'server_tool_use':
                join = block.get('id')
                check(isinstance(join, str) and bool(join), 'SC2-C1-server-join')
                if join in server_calls:
                    first, group = server_calls[join]
                    check(block == first, 'SC2-C1-duplicate-payload')
                    group['duplicate_links'].append({'source': coordinate, 'canonical': group['members'][0]})
                    group['members'].append(coordinate)
                else:
                    group = {'key': 'group-' + str(len(groups)), 'family': 'Messages', 'execution': 'server',
                             'origin_ref': 'fixture-M-origin', 'validation': {'kind': 'messages-id', 'join': join},
                             'members': [coordinate], 'duplicate_links': []}
                    groups.append(group)
                    server_calls[join] = (block, group)
                    states[group['key']] = 'LivePending'
            elif kind == 'tool_search_tool_result':
                validate_search_result(block.get('content'))
                join = block.get('tool_use_id')
                check(join in server_calls, 'SC2-C1-server-join')
                group = server_calls[join][1]
                check(states[group['key']] == 'LivePending', 'SC2-C1-server-result')
                group['members'].append(coordinate)
                states[group['key']] = 'Completed'
            elif kind == 'tool_use':
                join = block.get('id')
                check(isinstance(join, str) and bool(join) and join not in client_calls, 'SC2-C1-client-join')
                client_calls[join] = coordinate
            elif kind == 'tool_result':
                join = block.get('tool_use_id')
                check(join in client_calls and join not in results, 'SC2-C1-client-result')
                results[join] = coordinate
            else:
                check(kind == 'text', 'SC2-C1-block')
    check(bool(client_calls) and set(results) == set(client_calls) and set(states.values()) == {'Completed', 'LivePending'}, 'SC2-C1-mixed')
    waiting = [{'call_id': join, 'source': source, 'result': results[join]} for join, source in client_calls.items()]
    return groups, states, waiting


def validate_live_reload(case, artifacts, raw_artifacts):
    pre, post = artifacts['checkpoints/pre.json'], artifacts['checkpoints/post.json']
    history = artifacts['source.json']['items']
    check(pre == artifacts['reload/pre.json'] and post == artifacts['reload/post.json'], 'independent-reload')
    check(pre == post and pre['history'] == history and pre['kind'] == 'synthetic-checkpoint-observation', 'no-change-records')
    check(not contains_field(post, {'pin_owner', 'held_input', 'occurrence_clock'}), 'ephemeral-checkpoint')
    records = pre['records']
    check(all(records.get(key) == 1 for key in ('schema_version', 'policy_revision', 'reader_floor', 'limits_revision')), 'checkpoint-reader')
    check(records['projection'] == {'generation': 1, 'high_water': 1, 'disposition': 'KEEP'}, 'no-change-records')
    expected_groups, states, expected_waiting = live_links(history)
    groups = records['groups']
    check(len(groups) == len(expected_groups), 'SC2-C1-members')
    check([{key: group[key] for key in ('key', 'family', 'execution', 'origin_ref', 'validation', 'members')}
           for group in groups] == [{key: group[key] for key in ('key', 'family', 'execution', 'origin_ref', 'validation', 'members')}
                                   for group in expected_groups], 'SC2-C1-members')
    check([group['duplicate_links'] for group in groups] == [group['duplicate_links'] for group in expected_groups], 'SC2-C1-duplicate-links')
    lifecycle = records['lifecycle']
    check(lifecycle['exchange'] == 'LiveBound' and lifecycle['terminal'] is None and lifecycle['groups'] == states, 'SC2-C1-live-state')
    waiting = lifecycle['waiting_clients']
    check([row.get('call_id') for row in waiting] == [row['call_id'] for row in expected_waiting], 'SC2-C1-waiting-id')
    check([{key: row.get(key) for key in ('source', 'result')} for row in waiting]
          == [{key: row[key] for key in ('source', 'result')} for row in expected_waiting], 'SC2-C1-waiting-coordinates')
    check(all(set(row) == {'call_id', 'source', 'result', 'delivery'} and row['delivery'] == 'accepted-pending-send' for row in waiting), 'SC2-C1-delivery-state')
    check(artifacts.get('handoff.json') == {'binding_ref': 'fixture-binding-A', 'accepted_clients': expected_waiting,
                                         'delivered_clients': []}, 'SC2-C1-handoff-owner')
    ledger = artifacts['ledger.json']
    check(ledger['lifecycle'] == lifecycle and ledger['groups'] == groups, 'SC2-C1-links')
    check(ledger['source_digest'] == digest(artifacts['source.json']) and ledger['active_digest'] == digest(history), 'ledger-digests')
    expected_members = [{'source': i, 'action': 'KEEP', 'output': i, 'scope': 'request', 'reason': 'fixture-policy'} for i in range(len(history))]
    check(ledger['members'] == expected_members and ledger['omissions'] == [] and ledger['schema_version'] == 1, 'member-conservation')
    check(ledger['shared_charges'] == {'source': len(raw_artifacts['source.json']),
          'active': len(raw_artifacts['checkpoints/post.json']), 'ledger': len(raw_artifacts['ledger.json'])}, 'SC2-C1-charges')
    check(sum(ledger['shared_charges'].values()) * 3 + 4 * 1024 * 1024 <= 256 * 1024 * 1024, 'SC2-C1-lineage-cap')
    events = [{'sequence': i, 'type': kind} for i, kind in enumerate(case['required_events'])]
    events[0].update(mechanism=case['producer_mechanism'], source_class='synthetic-mutation', raw='source.json', decoded='ordered.json')
    check(artifacts['events.jsonl'] == events and not set(case['required_events']) & set(case['forbidden_events']), 'SC2-C1-live-events')
    check(artifacts['attempts.jsonl'] == [] and artifacts['bodies.json'] == []
          and not any(name.startswith('wire/') for name in artifacts), 'SC2-C1-no-send')
    check(artifacts['plan.json'] == {'action': 'KEEP', 'history_digest': digest(history), 'binding': case['route']['model'],
          'body_digest': None, 'generation': 1, 'send_only': True,
          'splice': {'dialect': 'Messages', 'selected': False}, 'activation': 'synthetic-selected'}, 'plan')
    validate_observation(case, artifacts)
    validate_display(artifacts, expected_members)
    validate_usage_and_authority(case, artifacts)


def validate_attempts(case, receipt, artifacts, target):
    attempts = artifacts['attempts.jsonl']
    bodies = artifacts['bodies.json']
    check(isinstance(attempts, list) and isinstance(bodies, list), 'attempt-shape')
    cap = 12 if case['budget'] == 'roundtrip' else 4
    check(len(attempts) <= cap, 'attempt-cap')
    check([attempt.get('number') for attempt in attempts] == list(range(1, len(attempts) + 1)), 'attempt-order')
    check(len(attempts) == len(bodies), 'hidden-attempt')
    request_files = {name for name in artifacts if re.fullmatch(r'wire/req-[0-9]{3}\.json', name)}
    response_files = {name for name in artifacts if re.fullmatch(r'wire/resp-[0-9]{3}\.json', name)}
    check(request_files == {body.get('request') for body in bodies}
          and response_files == {body.get('response') for body in bodies}, 'wire-inventory')
    previous = 0
    for index, (attempt, body) in enumerate(zip(attempts, bodies)):
        check(set(attempt) == {'number', 'request', 'reason', 'status', 'continuation', 'request_id'}
              and set(body) == {'request', 'response'}, 'attempt-fields')
        check(attempt['reason'] in ('initial', 'retry', 'compaction', 'continuation'), 'attempt-reason')
        check(type(attempt['continuation']) is int and previous <= attempt['continuation'] <= min(3, previous + 1), 'continuation-cap')
        previous = attempt['continuation']
        check(attempt['request'] == body['request'] and isinstance(attempt['request_id'], str)
              and bool(attempt['request_id']), 'request-binding')
        check(type(attempt['status']) is int and 100 <= attempt['status'] <= 599, 'http-status')
        if receipt['verdict'] == 'PASS':
            check(attempt['status'] == 200, 'recovered-failure')
        request, response = artifacts[body['request']], artifacts[body['response']]
        check(set(request) == {'model', 'store', 'stream', 'input', 'tools'} and request['store'] is False
              and request['model'] == case['route']['model'] and request['stream'] is True, 'actual-request')
        check(isinstance(request['input'], list) and isinstance(request['tools'], list), 'actual-body')
        if index == 0:
            check(request['input'] == target, 'body-fidelity')
            check(digest(request) == artifacts['plan.json']['body_digest'], 'plan-body')
        if case['action'] == 'F':
            f_items = [item for i, item in enumerate(request['input']) if i in (0, len(target) - 1)]
            validate_f_text(f_items)
        check(response.get('object') == 'response' and response.get('status') == 'completed'
              and isinstance(response.get('output'), list) and isinstance(response.get('usage'), dict), 'actual-response')
        check('fixture-lineage' not in strings(request), 'host-coordinate-leak')
    events = artifacts['events.jsonl']
    validate_event_bodies(case, artifacts)
    for event in events:
        if event['type'] == 'dispatch':
            check(event.get('permission') == 'allow' and event.get('historical') is False
                  and event.get('name') == 'read_fixture_0' and event.get('call_id') == 'fresh-call', 'dispatch-permission')
        elif event['type'] == 'result':
            check(event.get('call_id') == 'fresh-call' and event.get('value') == 'fixture-read-result', 'dispatch-result')
    if any(event['type'] == 'dispatch' for event in events):
        check(len(bodies) == 3, 'next-request-chain')
        first = artifacts[bodies[0]['response']]['output']
        expected_call = {'type': 'function_call', 'call_id': 'fresh-call', 'name': 'read_fixture_0', 'arguments': '{}'}
        check(first == [expected_call], 'actual-function-call')
        expected_result = {'type': 'function_call_output', 'call_id': 'fresh-call', 'output': 'fixture-read-result'}
        check(artifacts[bodies[1]['request']]['input'] == target + [expected_call, expected_result], 'automatic-result-request')
        later = {'type': 'message', 'role': 'user', 'content': [{'type': 'input_text', 'text': 'later input'}]}
        accumulated = target + [expected_call, expected_result] + artifacts[bodies[1]['response']]['output']
        check(artifacts[bodies[2]['request']]['input'] == accumulated + [later], 'later-user-request')


def validate_event_bodies(case, artifacts):
    events, bodies, attempts = artifacts['events.jsonl'], artifacts['bodies.json'], artifacts['attempts.jsonl']
    for event in events:
        if event['type'] in ('send', 'response', 'dispatch', 'result', 'automatic_next', 'later_user'):
            check(event.get('binding') == case['route']['model'], 'event-binding')
    sends = [event for event in events if event['type'] in ('send', 'automatic_next', 'later_user')]
    check(len(sends) == len(bodies) == len(attempts), 'event-send-count')
    for event, body, attempt in zip(sends, bodies, attempts):
        check(event.get('request') == body['request'] == attempt['request'], 'event-request-order')
    for event in events:
        if event['type'] == 'response':
            check(bool(bodies) and {key: event.get(key) for key in ('request', 'response')} == bodies[0], 'event-response-source')
        elif event['type'] == 'dispatch':
            check(bool(bodies) and event.get('response') == bodies[0]['response'], 'event-dispatch-source')
        elif event['type'] == 'result':
            check(len(bodies) > 1 and event.get('response') == bodies[0]['response']
                  and event.get('request') == bodies[1]['request'], 'event-result-source')


def validate_f_text(items):
    check(len(items) <= 6, 'F-group-cap')
    total = 0
    for index, item in enumerate(items):
        check(isinstance(item, dict) and set(item) == {'type', 'role', 'content'}
              and item['type'] == 'message' and item['role'] == 'assistant', 'F-shape')
        content = item['content']
        check(isinstance(content, list) and len(content) == 1 and set(content[0]) == {'type', 'text'}
              and content[0]['type'] == 'output_text', 'F-shape')
        text = content[0]['text']
        check(isinstance(text, str) and len(text.encode('utf-8')) <= 128, 'F-half-cap')
        expected = 'Historical discovery request; no current authority.' if index % 2 == 0 else 'Historical discovery completed; no current authority.'
        check(text == expected, 'F-future-information')
        total += len(text.encode('utf-8'))
    check(total <= 1024, 'F-total-cap')


def validate_observation(case, artifacts):
    observation = artifacts['observation.json']
    events = artifacts['events.jsonl']
    check(isinstance(observation, dict) and set(observation) == {'complete', 'start_sequence', 'end_sequence', 'barrier', 'counters'}, 'observation-fields')
    check(observation['complete'] is True and observation['start_sequence'] == 0
          and observation['end_sequence'] == len(events) - 1 and observation['barrier'] == 'observation_barrier'
          and events[-1]['type'] == 'observation_barrier', 'absence-observation')
    counters = {kind: sum(event['type'] == kind for event in events)
                for kind in ('projection_write', 'projection_ack', 'Applied', 'dispatch')}
    counters['send'] = len(artifacts['attempts.jsonl'])
    check(observation['counters'] == counters, 'observation-counters')
    send_events = sum(event['type'] in ('send', 'automatic_next', 'later_user') for event in events)
    check(send_events == counters['send'], 'send-conservation')
    if case['control'] in ('auxiliary', 'refusal', 'omission'):
        check(counters['dispatch'] == 0, 'forbidden-dispatch')
    if case['control'] == 'refusal':
        check(counters['send'] == 0, 'forbidden-send')


def validate_display(artifacts, members):
    display = artifacts['display.json']
    counts = {action: sum(member['action'] == action for member in members) for action in ('KEEP', 'F', 'STRIP', 'OMIT')}
    check(set(display) == {'tui', 'acp', 'host_summary'} and display['tui'] == counts and display['acp'] == counts, 'TUI-ACP-accounting')
    summary = display['host_summary']
    check(isinstance(summary, dict) and set(summary) == {'counts', 'reasons', 'rediscovery'}
          and summary['counts'] == counts and type(summary['rediscovery']) is bool, 'host-summary-shape')
    check(len(canonical(summary)) <= 512, 'host-summary-cap')
    check(isinstance(summary['reasons'], list) and len(summary['reasons']) <= 8, 'host-reason-cap')
    check(all(isinstance(reason, dict) and set(reason) == {'category', 'count'}
              and reason['category'] in ('fixture-policy', 'foreign-origin', 'unqualified', 'capacity',
                  'abandoned', 'suppressed', 'cancelled', 'unavailable')
              and type(reason['count']) is int and reason['count'] >= 0 for reason in summary['reasons']), 'host-reasons')
    reasons = {}
    for member in members:
        reasons[member['reason']] = reasons.get(member['reason'], 0) + 1
    check(summary['reasons'] == [{'category': reason, 'count': count} for reason, count in reasons.items()], 'host-reason-ledger')
    check(summary['rediscovery'] == bool(counts['F']), 'host-rediscovery')


def validate_retained_backing(source, artifacts):
    retained = [(item_index, definition_index, definition)
                for item_index, item in enumerate(artifacts['source.json']['items'])
                if item.get('type') == 'tool_search_output'
                for definition_index, definition in enumerate(item['tools'])]
    snapshots = source.get('eligible_snapshots', [])
    check(len(snapshots) == len(retained), 'KEEP-retained-registration')
    expected = []
    for snapshot, (item_index, definition_index, definition) in zip(snapshots, retained):
        check(set(snapshot) == {'source', 'definition', 'namespace', 'name', 'schema', 'eligible'}
              and snapshot['source'] == item_index and snapshot['definition'] == definition_index
              and snapshot['name'] == definition['name'] and snapshot['schema'] == definition['parameters']
              and isinstance(snapshot['namespace'], str) and bool(snapshot['namespace'])
              and snapshot['eligible'] is True, 'KEEP-retained-registration')
        expected.append({key: snapshot[key] for key in ('namespace', 'name', 'schema', 'eligible')})
    if retained:
        registrations = artifacts['authority.json']['registrations']
        check([{key: registration.get(key) for key in ('namespace', 'name', 'schema', 'eligible')}
               for registration in registrations] == expected, 'KEEP-current-registration')


def validate_usage_and_authority(case, artifacts):
    usage = artifacts['usage.json']
    bodies = artifacts['bodies.json']
    expected = [{'attempt': i + 1, 'usage': artifacts[body['response']]['usage']} for i, body in enumerate(bodies)]
    check(set(usage) == {'responses', 'aggregate', 'logical_terminal_count'} and usage['responses'] == expected, 'response-usage')
    fields = ('input_tokens', 'output_tokens', 'total_tokens')
    check(all(set(item['usage']) == set(fields) and all(type(item['usage'][k]) is int and item['usage'][k] >= 0 for k in fields) for item in expected), 'usage-presence')
    aggregate = {key: sum(item['usage'][key] for item in expected) for key in fields}
    check(usage['aggregate'] == aggregate and usage['logical_terminal_count'] == (0 if case['control'] == 'live-reload' else 1), 'usage-once')
    authority = artifacts['authority.json']
    check(set(authority) == {'registrations', 'loaded_before', 'loaded_after', 'search_dispatches', 'function_dispatches', 'historical_dispatches'}, 'authority-fields')
    registrations = authority['registrations']
    check(isinstance(registrations, list) and len(registrations) == 6, 'registration-backing')
    for registration in registrations:
        check(set(registration) == {'namespace', 'name', 'schema', 'schema_digest', 'eligible'}
              and registration['namespace'] == 'fixture_builtin' and registration['eligible'] is True
              and registration['schema_digest'] == digest(registration['schema']), 'registration-backing')
    names = [registration['name'] for registration in registrations]
    check(names == ['read_fixture_' + str(i) for i in range(6)] and authority['loaded_before'] == names, 'registration-identity')
    expected_loaded = [] if case['action'] in ('F', 'OMIT') else names
    check(authority['loaded_after'] == expected_loaded, 'loaded-resurrection')
    check(authority['search_dispatches'] == 0 and authority['historical_dispatches'] == 0
          and authority['function_dispatches'] == sum(e['type'] == 'dispatch' for e in artifacts['events.jsonl']), 'execution-once')
    for body in bodies:
        tools = artifacts[body['request']]['tools']
        check(len(tools) == 1 and tools[0].get('name') == names[0]
              and tools[0].get('parameters') == registrations[0]['schema'], 'declaration-backing')


def contains_field(value, fields):
    if isinstance(value, dict):
        return bool(set(value) & fields) or any(contains_field(child, fields) for child in value.values())
    return isinstance(value, list) and any(contains_field(child, fields) for child in value)


def strings(value):
    if isinstance(value, dict):
        return [text for child in value.values() for text in strings(child)]
    if isinstance(value, list):
        return [text for child in value for text in strings(child)]
    return [value] if isinstance(value, str) else []


def validate_accounting(accounting):
    check(isinstance(accounting, dict) and set(accounting) == {'records', 'charged_bytes', 'shared_other_bytes'}, 'hold-accounting-fields')
    records = accounting['records']
    check(isinstance(records, dict) and set(records) == {'header', 'entries', 'resident_payloads', 'prepared_copies', 'reserve'}, 'hold-record-fields')
    entries = records['entries']
    check(isinstance(entries, list) and len(entries) <= 128, 'hold-entry-count')
    keys = [entry.get('occurrence') for entry in entries]
    check(all(type(key) is int and key > 0 for key in keys) and keys == sorted(set(keys)), 'hold-occurrence')
    check(all(entry.get('source') in ('human', 'parent', 'monitor', 'deferred', 'skill', 'plan', 'reminder')
              and entry.get('storage') in ('custody', 'resident-reference') for entry in entries), 'hold-entry-shape')
    check(all(len(canonical(entry)) <= 32 * 1024 for entry in entries), 'hold-entry-bytes')
    reserve = records['reserve']
    check(isinstance(reserve, dict) and set(reserve) == {'hold_refusal', 'terminal', 'usage', 'omission'}
          and reserve['terminal'].get('accepted') == len(entries) and reserve['usage'].get('accepted') == len(entries)
          and reserve['omission'].get('accepted') == len(entries), 'hold-reserve')
    check(all(copy.get('occurrence') in keys for copy in records['prepared_copies']), 'hold-copy-source')
    charged = len(canonical(records))
    check(type(accounting['charged_bytes']) is int and accounting['charged_bytes'] == charged, 'hold-physical-charge')
    check(charged <= 256 * 1024, 'hold-aggregate-cap')
    other = accounting['shared_other_bytes']
    check(type(other) is int and other >= 0 and other + charged <= 4 * 1024 * 1024, 'hold-shared-cap')
    return charged


def validate_hold(value):
    fields = {'domain', 'serialized_pins', 'recovered_capability', 'accounting', 'cancel', 'release',
              'oversize_human', 'no_resident_overflow', 'D2', 'parent_persistence', 'parent_cancel',
              'human_persistence', 'mode_writes'}
    check(isinstance(value, dict) and set(value) == fields, 'hold-fields')
    safe(value)
    check(value['domain'] == 'ephemeral-observation', 'hold-domain')
    check(value['serialized_pins'] == [] and value['recovered_capability'] is False, 'hold-pins')
    validate_accounting(value['accounting'])
    cancel = value['cancel']
    check(cancel['accepted_humans_before'] == cancel['accepted_humans_after'], 'hold-cancel-custody')
    check(cancel['residents_after'] == [] and cancel['autostarted'] is False, 'hold-cancel-resident')
    check(cancel['parent_settled'] == cancel['parent_before'] and cancel['parent_after'] == [], 'hold-parent-settlement')
    release = value['release']
    accepted, prefix, suffix = release['accepted'], release['eligible_prefix'], release['cutoff_deferred']
    check(accepted == sorted(set(accepted)) and prefix + suffix == accepted, 'hold-cutoff')
    check(release['pending_calls'] == [], 'hold-pending-release')
    check(release['actual_safe_point'] == release['first_eligible_safe_point']
          and release['next_delivered_safe_point'] == release['next_safe_point']
          and release['fresh_prompt_required'] is False, 'hold-first-release')
    check(release['next_admitted_turn'] is True and release['after_prompt'] is True, 'hold-next-admitted')
    missing = release['missing_monitors']
    suppressions = release['suppressions']
    check([item['occurrence'] for item in suppressions] == missing
          and all(item['reason'] in ('drop-monitor', 'kill-background', 'completion-dedupe', 'session-end') for item in suppressions), 'hold-suppression')
    check(release['delivered_prefix'] == [key for key in prefix if key not in missing], 'hold-delivery-order')
    check(release['restored_suffix'] == suffix and release['later_delivered'] == suffix, 'hold-keyed-suffix')
    delivered = release['delivered_prefix'] + release['later_delivered']
    check(len(delivered) == len(set(delivered)), 'hold-duplicate-delivery')
    oversized = value['oversize_human']
    check(oversized['held_status'] == 'resident-backpressure' and oversized['unheld_prepared'] is True
          and oversized['held_cap_reapplied'] is False, 'hold-oversize-human')
    overflow = value['no_resident_overflow']
    check(overflow['source'] == 'deferred-followup' and overflow['outcome'] == 'typed-incomplete'
          and overflow['accepted_prefix'] == overflow['retained_prefix'], 'hold-prefix-incomplete')
    d2 = value['D2']
    check(d2['admitted'] is False and d2['unpinned_exposure'] == 50 and d2['accepted_entry_success'] is False
          and d2['diagnostic'] == 'unadmitted-eviction', 'hold-D2')
    persisted = set()
    for attempt in value['parent_persistence']:
        key = (attempt['binding'], attempt['slot'])
        check(not attempt['persisted'] or key not in persisted, 'hold-parent-persistence')
        if attempt['persisted']:
            persisted.add(key)
        check(not attempt['submitted'] or key in persisted, 'hold-parent-before-submit')
    parent_cancel = value['parent_cancel']
    check(parent_cancel['settled_binding'] != parent_cancel['next_binding'], 'hold-parent-cancel-owner')
    check(all(not row['persisted'] or row['submitted'] for row in value['human_persistence']), 'hold-human-after-submit')
    writes = value['mode_writes']
    check(writes['hold_writes'] == 0 and writes['release_writes'] == 0, 'hold-additional-writes')
    check(all(row['site'] == 'session_mode' and type(row['writes']) is int and row['writes'] >= 0
              for row in writes['normal_sites']), 'hold-normal-writes')


ROOT_FIELDS = {
    'handoff.json': 'binding_ref accepted_clients delivered_clients',
    'source.json': 'source_class items coordinates', 'ordered.json': 'source_class items coordinates',
    'checkpoints/pre.json': 'kind history records', 'reload/pre.json': 'kind history records',
    'checkpoints/post.json': 'kind history records', 'reload/post.json': 'kind history records',
    'ledger.json': 'schema_version members source_digest active_digest lifecycle groups omissions shared_charges',
    'route-manifest.json': 'backend dialect model row provider deployment revision endpoint principal_scope admission execution',
    'binary.json': 'kind executed build_provenance', 'plan.json': 'action history_digest binding body_digest generation send_only splice activation',
    'observation.json': 'complete start_sequence end_sequence barrier counters', 'display.json': 'tui acp host_summary',
    'usage.json': 'responses aggregate logical_terminal_count',
    'authority.json': 'registrations loaded_before loaded_after search_dispatches function_dispatches historical_dispatches',
    'hold.json': 'domain serialized_pins recovered_capability accounting cancel release oversize_human no_resident_overflow D2 parent_persistence parent_cancel human_persistence mode_writes',
}
EVENT_FIELDS = {'producer': 'mechanism source_class raw decoded', 'projection_ack': 'generation checkpoint',
                'send': 'request binding', 'response': 'request response binding',
                'dispatch': 'response call_id name permission historical binding', 'result': 'response request call_id value binding',
                'automatic_next': 'request binding', 'later_user': 'request binding'}
EMPTY_EVENTS = {'pre_reload', 'post_reload', 'plan', 'splice', 'backup', 'projection_write', 'Applied',
                'readiness', 'observation_barrier', 'retained_evidence', 'refusal', 'ordinary_history', 'terminal', 'omission'}


def exact_fields(value, fields):
    check(isinstance(value, dict) and set(value) == set(fields.split()), 'artifact-allowlist')


def atom_fields(fields):
    return dict.fromkeys(fields.split())


def capture_identifier(value):
    check(value is None or isinstance(value, str) or captured_identifier(value), 'artifact-allowlist')


def identifier_fields(fields):
    return {key: capture_identifier if key in ('id', 'call_id', 'item_id', 'response_id', 'tool_use_id', 'tool_call_id') else None
            for key in fields.split()}


def capture_structure(value, shape):
    if shape == 'payload':
        return
    if shape is None:
        check(not isinstance(value, (dict, list)), 'artifact-allowlist')
    elif isinstance(shape, dict):
        check(isinstance(value, dict) and set(value) <= set(shape), 'artifact-allowlist')
        for key, child in value.items():
            capture_structure(child, shape[key])
    elif isinstance(shape, list):
        check(isinstance(value, list), 'artifact-allowlist')
        for child in value:
            capture_structure(child, shape[0])
    elif isinstance(shape, tuple):
        check(isinstance(value, dict) and all(isinstance(key, str) for key in value), 'artifact-allowlist')
        for child in value.values():
            capture_structure(child, shape[0])
    else:
        shape(value)


def capture_coordinate(value):
    if isinstance(value, dict):
        capture_structure(value, atom_fields('item block'))
    else:
        check(type(value) is int, 'artifact-allowlist')


def capture_duplicate(value):
    if isinstance(value, list):
        check(len(value) == 2 and all(type(item) is int for item in value), 'artifact-allowlist')
    else:
        capture_structure(value, {'source': capture_coordinate, 'canonical': capture_coordinate})


def capture_group(value):
    shape = {'key': None, 'members': [capture_coordinate], 'duplicate_links': [capture_duplicate]}
    if isinstance(value, dict) and 'validation' in value:
        shape.update(family=None, execution=None, origin_ref=None, validation=atom_fields('kind join'))
    capture_structure(value, shape)


def capture_allowed_callers(value):
    check(value is None or isinstance(value, list) and all(isinstance(caller, str) for caller in value), 'artifact-allowlist')


def capture_opaque(value):
    if isinstance(value, dict):
        exact_fields(value, 'scheme_prefix sha256 byte_length')
        check(value['scheme_prefix'] in (None, 'litellm_enc:') and type(value['byte_length']) is int
              and value['byte_length'] >= 0 and isinstance(value['sha256'], str)
              and re.fullmatch('[a-f0-9]{64}', value['sha256']), 'artifact-allowlist')
    else:
        check(value is None or isinstance(value, str), 'artifact-allowlist')


def capture_search_content(value):
    # Malformed semantic fixtures remain writable, but metadata never does.
    if isinstance(value, dict):
        check(set(value) <= {'type', 'tool_references', 'error_code', 'error_message'}, 'artifact-allowlist')
        for key, child in value.items():
            if key == 'tool_references':
                references = child if isinstance(child, list) else [child]
                for reference in references:
                    capture_structure(reference, atom_fields('type tool_name'))
            elif key == 'error_message' and isinstance(child, list):
                capture_structure(child, [None])
            else:
                capture_structure(child, None)
    elif isinstance(value, list):
        for child in value:
            capture_search_content(child)
    else:
        capture_structure(value, None)


def capture_tool(value):
    fields = {**atom_fields('type id name description strict defer_loading namespace execution format'),
              'parameters': 'payload', 'allowed_callers': capture_allowed_callers, 'output_schema': 'payload',
              'tools': lambda child: capture_structure(child, [capture_tool]) if child is not None else None,
              'function': lambda child: capture_tool(child)}
    capture_structure(value, fields)


def capture_protocol(value):
    check(isinstance(value, dict), 'artifact-allowlist')
    kind = value.get('type', 'message')
    fields = {
        'message': {**identifier_fields('type id status role phase'), 'content': [capture_protocol]},
        'tool_search_call': {**identifier_fields('type id call_id execution status created_by'), 'arguments': 'payload'},
        'tool_search_output': {**identifier_fields('type id call_id execution status created_by'), 'tools': [capture_tool]},
        'function_call': {**identifier_fields('type id call_id name namespace arguments status caller'),
                          'provider_specific_fields': {'thought_signature': capture_opaque}},
        'function_call_output': {**identifier_fields('type id call_id status'), 'output': 'payload'},
        'server_tool_use': {**identifier_fields('type id name'), 'input': 'payload'},
        'tool_use': {**identifier_fields('type id name'), 'input': 'payload'},
        'tool_result': {**identifier_fields('type tool_use_id is_error'), 'content': 'payload'},
        'tool_search_tool_result': {**identifier_fields('type tool_use_id'), 'content': capture_search_content},
        'tool_search_tool_search_result': {'type': None, 'tool_references': [capture_protocol]},
        'tool_search_tool_result_error': atom_fields('type error_code error_message'),
        'tool_reference': atom_fields('type tool_name'),
        'reasoning': {**identifier_fields('type id status role'), 'encrypted_content': capture_opaque,
                      'summary': [capture_protocol], 'content': [capture_protocol]},
        'compaction': {**identifier_fields('type id'), 'encrypted_content': capture_opaque},
        'thinking': {**atom_fields('type thinking'), 'signature': capture_opaque},
        'compaction_trigger': atom_fields('type'),
        'redacted_thinking': {'type': None, 'data': capture_opaque},
        'additional_tools': {**identifier_fields('type id role'), 'tools': [capture_tool]},
        'image': {'type': None, 'source': atom_fields('type media_type data url')},
    }
    for text_kind in ('text', 'output_text', 'input_text', 'summary_text', 'reasoning_text'):
        fields[text_kind] = {**atom_fields('type text reasoning'), 'annotations': 'payload', 'logprobs': 'payload'}
    check(kind in fields and set(value) <= set(fields[kind]), 'artifact-allowlist')
    for key, child in value.items():
        capture_structure(child, fields[kind][key])


def capture_http_content(value):
    if not isinstance(value, str):
        capture_structure(value, [capture_http_protocol])


def capture_http_protocol(value):
    check(isinstance(value, dict), 'artifact-allowlist')
    if value.get('type', 'message') == 'message':
        capture_structure(value, {**identifier_fields('type id status role phase model_id tool_call_id'),
            'content': capture_http_content,
            'tool_calls': [{**identifier_fields('type id'), 'function': atom_fields('name arguments')}]})
    else:
        if 'cache_control' in value:
            capture_structure(value['cache_control'], atom_fields('type ttl'))
        capture_protocol({key: child for key, child in value.items() if key != 'cache_control'})


def capture_http_tool(value):
    capture_structure(value, {
        **atom_fields('type id name description strict defer_loading namespace max_uses execution format'),
        'parameters': 'payload', 'input_schema': 'payload', 'cache_control': atom_fields('type ttl'),
        'allowed_callers': capture_allowed_callers, 'output_schema': 'payload',
        'function': capture_http_tool,
        'tools': lambda child: capture_structure(child, [capture_http_tool]) if child is not None else None})


def capture_usage(value):
    shape = {**USAGE_SHAPE, **atom_fields('cost cache_creation_input_tokens cache_read_input_tokens'),
             'input_tokens_details': atom_fields('cached_tokens cache_write_tokens audio_tokens text_tokens'),
             'output_tokens_details': atom_fields('reasoning_tokens thinking_tokens audio_tokens text_tokens'),
             'cache_creation': atom_fields('ephemeral_5m_input_tokens ephemeral_1h_input_tokens'),
             'server_tool_use': atom_fields('web_search_requests web_fetch_requests')}
    if value is not None:
        capture_structure(value, {key: None if value.get(key) is None else child for key, child in shape.items()})


def capture_http_body(value):
    shape = {**dict.fromkeys(CAPTURE_FIELDS), 'id': capture_identifier, 'input': capture_http_content,
             'messages': [capture_http_protocol], 'output': [capture_http_protocol],
             'content': capture_http_content, 'system': capture_http_content, 'tools': [capture_http_tool],
             'thinking': atom_fields('type budget_tokens display'), 'reasoning': atom_fields('effort summary context mode generate_summary'),
             'usage': capture_usage, 'error': {**atom_fields('type code message param'),
                                               'provider_specific_fields': atom_fields('error')},
             'incomplete_details': atom_fields('reason'), 'output_config': atom_fields('effort'),
             'context_management': {'edits': [atom_fields('type keep')], 'applied_edits': [atom_fields('type keep')]},
             'stop_details': atom_fields('type category explanation'),
             'container': atom_fields('id expires_at'),
             'provider_specific_fields': {'thought_signatures': [capture_opaque], 'traffic_type': None},
             'stop_sequences': [None], 'include': [None], 'stream_options': atom_fields('include_usage')}
    if isinstance(value.get('tool_choice'), dict):
        shape['tool_choice'] = {**atom_fields('type name mode disable_parallel_tool_use'),
                               'tools': [capture_http_tool], 'function': atom_fields('name')}
    if isinstance(value.get('text'), dict):
        shape['text'] = {'format': {**atom_fields('type name description strict'), 'schema': 'payload'}, 'verbosity': None}
    for key in ('thinking', 'reasoning', 'usage', 'error', 'incomplete_details', 'context_management',
                'container', 'stop_details', 'output_config', 'stream_options'):
        if value.get(key) is None:
            shape[key] = None
    if isinstance(value.get('error'), str):
        shape['error'] = None
    capture_structure(value, shape)


def capture_stream_event(value):
    check(isinstance(value, dict), 'artifact-allowlist')
    kind = value.get('type')
    common = identifier_fields('type model sequence_number response_id')
    error = lambda child: capture_structure(child, {**atom_fields('type code message param'),
                                                    'provider_specific_fields': atom_fields('error')}) if isinstance(child, dict) else capture_structure(child, None)
    if kind in ('response.created', 'response.in_progress', 'response.completed', 'response.incomplete', 'response.failed'):
        shape = {**common, 'response': capture_http_body}
    elif kind in ('response.output_item.added', 'response.output_item.done'):
        shape = {**common, 'output_index': None, 'item': capture_http_protocol}
    elif kind in ('response.content_part.added', 'response.content_part.done',
                  'response.reasoning_summary_part.added', 'response.reasoning_summary_part.done'):
        shape = {**common, **identifier_fields('item_id output_index content_index summary_index status'), 'part': capture_protocol}
    elif kind in ('response.output_text.delta', 'response.output_text.done',
                  'response.reasoning_summary_text.delta', 'response.reasoning_summary_text.done',
                  'response.reasoning_text.delta', 'response.reasoning_text.done',
                  'response.function_call_arguments.delta', 'response.function_call_arguments.done'):
        shape = {**common, **identifier_fields('item_id output_index content_index summary_index delta text arguments name call_id obfuscation'),
                 'logprobs': 'payload'}
    elif kind == 'message_start':
        shape = {**common, 'message': capture_http_body}
    elif kind == 'message_delta':
        shape = {**common, 'delta': {**atom_fields('stop_reason stop_sequence'), 'container': atom_fields('id expires_at'),
                                    'stop_details': atom_fields('type category explanation')},
                 'usage': capture_usage, 'context_management': {'applied_edits': [atom_fields('type keep')]}}
        delta = value.get('delta', {})
        shape['delta'] = {key: None if delta.get(key) is None else child for key, child in shape['delta'].items()}
    elif kind == 'content_block_start':
        shape = {**common, 'index': None, 'content_block': capture_http_protocol}
    elif kind == 'content_block_delta':
        delta_shape = atom_fields('type text partial_json thinking')
        if value.get('delta', {}).get('type') == 'signature_delta':
            delta_shape.update(signature=capture_opaque, hp8r_signature_prefix=capture_opaque)
        shape = {**common, 'index': None, 'delta': delta_shape}
    elif kind == 'content_block_stop':
        shape = {**common, 'index': None}
    elif kind in ('message_stop', 'ping', 'keepalive', '[DONE]'):
        shape = common
    elif kind == 'error' or kind is None and 'error' in value:
        shape = {**common, **atom_fields('code message param'), 'error': error}
    else:
        raise ReceiptError('event-allowlist')
    capture_structure(value, shape)


WAITING_SHAPE = {**identifier_fields('call_id delivery'), 'source': capture_coordinate, 'result': capture_coordinate}
LIFECYCLE_SHAPE = {'exchange': None, 'groups': (None,), 'waiting_clients': [WAITING_SHAPE],
                   'terminal': lambda value: capture_structure(value, atom_fields('outcome')) if value is not None else None}
COUNTS_SHAPE = atom_fields('KEEP F STRIP OMIT')
USAGE_SHAPE = atom_fields('input_tokens output_tokens total_tokens')
CHECKPOINT_SHAPE = {'kind': None, 'history': [capture_protocol], 'records': {
    **atom_fields('schema_version policy_revision reader_floor limits_revision'),
    'lifecycle': LIFECYCLE_SHAPE, 'groups': [capture_group],
    'projection': atom_fields('generation high_water disposition')}}
HOLD_ACCOUNTING_SHAPE = {**atom_fields('charged_bytes shared_other_bytes'), 'records': {
    'header': {**atom_fields('binding pin_owner'), 'steer_stamps': [atom_fields('slot message_id')]},
    'entries': [{**atom_fields('occurrence source storage'), 'payload': 'payload'}],
    'resident_payloads': [{**atom_fields('occurrence'), 'payload': 'payload'}],
    'prepared_copies': [{**atom_fields('occurrence'), 'payload': 'payload'}],
    'reserve': {'hold_refusal': atom_fields('reason'), 'terminal': atom_fields('accepted outcome'),
                'usage': atom_fields('accepted'), 'omission': atom_fields('accepted')}}}
HOLD_SHAPE = {**atom_fields('domain recovered_capability'), 'serialized_pins': [None],
    'accounting': HOLD_ACCOUNTING_SHAPE,
    'cancel': {**{key: [None] for key in ('accepted_humans_before', 'accepted_humans_after', 'residents_before',
               'residents_after', 'parent_before', 'parent_settled', 'parent_after')}, 'autostarted': None},
    'release': {**{key: [None] for key in ('accepted', 'eligible_prefix', 'pending_calls', 'missing_monitors',
                'delivered_prefix', 'cutoff_deferred', 'restored_suffix', 'later_delivered')},
                'suppressions': [atom_fields('occurrence reason')],
                **atom_fields('first_eligible_safe_point actual_safe_point next_safe_point next_delivered_safe_point fresh_prompt_required next_admitted_turn after_prompt')},
    'oversize_human': atom_fields('held_status unheld_prepared held_cap_reapplied'),
    'no_resident_overflow': {**atom_fields('source outcome'), 'accepted_prefix': [None], 'retained_prefix': [None]},
    'D2': {**atom_fields('admitted unpinned_exposure accepted_entry_success diagnostic'), 'evicted': [None]},
    'parent_persistence': [atom_fields('binding slot persisted submitted latch_active')],
    'parent_cancel': atom_fields('settled_binding next_binding'),
    'human_persistence': [atom_fields('occurrence persisted submitted')],
    'mode_writes': {**atom_fields('hold_writes release_writes'), 'normal_sites': [atom_fields('site writes')]}}


RECORD_SHAPES = {
    'source.json': {'source_class': None, 'items': [capture_protocol], 'coordinates': [atom_fields('lineage occurrence')]},
    'ledger.json': {**atom_fields('schema_version source_digest active_digest'),
        'members': [atom_fields('source action output scope reason')], 'lifecycle': LIFECYCLE_SHAPE,
        'groups': [capture_group], 'omissions': [None], 'shared_charges': atom_fields('source active ledger')},
    'route-manifest.json': {**atom_fields(ROOT_FIELDS['route-manifest.json']), 'endpoint': atom_fields('scheme authority path')},
    'binary.json': atom_fields(ROOT_FIELDS['binary.json']),
    'plan.json': {**atom_fields(ROOT_FIELDS['plan.json']), 'splice': atom_fields('dialect selected')},
    'observation.json': {**atom_fields(ROOT_FIELDS['observation.json']),
        'counters': atom_fields('projection_write projection_ack Applied dispatch send')},
    'display.json': {'tui': COUNTS_SHAPE, 'acp': COUNTS_SHAPE, 'host_summary': {
        'counts': COUNTS_SHAPE, 'reasons': [atom_fields('category count')], 'rediscovery': None}},
    'usage.json': {'responses': [{'attempt': None, 'usage': USAGE_SHAPE}],
        'aggregate': USAGE_SHAPE, 'logical_terminal_count': None},
    'authority.json': {**atom_fields('search_dispatches function_dispatches historical_dispatches'),
        'registrations': [{**atom_fields('namespace name schema_digest eligible'), 'schema': 'payload'}],
        'loaded_before': [None], 'loaded_after': [None]},
    'handoff.json': {'binding_ref': None, 'accepted_clients': [{key: value for key, value in WAITING_SHAPE.items() if key != 'delivery'}],
        'delivered_clients': [None]},
    'hold.json': HOLD_SHAPE,
}
RECORD_SHAPES['ordered.json'] = RECORD_SHAPES['source.json']
for _checkpoint in ('checkpoints/pre.json', 'checkpoints/post.json', 'reload/pre.json', 'reload/post.json'):
    RECORD_SHAPES[_checkpoint] = CHECKPOINT_SHAPE


def capture_receipt(value, signed=False):
    shape = {**atom_fields(RECEIPT_INPUT_FIELDS), 'H': [None], 'failed_paths': [None],
             'reader_versions': dict.fromkeys(READERS)}
    if signed:
        shape.update(sha256=None, artifacts=[atom_fields('path sha256')])
    capture_structure(value, shape)


def artifact_shape(name, value):
    if name == 'receipt.json':
        exact_fields(value, RECEIPT_INPUT_FIELDS + ' artifacts sha256')
    elif name == 'verdict.json':
        fields = 'verdict tooling_only'
        if 'binary_executed' in value:
            fields += ' case layer reason binary_executed'
        exact_fields(value, fields)
    elif name in ROOT_FIELDS:
        exact_fields(value, ROOT_FIELDS[name])
    elif re.fullmatch(r'wire/req-[0-9]{3}\.json', name):
        exact_fields(value, 'model store stream input tools')
    elif re.fullmatch(r'wire/resp-[0-9]{3}\.json', name):
        exact_fields(value, 'id object status output usage')
    elif name in ('bodies.json', 'attempts.jsonl', 'events.jsonl'):
        check(isinstance(value, list), 'artifact-allowlist')
        for row in value:
            if name == 'events.jsonl':
                kind = row.get('type') if isinstance(row, dict) else None
                check(kind in EVENT_FIELDS or kind in EMPTY_EVENTS, 'artifact-allowlist')
                exact_fields(row, 'sequence type ' + EVENT_FIELDS.get(kind, ''))
            else:
                exact_fields(row, 'request response' if name == 'bodies.json' else 'number request reason status continuation request_id')
    else:
        raise ReceiptError('artifact-allowlist')
    if name in ('checkpoints/pre.json', 'checkpoints/post.json', 'reload/pre.json', 'reload/post.json'):
        records = value['records']
        exact_fields(records, 'schema_version policy_revision reader_floor limits_revision lifecycle groups projection')
        exact_fields(records['lifecycle'], 'exchange groups waiting_clients terminal')
        exact_fields(records['projection'], 'generation high_water disposition')
        for group in records['groups']:
            fields = 'key members duplicate_links'
            if 'validation' in group:
                fields += ' family execution origin_ref validation'
            exact_fields(group, fields)
    if name == 'source.json' or name == 'ordered.json':
        for coordinate in value['coordinates']:
            exact_fields(coordinate, 'lineage occurrence')
    if name == 'display.json':
        exact_fields(value['host_summary'], 'counts reasons rediscovery')
    if name in RECORD_SHAPES:
        capture_structure(value, RECORD_SHAPES[name])
    elif name == 'receipt.json':
        capture_receipt(value, signed=True)
    elif re.fullmatch(r'wire/req-[0-9]{3}\.json', name):
        capture_structure(value, {**atom_fields('model store stream'), 'input': [capture_protocol], 'tools': [capture_tool]})
    elif re.fullmatch(r'wire/resp-[0-9]{3}\.json', name):
        capture_structure(value, {**identifier_fields('id object status'), 'output': [capture_protocol], 'usage': USAGE_SHAPE})
    elif name in ('bodies.json', 'attempts.jsonl', 'events.jsonl'):
        for row in value:
            capture_structure(row, {key: capture_identifier if key == 'call_id' else None for key in row})
    elif name == 'verdict.json':
        capture_structure(value, dict.fromkeys(value))


RECEIPT_INPUT_FIELDS = ('schema_version utc case arm layer shape action rat_clause H manifest_digest reader_versions '
                        'assertion_version corpus_version policy_version source_class product_credit verdict failed_paths reason')


def prepare_report(receipt, artifacts):
    exact_fields(receipt, RECEIPT_INPUT_FIELDS)
    check(receipt['schema_version'] == 'hp8r-receipt/2', 'receipt-version')
    validate_receipt_utc(receipt['utc'])
    capture_receipt(receipt)
    safe(receipt)
    prepared = prepare_artifacts(artifacts)
    signed = dict(receipt)
    signed['artifacts'] = [{'path': name, 'sha256': sha(raw)} for name, raw in prepared]
    signed['sha256'] = digest(signed)
    captured = {name: [json_bytes(line) for line in raw.splitlines()] if name.endswith('.jsonl') else json_bytes(raw)
                for name, raw in prepared}
    return {**captured, 'receipt.json': signed,
            'verdict.json': {'verdict': receipt['verdict'], 'tooling_only': True}}


def prepare_artifacts(artifacts):
    prepared = []
    for name, value in artifacts.items():
        check(isinstance(name, str) and not Path(name).is_absolute() and '..' not in Path(name).parts, 'artifact-path')
        masked = map_opaque(value, False)
        artifact_shape(name, masked)
        safe(masked)
    for name, value in artifacts.items():
        captured = map_opaque(value, True)
        raw = b''.join(canonical(item) + b'\n' for item in captured) if name.endswith('.jsonl') else canonical(captured)
        check(len(raw) <= MAX_BODY, 'capture-cap')
        prepared.append((name, raw))
    check(len(prepared) <= 128 and sum(len(raw) for _, raw in prepared) <= 32 * 1024 * 1024, 'capture-cap')
    return prepared


def write_screened_artifacts(root, artifacts):
    root = Path(root)
    check(not root.exists(), 'immutable-output')
    prepared = prepare_artifacts(artifacts)
    root.mkdir(parents=True, mode=0o700)
    inventory = []
    for name, raw in prepared:
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open('xb') as stream:
            stream.write(raw)
        inventory.append({'path': name, 'sha256': sha(raw)})
    return inventory


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        manifest = json_bytes(args.manifest.read_bytes())
        validate_corpus(manifest, args.manifest.parent)
        verdict = validate_report(manifest, args.report)
        print(json.dumps({'valid': True, 'verdict': verdict, 'tooling_only': True}))
        return 0 if verdict == 'PASS' else 1
    except (ReceiptError, ProtocolError, KeyError, TypeError, IndexError, OSError, ValueError, AttributeError, RecursionError) as error:
        reason = str(error) if isinstance(error, ReceiptError) else type(error).__name__
        print(json.dumps({'valid': False, 'verdict': 'FAIL', 'reason': reason, 'tooling_only': True}))
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
