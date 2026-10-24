"""Synthetic receipt acceptance is tooling credit, never product qualification."""
import copy
import json
import os
import tempfile
from pathlib import Path
import unittest

from smoke.redteam.hp8r import validate

CORPUS = Path(__file__).parent / 'corpus'


def manifest():
    return json.loads((CORPUS / 'manifest.json').read_text())


def binary_fixture(root):
    executable = Path(root) / 'synthetic-binary'
    executable.write_bytes(b'#!/bin/sh\nexit 0\n')
    executable.chmod(0o700)
    return {'path': str(executable), 'sha256': validate.sha(executable.read_bytes()),
            'version': 'synthetic-not-executed', 'features': ['offline-fixture'],
            'source_commit': '2fcd97294a23cfb19b1d6e4c1a788840a17dc569',
            'build': {'phase': 'build-time', 'fingerprint_before': 'b' * 64,
                      'fingerprint_after': 'b' * 64, 'untracked_fixtures': {},
                      'command': ['fixture-only-not-a-build'], 'exit': 0}}


def fixture(case_id='tooling-f'):
    m = manifest()
    case = m['cases'][case_id]
    control = case['control']
    history = copy.deepcopy(m['synthetic_sources'][case['source_id']]['items'])
    source = {'source_class': 'synthetic-mutation', 'items': history,
              'coordinates': [{'lineage': 'fixture-lineage', 'occurrence': i} for i in range(len(history))]}
    records = {'schema_version': 1, 'policy_revision': 1, 'reader_floor': 1, 'limits_revision': 1,
               'lifecycle': {'exchange': 'Terminal', 'groups': {'group-0': 'Completed'},
                             'waiting_clients': [], 'terminal': {'outcome': 'completed'}},
               'groups': copy.deepcopy(m['synthetic_sources'][case['source_id']]['groups']),
               'projection': {'generation': 1, 'high_water': 1, 'disposition': 'KEEP'}}
    records['lifecycle']['groups'] = {group['key']: 'Completed' for group in records['groups']}
    if control == 'omission':
        records['lifecycle'] = {'exchange': 'Terminal', 'groups': {'group-0': 'AbandonedPending'},
                                'waiting_clients': [], 'terminal': {'outcome': 'cancelled'}}
    pre = {'kind': 'synthetic-checkpoint-observation', 'history': history, 'records': records}
    post = copy.deepcopy(pre)
    outgoing = copy.deepcopy(history)
    if case['action'] == 'F':
        outgoing[0] = {'type': 'message', 'role': 'assistant', 'content': [{'type': 'output_text', 'text': 'Historical discovery request; no current authority.'}]}
        outgoing[-1] = {'type': 'message', 'role': 'assistant', 'content': [{'type': 'output_text', 'text': 'Historical discovery completed; no current authority.'}]}
    elif control == 'omission':
        outgoing = history[1:]
    if control == 'positive':
        post['history'] = copy.deepcopy(outgoing)
        post['records']['projection'] = {'generation': 2, 'high_water': 2, 'disposition': 'F'}
    members = []
    for index in range(len(history)):
        action = 'F' if case['action'] == 'F' and index in (0, len(history) - 1) else 'OMIT' if control == 'omission' and index == 0 else 'KEEP'
        members.append({'source': index, 'action': action,
                        'output': None if action == 'OMIT' else index - (1 if control == 'omission' else 0),
                        'scope': 'durable' if control == 'positive' else 'request', 'reason': 'fixture-policy'})
    ledger = {'schema_version': 1, 'members': members,
              'source_digest': validate.digest(source), 'active_digest': validate.digest(post['history']),
              'lifecycle': copy.deepcopy(post['records']['lifecycle']), 'groups': copy.deepcopy(post['records']['groups']),
              'omissions': [0] if control == 'omission' else [], 'shared_charges': {'source': 0, 'active': 0, 'ledger': 0}}
    artifacts = {'source.json': source, 'ordered.json': copy.deepcopy(source),
                 'checkpoints/pre.json': pre, 'reload/pre.json': copy.deepcopy(pre),
                 'checkpoints/post.json': post, 'reload/post.json': copy.deepcopy(post),
                 'ledger.json': ledger, 'route-manifest.json': copy.deepcopy(case['route']),
                 'binary.json': {'kind': 'synthetic-mutation', 'executed': False, 'build_provenance': None}}
    body = {'model': case['route']['model'], 'store': False, 'stream': True,
            'input': outgoing, 'tools': [{'type': 'function', 'name': 'read_fixture_0',
            'parameters': {'type': 'object', 'properties': {}, 'required': [], 'additionalProperties': False}, 'strict': True}]}
    bodies, attempts = [], []
    sends = 0 if control in ('refusal', 'live-reload') else 3 if control in ('positive', 'no-change', 'ordinary') else 1
    for index in range(sends):
        request = copy.deepcopy(body)
        if index == 1:
            request['input'] = outgoing + [{'type': 'function_call', 'call_id': 'fresh-call', 'name': 'read_fixture_0', 'arguments': '{}'},
                                           {'type': 'function_call_output', 'call_id': 'fresh-call', 'output': 'fixture-read-result'}]
        elif index == 2:
            request['input'] = artifacts['wire/req-002.json']['input'] + artifacts['wire/resp-002.json']['output'] + [
                {'type': 'message', 'role': 'user', 'content': [{'type': 'input_text', 'text': 'later input'}]}]
        path = f'wire/req-{index + 1:03d}.json'
        artifacts[path] = request
        response = {'id': f'resp-{index + 1}', 'object': 'response', 'status': 'completed',
                    'output': [{'type': 'function_call', 'call_id': 'fresh-call', 'name': 'read_fixture_0', 'arguments': '{}'}] if index == 0 and sends == 3 else [],
                    'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}}
        if index == 1 and sends == 3:
            response['output'] = [{'type': 'message', 'id': 'msg_fixture_result', 'role': 'assistant', 'status': 'completed',
                                   'content': [{'type': 'output_text', 'text': 'The fixture read completed.', 'annotations': []}]}]
        artifacts[f'wire/resp-{index + 1:03d}.json'] = response
        bodies.append({'request': path, 'response': f'wire/resp-{index + 1:03d}.json'})
        attempts.append({'number': index + 1, 'request': path, 'reason': 'initial', 'status': 200,
                         'continuation': 0, 'request_id': f'logical-{index + 1}'})
    events = []
    for kind in case['required_events']:
        event = {'sequence': len(events), 'type': kind}
        if kind in ('send', 'response', 'dispatch', 'result', 'automatic_next', 'later_user'):
            event['binding'] = case['route']['model']
        if kind == 'producer':
            event.update(mechanism='synthetic-contract-chain', source_class='synthetic-mutation', raw='source.json', decoded='ordered.json')
        if kind == 'projection_ack':
            event.update(generation=2, checkpoint='checkpoints/post.json')
        if kind == 'send':
            event.update(request='wire/req-001.json')
        if kind == 'response':
            event.update(request='wire/req-001.json', response='wire/resp-001.json')
        if kind == 'dispatch':
            event.update(response='wire/resp-001.json', call_id='fresh-call', name='read_fixture_0', permission='allow', historical=False)
        if kind == 'result':
            event.update(response='wire/resp-001.json', request='wire/req-002.json', call_id='fresh-call', value='fixture-read-result')
        if kind in ('automatic_next', 'later_user'):
            event.update(request='wire/req-002.json' if kind == 'automatic_next' else 'wire/req-003.json')
        events.append(event)
    plan = {'action': case['action'], 'history_digest': validate.digest(pre['history']),
            'binding': case['route']['model'], 'body_digest': validate.digest(body) if sends else None,
            'generation': post['records']['projection']['generation'], 'send_only': control != 'positive',
            'splice': {'dialect': case['route']['dialect'], 'selected': case['route']['dialect'] == 'Codex'}, 'activation': 'synthetic-selected'}
    counters = {kind: sum(e['type'] == kind for e in events) for kind in ('projection_write', 'projection_ack', 'Applied', 'dispatch')}
    counters['send'] = sends
    counts = {action: sum(member['action'] == action for member in members) for action in ('KEEP', 'F', 'STRIP', 'OMIT')}
    display = {'tui': dict(counts), 'acp': dict(counts),
               'host_summary': {'counts': dict(counts), 'reasons': [{'category': 'fixture-policy', 'count': len(members)}],
                                'rediscovery': case['action'] == 'F'}}
    artifacts.update({'plan.json': plan, 'bodies.json': bodies, 'attempts.jsonl': attempts, 'events.jsonl': events,
                      'observation.json': {'complete': True, 'start_sequence': 0, 'end_sequence': len(events) - 1,
                                           'barrier': 'observation_barrier', 'counters': counters},
                      'display.json': display})
    receipt = {'schema_version': 'hp8r-receipt/2', 'utc': '2026-10-21T00:00:00Z', 'case': case_id, 'arm': 'synthetic',
               'layer': 'L0', 'shape': case['shape'], 'action': case['action'], 'rat_clause': case['rat_clause'],
               'H': case['H'], 'manifest_digest': validate.digest(m), 'reader_versions': m['reader_versions'],
               'assertion_version': m['assertion_version'], 'corpus_version': m['corpus_version'],
               'policy_version': m['policy_version'], 'source_class': 'synthetic-mutation', 'product_credit': False,
               'verdict': 'PASS', 'failed_paths': [], 'reason': 'synthetic tooling acceptance only'}
    usage = {'responses': [{'attempt': i + 1, 'usage': copy.deepcopy(artifacts[body['response']]['usage'])}
                           for i, body in enumerate(bodies)],
             'aggregate': {'input_tokens': sends, 'output_tokens': sends, 'total_tokens': sends * 2},
             'logical_terminal_count': 1}
    schema = {'type': 'object', 'properties': {}, 'required': [], 'additionalProperties': False}
    authority = {'registrations': [{'namespace': 'fixture_builtin', 'name': 'read_fixture_' + str(i),
                  'schema': schema, 'schema_digest': validate.digest(schema), 'eligible': True} for i in range(6)],
                 'loaded_before': ['read_fixture_' + str(i) for i in range(6)],
                 'loaded_after': [] if case['action'] in ('F', 'OMIT') else ['read_fixture_' + str(i) for i in range(6)],
                 'search_dispatches': 0, 'function_dispatches': 1 if sends == 3 else 0, 'historical_dispatches': 0}
    artifacts['usage.json'] = usage
    artifacts['authority.json'] = authority
    if control == 'live-reload':
        finish_live_fixture(artifacts)
    return receipt, artifacts


def set_live_charges(artifacts):
    ledger = artifacts['ledger.json']
    ledger['shared_charges'] = {'source': len(validate.canonical(artifacts['source.json'])),
                                'active': len(validate.canonical(artifacts['checkpoints/post.json'])), 'ledger': 0}
    while ledger['shared_charges']['ledger'] != len(validate.canonical(ledger)):
        ledger['shared_charges']['ledger'] = len(validate.canonical(ledger))


def finish_live_fixture(artifacts):
    waiting = [
        {'call_id': 'client-a', 'source': {'item': 0, 'block': 3}, 'result': {'item': 2, 'block': 0}, 'delivery': 'accepted-pending-send'},
        {'call_id': 'client-b', 'source': {'item': 0, 'block': 5}, 'result': {'item': 2, 'block': 1}, 'delivery': 'accepted-pending-send'}]
    lifecycle = {'exchange': 'LiveBound', 'groups': {'group-0': 'Completed', 'group-1': 'LivePending'},
                 'waiting_clients': waiting, 'terminal': None}
    for name in ('checkpoints/pre.json', 'checkpoints/post.json', 'reload/pre.json', 'reload/post.json'):
        artifacts[name]['records']['lifecycle'] = copy.deepcopy(lifecycle)
    artifacts['ledger.json']['lifecycle'] = copy.deepcopy(lifecycle)
    artifacts['handoff.json'] = {'binding_ref': 'fixture-binding-A',
        'accepted_clients': [{key: copy.deepcopy(row[key]) for key in ('call_id', 'source', 'result')} for row in waiting],
        'delivered_clients': []}
    artifacts['usage.json']['logical_terminal_count'] = 0
    set_live_charges(artifacts)


def write_fixture(root, receipt, artifacts, *, corrupt_hash=None):
    bundle = validate.prepare_report(copy.deepcopy(receipt), artifacts)
    signed = bundle['receipt.json']
    if corrupt_hash == 'wrong_artifact_hash':
        signed['artifacts'][0]['sha256'] = '0' * 64
        signed['sha256'] = validate.digest({key: value for key, value in signed.items() if key != 'sha256'})
    elif corrupt_hash == 'wrong_receipt_hash':
        signed['sha256'] = '0' * 64
    elif corrupt_hash is not None:
        raise ValueError('unknown synthetic hash fault')
    validate.write_screened_artifacts(root, bundle)
    return Path(root)


def hold_accounting(entries=None, copies=None, shared_other=0):
    entries = entries if entries is not None else [{'occurrence': 1, 'source': 'human', 'storage': 'custody', 'payload': {'text': 'accepted'}}]
    records = {'header': {'binding': 'fixture-binding', 'pin_owner': 'actor-A', 'steer_stamps': []},
               'entries': entries, 'resident_payloads': [], 'prepared_copies': copies or [],
               'reserve': {'hold_refusal': {'reason': 'capacity'},
                           'terminal': {'accepted': len(entries), 'outcome': 'incomplete'},
                           'usage': {'accepted': len(entries)}, 'omission': {'accepted': len(entries)}}}
    return {'records': records, 'charged_bytes': len(validate.canonical(records)), 'shared_other_bytes': shared_other}


def hold_fixture():
    return {'domain': 'ephemeral-observation', 'serialized_pins': [], 'recovered_capability': False,
            'accounting': hold_accounting(),
            'cancel': {'accepted_humans_before': [101], 'accepted_humans_after': [101],
                       'residents_before': [102], 'residents_after': [],
                       'parent_before': [201], 'parent_settled': [201], 'parent_after': [], 'autostarted': False},
            'release': {'accepted': [1, 2, 3, 4], 'eligible_prefix': [1, 2, 3], 'pending_calls': [],
                        'missing_monitors': [3], 'suppressions': [{'occurrence': 3, 'reason': 'drop-monitor'}],
                        'delivered_prefix': [1, 2], 'cutoff_deferred': [4], 'restored_suffix': [4],
                        'later_delivered': [4], 'first_eligible_safe_point': 1, 'actual_safe_point': 1,
                        'next_safe_point': 2, 'next_delivered_safe_point': 2, 'fresh_prompt_required': False,
                        'next_admitted_turn': True, 'after_prompt': True},
            'oversize_human': {'held_status': 'resident-backpressure', 'unheld_prepared': True, 'held_cap_reapplied': False},
            'no_resident_overflow': {'source': 'deferred-followup', 'outcome': 'typed-incomplete',
                                     'accepted_prefix': [1, 2], 'retained_prefix': [1, 2]},
            'D2': {'admitted': False, 'unpinned_exposure': 50, 'evicted': [301],
                   'accepted_entry_success': False, 'diagnostic': 'unadmitted-eviction'},
            'parent_persistence': [
                {'binding': 'owner-A', 'slot': 201, 'persisted': True, 'submitted': False, 'latch_active': True},
                {'binding': 'owner-A', 'slot': 201, 'persisted': False, 'submitted': True, 'latch_active': False},
                {'binding': 'owner-B', 'slot': 202, 'persisted': True, 'submitted': True, 'latch_active': False}],
            'parent_cancel': {'settled_binding': 'owner-A', 'next_binding': 'owner-B'},
            'human_persistence': [{'occurrence': 101, 'persisted': True, 'submitted': True},
                                  {'occurrence': 102, 'persisted': False, 'submitted': False}],
            'mode_writes': {'normal_sites': [{'site': 'session_mode', 'writes': 1}], 'hold_writes': 0, 'release_writes': 0}}


HOLD_REJECTIONS = {
    'durable_domain': (['domain'], 'durable-checkpoint'),
    'serialized_pins': (['serialized_pins'], [1]),
    'recovered_pins': (['recovered_capability'], True),
    'cancel_loses_custody': (['cancel', 'accepted_humans_after'], []),
    'cancel_autostarts': (['cancel', 'autostarted'], True),
    'parent_wrong_retention': (['cancel', 'parent_after'], [201]),
    'keyed_suffix_lost': (['release', 'restored_suffix'], []),
    'cutoff_suffix_stranded': (['release', 'later_delivered'], []),
    'held_cap_reapplied': (['oversize_human', 'held_cap_reapplied'], True),
    'prefix_claimed_complete': (['no_resident_overflow', 'outcome'], 'complete'),
    'prefix_lost': (['no_resident_overflow', 'retained_prefix'], []),
    'D2_false_success': (['D2', 'accepted_entry_success'], True),
    'additional_hold_write': (['mode_writes', 'hold_writes'], 1),
    'additional_release_write': (['mode_writes', 'release_writes'], 1),
    'parent_retry_repersisted': (['parent_persistence', 1, 'persisted'], True),
    'parent_cancel_reopens_owner': (['parent_cancel', 'next_binding'], 'owner-A'),
    'human_persisted_before_submit': (['human_persistence', 1, 'persisted'], True),
    'release_while_pending': (['release', 'pending_calls'], ['server-call']),
    'release_needs_prompt': (['release', 'fresh_prompt_required'], True),
    'release_late_safe_point': (['release', 'actual_safe_point'], 2),
    'duplicate_delivery': (['release', 'delivered_prefix'], [1, 1]),
    'reordered_delivery': (['release', 'delivered_prefix'], [2, 1]),
    'unexplained_monitor_loss': (['release', 'suppressions'], []),
}


def bad_hold(name):
    value = hold_fixture()
    path, replacement = HOLD_REJECTIONS[name]
    obj = value
    for key in path[:-1]:
        obj = obj[key]
    obj[path[-1]] = copy.deepcopy(replacement)
    return value


class ReceiptContractTests(unittest.TestCase):
    def test_corpus_checks_source_hash_and_raw_facts(self):
        original = manifest()
        validate.validate_corpus(original, CORPUS)
        bad = copy.deepcopy(original)
        bad['sources'][0]['sha256'] = '0' * 64
        with self.assertRaisesRegex(validate.ReceiptError, 'source-hash'):
            validate.validate_corpus(bad, CORPUS)

    def test_terminal_capture_cannot_be_relabelled_as_live_pause(self):
        bad = manifest()
        bad['sources'][0]['facts']['stop_reasons'] = ['pause_turn']
        with self.assertRaisesRegex(validate.ReceiptError, 'source-facts'):
            validate.validate_corpus(bad, CORPUS)

    def test_corpus_classification_transform_reader_and_normalization_are_frozen(self):
        for change in ('class', 'reader', 'normalization', 'transform', 'credit'):
            bad = manifest()
            if change == 'class':
                bad['sources'][0]['source_class'] = 'live-product-pass'
            elif change == 'reader':
                del bad['reader_versions']['ordered']
            elif change == 'normalization':
                bad['normalization'] = ['rewrite-xwire-ids']
            elif change == 'transform':
                del bad['sources'][0]['transformations']
            else:
                bad['sources'][0]['product_credit'] = True
            with self.subTest(change=change):
                with self.assertRaises(validate.ReceiptError):
                    validate.validate_corpus(bad, CORPUS)

    def test_native_old_arm_is_manifest_only_na_without_any_receipt(self):
        case = manifest()['cases']['tooling-f']
        with self.assertRaisesRegex(validate.ReceiptError, 'na-receipt'):
            validate.validate_applicability(case, {'arm': 'dac96d87', 'verdict': 'BLOCKED'})
        self.assertEqual(validate.required_arms(case), ['synthetic'])

    def test_compatible_old_arm_needs_demonstrated_seam(self):
        case = manifest()['cases']['tooling-ordinary']
        case['arms']['dac96d87'] = {'applicability': 'APPLICABLE', 'kind': 'real-binary'}
        with self.assertRaisesRegex(validate.ReceiptError, 'seam-reachability'):
            validate.validate_applicability(case, {'arm': 'dac96d87', 'verdict': 'PASS'})

    def test_fail_to_fail_is_preserved_and_setup_failure_is_not_kill(self):
        before = {'verdict': 'FAIL', 'failure_kind': 'assertion', 'predicate': 'body-order'}
        after = {'verdict': 'FAIL', 'failure_kind': 'assertion', 'predicate': 'body-order'}
        self.assertEqual(validate.differential(before, after), {'before': 'FAIL', 'after': 'FAIL', 'behavioral_kill': False})
        after['verdict'] = 'PASS'
        for failure in ('decode', 'setup', 'compile'):
            before['failure_kind'] = failure
            self.assertFalse(validate.differential(before, after)['behavioral_kill'])
        before['failure_kind'] = 'assertion'
        self.assertTrue(validate.differential(before, after)['behavioral_kill'])


    def test_binary_requires_build_time_not_current_checkout_fingerprint(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            binary = binary_fixture(temp)
            validate.validate_binary(binary)
            binary['build']['phase'] = 'current-checkout'
            with self.assertRaisesRegex(validate.ReceiptError, 'build-time'):
                validate.validate_binary(binary)

    def test_binary_digest_path_and_build_equality_are_measured(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            original = binary_fixture(temp)
            for change in ('digest', 'path', 'build'):
                binary = copy.deepcopy(original)
                if change == 'digest':
                    binary['sha256'] = '0' * 64
                elif change == 'path':
                    binary['path'] = 'synthetic-binary'
                else:
                    binary['build']['fingerprint_after'] = 'c' * 64
                with self.subTest(change=change):
                    with self.assertRaises(validate.ReceiptError):
                        validate.validate_binary(binary)

    def test_missing_binary_version_features_commit_or_fixture_inventory_blocks(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            original = binary_fixture(temp)
            for key in ('version', 'features', 'source_commit', 'untracked_fixtures'):
                binary = copy.deepcopy(original)
                if key == 'untracked_fixtures':
                    del binary['build'][key]
                else:
                    del binary[key]
                with self.subTest(key=key):
                    with self.assertRaises(validate.ReceiptError):
                        validate.validate_binary(binary)


    def test_receipt_rejects_missing_action_producer_even_after_resigning(self):
        receipt, artifacts = fixture()
        artifacts['events.jsonl'] = [event for event in artifacts['events.jsonl'] if event['type'] != 'producer']
        for i, event in enumerate(artifacts['events.jsonl']):
            event['sequence'] = i
        artifacts['observation.json']['end_sequence'] -= 1
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            root = write_fixture(Path(temp) / 'bad', receipt, artifacts)
            with self.assertRaises(validate.ReceiptError):
                validate.validate_report(manifest(), root)

    def test_complete_positive_auxiliary_refusal_nochange_ordinary_omission_chains(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            for case in manifest()['cases']:
                with self.subTest(case=case):
                    receipt, artifacts = fixture(case)
                    root = write_fixture(Path(temp) / case, receipt, artifacts)
                    self.assertEqual(validate.validate_report(manifest(), root), 'PASS')



    def test_accepted_option_K_custody_cannot_disappear_on_cancel(self):
        value = hold_fixture()
        value['cancel']['accepted_humans_after'] = []
        with self.assertRaises(validate.ReceiptError):
            validate.validate_hold(value)

    def test_hold_positive_distinguishes_suppression_cutoff_parent_and_normal_writes(self):
        validate.validate_hold(hold_fixture())
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            receipt, artifacts = fixture('tooling-keep')
            artifacts['hold.json'] = hold_fixture()
            root = write_fixture(Path(temp) / 'hold-positive', receipt, artifacts)
            self.assertEqual(validate.validate_report(manifest(), root), 'PASS')



REJECTIONS = {
    'wrong_manifest': ('tooling-f', 'r', ['manifest_digest'], '0' * 64),
    'wrong_route': ('tooling-f', 'a', ['route-manifest.json', 'deployment'], 'other'),
    'wrong_binary': ('tooling-f', 'a', ['binary.json', 'executed'], True),
    'wrong_reader': ('tooling-f', 'r', ['reader_versions', 'ordered'], 2),
    'missing_H': ('tooling-f', 'r', ['H'], []),
    'wrong_layer': ('tooling-f', 'r', ['layer'], 'L1'),
    'wrong_shape': ('tooling-f', 'r', ['shape'], 'different-shape'),
    'wrong_action': ('tooling-f', 'r', ['action'], 'KEEP'),
    'na_receipt': ('tooling-f', 'r', ['arm'], 'dac96d87'),
    'no_activation': ('tooling-f', 'a', ['plan.json', 'activation'], None),
    'stale_plan': ('tooling-f', 'a', ['plan.json', 'history_digest'], '0' * 64),
    'splice_bypass': ('tooling-f', 'a', ['plan.json', 'splice', 'selected'], False),
    'optional_id': ('tooling-keep', 'a', ['ordered.json', 'items', 2, 'id'], 'invented-output-id'),
    'required_join': ('tooling-keep', 'a', ['ordered.json', 'items', 2, 'call_id'], 'invented-join'),
    'missing_KEEP': ('tooling-keep', 'a', ['ledger.json', 'members', 0, 'action'], 'F'),
    'missing_omission': ('tooling-omit', 'a', ['ledger.json', 'omissions'], []),
    'reload_waiting_link': ('tooling-f', 'a', ['reload/post.json', 'records', 'lifecycle', 'waiting_clients'], [{'call_id': 'wrong', 'delivery': 'accepted', 'source': {'item': 0, 'block': 0}, 'result': {'item': 0, 'block': 1}}]),
    'reload_duplicate_link': ('tooling-f', 'a', ['reload/post.json', 'records', 'groups', 0, 'duplicate_links'], [[0, 2]]),
    'ledger_lifecycle_link': ('tooling-f', 'a', ['ledger.json', 'lifecycle', 'waiting_clients'], [{'call_id': 'wrong', 'delivery': 'accepted', 'source': {'item': 0, 'block': 0}, 'result': {'item': 0, 'block': 1}}]),
    'ledger_group_link': ('tooling-f', 'a', ['ledger.json', 'groups', 0, 'members'], [0]),
    'unobserved_absence': ('tooling-refusal', 'a', ['observation.json', 'complete'], False),
    'wrong_ACP_count': ('tooling-f', 'a', ['display.json', 'acp', 'F'], 0),
    'wrong_TUI_count': ('tooling-f', 'a', ['display.json', 'tui', 'F'], 0),
    'double_usage': ('tooling-f', 'a', ['usage.json', 'aggregate', 'input_tokens'], 6),
    'double_terminal': ('tooling-f', 'a', ['usage.json', 'logical_terminal_count'], 2),
    'loaded_resurrection': ('tooling-f', 'a', ['authority.json', 'loaded_after'], ['read_fixture_0']),
    'historical_reexecution': ('tooling-ordinary', 'a', ['authority.json', 'historical_dispatches'], 1),
    'schema_replacement': ('tooling-keep', 'a', ['authority.json', 'registrations', 0, 'schema_digest'], '0' * 64),
    'recovered_http_failure': ('tooling-f', 'a', ['attempts.jsonl', 0, 'status'], 400),
    'continuation_overflow': ('tooling-f', 'a', ['attempts.jsonl', 0, 'continuation'], 4),
    'future_information': ('tooling-f', 'a', ['wire/req-001.json', 'input', 0, 'content', 0, 'text'], 'Historical discovery completed; no current authority.'),
    'host_coordinate_leak': ('tooling-f', 'a', ['wire/req-003.json', 'input', -1, 'content', 0, 'text'], 'fixture-lineage'),
    'missing_artifact': ('tooling-f', 'special', [], None),
    'wrong_artifact_hash': ('tooling-f', 'special', [], None),
    'wrong_receipt_hash': ('tooling-f', 'special', [], None),
    'missing_producer': ('tooling-f', 'event-drop', ['producer'], None),
    'missing_plan': ('tooling-f', 'event-drop', ['plan'], None),
    'missing_ack': ('tooling-f', 'event-drop', ['projection_ack'], None),
    'missing_splice': ('tooling-f', 'event-drop', ['splice'], None),
    'missing_later_user': ('tooling-f', 'event-drop', ['later_user'], None),
    'fabricated_producer': ('tooling-f', 'event-set', ['producer', 'mechanism'], 'private-P1-capture'),
    'bad_ack_generation': ('tooling-f', 'event-set', ['projection_ack', 'generation'], 1),
    'forbidden_projection': ('tooling-auxiliary', 'special', [], None),
    'ack_after_send': ('tooling-f', 'special', [], None),
    'reordered_blocks': ('tooling-f', 'special', [], None),
    'lost_block': ('tooling-f', 'special', [], None),
    'duplicate_block': ('tooling-f', 'special', [], None),
    'hidden_attempt': ('tooling-f', 'special', [], None),
    'nochange_records': ('tooling-keep', 'special', [], None),
}


def negative_fixture(name):
    case, owner, path, value = REJECTIONS[name]
    receipt, artifacts = fixture(case)
    if owner in ('r', 'a'):
        obj = receipt if owner == 'r' else artifacts
        for part in path[:-1]:
            obj = obj[part]
        obj[path[-1]] = copy.deepcopy(value)
    elif owner == 'event-drop':
        artifacts['events.jsonl'] = [e for e in artifacts['events.jsonl'] if e['type'] != path[0]]
    elif owner == 'event-set':
        next(e for e in artifacts['events.jsonl'] if e['type'] == path[0])[path[1]] = value
    elif name == 'missing_artifact':
        del artifacts['ordered.json']
    elif name == 'forbidden_projection':
        artifacts['events.jsonl'].insert(0, {'type': 'projection_write'})
    elif name == 'ack_after_send':
        events = artifacts['events.jsonl']
        a, b = next(i for i, e in enumerate(events) if e['type'] == 'projection_ack'), next(i for i, e in enumerate(events) if e['type'] == 'send')
        events[a], events[b] = events[b], events[a]
    elif name == 'reordered_blocks':
        artifacts['ordered.json']['items'].reverse()
    elif name == 'lost_block':
        artifacts['ordered.json']['items'].pop()
    elif name == 'duplicate_block':
        artifacts['ordered.json']['items'].append(copy.deepcopy(artifacts['ordered.json']['items'][0]))
    elif name == 'hidden_attempt':
        artifacts['attempts.jsonl'].pop()
    elif name == 'nochange_records':
        artifacts['checkpoints/post.json']['records']['projection']['generation'] += 1
        artifacts['reload/post.json'] = copy.deepcopy(artifacts['checkpoints/post.json'])
    events = artifacts['events.jsonl']
    for index, event in enumerate(events):
        event['sequence'] = index
    artifacts['observation.json']['end_sequence'] = len(events) - 1
    artifacts['observation.json']['counters'] = {kind: sum(e['type'] == kind for e in events)
        for kind in ('projection_write', 'projection_ack', 'Applied', 'dispatch')}
    artifacts['observation.json']['counters']['send'] = len(artifacts['attempts.jsonl'])
    if name == 'missing_KEEP':
        counts = {action: sum(member['action'] == action for member in artifacts['ledger.json']['members']) for action in ('KEEP', 'F', 'STRIP', 'OMIT')}
        artifacts['display.json']['tui'] = counts
        artifacts['display.json']['acp'] = counts
        artifacts['display.json']['host_summary']['counts'] = counts
    return receipt, artifacts


def write_negative(root, name):
    receipt, artifacts = negative_fixture(name)
    fault = name if name in ('wrong_artifact_hash', 'wrong_receipt_hash') else None
    return write_fixture(root, receipt, artifacts, corrupt_hash=fault)


REJECTION_REASONS = {'ack_after_send': 'event-order',
 'bad_ack_generation': 'projection-ack',
 'continuation_overflow': 'continuation-cap',
 'double_terminal': 'usage-once',
 'double_usage': 'usage-once',
 'duplicate_block': 'ordered-fidelity',
 'fabricated_producer': 'producer-mechanism',
 'forbidden_projection': 'forbidden-effect',
 'future_information': 'body-fidelity',
 'hidden_attempt': 'hidden-attempt',
 'historical_reexecution': 'execution-once',
 'host_coordinate_leak': 'host-coordinate-leak',
 'ledger_group_link': 'SC2-C1-links',
 'ledger_lifecycle_link': 'SC2-C1-links',
 'loaded_resurrection': 'loaded-resurrection',
 'lost_block': 'ordered-fidelity',
 'missing_H': 'H-witness',
 'missing_KEEP': 'keep-action-witness',
 'missing_ack': 'missing-witness',
 'missing_artifact': 'missing-artifact',
 'missing_later_user': 'missing-witness',
 'missing_omission': 'omission-ledger',
 'missing_plan': 'missing-witness',
 'missing_producer': 'missing-witness',
 'missing_splice': 'missing-witness',
 'na_receipt': 'na-receipt',
 'no_activation': 'activation',
 'nochange_records': 'no-change-records',
 'optional_id': 'ordered-fidelity',
 'recovered_http_failure': 'recovered-failure',
 'reload_duplicate_link': 'independent-reload',
 'reload_waiting_link': 'independent-reload',
 'reordered_blocks': 'ordered-fidelity',
 'required_join': 'ordered-fidelity',
 'schema_replacement': 'registration-backing',
 'splice_bypass': 'splice',
 'stale_plan': 'plan',
 'unobserved_absence': 'absence-observation',
 'wrong_ACP_count': 'TUI-ACP-accounting',
 'wrong_TUI_count': 'TUI-ACP-accounting',
 'wrong_action': 'action',
 'wrong_artifact_hash': 'artifact-hash',
 'wrong_binary': 'binary-kind',
 'wrong_layer': 'layer-shape',
 'wrong_manifest': 'manifest-hash',
 'wrong_reader': 'receipt-readers',
 'wrong_receipt_hash': 'receipt-hash',
 'wrong_route': 'route',
 'wrong_shape': 'layer-shape'}


def rejection_test(name):
    def test(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            root = write_negative(Path(temp) / name, name)
            with self.assertRaises(validate.ReceiptError) as error:
                validate.validate_report(manifest(), root)
            self.assertEqual(str(error.exception), REJECTION_REASONS[name])
    test.__name__ = 'test_reject_' + name
    return test


for _name in REJECTIONS:
    setattr(ReceiptContractTests, 'test_reject_' + _name, rejection_test(_name))


def sized_entry(size, occurrence=1):
    entry = {'occurrence': occurrence, 'source': 'human', 'storage': 'custody', 'payload': {'text': ''}}
    entry['payload']['text'] = 'x' * (size - len(validate.canonical(entry)))
    return entry


def cap_accounting(name):
    if name == 'entry_count':
        return hold_accounting([sized_entry(128, i + 1) for i in range(129)])
    if name == 'entry_bytes':
        return hold_accounting([sized_entry(32 * 1024 + 1)])
    if name == 'aggregate':
        return hold_accounting([sized_entry(30000, i + 1) for i in range(9)])
    if name == 'shared':
        value = hold_accounting()
        value['shared_other_bytes'] = 4 * 1024 * 1024 - value['charged_bytes'] + 1
        return value
    if name == 'prepared_copy':
        value = hold_accounting(copies=[{'occurrence': 1, 'payload': {'text': 'extra real copy'}}])
        value['charged_bytes'] -= len(validate.canonical(value['records']['prepared_copies'][0]))
        return value
    if name == 'metadata':
        value = hold_accounting()
        value['records']['header']['steer_stamps'] = [{'slot': 1, 'message_id': 'metadata' * 10}]
        return value
    value = hold_accounting()
    del value['records']['reserve']['omission']
    value['charged_bytes'] = len(validate.canonical(value['records']))
    return value


def hold_rejection_test(name):
    def test(self):
        with self.assertRaises(validate.ReceiptError):
            validate.validate_hold(bad_hold(name))
    test.__name__ = 'test_hold_reject_' + name
    return test


def cap_rejection_test(name):
    def test(self):
        with self.assertRaises(validate.ReceiptError):
            validate.validate_accounting(cap_accounting(name))
    test.__name__ = 'test_cap_reject_' + name
    return test


def test_inclusive_bounds_and_identical_payload_occurrences(self):
    validate.validate_accounting(hold_accounting([sized_entry(128, i + 1) for i in range(128)]))
    validate.validate_accounting(hold_accounting([sized_entry(32 * 1024)]))
    value = hold_accounting(copies=[{'occurrence': 1, 'payload': {'text': 'copy'}}])
    self.assertEqual(validate.validate_accounting(value), len(validate.canonical(value['records'])))


def test_F_future_information_and_UTF8_caps(self):
    for text in ('Historical discovery completed; no current authority.', 'é' * 65):
        with self.subTest(utf8_bytes=len(text.encode())):
            with self.assertRaises(validate.ReceiptError):
                validate.validate_f_text([{'type': 'message', 'role': 'assistant', 'content': [{'type': 'output_text', 'text': text}]}])


def test_total_attempt_cap_applies_before_body_validation(self):
    receipt, artifacts = fixture('tooling-auxiliary')
    original_request = copy.deepcopy(artifacts['wire/req-001.json'])
    original_response = copy.deepcopy(artifacts['wire/resp-001.json'])
    artifacts['bodies.json'], artifacts['attempts.jsonl'] = [], []
    for index, reason in enumerate(('initial', 'retry', 'compaction', 'continuation', 'retry')):
        req, resp = f'wire/req-{index + 1:03d}.json', f'wire/resp-{index + 1:03d}.json'
        artifacts[req], artifacts[resp] = copy.deepcopy(original_request), copy.deepcopy(original_response)
        artifacts['bodies.json'].append({'request': req, 'response': resp})
        artifacts['attempts.jsonl'].append({'number': index + 1, 'request': req, 'reason': reason,
            'status': 200, 'continuation': min(3, index), 'request_id': f'logical-{index}'})
    with self.assertRaises(validate.ReceiptError):
        validate.validate_attempts(manifest()['cases']['tooling-auxiliary'], receipt, artifacts, original_request['input'])


for _name in HOLD_REJECTIONS:
    setattr(ReceiptContractTests, 'test_hold_reject_' + _name, hold_rejection_test(_name))
for _name in ('entry_count', 'entry_bytes', 'aggregate', 'shared', 'prepared_copy', 'metadata', 'reserve'):
    setattr(ReceiptContractTests, 'test_cap_reject_' + _name, cap_rejection_test(_name))
ReceiptContractTests.test_inclusive_bounds_and_identical_payload_occurrences = test_inclusive_bounds_and_identical_payload_occurrences
ReceiptContractTests.test_F_future_information_and_UTF8_caps = test_F_future_information_and_UTF8_caps
ReceiptContractTests.test_total_attempt_cap_applies_before_body_validation = test_total_attempt_cap_applies_before_body_validation


def test_artifact_capture_allowlists_all_fields_before_hash_or_write(self):
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        destination = Path(temp) / 'not-created'
        artifacts = {'wire/req-001.json': {'model': 'fixture-R', 'store': False, 'stream': True, 'input': [], 'tools': []},
                     'plan.json': {'cache_identity_hash': 'a' * 64}}
        with self.assertRaises(validate.ReceiptError):
            validate.write_screened_artifacts(destination, artifacts)
        self.assertFalse(destination.exists())
        with self.assertRaises(validate.ReceiptError):
            validate.write_screened_artifacts(destination, {'unrecognized.json': {'opaque': 'a' * 64}})
        self.assertFalse(destination.exists())
        inventory = validate.write_screened_artifacts(destination, {'wire/req-001.json': artifacts['wire/req-001.json']})
        self.assertEqual(inventory, [{'path': 'wire/req-001.json', 'sha256': validate.sha((destination / 'wire/req-001.json').read_bytes())}])


ReceiptContractTests.test_artifact_capture_allowlists_all_fields_before_hash_or_write = test_artifact_capture_allowlists_all_fields_before_hash_or_write


def test_vertex_bank_hash_and_terminal_facts_are_revalidated(self):
    for key, value in (('sha256', '0' * 64), ('stop_reasons', ['pause_turn'])):
        bad = manifest()
        bad['vertex_bank_inventory'][0][key] = value
        with self.subTest(key=key):
            with self.assertRaises(validate.ReceiptError):
                validate.validate_corpus(bad, CORPUS)


ReceiptContractTests.test_vertex_bank_hash_and_terminal_facts_are_revalidated = test_vertex_bank_hash_and_terminal_facts_are_revalidated


def test_nonpass_verdicts_preserve_failure_and_no_launch_categories(self):
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        for verdict in ('FAIL', 'BLOCKED', 'UNVERIFIED'):
            receipt, artifacts = fixture('tooling-refusal')
            receipt['verdict'] = verdict
            receipt['failed_paths'] = ['/refusal'] if verdict == 'FAIL' else []
            receipt['reason'] = 'synthetic status-category control'
            root = write_fixture(Path(temp) / verdict, receipt, artifacts)
            self.assertEqual(validate.validate_report(manifest(), root), verdict)
        receipt, artifacts = fixture()
        receipt['verdict'] = 'BLOCKED'
        receipt['failed_paths'] = []
        root = write_fixture(Path(temp) / 'launched-blocked', receipt, artifacts)
        with self.assertRaises(validate.ReceiptError):
            validate.validate_report(manifest(), root)


ReceiptContractTests.test_nonpass_verdicts_preserve_failure_and_no_launch_categories = test_nonpass_verdicts_preserve_failure_and_no_launch_categories

def test_live_mixed_reload_is_nonterminal_with_exact_links_and_charges(self):
    receipt, artifacts = fixture('tooling-live-reload')
    pre = artifacts['checkpoints/pre.json']
    self.assertEqual(pre['records']['lifecycle']['exchange'], 'LiveBound')
    self.assertIsNone(pre['records']['lifecycle']['terminal'])
    self.assertEqual(pre['records']['lifecycle']['groups'], {'group-0': 'Completed', 'group-1': 'LivePending'})
    self.assertEqual(pre['records']['lifecycle']['waiting_clients'], [
        {'call_id': 'client-a', 'source': {'item': 0, 'block': 3}, 'result': {'item': 2, 'block': 0}, 'delivery': 'accepted-pending-send'},
        {'call_id': 'client-b', 'source': {'item': 0, 'block': 5}, 'result': {'item': 2, 'block': 1}, 'delivery': 'accepted-pending-send'}])
    self.assertEqual(pre['records']['groups'][1]['duplicate_links'], [
        {'source': {'item': 1, 'block': 0}, 'canonical': {'item': 0, 'block': 2}}])
    self.assertEqual(artifacts['usage.json']['logical_terminal_count'], 0)
    self.assertTrue(all(value > 0 for value in artifacts['ledger.json']['shared_charges'].values()))
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        root = write_fixture(Path(temp) / 'live', receipt, artifacts)
        self.assertEqual(json.loads((root / 'reload/pre.json').read_text()), pre)
        self.assertEqual(validate.validate_report(manifest(), root), 'PASS')


def live_negative_fixture(name):
    receipt, artifacts = fixture('tooling-live-reload')
    records = copy.deepcopy(artifacts['checkpoints/pre.json']['records'])
    if name == 'waiting_id':
        records['lifecycle']['waiting_clients'][0]['call_id'] = 'other-client'
    elif name == 'delivery':
        records['lifecycle']['waiting_clients'][0]['delivery'] = 'delivered'
    elif name == 'result_coordinate':
        records['lifecycle']['waiting_clients'][0]['result'] = {'item': 2, 'block': 1}
    elif name == 'membership':
        records['groups'][1]['members'][1] = {'item': 0, 'block': 3}
    elif name == 'duplicate_link':
        records['groups'][1]['duplicate_links'][0]['canonical'] = {'item': 0, 'block': 0}
    elif name == 'terminal':
        records['lifecycle']['exchange'] = 'Terminal'
        records['lifecycle']['terminal'] = {'outcome': 'completed'}
    for path in ('checkpoints/pre.json', 'checkpoints/post.json', 'reload/pre.json', 'reload/post.json'):
        artifacts[path]['records'] = copy.deepcopy(records)
    artifacts['ledger.json']['lifecycle'] = copy.deepcopy(records['lifecycle'])
    artifacts['ledger.json']['groups'] = copy.deepcopy(records['groups'])
    set_live_charges(artifacts)
    if name == 'charges':
        artifacts['ledger.json']['shared_charges']['ledger'] = 0
    return receipt, artifacts


LIVE_REJECTIONS = {'waiting_id': 'SC2-C1-waiting-id', 'delivery': 'SC2-C1-delivery-state',
    'result_coordinate': 'SC2-C1-waiting-coordinates', 'membership': 'SC2-C1-members',
    'duplicate_link': 'SC2-C1-duplicate-links', 'terminal': 'SC2-C1-live-state', 'charges': 'SC2-C1-charges'}


def live_rejection_test(name):
    def test(self):
        receipt, artifacts = live_negative_fixture(name)
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            root = write_fixture(Path(temp) / name, receipt, artifacts)
            with self.assertRaisesRegex(validate.ReceiptError, LIVE_REJECTIONS[name]):
                validate.validate_report(manifest(), root)
    return test


ReceiptContractTests.test_live_mixed_reload_is_nonterminal_with_exact_links_and_charges = test_live_mixed_reload_is_nonterminal_with_exact_links_and_charges
for _name in LIVE_REJECTIONS:
    setattr(ReceiptContractTests, 'test_live_reload_reject_' + _name, live_rejection_test(_name))


def keep_backing_fixture(change=None):
    receipt, artifacts = fixture('tooling-keep')
    definitions = artifacts['source.json']['items'][2]['tools']
    registrations = artifacts['authority.json']['registrations']
    for registration, definition in zip(registrations, definitions):
        registration['schema'] = copy.deepcopy(definition['parameters'])
        registration['schema_digest'] = validate.digest(registration['schema'])
    for body in artifacts['bodies.json']:
        artifacts[body['request']]['tools'][0]['parameters'] = copy.deepcopy(registrations[0]['schema'])
    artifacts['plan.json']['body_digest'] = validate.digest(artifacts['wire/req-001.json'])
    registration = registrations[4]
    if change == 'schema':
        registration['schema']['properties']['replacement'] = {'type': 'integer'}
    elif change == 'optional_presence':
        del registration['schema']['required']
    elif change == 'namespace':
        registration['namespace'] = 'fixture_mcp'
    elif change == 'eligibility':
        registration['eligible'] = False
    registration['schema_digest'] = validate.digest(registration['schema'])
    return receipt, artifacts


def keep_backing_test(change):
    def test(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            receipt, artifacts = keep_backing_fixture()
            root = write_fixture(Path(temp) / 'qualified', receipt, artifacts)
            self.assertEqual(validate.validate_report(manifest(), root), 'PASS')
            receipt, artifacts = keep_backing_fixture(change)
            root = write_fixture(Path(temp) / 'replacement', receipt, artifacts)
            with self.assertRaisesRegex(validate.ReceiptError, 'KEEP-current-registration'):
                validate.validate_report(manifest(), root)
    return test


for _name in ('schema', 'optional_presence', 'namespace', 'eligibility'):
    setattr(ReceiptContractTests, 'test_KEEP_rejects_self_consistent_' + _name, keep_backing_test(_name))


def test_KEEP_checks_each_definition_with_self_consistent_declarations(self):
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        for index in range(6):
            receipt, artifacts = keep_backing_fixture()
            registration = artifacts['authority.json']['registrations'][index]
            registration['schema']['properties']['replacement'] = {'type': 'integer'}
            registration['schema_digest'] = validate.digest(registration['schema'])
            for body in artifacts['bodies.json']:
                tool = artifacts[body['request']]['tools'][0]
                if tool['name'] == registration['name']:
                    tool['parameters'] = copy.deepcopy(registration['schema'])
            artifacts['plan.json']['body_digest'] = validate.digest(artifacts['wire/req-001.json'])
            with self.subTest(definition=index):
                root = write_fixture(Path(temp) / str(index), receipt, artifacts)
                with self.assertRaisesRegex(validate.ReceiptError, 'KEEP-current-registration'):
                    validate.validate_report(manifest(), root)


ReceiptContractTests.test_KEEP_checks_each_definition_with_self_consistent_declarations = test_KEEP_checks_each_definition_with_self_consistent_declarations


REPORT_SCREENING_CASES = ('derived_hash', 'headers', 'sentinel', 'unknown_field', 'receipt_sentinel')


def report_writer_screening_test(change):
    def test(self):
        from unittest import mock
        receipt, artifacts = fixture('tooling-keep')
        if change == 'derived_hash':
            artifacts['authority.json']['registrations'][-1]['cache_identity_hash'] = 'a' * 64
        elif change == 'headers':
            artifacts['authority.json']['registrations'][-1]['headers'] = {'Authorization': 'synthetic-local-only'}
        elif change == 'sentinel':
            artifacts['wire/resp-003.json']['output'] = [{'type': 'message', 'content': [{'type': 'output_text', 'text': '<synthetic-secret>'}]}]
        elif change == 'unknown_field':
            artifacts['plan.json']['opaque_unlisted'] = 'not-allowed'
        else:
            receipt['reason'] = '<synthetic-secret>'
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            destination = Path(temp) / 'not-created'
            with mock.patch.object(validate.hashlib, 'sha256', wraps=validate.hashlib.sha256) as hashed:
                with self.assertRaisesRegex(validate.ReceiptError, 'excluded|allowlist'):
                    write_fixture(destination, receipt, artifacts)
                hashed.assert_not_called()
            self.assertFalse(destination.exists())
    return test


for _name in REPORT_SCREENING_CASES:
    setattr(ReceiptContractTests, 'test_actual_report_writer_screens_' + _name, report_writer_screening_test(_name))


def test_later_user_preserves_accumulated_function_pair_and_assistant_output(self):
    for case in ('tooling-f', 'tooling-keep', 'tooling-ordinary', 'tooling-f-other', 'hold-positive'):
        with self.subTest(case=case):
            receipt, artifacts = fixture('tooling-keep' if case == 'hold-positive' else case)
            if case == 'hold-positive':
                artifacts['hold.json'] = hold_fixture()
            automatic = artifacts['wire/req-002.json']['input']
            answer = artifacts['wire/resp-002.json']['output']
            later = {'type': 'message', 'role': 'user', 'content': [{'type': 'input_text', 'text': 'later input'}]}
            self.assertEqual(artifacts['wire/req-003.json']['input'], automatic + answer + [later])
            self.assertTrue(answer)
            with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
                root = write_fixture(Path(temp) / case, receipt, artifacts)
                self.assertEqual(validate.validate_report(manifest(), root), 'PASS')


def later_drop_fixture(kind):
    receipt, artifacts = fixture('tooling-keep')
    later = artifacts['wire/req-003.json']['input'][-1]
    if kind == 'pair':
        artifacts['wire/req-003.json']['input'] = artifacts['wire/req-001.json']['input'] + [later]
    else:
        artifacts['wire/req-003.json']['input'] = artifacts['wire/req-002.json']['input'] + [later]
    return receipt, artifacts


def later_drop_test(kind):
    def test(self):
        receipt, artifacts = later_drop_fixture(kind)
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            root = write_fixture(Path(temp) / 'unaccounted-drop', receipt, artifacts)
            with self.assertRaisesRegex(validate.ReceiptError, 'later-user-request'):
                validate.validate_report(manifest(), root)
    return test


ReceiptContractTests.test_later_user_preserves_accumulated_function_pair_and_assistant_output = test_later_user_preserves_accumulated_function_pair_and_assistant_output
for _name in ('pair', 'assistant'):
    setattr(ReceiptContractTests, 'test_unaccounted_later_drop_' + _name, later_drop_test(_name))


NESTED_METADATA_RECORDS = {
    'registration': ('tooling-keep', ['authority.json', 'registrations', 5]),
    'ledger_member': ('tooling-keep', ['ledger.json', 'members', 0]),
    'route_endpoint': ('tooling-keep', ['route-manifest.json', 'endpoint']),
    'usage_response': ('tooling-keep', ['usage.json', 'responses', 0]),
    'host_reason': ('tooling-keep', ['display.json', 'host_summary', 'reasons', 0]),
    'checkpoint_group': ('tooling-live-reload', ['checkpoints/pre.json', 'records', 'groups', 1]),
    'group_validation': ('tooling-live-reload', ['checkpoints/pre.json', 'records', 'groups', 1, 'validation']),
    'duplicate_link': ('tooling-live-reload', ['checkpoints/pre.json', 'records', 'groups', 1, 'duplicate_links', 0]),
    'waiting_client': ('tooling-live-reload', ['reload/pre.json', 'records', 'lifecycle', 'waiting_clients', 0]),
    'handoff_client': ('tooling-live-reload', ['handoff.json', 'accepted_clients', 0]),
    'hold_header': ('tooling-keep', ['hold.json', 'accounting', 'records', 'header']),
    'hold_entry': ('tooling-keep', ['hold.json', 'accounting', 'records', 'entries', 0]),
    'hold_copy': ('tooling-keep', ['hold.json', 'accounting', 'records', 'prepared_copies', 0]),
    'hold_reserve': ('tooling-keep', ['hold.json', 'accounting', 'records', 'reserve', 'terminal']),
    'hold_suppression': ('tooling-keep', ['hold.json', 'release', 'suppressions', 0]),
    'protocol_item': ('tooling-keep', ['wire/req-001.json', 'input', 0]),
}


def nested_metadata_writer_test(name):
    def test(self):
        from unittest import mock
        case, path = NESTED_METADATA_RECORDS[name]
        receipt, artifacts = fixture(case)
        if path[0] == 'hold.json':
            artifacts['hold.json'] = hold_fixture()
            artifacts['hold.json']['accounting'] = hold_accounting(copies=[{'occurrence': 1, 'payload': {'text': 'copy'}}])
        record = artifacts
        for key in path:
            record = record[key]
        record['model_cache'] = {'digest': 'f' * 64}
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            destination = Path(temp) / 'not-created'
            with mock.patch.object(validate.hashlib, 'sha256', wraps=validate.hashlib.sha256) as hashed:
                with self.assertRaisesRegex(validate.ReceiptError, 'allowlist'):
                    write_fixture(destination, receipt, artifacts)
                hashed.assert_not_called()
            self.assertFalse(destination.exists())
    return test


def test_capture_preserves_declared_arbitrary_schema_and_payload_locations(self):
    receipt, artifacts = fixture('tooling-keep')
    schema = {'type': 'object', 'properties': {'model_cache': {'type': 'object', 'properties': {'digest': {'type': 'string'}}}}}
    artifacts['authority.json']['registrations'][0]['schema'] = copy.deepcopy(schema)
    artifacts['wire/req-001.json']['tools'][0]['parameters'] = copy.deepcopy(schema)
    artifacts['hold.json'] = hold_fixture()
    payload = {'model_cache': {'digest': 'public-payload-not-metadata'}}
    artifacts['hold.json']['accounting']['records']['entries'][0]['payload'] = payload
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        root = write_fixture(Path(temp) / 'allowed-payloads', receipt, artifacts)
        self.assertEqual(json.loads((root / 'authority.json').read_text())['registrations'][0]['schema'], schema)
        self.assertEqual(json.loads((root / 'hold.json').read_text())['accounting']['records']['entries'][0]['payload'], payload)


for _name in NESTED_METADATA_RECORDS:
    setattr(ReceiptContractTests, 'test_actual_writer_rejects_nested_metadata_' + _name, nested_metadata_writer_test(_name))
ReceiptContractTests.test_capture_preserves_declared_arbitrary_schema_and_payload_locations = test_capture_preserves_declared_arbitrary_schema_and_payload_locations


SEARCH_RESULT_REJECTIONS = {
    'array': [],
    'missing_references': {'type': 'tool_search_tool_search_result'},
    'wrong_references_type': {'type': 'tool_search_tool_search_result', 'tool_references': {}},
    'wrong_reference_kind': {'type': 'tool_search_tool_search_result', 'tool_references': [{'type': 'text', 'tool_name': 'read_fixture_0'}]},
    'empty_reference_name': {'type': 'tool_search_tool_search_result', 'tool_references': [{'type': 'tool_reference', 'tool_name': ''}]},
    'missing_error_code': {'type': 'tool_search_tool_result_error'},
    'unknown_error_code': {'type': 'tool_search_tool_result_error', 'error_code': 'invented'},
    'wrong_error_message': {'type': 'tool_search_tool_result_error', 'error_code': 'unavailable', 'error_message': []},
    'wrong_result_kind': {'type': 'tool_result', 'tool_references': []},
}


def search_result_fixture(content):
    m = manifest()
    receipt, artifacts = fixture('tooling-live-reload')
    history = copy.deepcopy(artifacts['source.json']['items'])
    history[0]['content'][1]['content'] = copy.deepcopy(content)
    m['synthetic_sources']['live-mixed']['items'] = copy.deepcopy(history)
    for name in ('source.json', 'ordered.json'):
        artifacts[name]['items'] = copy.deepcopy(history)
    for name in ('checkpoints/pre.json', 'checkpoints/post.json', 'reload/pre.json', 'reload/post.json'):
        artifacts[name]['history'] = copy.deepcopy(history)
    artifacts['ledger.json']['source_digest'] = validate.digest(artifacts['source.json'])
    artifacts['ledger.json']['active_digest'] = validate.digest(history)
    artifacts['plan.json']['history_digest'] = validate.digest(history)
    receipt['manifest_digest'] = validate.digest(m)
    set_live_charges(artifacts)
    return m, receipt, artifacts


def search_result_rejection_test(name):
    def test(self):
        m, receipt, artifacts = search_result_fixture(SEARCH_RESULT_REJECTIONS[name])
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            root = write_fixture(Path(temp) / name, receipt, artifacts)
            with self.assertRaisesRegex(validate.ReceiptError, 'SC2-C1-search-result'):
                validate.validate_report(m, root)
    return test


def test_live_reload_empty_search_is_the_first_party_protocol_object(self):
    receipt, artifacts = fixture('tooling-live-reload')
    self.assertEqual(artifacts['source.json']['items'][0]['content'][1]['content'],
                     {'type': 'tool_search_tool_search_result', 'tool_references': []})
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        root = write_fixture(Path(temp) / 'empty-search', receipt, artifacts)
        self.assertEqual(validate.validate_report(manifest(), root), 'PASS')


def test_live_reload_accepts_protocol_result_and_error_variants(self):
    contents = [
        {'type': 'tool_search_tool_search_result', 'tool_references': []},
        {'type': 'tool_search_tool_search_result', 'tool_references': [{'type': 'tool_reference', 'tool_name': 'read_fixture_0'}]},
    ]
    for code in ('invalid_tool_input', 'unavailable', 'too_many_requests', 'execution_time_exceeded'):
        error = {'type': 'tool_search_tool_result_error', 'error_code': code}
        contents.extend([error, {**error, 'error_message': None}, {**error, 'error_message': 'synthetic search error'}])
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        for index, content in enumerate(contents):
            with self.subTest(index=index):
                m, receipt, artifacts = search_result_fixture(content)
                root = write_fixture(Path(temp) / str(index), receipt, artifacts)
                self.assertEqual(validate.validate_report(m, root), 'PASS')


for _name in SEARCH_RESULT_REJECTIONS:
    setattr(ReceiptContractTests, 'test_live_reload_rejects_search_result_' + _name, search_result_rejection_test(_name))
ReceiptContractTests.test_live_reload_empty_search_is_the_first_party_protocol_object = test_live_reload_empty_search_is_the_first_party_protocol_object
ReceiptContractTests.test_live_reload_accepts_protocol_result_and_error_variants = test_live_reload_accepts_protocol_result_and_error_variants


CAUSAL_EVENT_REJECTIONS = {
    'send_binding': ('send', 'binding', 'fixture-M'),
    'response_binding': ('response', 'binding', 'fixture-M'),
    'dispatch_binding': ('dispatch', 'binding', 'fixture-M'),
    'result_binding': ('result', 'binding', 'fixture-M'),
    'automatic_binding': ('automatic_next', 'binding', 'fixture-M'),
    'later_binding': ('later_user', 'binding', 'fixture-M'),
    'send_request': ('send', 'request', 'wire/req-003.json'),
    'response_request': ('response', 'request', 'wire/req-003.json'),
    'response_body': ('response', 'response', 'wire/resp-003.json'),
    'dispatch_response': ('dispatch', 'response', 'wire/resp-003.json'),
    'result_response': ('result', 'response', 'wire/resp-003.json'),
    'result_request': ('result', 'request', 'wire/req-003.json'),
    'automatic_request': ('automatic_next', 'request', 'wire/req-001.json'),
    'later_request': ('later_user', 'request', 'wire/req-002.json'),
}


def causal_event_fixture(name):
    receipt, artifacts = fixture('tooling-f')
    kind, field, value = CAUSAL_EVENT_REJECTIONS[name]
    next(event for event in artifacts['events.jsonl'] if event['type'] == kind)[field] = value
    return receipt, artifacts


def causal_event_test(name):
    def test(self):
        receipt, artifacts = causal_event_fixture(name)
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            root = write_fixture(Path(temp) / name, receipt, artifacts)
            with self.assertRaisesRegex(validate.ReceiptError, 'event-'):
                validate.validate_report(manifest(), root)
    return test


def test_transport_events_bind_each_causal_body_to_its_ordered_attempt(self):
    m = manifest()
    for case in ('tooling-f', 'tooling-keep', 'tooling-ordinary', 'tooling-f-other', 'tooling-auxiliary'):
        receipt, artifacts = fixture(case)
        events = artifacts['events.jsonl']
        bodies = artifacts['bodies.json']
        sends = [event for event in events if event['type'] in ('send', 'automatic_next', 'later_user')]
        self.assertEqual([event['request'] for event in sends], [body['request'] for body in bodies])
        self.assertEqual([event['request'] for event in sends], [attempt['request'] for attempt in artifacts['attempts.jsonl']])
        for event in events:
            if event['type'] in ('send', 'response', 'dispatch', 'result', 'automatic_next', 'later_user'):
                self.assertEqual(event.get('binding'), m['cases'][case]['route']['model'])
            if event['type'] == 'response':
                self.assertEqual({key: event.get(key) for key in ('request', 'response')}, bodies[0])
            if event['type'] in ('dispatch', 'result'):
                self.assertEqual(event.get('response'), bodies[0]['response'])
            if event['type'] == 'result':
                self.assertEqual(event.get('request'), bodies[1]['request'])
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            root = write_fixture(Path(temp) / case, receipt, artifacts)
            self.assertEqual(validate.validate_report(m, root), 'PASS')


for _name in CAUSAL_EVENT_REJECTIONS:
    setattr(ReceiptContractTests, 'test_causal_event_rejects_' + _name, causal_event_test(_name))
ReceiptContractTests.test_transport_events_bind_each_causal_body_to_its_ordered_attempt = test_transport_events_bind_each_causal_body_to_its_ordered_attempt


DISPLAY_REJECTIONS = {
    'missing_reasons': ('reasons', []),
    'wrong_reason_total': ('reasons', [{'category': 'fixture-policy', 'count': 0}]),
    'missing_rediscovery': ('rediscovery', False),
}


def display_negative_fixture(name):
    receipt, artifacts = fixture('tooling-f')
    field, value = DISPLAY_REJECTIONS[name]
    artifacts['display.json']['host_summary'][field] = copy.deepcopy(value)
    return receipt, artifacts


def display_rejection_test(name):
    def test(self):
        receipt, artifacts = display_negative_fixture(name)
        with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
            root = write_fixture(Path(temp) / name, receipt, artifacts)
            with self.assertRaisesRegex(validate.ReceiptError, 'host-'):
                validate.validate_report(manifest(), root)
    return test


for _name in DISPLAY_REJECTIONS:
    setattr(ReceiptContractTests, 'test_display_reconciles_' + _name, display_rejection_test(_name))


def test_capture_corpus_authority_requires_the_frozen_binding(self):
    original = manifest()
    for fault in ('missing', 'hash', 'path'):
        changed = copy.deepcopy(original)
        if fault == 'missing':
            changed.pop('capture_corpus', None)
        else:
            changed.setdefault('capture_corpus', {'path': '/unavailable-corpus', 'sha256': '0' * 64})
            if fault == 'hash':
                changed['capture_corpus']['sha256'] = '0' * 64
            else:
                changed['capture_corpus']['path'] = '/outside-approved-roots/corpus.json'
        with self.assertRaisesRegex(validate.ReceiptError, 'capture-corpus'):
            validate.validate_corpus(changed, CORPUS)
    validate.validate_corpus(original, CORPUS)


ReceiptContractTests.test_capture_corpus_authority_requires_the_frozen_binding = test_capture_corpus_authority_requires_the_frozen_binding




def corrupt_utc_receipt(root, value, *, resign=True):
    path = Path(root) / 'receipt.json'
    receipt = validate.json_bytes(path.read_bytes())
    if value is None:
        receipt.pop('utc', None)
    else:
        receipt['utc'] = value
    validate.capture_receipt(receipt, signed=True)
    validate.safe(receipt)
    if resign:
        receipt['sha256'] = validate.digest({key: child for key, child in receipt.items() if key != 'sha256'})
    path.write_bytes(validate.canonical(receipt))


def test_receipt_writer_freezes_utc_identity(self):
    from datetime import datetime, timezone
    receipt, artifacts = fixture()
    self.assertIsInstance(receipt.get('utc'), str, 'writer omitted frozen UTC identity')
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        root = write_fixture(Path(temp) / 'receipt', receipt, artifacts)
        try:
            verdict = validate.validate_report(manifest(), root)
        except validate.ReceiptError as error:
            self.fail('reader rejected writer UTC identity: ' + str(error))
        signed = validate.json_bytes((root / 'receipt.json').read_bytes())
        self.assertEqual(verdict, 'PASS')
        self.assertEqual(signed['utc'], receipt['utc'])
        self.assertEqual(signed['schema_version'], 'hp8r-receipt/2')
        self.assertTrue(signed['utc'].endswith('Z'))
        self.assertEqual(datetime.fromisoformat(signed['utc']).tzinfo, timezone.utc)
        self.assertEqual(validate.prepare_report(receipt, artifacts)['receipt.json'], signed)
        before = (root / 'receipt.json').read_bytes()
        self.assertEqual(validate.validate_report(manifest(), root), 'PASS')
        self.assertEqual((root / 'receipt.json').read_bytes(), before)


def test_receipt_reader_rejects_missing_utc(self):
    receipt, artifacts = fixture()
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        root = write_fixture(Path(temp) / 'missing', receipt, artifacts)
        corrupt_utc_receipt(root, None)
        with self.assertRaisesRegex(validate.ReceiptError, 'receipt-fields'):
            validate.validate_report(manifest(), root)


def test_receipt_reader_rejects_non_utc_or_invalid_timestamp(self):
    invalid = ['', '2026-10-21T12:34:56', '2026-10-21T12:34:56+00:00', '2026-10-21T12:34:56+01:00',
               '2026-10-21 12:34:56Z', '2026-10-21T12:34:56z', '2026-02-30T12:34:56Z', 42, True]
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        for index, value in enumerate(invalid):
            with self.subTest(index=index):
                receipt, artifacts = fixture()
                root = write_fixture(Path(temp) / str(index), receipt, artifacts)
                corrupt_utc_receipt(root, value)
                with self.assertRaisesRegex(validate.ReceiptError, 'receipt-utc'):
                    validate.validate_report(manifest(), root)
                receipt['utc'] = value
                with self.assertRaisesRegex(validate.ReceiptError, 'receipt-utc'):
                    validate.prepare_report(receipt, artifacts)
        for index, value in enumerate(['2026-10-21T12:34:56Z', '2026-10-21T12:34:56.123456Z']):
            receipt, artifacts = fixture()
            receipt['utc'] = value
            root = write_fixture(Path(temp) / ('valid-' + str(index)), receipt, artifacts)
            self.assertEqual(validate.validate_report(manifest(), root), 'PASS')


def test_receipt_utc_changes_break_receipt_hash(self):
    receipt, artifacts = fixture()
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        root = write_fixture(Path(temp) / 'changed', receipt, artifacts)
        corrupt_utc_receipt(root, '2026-10-21T12:34:56Z', resign=False)
        with self.assertRaisesRegex(validate.ReceiptError, 'receipt-hash'):
            validate.validate_report(manifest(), root)


ReceiptContractTests.test_receipt_writer_freezes_utc_identity = test_receipt_writer_freezes_utc_identity
ReceiptContractTests.test_receipt_reader_rejects_missing_utc = test_receipt_reader_rejects_missing_utc
ReceiptContractTests.test_receipt_reader_rejects_non_utc_or_invalid_timestamp = test_receipt_reader_rejects_non_utc_or_invalid_timestamp
ReceiptContractTests.test_receipt_utc_changes_break_receipt_hash = test_receipt_utc_changes_break_receipt_hash



def test_message_wrapped_source_ordered_ciphertext_is_projected(self):
    from smoke.redteam.hp8r import test_provider
    from unittest import mock
    ciphertext = 'R14-message-wrapped-signature-π'
    block = {'type': 'thinking', 'thinking': 'Keep this text.', 'signature': ciphertext}
    message = {'type': 'message', 'role': 'assistant', 'content': [block]}
    source = {'source_class': 'synthetic-mutation', 'items': [message],
              'coordinates': [{'lineage': 'fixture-lineage', 'occurrence': 0}]}
    artifacts = {'source.json': source, 'ordered.json': copy.deepcopy(source)}
    expected = copy.deepcopy(source)
    expected['items'][0]['content'][0]['signature'] = test_provider.signature_projection(ciphertext)
    with tempfile.TemporaryDirectory(dir=os.environ.get('HP8R_TEST_ROOT')) as temp:
        destination = Path(temp) / 'report'
        with mock.patch.object(test_provider.provider.hashlib, 'sha256', wraps=test_provider.provider.hashlib.sha256) as hashed:
            inventory = validate.write_screened_artifacts(destination, artifacts)
            self.assertEqual({row['path'] for row in inventory}, set(artifacts))
            for name in artifacts:
                written = (destination / name).read_bytes()
                self.assertTrue(ciphertext.encode() not in written, 'raw message-wrapped ciphertext persisted')
                self.assertEqual(json.loads(written), expected)
            self.assertTrue(all(call.args[0] == ciphertext.encode() or ciphertext.encode() not in call.args[0]
                                for call in hashed.call_args_list), 'raw message-wrapped ciphertext hashed as artifact')
        for name in artifacts:
            self.assertEqual(artifacts[name], source)


ReceiptContractTests.test_message_wrapped_source_ordered_ciphertext_is_projected = test_message_wrapped_source_ordered_ciphertext_is_projected

if __name__ == '__main__':
    unittest.main()
