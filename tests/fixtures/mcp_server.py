"""Local deterministic MCP peer for stdio and Streamable HTTP integration tests."""
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

initialized = False
cancelled = []
calls = 0


def handle(request):
    global initialized, calls
    method = request['method']
    params = request.get('params', {})
    if method == 'notifications/initialized':
        initialized = True
        return
    if method == 'notifications/cancelled':
        cancelled.append(params['requestId'])
        return
    if method == 'initialize':
        if os.environ.get('MCP_TEST_HANG'):
            import time
            time.sleep(20)
        result = {'protocolVersion': '2025-03-26', 'capabilities': {'tools': {}},
                  'serverInfo': {'name': 'fixture', 'version': '1'}}
    elif method == 'tools/list':
        assert initialized
        name = 'write' if params.get('cursor') else 'echo'
        result = {'tools': [{'name': name, 'description': name,
                            'inputSchema': {'type': 'object', 'properties': {'text': {'type': 'string'}}},
                            'annotations': {'readOnlyHint': True}}]}
        if name == 'echo':
            result['nextCursor'] = 'page2'
    elif method == 'tools/call':
        calls += 1
        args = params['arguments']
        if args.get('hang'):
            return
        if args.get('crash'):
            os._exit(3)
        if args.get('protocol_error'):
            return {'jsonrpc': '2.0', 'id': request['id'], 'error': {'code': -32602, 'message': 'bad arguments'}}
        result = {'content': [{'type': 'text', 'text': args.get('text', '')}],
                  'structuredContent': {'name': params['name'], 'calls': calls,
                                        'cwd': os.getcwd(), 'env': os.environ.get('MCP_TEST_VALUE'),
                                        'cancelled': list(cancelled)}, 'isError': args.get('fail', False)}
    else:
        return {'jsonrpc': '2.0', 'id': request['id'], 'error': {'code': -32601, 'message': 'unknown method'}}
    return {'jsonrpc': '2.0', 'id': request['id'], 'result': result}


if '--http' in sys.argv:
    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            assert self.headers.get('X-MCP-Test') == 'header-value'
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            response = handle(request)
            if response is None:
                self.send_response(202)
                self.end_headers()
                return
            body = json.dumps(response).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            self.send_response(405)
            self.end_headers()

        def log_message(self, *_):
            pass

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    print(server.server_address[1], flush=True)
    server.serve_forever()
else:
    for line in sys.stdin:
        response = handle(json.loads(line))
        if response is not None:
            print(json.dumps(response), flush=True)
