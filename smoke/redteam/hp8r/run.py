#!/usr/bin/env python3
"""HP-0 offline runner interface; product arms require their own frozen hand-back."""
import argparse
import json
import os
from pathlib import Path
import selectors
import subprocess
import time

if __package__:
    from .loopback_provider import MAX_BODY, ProtocolError, require, screen
    from .validate import write_screened_artifacts
else:
    from loopback_provider import MAX_BODY, ProtocolError, require, screen
    from validate import write_screened_artifacts


METHODS = frozenset(('initialize', 'session/new', 'session/load', 'session/set_model',
                     'session/prompt', 'session/cancel'))


def isolated_environment(home):
    path = Path(home)
    require(path.is_absolute(), 'absolute-home')
    return {'HOME': str(path), 'GROK_HOME': str(path / 'grok'), 'TMPDIR': str(path / 'tmp'),
            'PATH': '/usr/bin:/bin', 'PYTHONDONTWRITEBYTECODE': '1', 'GROK_AUTH_EXPIRED': '1',
            'OPENAI_API_KEY': 'synthetic-local-only', 'ANTHROPIC_API_KEY': 'synthetic-local-only',
            'NO_PROXY': '*', 'OTEL_SDK_DISABLED': 'true'}


class AcpProcess:
    """Single-owner bounded NDJSON-RPC transport with interleaved pending requests."""
    def __init__(self, argv, home, timeout=5):
        self.home = Path(home)
        self.home.mkdir(mode=0o700, parents=True, exist_ok=False)
        (self.home / 'tmp').mkdir(mode=0o700)
        (self.home / 'grok').mkdir(mode=0o700)
        self.process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, env=isolated_environment(home), cwd=home, start_new_session=True)
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ, 'stdout')
        self.selector.register(self.process.stderr, selectors.EVENT_READ, 'stderr')
        self.buffer = b''
        self.stderr_bytes = 0
        self.events = []
        self.event_bytes = 0
        self.sequence = 0
        self.timeout = timeout
        self.pending = {}
        self.pending_bytes = 0
        os.set_blocking(self.process.stdin.fileno(), False)

    def send(self, method, params):
        require(method in METHODS and isinstance(params, dict), 'acp-method')
        if method == 'session/cancel':
            self.notify(method, params)
            return None
        require(len(self.pending) < 128, 'acp-pending-cap')
        self.sequence += 1
        identifier = self.sequence
        deadline = time.monotonic() + self.timeout
        self.pending[identifier] = {'deadline': deadline, 'response': None, 'bytes': 0}
        try:
            self._write({'jsonrpc': '2.0', 'id': identifier, 'method': method, 'params': params}, deadline)
        except BaseException:
            del self.pending[identifier]
            raise
        return identifier

    def notify(self, method, params):
        require(method == 'session/cancel' and isinstance(params, dict), 'acp-notification')
        self._write({'jsonrpc': '2.0', 'method': method, 'params': params}, time.monotonic() + self.timeout)

    def call(self, method, params):
        identifier = self.send(method, params)
        return None if identifier is None else self.receive(identifier)

    def _write(self, value, deadline):
        screen(value)
        raw = (json.dumps(value, allow_nan=False) + '\n').encode()
        require(len(raw) <= MAX_BODY, 'acp-cap')
        remaining = memoryview(raw)
        with selectors.DefaultSelector() as writable:
            writable.register(self.process.stdin, selectors.EVENT_WRITE)
            while remaining:
                timeout = deadline - time.monotonic()
                require(timeout > 0, 'acp-send-timeout')
                try:
                    count = os.write(self.process.stdin.fileno(), remaining)
                except BlockingIOError:
                    require(bool(writable.select(timeout)), 'acp-send-timeout')
                    continue
                except OSError as error:
                    raise ProtocolError('acp-write') from error
                require(count > 0, 'acp-write')
                remaining = remaining[count:]

    def receive(self, identifier):
        require(type(identifier) is int and identifier in self.pending, 'acp-unknown-request')
        request = self.pending[identifier]
        while request['response'] is None:
            value, size = self._next_message(request['deadline'])
            self._accept(value, size)
        del self.pending[identifier]
        self.pending_bytes -= request['bytes']
        return request['response']

    def _accept(self, value, size):
        require(isinstance(value, dict) and value.get('jsonrpc') == '2.0', 'acp-envelope')
        screen(value)
        if 'method' in value:
            require('id' not in value, 'acp-reverse-request')
            require(set(value) <= {'jsonrpc', 'method', 'params'}
                    and isinstance(value['method'], str) and bool(value['method']), 'acp-allowlist')
            require(len(self.events) < 128, 'acp-event-cap')
            self.event_bytes += size
            require(self.event_bytes <= MAX_BODY, 'acp-event-byte-cap')
            self.events.append(value)
            return
        require(set(value) <= {'jsonrpc', 'id', 'result', 'error'}
                and ('result' in value) != ('error' in value), 'acp-result')
        identifier = value.get('id')
        require(type(identifier) is int and identifier in self.pending, 'acp-foreign-id-or-reverse-request')
        request = self.pending[identifier]
        require(request['response'] is None, 'acp-duplicate-response')
        require(time.monotonic() <= request['deadline'], 'acp-timeout')
        self.pending_bytes += size
        require(self.pending_bytes <= MAX_BODY, 'acp-pending-byte-cap')
        request.update(response=value, bytes=size)

    def _next_message(self, deadline):
        while True:
            require(time.monotonic() < deadline, 'acp-timeout')
            if b'\n' in self.buffer:
                line, self.buffer = self.buffer.split(b'\n', 1)
                try:
                    return json.loads(line), len(line)
                except (ValueError, UnicodeError) as error:
                    raise ProtocolError('acp-json') from error
            remaining = deadline - time.monotonic()
            require(remaining > 0, 'acp-timeout')
            ready = self.selector.select(remaining)
            require(bool(ready), 'acp-timeout')
            for key, _ in ready:
                data = os.read(key.fileobj.fileno(), 65536)
                if not data:
                    self.selector.unregister(key.fileobj)
                    if key.data == 'stdout':
                        raise ProtocolError('acp-truncated')
                elif key.data == 'stderr':
                    self.stderr_bytes += len(data)
                    require(self.stderr_bytes <= MAX_BODY, 'acp-stderr-cap')
                else:
                    self.buffer += data
                    require(len(self.buffer) <= MAX_BODY, 'acp-cap')

    def close(self):
        import signal
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=2)
        self.selector.close()
        self.pending.clear()
        self.pending_bytes = 0
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            stream.close()

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--layer', choices=('L0', 'L1', 'L2'), required=True)
    parser.add_argument('--case', required=True)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--bin', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--authorization', type=Path)
    args = parser.parse_args(argv)
    if not args.bin.is_absolute():
        parser.error('--bin requires an absolute executable path')
    if args.layer == 'L2' and args.authorization is None:
        parser.error('L2 requires --authorization')
    return args


def main(argv=None):
    args = parse_args(argv)
    try:
        manifest = json.loads(args.manifest.read_text())
        case = manifest['cases'][args.case]
        require(args.layer in case['layers'], 'case-layer')
        require(args.layer != 'L2', 'HP-0-no-live-authorization')
        require(case.get('execution') == 'tooling-validation', 'product-hand-back-required')
        require(args.bin.is_file() and os.access(args.bin, os.X_OK), 'binary-missing')
        result = {'case': args.case, 'layer': args.layer, 'verdict': 'BLOCKED',
                  'reason': 'Product arm not launched: committed producer and build-time provenance required.',
                  'tooling_only': True, 'binary_executed': False}
        write_screened_artifacts(args.out, {'verdict.json': result})
        return 1
    except (KeyError, ValueError, OSError, TypeError, AttributeError, RecursionError) as error:
        print(json.dumps({'verdict': 'BLOCKED', 'reason': type(error).__name__, 'binary_executed': False}))
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
