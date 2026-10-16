"""Real parallel helper requests, context/tool isolation, usage and live cancellation."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading

from tui_queue_pty import Terminal

requests = []
helpers_started = set()
helpers_release = threading.Event()
slow_started = threading.Event()
slow_release = threading.Event()


class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        if not body.get('stream'):
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"Helpers test"}}]}')
            return
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        requests.append(body)
        system = body['messages'][0]['content']
        user_index = max(i for i, item in enumerate(body['messages']) if item['role'] == 'user')
        prompt = body['messages'][user_index]['content']
        results = [item for item in body['messages'][user_index + 1:] if item['role'] == 'tool']
        worker = system.startswith('TAKIZA_SUBAGENT_ROLE:')
        calls = []
        if worker:
            profile = system.splitlines()[0].split(': ')[1]
            if 'slow task' in prompt:
                slow_started.set()
                slow_release.wait(20)
            elif not results:
                helpers_started.add(profile)
                helpers_release.wait(15)
            if not results:
                calls = [('read_file', {'path': 'input.txt'})]
            else:
                content = profile.upper() + '_REPORT input.txt:1 evidence checked'
        else:
            cancel_case = 'cancel_case' in prompt or 'stop_parent' in prompt
            spawned = [json.loads(item['content'])['id'] for item in results
                       if item.get('name') == 'spawn_agent' and '"id"' in item['content']]
            waited = [item for item in results if item.get('name') == 'wait_agent']
            if not results:
                if cancel_case:
                    calls = [('spawn_agent', {'profile': 'explore', 'task': 'slow task; inspect input.txt'})]
                else:
                    calls = [
                        ('spawn_agent', {'profile': 'explore', 'task': 'Investigate input.txt', 'context': 'Find evidence'}),
                        ('spawn_agent', {'profile': 'reviewer', 'task': 'Review input.txt', 'context': 'Look for concrete defects'}),
                        ('spawn_agent', {'profile': 'explore', 'task': 'extra helper should hit capacity'}),
                    ]
            elif spawned and not waited:
                calls = [('wait_agent', {'id': id}) for id in spawned]
            else:
                content = 'SUBAGENTS_CANCELLED' if cancel_case else 'SUBAGENTS_DONE'
        if calls:
            delta = {'tool_calls': [{'index': i, 'id': f'call_{len(requests)}_{i}', 'type': 'function',
                'function': {'name': name, 'arguments': json.dumps(args)}} for i, (name, args) in enumerate(calls)]}
            finish = 'tool_calls'
        else:
            delta, finish = {'content': content}, 'stop'
        try:
            data = {'choices': [{'delta': delta, 'finish_reason': finish}],
                    'usage': {'prompt_tokens': 10, 'completion_tokens': 3, 'total_tokens': 13}}
            self.wfile.write(('data: ' + json.dumps(data) + '\n\ndata: [DONE]\n\n').encode())
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def run(binary, mode):
    requests.clear()
    helpers_started.clear()
    helpers_release.clear()
    slow_started.clear()
    slow_release.clear()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-subagents-') as directory:
            workspace = Path(directory)
            (workspace / '.takiza').mkdir()
            (workspace / 'input.txt').write_text('source evidence')
            (workspace / '.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber', 'language': 'en',
                'mode': 'manual', 'execution_mode': mode, 'model': 'test-model', 'api_key': 'test-only',
                'base_url': f'http://127.0.0.1:{server.server_port}/v1',
            }))
            terminal = Terminal(binary, workspace, 110, 32, initial_prompt=None, controlling_tty=True)
            try:
                terminal.wait(lambda: b'Shift+Tab' in terminal.output, 'editor missing')
                terminal.send('parallel TOP_SECRET_PARENT_CONTEXT\r')
                terminal.wait(lambda: helpers_started == {'explore', 'reviewer'}, 'helpers did not start concurrently', timeout=10)
                terminal.wait(lambda: any('Two subagents are already running' in json.dumps(body) for body in requests),
                              'concurrency limit was not enforced')
                terminal.send('/agents\r')
                terminal.wait(lambda: b'agent-1' in terminal.output and b'agent-2' in terminal.output, 'live helpers not listed')
                helpers_release.set()
                terminal.wait(lambda: b'SUBAGENTS_DONE' in terminal.output, 'parent did not receive helper reports', timeout=15)
                children = [body for body in requests if body['messages'][0]['content'].startswith('TAKIZA_SUBAGENT_ROLE:')]
                assert len(children) == 4, 'unexpected helper turn count'
                for body in children:
                    names = {tool['function']['name'] for tool in body['tools']}
                    assert {'read_file', 'grep_search'} <= names
                    assert not {'write_file', 'edit_file', 'run_command', 'ask_question', 'spawn_agent'} & names
                    assert not any(name.startswith('mcp__') for name in names)
                    assert 'TOP_SECRET_PARENT_CONTEXT' not in json.dumps(body), 'parent conversation leaked into helper'
                root_last = next(body for body in reversed(requests) if not body['messages'][0]['content'].startswith('TAKIZA_SUBAGENT_ROLE:'))
                root_results = [json.loads(item['content']) for item in root_last['messages'] if item.get('name') == 'wait_agent']
                assert len(root_results) == 2 and all(item['status'] == 'completed' for item in root_results)
                assert all(item['tokens'] == 26 and 'input.txt:1' in item['report'] for item in root_results)
                def recorded_usage():
                    for path in (workspace / '.takiza/sessions').glob('*.json'):
                        try:
                            usage = json.loads(path.read_text()).get('token_usage') or {}
                        except json.JSONDecodeError:
                            continue
                        if usage.get('total') == 91 and usage.get('complete'):
                            return True
                    return False
                terminal.wait(recorded_usage, 'session usage did not include the 52 helper tokens')
                terminal.send('/agents agent-1\r')
                terminal.send('\x0f')  # Expand the shared command preview to inspect the full report.
                terminal.wait(lambda: b'EXPLORE_REPORT' in terminal.output, 'helper report not inspectable')

                terminal.send('cancel_case\r')
                terminal.wait(slow_started.is_set, 'slow helper not started', timeout=10)
                terminal.output = b''
                terminal.send('/agents stop agent-3\r')
                terminal.wait(lambda: b'SUBAGENTS_CANCELLED' in terminal.output, 'cancelling helper stalled parent', timeout=10)
                terminal.send('/agents agent-3\r')
                terminal.wait(lambda: b'cancelled' in terminal.output, 'cancelled helper not inspectable')
                slow_started.clear()
                terminal.send('stop_parent\r')
                terminal.wait(slow_started.is_set, 'second slow helper not started', timeout=10)
                terminal.send('\x03')
                terminal.pump(.5)
                terminal.send('/agents agent-4\r')
                terminal.wait(lambda: b'agent-4' in terminal.output and b'cancelled' in terminal.output,
                              'stopping parent did not stop helper', timeout=10)
                assert (workspace / 'input.txt').read_text() == 'source evidence'
                assert not (workspace / 'command-ran').exists()
            finally:
                helpers_release.set()
                slow_release.set()
                terminal.close()
    finally:
        server.shutdown()
        server.server_close()
    print(f'subagents TUI ({mode}): OK')


if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    for mode in ['allow-all', 'plan']:
        run(binary, mode)
