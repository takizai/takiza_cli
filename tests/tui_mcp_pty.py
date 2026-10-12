"""Dynamic MCP schemas, permission denial, server trust, reset and reconnect."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen

requests = []
class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        data = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200)
        if not data.get('stream'):
            self.send_header('Content-Type', 'application/json'); self.end_headers()
            self.wfile.write(b'{"choices":[{"message":{"content":"MCP test"}}]}'); return
        requests.append(data)
        self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
        last = next((m for m in reversed(data['messages']) if m['role'] != 'system'), {})
        index = len(requests)
        if last.get('role') == 'tool':
            delta = {'content': f'DONE_{index}'}; finish = 'stop'
        else:
            delta = {'tool_calls': [{'index': 0, 'id': f'call_{index}', 'type': 'function', 'function': {
                'name': 'mcp__demo__echo', 'arguments': json.dumps({'text': f'ARGUMENT_{index}'})}}]}
            finish = 'tool_calls'
        event = {'choices': [{'delta': delta, 'finish_reason': finish}]}
        self.wfile.write(('data: ' + json.dumps(event) + '\n\ndata: [DONE]\n\n').encode()); self.wfile.flush()

def run(binary):
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-mcp-pty-') as directory:
            workspace = Path(directory); (workspace / '.takiza').mkdir()
            log = workspace / 'calls.jsonl'
            fixture = Path(__file__).parent / 'fixtures/integration_server.py'
            config = {'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber', 'mode': 'manual',
                'model': 'test-model', 'api_key': 'test-only', 'base_url': f'http://127.0.0.1:{server.server_port}/v1',
                'mcpServers': {'demo': {'transport': 'stdio', 'command': sys.executable,
                    'args': [str(fixture.resolve()), 'mcp', '--log', str(log)]}}}
            (workspace / '.takiza/config.json').write_text(json.dumps(config))
            t = Terminal(binary, workspace, 110, 40, initial_prompt='call fixture')
            def screen(): return '\n'.join(Screen(t.output, 110, 40).text)
            def count(method): return sum(json.loads(line).get('method') == method for line in log.read_text().splitlines()) if log.exists() else 0
            try:
                t.wait(lambda: 'MCP tool permission' in screen(), 'MCP approval missing')
                assert 'MCP server: demo' in screen() and 'Tool: echo' in screen() and 'ARGUMENT_1' in screen(), screen()
                assert count('tools/call') == 0
                t.send('\x1b[B\r'); t.wait(lambda: 'DONE_2' in screen(), 'denied result missing')
                assert count('tools/call') == 0
                denied = screen()
                assert 'MCP · demo / echo' in denied and 'Declined by you. Nothing was executed.' in denied, denied
                assert 'Command execution denied' not in denied and 'approval denied' not in denied and '· failed' not in denied, denied
                assert any(x['function']['name'] == 'mcp__demo__echo' for x in requests[0]['tools'])
                t.send('call again\r'); t.wait(lambda: 'MCP tool permission' in screen(), 'second approval missing')
                t.send('\x1b[B\x1b[B\r'); t.wait(lambda: 'DONE_4' in screen(), 'trusted call failed')
                assert count('tools/call') == 1
                t.send('/reset\r'); t.pump(.3); t.send('call after reset\r')
                t.wait(lambda: 'DONE_6' in screen(), 'trust or runtime lost at reset')
                assert count('initialize') == 1 and count('tools/call') == 2
                t.send('/tools\r'); t.wait(lambda: 'Autonomous Agent Tools' in screen(), 'registry explorer missing')
                for _ in range(12): t.send('\x1b[B')
                t.pump(.2); assert 'mcp__demo__' in screen(), screen()
                t.send('\x1b'); t.pump(.2); t.send('/mcp disconnect demo\r'); t.pump(.3)
                t.send('/mcp connect demo\r'); t.pump(.5); assert count('initialize') == 2
                t.send('call after reconnect\r'); t.wait(lambda: 'MCP tool permission' in screen(), 'disconnect did not revoke trust')
                t.send('\x1b'); t.wait(lambda: 'DONE_8' in screen(), 'escape did not deny')
                t.send('/exit\r'); t.wait(lambda: t.process.poll() is not None, 'exit stalled')
                assert t.process.returncode == 0
                print('PASS MCP: schemas, denial, trust, reset, scrolling, reconnect, exit')
            except Exception:
                Path('/tmp/takiza-mcp-pty.log').write_bytes(t.output); raise
            finally: t.close()
    finally: server.shutdown(); server.server_close(); thread.join(timeout=2)

def run_management_cancel(binary):
    with tempfile.TemporaryDirectory(prefix='takiza-mcp-cancel-') as directory:
        workspace = Path(directory)
        (workspace / '.takiza').mkdir()
        (workspace / '.takiza/config.json').write_text(json.dumps({
            'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber', 'mode': 'manual',
            'model': 'test-model', 'api_key': 'test-only', 'base_url': 'http://127.0.0.1:1/v1',
            'mcpServers': {'slow': {'transport': 'stdio', 'command': sys.executable,
                'args': ['-c', "import time; time.sleep(60)"]}}}))
        t = Terminal(binary, workspace, 100, 30, initial_prompt=None)
        def screen(): return '\n'.join(Screen(t.output, 100, 30).text)
        try:
            t.wait(lambda: 'You' in screen(), 'initial editor missing')
            t.send('/mcp connect slow\r')
            t.wait(lambda: 'Managing integration' in screen(), 'management activity missing')
            t.send('\x1b')
            t.wait(lambda: 'Integration management cancelled.' in screen(), 'management did not cancel')
            t.send('/exit\r')
            t.wait(lambda: t.process.poll() is not None, 'exit after cancelled connection stalled')
            assert t.process.returncode == 0
            print('PASS MCP: cancel slow connection, restore editor, exit')
        finally: t.close()

if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    run(binary)
    run_management_cancel(binary)
