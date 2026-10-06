"""Context compaction against an isolated local API; no real model calls."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen

requests = []
summary_requests = []
mode = 'manual'
fail_summary = False
hold_summary = False
summary_started = threading.Event()
release = threading.Event()

class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        summary = 'Summarize conversation history' in body['messages'][0].get('content', '')
        if summary:
            summary_requests.append(body)
            summary_started.set()
            if hold_summary: release.wait(10)
            self.send_response(200)
            self.end_headers()
            content = '' if fail_summary else 'Earlier task completed. Preserve file README.md and pending user requirements.'
            try: self.wfile.write(json.dumps({'choices': [{'message': {'content': content}}], 'usage': {'total_tokens': 50}}).encode())
            except (BrokenPipeError, ConnectionResetError): pass
            return
        if not body.get('stream'):
            self.send_response(200); self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"Compact test"}}]}')
            return
        requests.append(body)
        overflow = mode == 'auto' and len(requests) == 2
        self.send_response(400 if overflow else 200)
        self.end_headers()
        if overflow:
            self.wfile.write(b'{"error":{"code":"context_length_exceeded"}}')
            return
        content = 'OLD_REPLY ' + ('long context ' * 100) if len(requests) == 1 else 'COMPACT_REPLY_OK'
        self.wfile.write(('data: ' + json.dumps({'choices': [{'delta': {'content': content}, 'finish_reason': 'stop'}]}) + '\n\ndata: {"choices":[],"usage":{"total_tokens":100}}\n\ndata: [DONE]\n\n').encode())


def screen(t): return '\n'.join(Screen(t.output, 110, 30).text)
def saved(workspace):
    session_id = (workspace / '.takiza/latest_session').read_text()
    return json.loads((workspace / '.takiza/sessions' / (session_id + '.json')).read_text())

def run(binary, port, scenario):
    global mode, fail_summary, hold_summary
    mode = scenario
    fail_summary = hold_summary = False
    requests.clear(); summary_requests.clear(); summary_started.clear(); release.clear()
    with tempfile.TemporaryDirectory(prefix='takiza-compact-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber', 'mode': 'manual',
            'model': 'test-model', 'api_key': 'test-only', 'base_url': f'http://127.0.0.1:{port}/v1'}))
        t = Terminal(binary, workspace, 110, 30, initial_prompt='original prompt')
        try:
            t.wait(lambda: '100 tokens' in screen(t), 'initial answer missing')
            t.pump(.3)
            t.send('latest prompt\r')
            t.wait(lambda: 'COMPACT_REPLY_OK' in screen(t), 'second answer missing', timeout=10)
            t.pump(.2)
            before = saved(workspace)
            checkpoints = set((workspace / '.takiza/checkpoints').glob('*.json'))
            if scenario == 'manual':
                fail_summary = True
                t.send('/compact\r')
                t.wait(lambda: 'empty context summary' in screen(t), 'summary failure missing')
                assert saved(workspace)['messages'] == before['messages'], 'failed summary changed context'
                fail_summary = False; hold_summary = True; summary_started.clear()
                t.send('/compact\r')
                t.wait(summary_started.is_set, 'cancel test summary not started')
                t.send('\x1b')
                t.wait(lambda: 'compaction cancelled' in screen(t), 'cancel message missing')
                assert saved(workspace)['messages'] == before['messages'], 'cancel changed context'
                release.set(); hold_summary = False
                t.send('/compact\r')
                t.wait(lambda: 'Context compacted:' in screen(t), 'manual compact missing', timeout=10)
                t.pump(.2)
                after = saved(workspace)
                assert len(json.dumps(after['messages'])) < len(json.dumps(before['messages']))
                assert [m for m in after['messages'] if m['role'] == 'user'][-1]['content'] == 'latest prompt'
                assert after['history'][:len(before['history'])] == before['history'], 'visible history lost'
                assert set((workspace / '.takiza/checkpoints').glob('*.json')) == checkpoints
            else:
                assert len(requests) == 3, 'automatic request was not retried once'
                assert requests[-1]['messages'][-1]['content'] == 'latest prompt'
                assert requests[-1]['messages'][0] == requests[1]['messages'][0], 'system instructions changed'
                assert len(json.dumps(requests[-1]['messages'])) < len(json.dumps(requests[1]['messages']))
                assert any('Context window exceeded' in item.get('text', '') for item in before['history']) or b'compacting automatically' in t.output
            assert summary_requests
            assert all('tools' not in request and request['max_tokens'] == 2048 for request in summary_requests)
            t.send('/exit\r')
            t.wait(lambda: t.process.poll() is not None, 'exit did not finish')
            assert t.process.returncode == 0
        finally:
            release.set(); t.close()

if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        for scenario in ['manual', 'auto']: run(binary, server.server_port, scenario)
        print('PASS compaction: manual, automatic retry, bounded chunks, history/prompt preservation, failure, cancellation, persistence')
    finally: server.shutdown()
