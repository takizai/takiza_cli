"""Assistant narration and reasoning survive tool transitions and /tools."""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
from tui_queue_pty import Terminal, Screen

finish = threading.Event()
requests = []
class API(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.send_response(200); self.end_headers()
        if getattr(self.server, 'repair', False) and body.get('tools'):
            requests.append(body)
            if not body.get('stream'):
                self.wfile.write(json.dumps({'choices': [{'message': {'tool_calls': [{'id': 'repaired',
                    'type': 'function', 'function': {'name': 'mcp__demo__echo',
                        'arguments': json.dumps({'text': 'MCP_REPAIR_RESULT'})}}]}}]}).encode())
                return
            if len(requests) <= 2:
                delta = {'content': 'NARRATION_BEFORE_MCP', 'tool_calls': [{'index': 0,
                    'id': 'broken', 'function': {'name': 'mcp__demo__echo', 'arguments': ''}}]}
                delta['reasoning_content'] = f'REASONING_BEFORE_MCP_{len(requests)}'
            else:
                finish.wait(10)
                delta = {'content': 'FINAL_REPAIRED_TEXT'}
            try:
                self.wfile.write(('data: ' + json.dumps({'choices': [{'delta': delta}]}) + '\n\ndata: [DONE]\n\n').encode())
            except (BrokenPipeError, ConnectionResetError): pass
            return
        if not body.get('stream'):
            self.wfile.write(b'{"choices":[{"message":{"content":"Tool text test"}}]}'); return
        requests.append(body)
        step = len(requests)
        if step <= 2:
            delta = {'content': f'NARRATION_{step}', 'tool_calls': [{'index': 0, 'id': f'call_{step}', 'function': {'name': 'list_dir', 'arguments': '{}'}}]}
            if self.server.reasoning: delta['reasoning_content'] = f'REASONING_{step}'
        else:
            finish.wait(10)
            delta = {'content': 'FINAL_TOOL_TEXT'}
        try:
            self.wfile.write(('data: ' + json.dumps({'choices': [{'delta': delta}]}) + '\n\ndata: [DONE]\n\n').encode())
        except (BrokenPipeError, ConnectionResetError): pass

def run(binary, reasoning):
    finish.clear(); requests.clear()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    server.reasoning = reasoning
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-tool-text-') as directory:
            workspace = Path(directory)
            (workspace / '.takiza').mkdir()
            (workspace / '.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber', 'mode': 'manual',
                'model': 'test-model', 'api_key': 'test-only', 'base_url': f'http://127.0.0.1:{server.server_port}/v1'}))
            t = Terminal(binary, workspace, 110, 40, initial_prompt='inspect the project')
            def screen(): return '\n'.join(Screen(t.output, 110, 40).text)
            try:
                t.wait(lambda: len(requests) == 3 and 'NARRATION_2' in screen(), 'tool narration lost')
                assert 'NARRATION_1' in screen(), 'earlier narration disappeared at next tool'
                if reasoning:
                    assert 'REASONING_1' in screen() and 'REASONING_2' in screen(), 'reasoning was overwritten'
                finish.set()
                t.wait(lambda: 'FINAL_TOOL_TEXT' in screen(), 'final answer missing')
                t.pump(.3)
                t.send('/tools\r')
                t.pump(.3)
                t.send('\x1b')
                t.wait(lambda: 'FINAL_TOOL_TEXT' in screen(), '/tools did not restore conversation')
                assert 'NARRATION_1' in screen() and 'NARRATION_2' in screen()
                t.send('/exit\r')
                t.wait(lambda: t.process.poll() is not None, 'exit failed')
                session_id = (workspace / '.takiza/latest_session').read_text()
                saved = json.loads((workspace / '.takiza/sessions' / (session_id + '.json')).read_text())
                history = saved['history']
                for text in ['NARRATION_1', 'NARRATION_2', 'FINAL_TOOL_TEXT']:
                    assert sum(item.get('AssistantMessage') == text for item in history) == 1, (text, history)
                if reasoning:
                    assert {'REASONING_1', 'REASONING_2'} <= {item.get('Thought') for item in history}
                else:
                    assert not any(item.get('Thought', '').startswith('NARRATION_') for item in history)
                print(f'PASS tool text: reasoning={reasoning}, narration, multiple tools, /tools, saved history')
            finally: finish.set(); t.close()
    finally: finish.set(); server.shutdown()

def run_mcp_repair(binary):
    finish.clear(); requests.clear()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), API)
    server.repair = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='takiza-mcp-narration-') as directory:
            workspace = Path(directory); (workspace / '.takiza').mkdir()
            fixture = Path(__file__).parent / 'fixtures/integration_server.py'
            log = workspace / 'calls.jsonl'
            (workspace / '.takiza/config.json').write_text(json.dumps({
                'agreed_to_terms': True, 'terms_version': '1.0.0', 'theme': 'Amber', 'mode': 'manual',
                'model': 'test-model', 'api_key': 'test-only', 'auto_approve': True,
                'base_url': f'http://127.0.0.1:{server.server_port}/v1',
                'mcpServers': {'demo': {'transport': 'stdio', 'command': sys.executable,
                    'args': [str(fixture.resolve()), 'mcp', '--log', str(log)]}}}))
            t = Terminal(binary, workspace, 110, 40, initial_prompt='test MCP')
            def screen(): return '\n'.join(Screen(t.output, 110, 40).text)
            try:
                t.wait(lambda: len(requests) == 4 and 'MCP_REPAIR_RESULT' in screen(), 'repaired MCP call missing', timeout=8)
                assert screen().count('NARRATION_BEFORE_MCP') == 1, screen()
                assert 'REASONING_BEFORE_MCP_1' in screen() and 'REASONING_BEFORE_MCP_2' in screen(), screen()
                calls = [json.loads(line) for line in log.read_text().splitlines()]
                assert sum(call.get('method') == 'tools/call' for call in calls) == 1
                assert not requests[2].get('stream'), 'fallback was not exercised'
                finish.set()
                t.wait(lambda: 'FINAL_REPAIRED_TEXT' in screen(), 'final repaired answer missing')
                assert screen().count('NARRATION_BEFORE_MCP') == 1
                t.send('/exit\r'); t.wait(lambda: t.process.poll() is not None, 'exit failed')
                sid = (workspace / '.takiza/latest_session').read_text()
                saved = json.loads((workspace / '.takiza/sessions' / (sid + '.json')).read_text())
                assert sum(item.get('AssistantMessage') == 'NARRATION_BEFORE_MCP' for item in saved['history']) == 1
                assert {'REASONING_BEFORE_MCP_1', 'REASONING_BEFORE_MCP_2'} <= {item.get('Thought') for item in saved['history']}
                print('PASS MCP narration: invalid calls, retry deduplication, nonstream repair, one execution, saved history')
            finally: finish.set(); t.close()
    finally: finish.set(); server.shutdown()

if __name__ == '__main__':
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/takiza').resolve()
    for reasoning in [False, True]: run(binary, reasoning)
    run_mcp_repair(binary)
